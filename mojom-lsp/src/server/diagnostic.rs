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

use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use std::sync::mpsc::{channel, Sender};
use std::thread::{self, JoinHandle};

use lsp_types::Url as Uri;

use crate::syntax;

use super::clangd::ClangdWrapper;
use super::document_symbol::{get_identifier, DocumentSymbol};
use super::imported_files::{check_imports, ImportedFiles};
use super::messagesender::MessageSender;
use super::mojomast::MojomAst;
use super::protocol::NotificationMessage;

pub(crate) fn create_diagnostic(range: lsp_types::Range, message: String) -> lsp_types::Diagnostic {
    lsp_types::Diagnostic {
        range: range,
        severity: Some(lsp_types::DiagnosticSeverity::ERROR),
        code: Some(lsp_types::NumberOrString::String("mojom".to_owned())),
        code_description: None,
        data: None,
        source: Some("mojom-lsp".to_owned()),
        message: message,
        related_information: None,
        tags: None,
    }
}

enum DiagnosticMessage {
    CheckSyntax((Uri, String)),
    DidCloseTextDocument(lsp_types::DidCloseTextDocumentParams),
    GotoDefinition(
        (
            Uri,
            lsp_types::Position,
            Sender<Option<lsp_types::Location>>,
        ),
    ),
    GotoImplementation(
        (
            lsp_types::request::GotoImplementationParams,
            Sender<anyhow::Result<Option<lsp_types::request::GotoImplementationResponse>>>,
        ),
    ),
    FindReferences(
        (
            lsp_types::ReferenceParams,
            Sender<anyhow::Result<Option<Vec<lsp_types::Location>>>>,
        ),
    ),
}

pub(crate) struct DiagnosticsThread {
    handle: JoinHandle<anyhow::Result<()>>,
    sender: Sender<DiagnosticMessage>,
}

impl DiagnosticsThread {
    #[allow(unused)]
    pub(crate) fn join(self) {
        self.handle.join().unwrap();
    }

    pub(crate) fn check(&self, uri: Uri, text: String) {
        self.sender
            .send(DiagnosticMessage::CheckSyntax((uri, text)))
            .unwrap();
    }

    pub(crate) fn did_close_text_document(&self, params: lsp_types::DidCloseTextDocumentParams) {
        self.sender
            .send(DiagnosticMessage::DidCloseTextDocument(params))
            .unwrap();
    }

    pub(crate) fn goto_definition(
        &self,
        uri: Uri,
        pos: lsp_types::Position,
    ) -> Option<lsp_types::Location> {
        let (loc_sender, loc_receiver) = channel::<Option<lsp_types::Location>>();
        self.sender
            .send(DiagnosticMessage::GotoDefinition((uri, pos, loc_sender)))
            .unwrap();
        let loc = loc_receiver.recv().unwrap();
        loc
    }

    pub(crate) fn goto_implementation(
        &self,
        params: lsp_types::request::GotoImplementationParams,
    ) -> anyhow::Result<Option<lsp_types::request::GotoImplementationResponse>> {
        let (sender, receiver) = channel();
        self.sender
            .send(DiagnosticMessage::GotoImplementation((params, sender)))
            .unwrap();
        let response = receiver.recv().unwrap();
        response
    }

    pub(crate) fn find_references(
        &self,
        params: lsp_types::ReferenceParams,
    ) -> anyhow::Result<Option<Vec<lsp_types::Location>>> {
        let (sender, receiver) = channel();
        self.sender
            .send(DiagnosticMessage::FindReferences((params, sender)))
            .unwrap();
        let response = receiver.recv().unwrap();
        response
    }
}

pub(crate) fn start_diagnostics_thread(
    root_path: PathBuf,
    msg_sender: MessageSender,
    clangd: Option<ClangdWrapper>,
) -> DiagnosticsThread {
    let mut diag = Diagnostic::new(root_path, msg_sender, clangd);
    let (sender, receiver) = channel::<DiagnosticMessage>();
    let handle = thread::spawn(move || {
        loop {
            let msg = match receiver.recv() {
                Ok(msg) => msg,
                Err(_) => break,
            };

            match msg {
                DiagnosticMessage::CheckSyntax((uri, text)) => {
                    diag.check(uri, text);
                }
                DiagnosticMessage::DidCloseTextDocument(params) => {
                    diag.did_close_text_document(params);
                }
                DiagnosticMessage::GotoDefinition((uri, pos, loc_sender)) => {
                    let loc = diag.find_definition(uri, pos);
                    loc_sender.send(loc).unwrap();
                }
                DiagnosticMessage::GotoImplementation((params, sender)) => {
                    let response = diag.goto_implementation(params);
                    sender.send(response).unwrap();
                }
                DiagnosticMessage::FindReferences((params, sender)) => {
                    let response = diag.find_references(params);
                    sender.send(response).unwrap();
                }
            }
        }
        if let Some(clangd) = diag.clangd {
            clangd.terminate()?;
        }

        Ok(())
    });

    DiagnosticsThread {
        handle: handle,
        sender: sender,
    }
}

struct Diagnostic {
    // Workspace root path.
    root_path: PathBuf,
    // A message sender. It is used in the diagnostics thread to send
    // notifications.
    msg_sender: MessageSender,
    // A clangd client for goto implementation.
    clangd: Option<ClangdWrapper>,
    // Current parsed syntax tree with the original text.
    ast: Option<MojomAst>,
    // Parsed mojom files that are imported from the current document.
    imported_files: Option<ImportedFiles>,
}

impl Diagnostic {
    fn new(root_path: PathBuf, msg_sender: MessageSender, clangd: Option<ClangdWrapper>) -> Self {
        Diagnostic {
            root_path: root_path,
            msg_sender: msg_sender,
            clangd,
            ast: None,
            imported_files: None,
        }
    }

    fn check(&mut self, uri: Uri, text: String) {
        self.check_syntax(uri.clone(), text);
        self.check_imported_files();
    }

    fn did_close_text_document(&mut self, params: lsp_types::DidCloseTextDocumentParams) {
        if self.is_same_uri(&params.text_document.uri) {
            // Drop parsed results.
            self.ast.take();
            self.imported_files.take();
        }
    }

    fn find_definition(
        &mut self,
        uri: Uri,
        pos: lsp_types::Position,
    ) -> Option<lsp_types::Location> {
        if !self.is_same_uri(&uri) {
            // TODO: Don't use unwrap().
            self.open(uri).unwrap();
        }

        if let Some(ast) = &self.ast {
            let ident = get_identifier(&ast.text, &pos);
            let loc = find_definition_in_doc(ast, &ident).or(find_definition_in_imported_files(
                &self.imported_files,
                &ident,
            ));
            loc
        } else {
            None
        }
    }

    fn is_same_uri(&self, uri: &Uri) -> bool {
        if let Some(ast) = &self.ast {
            *uri == ast.uri
        } else {
            false
        }
    }

    fn open(&mut self, uri: Uri) -> std::io::Result<()> {
        let path = uri.to_file_path().unwrap();
        let mut text = String::new();
        File::open(path).and_then(|mut f| f.read_to_string(&mut text))?;
        self.check(uri, text);
        Ok(())
    }

    fn check_syntax(&mut self, uri: Uri, text: String) {
        let mojom = syntax::parse(&text);
        let diagnostics = match mojom {
            Ok(mojom) => {
                let analytics = super::semantic::check_semantics(&text, &mojom);
                // TODO: Don't store ast when semantics check fails?
                self.ast = Some(MojomAst::from_mojom(
                    uri.clone(),
                    text,
                    mojom,
                    analytics.module,
                ));
                analytics.diagnostics
            }
            Err(err) => {
                self.ast = None;
                let (start, end) = err.range();
                let range = into_lsp_range(&start, &end);
                let diagnostic = create_diagnostic(range, err.to_string());
                vec![diagnostic]
            }
        };

        let params = lsp_types::PublishDiagnosticsParams {
            uri: uri,
            diagnostics: diagnostics,
            // TODO: Support version
            version: None,
        };
        publish_diagnostics(&self.msg_sender, params);
    }

    fn check_imported_files(&mut self) {
        if let Some(ast) = &self.ast {
            let imported_files = check_imports(&self.root_path, ast);
            self.imported_files = Some(imported_files);
        }
    }

    fn find_symbol(
        &mut self,
        params: &lsp_types::TextDocumentPositionParams,
    ) -> anyhow::Result<Option<DocumentSymbol>> {
        let uri = &params.text_document.uri;
        if !self.is_same_uri(uri) {
            self.open(uri.clone())?;
        }

        let ast = match self.ast.as_ref() {
            Some(ast) => ast,
            None => return Ok(None),
        };
        let symbol = match ast.find_symbol_from_position(&params.position) {
            Some(symbols) => symbols,
            None => return Ok(None),
        };
        Ok(Some(symbol))
    }

    fn goto_implementation(
        &mut self,
        params: lsp_types::request::GotoImplementationParams,
    ) -> anyhow::Result<Option<lsp_types::request::GotoImplementationResponse>> {
        let symbol = match self.find_symbol(&params.text_document_position_params)? {
            Some(symbol) => symbol,
            None => return Ok(None),
        };
        let clangd = match self.clangd.as_mut() {
            Some(clangd) => clangd,
            None => return Ok(None),
        };
        let response = clangd.goto_implementation(params, symbol)?;
        Ok(response)
    }

    fn find_references(
        &mut self,
        params: lsp_types::ReferenceParams,
    ) -> anyhow::Result<Option<Vec<lsp_types::Location>>> {
        let mut symbol = match self.find_symbol(&params.text_document_position)? {
            Some(symbol) => symbol,
            None => return Ok(None),
        };

        let clangd = match self.clangd.as_mut() {
            Some(clangd) => clangd,
            None => return Ok(None),
        };

        // Use Proxy for references.
        match &mut symbol {
            DocumentSymbol::Interface(ref mut interface) => interface.name += "Proxy",
            DocumentSymbol::Method(ref mut method) => method.interface_name += "Proxy",
            _ => (),
        }

        let response = clangd.find_references(params, symbol)?;
        Ok(response)
    }
}

pub(crate) fn into_lsp_range(start: &syntax::LineCol, end: &syntax::LineCol) -> lsp_types::Range {
    lsp_types::Range {
        start: lsp_types::Position::new(start.line as u32, start.col as u32),
        end: lsp_types::Position::new(end.line as u32, end.col as u32),
    }
}

fn publish_diagnostics(msg_sender: &MessageSender, params: lsp_types::PublishDiagnosticsParams) {
    let params = serde_json::to_value(&params).unwrap();
    let msg = NotificationMessage {
        method: "textDocument/publishDiagnostics".to_owned(),
        params: Some(params),
    };
    msg_sender.send_notification(msg);
}

fn find_definition_in_doc(ast: &MojomAst, ident: &str) -> Option<lsp_types::Location> {
    super::definition::find_definition_preorder(ident, ast)
}

fn find_definition_in_imported_files(
    imported_files: &Option<ImportedFiles>,
    ident: &str,
) -> Option<lsp_types::Location> {
    imported_files
        .as_ref()
        .and_then(|ref imported_files| imported_files.find_definition(ident))
}
