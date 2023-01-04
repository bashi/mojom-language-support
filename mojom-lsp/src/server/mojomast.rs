// Copyright 2020 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::document_symbol::{
    ConstSymbol, DocumentSymbol, EnumSymbol, InterfaceSymbol, MethodSymbol, StructSymbol,
    UnionSymbol,
};
use crate::syntax::{self, MojomFile, Traversal};

#[derive(Debug)]
pub(crate) struct MojomAst {
    uri: lsp_types::Url,
    text: String,
    mojom: MojomFile,
}

impl MojomAst {
    #[cfg(test)]
    pub(crate) fn from_path(path: impl AsRef<std::path::Path>) -> anyhow::Result<MojomAst> {
        let text = std::fs::read_to_string(path.as_ref())?;
        let mojom =
            syntax::parse(&text).map_err(|err| anyhow::anyhow!("Failed to parse: {:?}", err))?;
        let uri = lsp_types::Url::from_file_path(path)
            .map_err(|err| anyhow::anyhow!("Failed to convert path to Uri: {:?}", err))?;
        Ok(MojomAst { uri, text, mojom })
    }

    pub(crate) fn from_mojom(uri: lsp_types::Url, text: String, mojom: MojomFile) -> MojomAst {
        MojomAst { uri, text, mojom }
    }

    // SAFETY: Only called from `self`.
    fn text(&self, field: &syntax::Range) -> &str {
        &self.text[field.start..field.end]
    }

    // SAFETY: Only called from `self`.
    fn line_col(&self, offset: usize) -> syntax::LineCol {
        syntax::line_col(&self.text, offset).unwrap()
    }

    // SAFETY: Only called from `self`.
    fn lsp_range(&self, field: &syntax::Range) -> lsp_types::Range {
        let pos = self.line_col(field.start);
        let start = lsp_types::Position::new(pos.line as u32, pos.col as u32);
        let pos = self.line_col(field.end);
        let end = lsp_types::Position::new(pos.line as u32, pos.col as u32);
        lsp_types::Range::new(start, end)
    }

    pub(crate) fn get_identifier(&self, pos: &lsp_types::Position) -> &str {
        // TODO: The current implementation isn't accurate.

        let is_identifier_char =
            |ch: char| -> bool { ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' };

        let offset = self.get_offset_from_position(pos);
        let mut s = offset;
        for ch in self.text[..offset].chars().rev() {
            if !is_identifier_char(ch) {
                break;
            }
            s -= 1;
        }
        let mut e = offset;
        for ch in self.text[offset..].chars() {
            if !is_identifier_char(ch) {
                break;
            }
            e += 1;
        }
        &self.text[s..e]
    }

    pub(crate) fn find_symbol_from_position(
        &self,
        position: &lsp_types::Position,
    ) -> Option<DocumentSymbol> {
        let offset = self.get_offset_from_position(position);
        let interfaces = self.mojom.stmts.iter().filter_map(|stmt| match stmt {
            syntax::Statement::Interface(interface) => Some(interface),
            _ => None,
        });

        for interface in interfaces {
            if !interface.range.contains(offset) {
                continue;
            }
            if interface.name.contains(offset) {
                return Some(DocumentSymbol::Interface(InterfaceSymbol {
                    name: self.text(&interface.name).to_string(),
                    range: self.lsp_range(&interface.range),
                }));
            }

            let methods = interface.members.iter().filter_map(|member| match member {
                syntax::InterfaceMember::Method(method) => Some(method),
                _ => None,
            });
            for method in methods {
                if method.name.contains(offset) {
                    let interface_name = self.text(&interface.name).to_string();
                    let name = self.text(&method.name).to_string();
                    let range = self.lsp_range(&method.range);
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

    pub(crate) fn get_document_symbols(&self) -> Vec<DocumentSymbol> {
        let mut symbols = Vec::new();
        let mut interface = None;

        macro_rules! push_symbol {
            ($kind:tt, $typ:ident, $node:expr) => {
                let name = self.text(&$node.name).to_string();
                let range = self.lsp_range(&$node.range);
                let symbol = $typ { name, range };
                symbols.push(DocumentSymbol::$kind(symbol));
            };
        }

        for traversal in syntax::preorder(&self.mojom) {
            match traversal {
                Traversal::EnterInterface(node) => {
                    let name = self.text(&node.name).to_string();
                    let range = self.lsp_range(&node.range);
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
                    let name = self.text(&node.name).to_string();
                    let range = self.lsp_range(&node.range);
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

    pub(crate) fn find_definition(&self, ident: &str) -> Option<lsp_types::Location> {
        let mut path = Vec::new();
        for traversal in syntax::preorder(&self.mojom) {
            let loc = match traversal {
                Traversal::EnterInterface(node) => {
                    let loc = self.match_field(ident, &node.name, &mut path);
                    let name = self.text(&node.name);
                    path.push(name);
                    loc
                }
                Traversal::LeaveInterface(_) => {
                    path.pop();
                    None
                }
                Traversal::EnterStruct(node) => {
                    let loc = self.match_field(ident, &node.name, &mut path);
                    let name = self.text(&node.name);
                    path.push(name);
                    loc
                }
                Traversal::LeaveStruct(_) => {
                    path.pop();
                    None
                }
                Traversal::Union(node) => self.match_field(ident, &node.name, &mut path),
                Traversal::Enum(node) => self.match_field(ident, &node.name, &mut path),
                Traversal::Const(node) => self.match_field(ident, &node.name, &mut path),
                _ => None,
            };
            if loc.is_some() {
                return loc;
            }
        }
        None
    }

    fn match_field<'a, 'b, 'c>(
        &'a self,
        target: &'a str,
        field: &'b syntax::Range,
        path: &'c mut Vec<&'a str>,
    ) -> Option<lsp_types::Location> {
        let name = self.text(field);
        path.push(name);
        let ident = path.join(".");
        path.pop();
        if ident == target {
            let range = self.lsp_range(field);
            return Some(lsp_types::Location::new(self.uri.clone(), range));
        }
        None
    }

    fn get_offset_from_position(&self, pos: &lsp_types::Position) -> usize {
        let pos_line = pos.line as usize;
        let pos_col = pos.character as usize;
        let mut offset = 0;
        for (i, line) in self.text.lines().enumerate() {
            if i == pos_line {
                break;
            }
            offset += line.len() + 1;
        }
        offset + pos_col
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_symbol_from_position() -> anyhow::Result<()> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("my_interface.mojom");
        let ast = MojomAst::from_path(&path)?;

        let expected = Some(DocumentSymbol::Interface(InterfaceSymbol {
            name: "MyInterface".to_string(),
            range: lsp_types::Range::new(
                lsp_types::Position::new(3, 0),
                lsp_types::Position::new(8, 2),
            ),
        }));
        let symbol = ast.find_symbol_from_position(&lsp_types::Position {
            line: 3,
            character: 15,
        });
        assert_eq!(symbol, expected);

        let expected = Some(DocumentSymbol::Method(MethodSymbol {
            name: "DoSomething".to_string(),
            interface_name: "MyInterface".to_string(),
            range: lsp_types::Range::new(
                lsp_types::Position::new(7, 4),
                lsp_types::Position::new(7, 31),
            ),
        }));
        let symbol = ast.find_symbol_from_position(&lsp_types::Position {
            line: 7,
            character: 4,
        });
        assert_eq!(symbol, expected);
        let symbol = ast.find_symbol_from_position(&lsp_types::Position {
            line: 7,
            character: 14,
        });
        assert_eq!(symbol, expected);

        let symbol = ast.find_symbol_from_position(&lsp_types::Position {
            line: 7,
            character: 15,
        });
        assert_eq!(symbol, None);

        Ok(())
    }
}
