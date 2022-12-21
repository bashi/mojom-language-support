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

use super::diagnostic::get_offset_from_position;
use crate::syntax::{self, Module, MojomFile};

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
pub(crate) enum DocumentSymbol {
    Method(MethodSymbol),
    Interface(InterfaceSymbol),
}

impl DocumentSymbol {
    pub(crate) fn is_lsp_kind(&self, kind: lsp_types::SymbolKind) -> bool {
        match self {
            Self::Method(_) => kind == lsp_types::SymbolKind::METHOD,
            Self::Interface(_) => kind == lsp_types::SymbolKind::CLASS,
        }
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Method(method) => &method.name,
            Self::Interface(interface) => &interface.name,
        }
    }
}

#[derive(Debug)]
pub(crate) struct MojomAst {
    pub(crate) uri: lsp_types::Url,
    pub(crate) text: String,
    pub(crate) mojom: MojomFile,

    module: Option<Module>,
}

impl MojomAst {
    #[cfg(test)]
    pub(crate) fn from_path(path: impl AsRef<std::path::Path>) -> anyhow::Result<MojomAst> {
        let text = std::fs::read_to_string(path.as_ref())?;
        let mojom =
            syntax::parse(&text).map_err(|err| anyhow::anyhow!("Failed to parse: {:?}", err))?;
        let analytics = super::semantic::check_semantics(&text, &mojom);
        let uri = lsp_types::Url::from_file_path(path)
            .map_err(|err| anyhow::anyhow!("Failed to convert path to Uri: {:?}", err))?;
        Ok(MojomAst {
            uri,
            text,
            mojom,
            module: analytics.module,
        })
    }

    pub(crate) fn from_mojom(
        uri: lsp_types::Url,
        text: String,
        mojom: MojomFile,
        module: Option<Module>,
    ) -> MojomAst {
        MojomAst {
            uri,
            text,
            mojom,
            module,
        }
    }

    pub(crate) fn text(&self, field: &syntax::Range) -> &str {
        // Can panic.
        &self.text[field.start..field.end]
    }

    pub(crate) fn line_col(&self, offset: usize) -> syntax::LineCol {
        // Can panic.
        syntax::line_col(&self.text, offset).unwrap()
    }

    pub(crate) fn module_name(&self) -> Option<&str> {
        self.module
            .as_ref()
            .map(|ref module| self.text(&module.name))
    }

    pub(crate) fn lsp_range(&self, field: &syntax::Range) -> lsp_types::Range {
        let pos = self.line_col(field.start);
        let start = lsp_types::Position::new(pos.line as u32, pos.col as u32);
        let pos = self.line_col(field.end);
        let end = lsp_types::Position::new(pos.line as u32, pos.col as u32);
        lsp_types::Range::new(start, end)
    }

    #[allow(unused)]
    pub(crate) fn find_symbol_from_position(
        &self,
        position: &lsp_types::Position,
    ) -> Option<DocumentSymbol> {
        let offset = get_offset_from_position(&self.text, position);
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
