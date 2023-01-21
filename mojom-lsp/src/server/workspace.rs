use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::notification::{self, Notification};
use lsp_types::request::{self, Request};
use lsp_types::Url as Uri;
use tokio::sync::mpsc;

use super::clangd::ClangdMessage;
use super::document_symbol::DocumentSymbol;
use super::mojomast::MojomAst;
use super::protocol::{
    ErrorCodes, NotificationMessage, RequestMessage, ResponseError, ResponseMessage,
};
use super::server::RpcSender;
use crate::syntax;

pub(crate) async fn workspace_task(
    root_path: PathBuf,
    gen_path: PathBuf,
    rpc_sender: RpcSender,
    clangd_sender: Option<mpsc::Sender<ClangdMessage>>,
    sender: mpsc::Sender<WorkspaceMessage>,
    mut receiver: mpsc::Receiver<WorkspaceMessage>,
) -> anyhow::Result<()> {
    check_workspace_mojoms(&root_path, &gen_path, sender)?;
    let mut workspace = Workspace::new(root_path, gen_path, rpc_sender, clangd_sender);

    while let Some(message) = receiver.recv().await {
        match message {
            WorkspaceMessage::RpcRequest(request) => workspace.handle_request(request).await?,
            WorkspaceMessage::RpcResponse(response) => workspace.handle_response(response).await?,
            WorkspaceMessage::RpcNotification(notification) => {
                workspace.handle_notification(notification).await?
            }
            WorkspaceMessage::DocumentSymbolsParsed(uri, parsed) => {
                workspace.document_symbols_parsed(uri, parsed).await?
            }
        }
    }

    Ok(())
}

#[derive(Debug)]
pub(crate) enum WorkspaceMessage {
    RpcRequest(RequestMessage),
    RpcResponse(ResponseMessage),
    RpcNotification(NotificationMessage),

    DocumentSymbolsParsed(Uri, ParsedSymbols),
}

#[derive(Debug)]
pub(crate) struct ParsedSymbols {
    module_name: Option<String>,
    symbols: Vec<DocumentSymbol>,
}

impl ParsedSymbols {
    fn find_definition(&self, ident: &str) -> Option<lsp_types::Range> {
        for symbol in &self.symbols {
            if symbol.name() == ident {
                return Some(symbol.range().clone());
            }
            if let Some(module_name) = &self.module_name {
                let qualified_name = format!("{}.{}", module_name, symbol.name());
                if qualified_name == ident {
                    return Some(symbol.range().clone());
                }
            }
        }
        None
    }
}

struct ParsedMojom {
    #[allow(unused)]
    version: Option<i32>,
    ast: MojomAst,
    module_name: Option<String>,
    import_uris: Vec<Uri>,
}

struct Workspace {
    root_path: PathBuf,
    gen_path: PathBuf,
    rpc_sender: RpcSender,
    clangd_sender: Option<mpsc::Sender<ClangdMessage>>,
    parsed_files: HashMap<Uri, ParsedMojom>,
    symbols: HashMap<Uri, ParsedSymbols>,
}

impl Workspace {
    fn new(
        root_path: PathBuf,
        gen_path: PathBuf,
        rpc_sender: RpcSender,
        clangd_sender: Option<mpsc::Sender<ClangdMessage>>,
    ) -> Self {
        Workspace {
            root_path,
            gen_path,
            rpc_sender,
            clangd_sender,
            parsed_files: HashMap::new(),
            symbols: HashMap::new(),
        }
    }

    async fn handle_request(&mut self, request: RequestMessage) -> anyhow::Result<()> {
        match request.method.as_str() {
            request::Shutdown::METHOD => unreachable!(),
            request::GotoDefinition::METHOD => {
                self.goto_definition(request.id, request.get_params()?)
                    .await?;
            }
            request::GotoImplementation::METHOD => {
                self.goto_implementation(request.id, request.get_params()?)
                    .await?;
            }
            request::References::METHOD => {
                self.references(request.id, request.get_params()?).await?;
            }
            request::WorkspaceSymbol::METHOD => {
                let params = request.get_params::<lsp_types::WorkspaceSymbolParams>()?;
                self.find_symbol(request.id, params.query).await?;
            }
            request::DocumentSymbolRequest::METHOD => {
                self.document_symbol(request.id, request.get_params()?)
                    .await?;
            }
            _ => {
                log::info!("Unimplemented request: {}", request.method);
                let message = format!("{} is not implemented", request.method);
                let error = ResponseError::new(ErrorCodes::InternalError, message);
                self.rpc_sender
                    .send_error_response(request.id, error)
                    .await?;
                return Ok(());
            }
        };

        Ok(())
    }

    async fn handle_response(&mut self, response: ResponseMessage) -> anyhow::Result<()> {
        log::info!("Unimplemehted response: {:#?}", response);
        Ok(())
    }

    async fn handle_notification(
        &mut self,
        notification: NotificationMessage,
    ) -> anyhow::Result<()> {
        match notification.method.as_str() {
            notification::Exit::METHOD => unreachable!(),
            notification::DidOpenTextDocument::METHOD => {
                self.did_open_text_document(notification.get_params()?)
                    .await?;
            }
            notification::DidChangeTextDocument::METHOD => {
                self.did_change_text_document(notification.get_params()?)
                    .await?;
            }
            notification::DidCloseTextDocument::METHOD => {
                self.did_close_text_document(notification.get_params()?)
                    .await?;
            }
            // Ignore some notifications for now.
            notification::Cancel::METHOD
            | notification::SetTrace::METHOD
            | notification::LogTrace::METHOD
            | notification::DidChangeConfiguration::METHOD
            | notification::WillSaveTextDocument::METHOD
            | notification::DidSaveTextDocument::METHOD => (),
            _ => {
                log::info!("Unimplemented notification: {}", notification.method);
            }
        }
        Ok(())
    }

    async fn document_symbols_parsed(
        &mut self,
        uri: Uri,
        parsed: ParsedSymbols,
    ) -> anyhow::Result<()> {
        self.symbols.insert(uri, parsed);
        Ok(())
    }

    async fn find_symbol(&self, id: u64, query: String) -> anyhow::Result<()> {
        let mut symbols = Vec::new();

        let target_name = match query.split("::").filter(|s| s.len() > 0).last() {
            Some(name) => name,
            None => return Ok(()),
        };

        let match_symbol = |symbol: &DocumentSymbol| {
            if symbol.name() == target_name {
                return true;
            }
            if target_name.ends_with("Ptr")
                && symbol.name() == &target_name[..target_name.len() - "Ptr".len()]
            {
                return true;
            }
            if target_name.ends_with("InterfaceBase")
                && symbol.name() == &target_name[..target_name.len() - "InterfaceBase".len()]
            {
                return true;
            }
            false
        };

        for (uri, parsed) in self.symbols.iter() {
            for symbol in parsed.symbols.iter() {
                if match_symbol(symbol) {
                    let location =
                        lsp_types::Location::new(uri.clone(), symbol.name_range().clone());
                    #[allow(deprecated)]
                    let symbol = lsp_types::SymbolInformation {
                        name: symbol.name().to_string(),
                        kind: symbol.kind(),
                        location,
                        tags: None,
                        deprecated: None,
                        container_name: None,
                    };
                    symbols.push(symbol);
                }
            }
        }
        self.rpc_sender.send_success_response(id, symbols).await?;
        Ok(())
    }

    async fn goto_definition(
        &mut self,
        id: u64,
        params: lsp_types::GotoDefinitionParams,
    ) -> anyhow::Result<()> {
        let uri = &params.text_document_position_params.text_document.uri;

        let parsed = match self.parsed_files.get(uri) {
            Some(parsed) => parsed,
            None => {
                self.rpc_sender.send_null_response(id).await?;
                return Ok(());
            }
        };

        let position = &params.text_document_position_params.position;

        if let Some(location) =
            parsed
                .ast
                .import_path_from_position(&self.root_path, &self.gen_path, position)
        {
            self.rpc_sender.send_success_response(id, location).await?;
            return Ok(());
        }

        // Find definition in the same document.
        let ident = parsed.ast.get_identifier(position);
        if let Some(range) = parsed.ast.find_definition(ident) {
            let location = lsp_types::Location::new(uri.clone(), range);
            self.rpc_sender.send_success_response(id, location).await?;
            return Ok(());
        }

        // Find definition from imports.
        for import_uri in &parsed.import_uris {
            let import_symbols = match self.symbols.get(import_uri) {
                Some(symbols) => symbols,
                None => {
                    let path = import_uri.to_file_path().unwrap();
                    let parsed =
                        match parse_mojom_symbols(&self.root_path, &self.gen_path, path).await {
                            Ok(parsed) => parsed,
                            Err(err) => {
                                log::debug!("Failed to parse import: {}: {:?}", import_uri, err);
                                continue;
                            }
                        };
                    self.symbols.insert(import_uri.clone(), parsed);
                    self.symbols.get(import_uri).unwrap()
                }
            };

            let range = match import_symbols.find_definition(ident) {
                Some(range) => range,
                None => continue,
            };
            let location = lsp_types::Location::new(import_uri.clone(), range);
            self.rpc_sender.send_success_response(id, location).await?;
            return Ok(());
        }

        self.rpc_sender.send_null_response(id).await?;
        Ok(())
    }

    async fn goto_implementation(
        &mut self,
        id: u64,
        params: request::GotoImplementationParams,
    ) -> anyhow::Result<()> {
        let clangd_sender = match self.clangd_sender.as_ref() {
            Some(sender) => sender,
            None => {
                self.rpc_sender.send_null_response(id).await?;
                return Ok(());
            }
        };

        let uri = &params.text_document_position_params.text_document.uri;
        let position = &params.text_document_position_params.position;
        let symbol = match self
            .parsed_files
            .get(uri)
            .and_then(|parsed| parsed.ast.find_symbol_from_position(position))
        {
            Some(symbol) => symbol,
            None => {
                self.rpc_sender.send_null_response(id).await?;
                return Ok(());
            }
        };

        clangd_sender
            .send(ClangdMessage::GotoImplementation(id, uri.clone(), symbol))
            .await?;
        Ok(())
    }

    async fn references(
        &mut self,
        id: u64,
        params: lsp_types::ReferenceParams,
    ) -> anyhow::Result<()> {
        let clangd_sender = match self.clangd_sender.as_ref() {
            Some(sender) => sender,
            None => {
                self.rpc_sender.send_null_response(id).await?;
                return Ok(());
            }
        };

        let uri = &params.text_document_position.text_document.uri;
        let position = &params.text_document_position.position;
        let symbol = match self
            .parsed_files
            .get(uri)
            .and_then(|parsed| parsed.ast.find_symbol_from_position(position))
        {
            Some(symbol) => symbol,
            None => {
                self.rpc_sender.send_null_response(id).await?;
                return Ok(());
            }
        };

        clangd_sender
            .send(ClangdMessage::References(id, uri.clone(), symbol))
            .await?;
        Ok(())
    }

    async fn document_symbol(
        &mut self,
        id: u64,
        params: lsp_types::DocumentSymbolParams,
    ) -> anyhow::Result<()> {
        let uri = params.text_document.uri;
        let parsed = match self.parsed_files.get(&uri) {
            Some(parsed) => parsed,
            None => {
                log::debug!("No parsed: {:?}", uri);
                self.rpc_sender.send_success_response(id, ()).await?;
                return Ok(());
            }
        };
        let symbols = parsed
            .ast
            .get_document_symbols()
            .into_iter()
            .map(|s| s.lsp_symbol(&uri))
            .collect::<Vec<_>>();
        self.rpc_sender.send_success_response(id, symbols).await?;
        Ok(())
    }

    async fn did_open_text_document(
        &mut self,
        params: lsp_types::DidOpenTextDocumentParams,
    ) -> anyhow::Result<()> {
        self.update_mojom(
            params.text_document.uri,
            params.text_document.text,
            Some(params.text_document.version),
        )
        .await?;
        Ok(())
    }

    async fn did_change_text_document(
        &mut self,
        mut params: lsp_types::DidChangeTextDocumentParams,
    ) -> anyhow::Result<()> {
        let uri = &params.text_document.uri;
        // Assume the client send the whole text.
        let text = match params.content_changes.pop() {
            Some(event) => event.text,
            None => anyhow::bail!("No text"),
        };
        let version = Some(params.text_document.version);
        self.update_mojom(uri.clone(), text, version).await?;
        Ok(())
    }

    async fn update_mojom(
        &mut self,
        uri: Uri,
        text: String,
        version: Option<i32>,
    ) -> anyhow::Result<()> {
        self.parsed_files.remove(&uri);
        self.symbols.remove(&uri);

        let mut diagnostics = Vec::new();
        let parsed = check_syntax_and_semantics(
            &self.root_path,
            &self.gen_path,
            text,
            version,
            &mut diagnostics,
        )
        .await;

        if let Some(parsed) = parsed {
            let symbols = ParsedSymbols {
                symbols: parsed.ast.get_document_symbols(),
                module_name: parsed.module_name.clone(),
            };
            self.symbols.insert(uri.clone(), symbols);
            self.parsed_files.insert(uri.clone(), parsed);
        }

        self.rpc_sender
            .send_notification_message(
                notification::PublishDiagnostics::METHOD,
                lsp_types::PublishDiagnosticsParams {
                    uri,
                    diagnostics,
                    version,
                },
            )
            .await?;
        Ok(())
    }

    async fn did_close_text_document(
        &mut self,
        params: lsp_types::DidCloseTextDocumentParams,
    ) -> anyhow::Result<()> {
        self.parsed_files.remove(&params.text_document.uri);
        Ok(())
    }
}

fn check_workspace_mojoms(
    root_path: impl AsRef<Path>,
    gen_path: impl AsRef<Path>,
    sender: mpsc::Sender<WorkspaceMessage>,
) -> anyhow::Result<()> {
    let mut directories = Vec::new();
    directories.push(root_path.as_ref().to_owned());
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(directory)?;
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() && entry.file_name() != "out" {
                directories.push(entry.path());
                continue;
            }
            if file_type.is_file() {
                let path = entry.path();
                if path.extension().map(|ext| ext == "mojom").unwrap_or(false) {
                    let sender = sender.clone();
                    tokio::spawn(check_single_mojom(
                        root_path.as_ref().to_owned(),
                        gen_path.as_ref().to_owned(),
                        path,
                        sender,
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn check_single_mojom(
    root_path: PathBuf,
    gen_path: PathBuf,
    path: PathBuf,
    sender: mpsc::Sender<WorkspaceMessage>,
) -> anyhow::Result<()> {
    let uri = lsp_types::Url::from_file_path(&path)
        .map_err(|err| anyhow::anyhow!("Failed to convert path to Uri: {:?}", err))?;
    let parsed = parse_mojom_symbols(root_path, gen_path, path).await?;
    sender
        .send(WorkspaceMessage::DocumentSymbolsParsed(uri, parsed))
        .await?;
    Ok(())
}

async fn parse_mojom_symbols(
    root_path: impl AsRef<Path>,
    gen_path: impl AsRef<Path>,
    path: PathBuf,
) -> anyhow::Result<ParsedSymbols> {
    let text = tokio::fs::read_to_string(&path).await?;
    let mojom =
        syntax::parse(&text).map_err(|err| anyhow::anyhow!("Failed to parse mojom: {:?}", err))?;
    let ast = MojomAst::from_mojom(text, mojom);
    let mut result = ast.check_semantics(root_path, gen_path);
    let module_name = result.module_name.take();
    let symbols = ast.get_document_symbols();
    let parsed = ParsedSymbols {
        module_name,
        symbols,
    };
    Ok(parsed)
}

async fn check_syntax_and_semantics(
    root_path: impl AsRef<Path>,
    gen_path: impl AsRef<Path>,
    text: String,
    version: Option<i32>,
    diagnostics: &mut Vec<lsp_types::Diagnostic>,
) -> Option<ParsedMojom> {
    let mojom = match syntax::parse(&text) {
        Ok(mojom) => mojom,
        Err(err) => {
            diagnostics.push(err.lsp_diagnostic());
            return None;
        }
    };

    let ast = MojomAst::from_mojom(text, mojom);
    let mut result = ast.check_semantics(root_path, gen_path);
    let module_name = result.module_name;
    let import_uris = result.import_uris;
    diagnostics.append(&mut result.diagnostics);

    Some(ParsedMojom {
        version,
        ast,
        module_name,
        import_uris,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_parse_all_mojom() -> anyhow::Result<()> {
        let root_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata");
        let gen_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata");
        let (sender, mut receiver) = mpsc::channel(64);

        check_workspace_mojoms(&root_path, &gen_path, sender)?;

        let mut num_parsed = 0;
        while let Some(message) = receiver.recv().await {
            match message {
                WorkspaceMessage::DocumentSymbolsParsed(_, _) => num_parsed += 1,
                _ => unreachable!(),
            }
        }

        assert_eq!(num_parsed, 3);
        Ok(())
    }
}
