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

use std::path::Path;

use lsp_types::Url as Uri;

use super::document_symbol::{
    ConstSymbol, DocumentSymbol, EnumSymbol, InterfaceSymbol, MethodSymbol, StructSymbol,
    UnionSymbol,
};
use crate::syntax::{self, MojomFile, Traversal};

fn create_semantics_diagnostic(range: lsp_types::Range, message: String) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic {
        range,
        severity: Some(lsp_types::DiagnosticSeverity::ERROR),
        code: Some(lsp_types::NumberOrString::String("mojom".to_owned())),
        source: Some("mojom-lsp".to_owned()),
        message,
        ..Default::default()
    }
}

pub(crate) struct SemanticsResult {
    pub(crate) module_name: Option<String>,
    pub(crate) import_uris: Vec<Uri>,
    pub(crate) diagnostics: Vec<lsp_types::Diagnostic>,
}

#[derive(Debug)]
pub(crate) struct MojomAst {
    text: String,
    mojom: MojomFile,
}

impl MojomAst {
    #[cfg(test)]
    pub(crate) fn from_path(path: impl AsRef<std::path::Path>) -> anyhow::Result<MojomAst> {
        let text = std::fs::read_to_string(path.as_ref())?;
        let mojom =
            syntax::parse(&text).map_err(|err| anyhow::anyhow!("Failed to parse: {:?}", err))?;
        Ok(MojomAst { text, mojom })
    }

    pub(crate) fn from_mojom(text: String, mojom: MojomFile) -> MojomAst {
        MojomAst { text, mojom }
    }

    // SAFETY: Only called from `self`.
    fn text(&self, field: &syntax::Range) -> &str {
        &self.text[field.start..field.end]
    }

    // SAFETY: Only called from `self`.
    fn lsp_range(&self, range: &syntax::Range) -> lsp_types::Range {
        let pos = syntax::line_col(&self.text, range.start).unwrap();
        let start = lsp_types::Position::new(pos.line as u32, pos.col as u32);
        let pos = syntax::line_col(&self.text, range.end).unwrap();
        let end = lsp_types::Position::new(pos.line as u32, pos.col as u32);
        lsp_types::Range::new(start, end)
    }

    pub(crate) fn check_semantics(&self, root_path: impl AsRef<Path>) -> SemanticsResult {
        let mut diagnostics = Vec::new();

        // Find module name.
        let module_name = {
            let mut modules = self.mojom.stmts.iter().filter_map(|stmt| match stmt {
                syntax::Statement::Module(module) => Some(module),
                _ => None,
            });

            let first_module = modules.next();
            for invalid_module in modules {
                let range = self.lsp_range(&invalid_module.range);
                let message = format!(
                    "Found more than one module statement: {}",
                    self.text(&invalid_module.name),
                );
                diagnostics.push(create_semantics_diagnostic(range, message));
            }

            first_module.map(|module| self.text(&module.name).to_string())
        };

        // Imports
        let import_stmts = self.mojom.stmts.iter().filter_map(|stmt| match stmt {
            syntax::Statement::Import(import) => Some(import),
            _ => None,
        });

        let mut import_uris = Vec::new();
        for import in import_stmts {
            let path = self.text(&import.path);
            // `import.path` include double quotes.
            let path = &path[1..path.len() - 1];
            let path = match root_path.as_ref().join(path).canonicalize() {
                Ok(path) => path,
                Err(err) => {
                    let range = self.lsp_range(&import.range);
                    let message = format!("Failed to find import path: {}: {:?}", path, err);
                    diagnostics.push(create_semantics_diagnostic(range, message));
                    continue;
                }
            };

            if !path.exists() {
                let range = self.lsp_range(&import.range);
                let message = format!("Import path does not exist: {:?}", path);
                diagnostics.push(create_semantics_diagnostic(range, message));
                continue;
            }

            let uri = Uri::from_file_path(path).unwrap();
            import_uris.push(uri);
        }

        SemanticsResult {
            module_name,
            import_uris,
            diagnostics,
        }
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
                    name_range: self.lsp_range(&interface.name),
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
                        name_range: self.lsp_range(&method.name),
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
                let name_range = self.lsp_range(&$node.name);
                let range = self.lsp_range(&$node.range);
                let symbol = $typ {
                    name,
                    name_range,
                    range,
                };
                symbols.push(DocumentSymbol::$kind(symbol));
            };
        }

        for traversal in syntax::preorder(&self.mojom) {
            match traversal {
                Traversal::EnterInterface(node) => {
                    let name = self.text(&node.name).to_string();
                    let name_range = self.lsp_range(&node.name);
                    let range = self.lsp_range(&node.range);
                    debug_assert!(interface.is_none());
                    interface = Some(name.clone());
                    let symbol = InterfaceSymbol {
                        name,
                        name_range,
                        range,
                    };
                    symbols.push(DocumentSymbol::Interface(symbol));
                }
                Traversal::LeaveInterface(_) => {
                    debug_assert!(interface.is_some());
                    interface = None;
                }
                Traversal::Method(node) => {
                    debug_assert!(interface.is_some());
                    let interface_name = interface.as_ref().unwrap().clone();
                    let name_range = self.lsp_range(&node.name);
                    let name = self.text(&node.name).to_string();
                    let range = self.lsp_range(&node.range);
                    let symbol = MethodSymbol {
                        interface_name,
                        name,
                        name_range,
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

    pub(crate) fn find_definition(&self, ident: &str) -> Option<lsp_types::Range> {
        let mut path = Vec::new();
        for traversal in syntax::preorder(&self.mojom) {
            let range = match traversal {
                Traversal::EnterInterface(node) => {
                    let range = self.match_field(ident, &node.name, &mut path);
                    let name = self.text(&node.name);
                    path.push(name);
                    range
                }
                Traversal::LeaveInterface(_) => {
                    path.pop();
                    None
                }
                Traversal::EnterStruct(node) => {
                    let range = self.match_field(ident, &node.name, &mut path);
                    let name = self.text(&node.name);
                    path.push(name);
                    range
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
            if range.is_some() {
                return range;
            }
        }
        None
    }

    fn match_field<'a, 'b, 'c>(
        &'a self,
        target: &'a str,
        field: &'b syntax::Range,
        path: &'c mut Vec<&'a str>,
    ) -> Option<lsp_types::Range> {
        let name = self.text(field);
        path.push(name);
        let ident = path.join(".");
        path.pop();
        if ident == target {
            let range = self.lsp_range(field);
            return Some(range);
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
            name_range: lsp_types::Range::new(
                lsp_types::Position::new(3, 10),
                lsp_types::Position::new(3, 21),
            ),
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
            name_range: lsp_types::Range::new(
                lsp_types::Position::new(7, 4),
                lsp_types::Position::new(7, 15),
            ),
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
