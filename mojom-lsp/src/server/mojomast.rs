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

use super::document_symbol::{self, DocumentSymbol};
use crate::syntax::{self, Module, MojomFile};

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

    pub(crate) fn find_symbol_from_position(
        &self,
        position: &lsp_types::Position,
    ) -> Option<DocumentSymbol> {
        document_symbol::find_symbol_from_position(&self.text, &self.mojom, position)
    }
}

#[cfg(test)]
mod tests {
    use super::document_symbol::{InterfaceSymbol, MethodSymbol};
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
