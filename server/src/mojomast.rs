use mojom_syntax::Error as ParseError;
use mojom_syntax::{self, parse, Module, MojomFile, Statement};

#[derive(Debug)]
pub(crate) struct MojomAst {
    pub(crate) uri: lsp_types::Url,
    pub(crate) text: String,
    pub(crate) mojom: MojomFile,

    module: Option<Module>,
}

impl MojomAst {
    pub(crate) fn new<S: Into<String>>(
        uri: lsp_types::Url,
        text: S,
    ) -> std::result::Result<MojomAst, ParseError> {
        let text = text.into();
        let mojom = parse(&text)?;
        let module = find_module_stmt(&mojom);
        Ok(MojomAst {
            uri: uri,
            text: text,
            mojom: mojom,
            module: module,
        })
    }

    pub(crate) fn from_mojom(uri: lsp_types::Url, text: String, mojom: MojomFile) -> MojomAst {
        let module = find_module_stmt(&mojom);
        MojomAst {
            uri: uri,
            text: text,
            mojom: mojom,
            module: module,
        }
    }

    pub(crate) fn text(&self, field: &mojom_syntax::Range) -> &str {
        // Can panic.
        &self.text[field.start..field.end]
    }

    pub(crate) fn line_col(&self, offset: usize) -> (usize, usize) {
        // Can panic.
        mojom_syntax::line_col(&self.text, offset).unwrap()
    }

    pub(crate) fn module_name(&self) -> Option<&str> {
        self.module
            .as_ref()
            .map(|ref module| self.text(&module.name))
    }
}

fn find_module_stmt(mojom: &MojomFile) -> Option<Module> {
    // This function assumes that `mojom` has only one Module, which should be
    // checked in semantics analysis.
    for stmt in &mojom.stmts {
        match stmt {
            Statement::Module(module) => {
                return Some(module.clone());
            }
            _ => continue,
        }
    }
    None
}
