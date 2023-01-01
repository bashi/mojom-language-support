use crate::syntax::{self, preorder, MojomFile, Traversal};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InterfaceSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MethodSymbol {
    pub(crate) name: String,
    pub(crate) interface_name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct StructSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UnionSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct EnumSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ConstSymbol {
    pub(crate) name: String,
    pub(crate) range: lsp_types::Range,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DocumentSymbol {
    Method(MethodSymbol),
    Interface(InterfaceSymbol),
    Struct(StructSymbol),
    Union(UnionSymbol),
    Enum(EnumSymbol),
    Const(ConstSymbol),
}

impl DocumentSymbol {
    pub(crate) fn name(&self) -> &str {
        match self {
            DocumentSymbol::Method(s) => &s.name,
            DocumentSymbol::Interface(s) => &s.name,
            DocumentSymbol::Struct(s) => &s.name,
            DocumentSymbol::Union(s) => &s.name,
            DocumentSymbol::Enum(s) => &s.name,
            DocumentSymbol::Const(s) => &s.name,
        }
    }

    pub(crate) fn range(&self) -> &lsp_types::Range {
        match self {
            DocumentSymbol::Method(s) => &s.range,
            DocumentSymbol::Interface(s) => &s.range,
            DocumentSymbol::Struct(s) => &s.range,
            DocumentSymbol::Union(s) => &s.range,
            DocumentSymbol::Enum(s) => &s.range,
            DocumentSymbol::Const(s) => &s.range,
        }
    }
}

pub(crate) fn get_document_symbols(text: &str, mojom: &MojomFile) -> Vec<DocumentSymbol> {
    let get_text = |range: &syntax::Range| -> String { text[range.start..range.end].to_string() };
    let lsp_range = |range: &syntax::Range| -> lsp_types::Range {
        let line_col = syntax::line_col(text, range.start).unwrap();
        let start = lsp_types::Position::new(line_col.line as u32, line_col.col as u32);
        let line_col = syntax::line_col(text, range.end).unwrap();
        let end = lsp_types::Position::new(line_col.line as u32, line_col.col as u32);
        lsp_types::Range::new(start, end)
    };

    let mut symbols = Vec::new();
    let mut interface = None;

    macro_rules! push_symbol {
        ($kind:tt, $typ:ident, $node:expr) => {
            let name = get_text(&$node.name);
            let range = lsp_range(&$node.range);
            let symbol = $typ { name, range };
            symbols.push(DocumentSymbol::$kind(symbol));
        };
    }

    for traversal in preorder(mojom) {
        match traversal {
            Traversal::EnterInterface(node) => {
                let name = get_text(&node.name);
                let range = lsp_range(&node.range);
                debug_assert!(interface.is_none());
                interface = Some(name.clone());
                let symbol = InterfaceSymbol { name, range };
                symbols.push(DocumentSymbol::Interface(symbol));
            }
            Traversal::LeaveInterface(_) => {
                debug_assert!(interface.is_some());
                interface = None;
            }
            Traversal::Method(node) => {
                debug_assert!(interface.is_some());
                let interface_name = interface.as_ref().unwrap().clone();
                let name = get_text(&node.name);
                let range = lsp_range(&node.range);
                let symbol = MethodSymbol {
                    interface_name,
                    name,
                    range,
                };
                symbols.push(DocumentSymbol::Method(symbol));
            }
            Traversal::EnterStruct(node) => {
                push_symbol!(Struct, StructSymbol, node);
            }
            Traversal::Union(node) => {
                push_symbol!(Union, UnionSymbol, node);
            }
            Traversal::Enum(node) => {
                push_symbol!(Enum, EnumSymbol, node);
            }
            Traversal::Const(node) => {
                push_symbol!(Const, ConstSymbol, node);
            }
            _ => (),
        }
    }
    symbols
}

pub(crate) fn find_symbol_from_position(
    text: &str,
    mojom: &MojomFile,
    position: &lsp_types::Position,
) -> Option<DocumentSymbol> {
    let get_text = |range: &syntax::Range| -> &str { &text[range.start..range.end] };
    let lsp_range = |range: &syntax::Range| -> lsp_types::Range {
        let line_col = syntax::line_col(text, range.start).unwrap();
        let start = lsp_types::Position::new(line_col.line as u32, line_col.col as u32);
        let line_col = syntax::line_col(text, range.end).unwrap();
        let end = lsp_types::Position::new(line_col.line as u32, line_col.col as u32);
        lsp_types::Range::new(start, end)
    };

    let offset = get_offset_from_position(text, position);
    let interfaces = mojom.stmts.iter().filter_map(|stmt| match stmt {
        syntax::Statement::Interface(interface) => Some(interface),
        _ => None,
    });

    for interface in interfaces {
        if !interface.range.contains(offset) {
            continue;
        }
        if interface.name.contains(offset) {
            return Some(DocumentSymbol::Interface(InterfaceSymbol {
                name: get_text(&interface.name).to_string(),
                range: lsp_range(&interface.range),
            }));
        }

        let methods = interface.members.iter().filter_map(|member| match member {
            syntax::InterfaceMember::Method(method) => Some(method),
            _ => None,
        });
        for method in methods {
            if method.name.contains(offset) {
                let interface_name = get_text(&interface.name).to_string();
                let name = get_text(&method.name).to_string();
                let range = lsp_range(&method.range);
                return Some(DocumentSymbol::Method(MethodSymbol {
                    name,
                    interface_name,
                    range,
                }));
            }
        }
    }

    None
}

pub(crate) fn get_offset_from_position(text: &str, pos: &lsp_types::Position) -> usize {
    let pos_line = pos.line as usize;
    let pos_col = pos.character as usize;
    let mut offset = 0;
    for (i, line) in text.lines().enumerate() {
        if i == pos_line {
            break;
        }
        offset += line.len() + 1;
    }
    offset + pos_col
}

#[inline(always)]
fn is_identifier_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'
}

pub(crate) fn get_identifier<'a>(text: &'a str, pos: &lsp_types::Position) -> &'a str {
    // TODO: The current implementation isn't accurate.

    let offset = get_offset_from_position(text, pos);
    let mut s = offset;
    for ch in text[..offset].chars().rev() {
        if !is_identifier_char(ch) {
            break;
        }
        s -= 1;
    }
    let mut e = offset;
    for ch in text[offset..].chars() {
        if !is_identifier_char(ch) {
            break;
        }
        e += 1;
    }
    &text[s..e]
}
