// Copyright 2022 Google LLC
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lsp_types::notification::{self, Notification};
use lsp_types::request::{self, Request};
use lsp_types::Url as Uri;
use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::clangd::{self, ClangdMessage, ClangdParams};
use super::document_symbol::DocumentSymbol;
use super::mojomast::{check_mojom_text, MojomAst};
use super::protocol::{
    Message, NotificationMessage, RequestMessage, ResponseError, ResponseMessage,
};
use super::rpc;
use super::workspace;
use crate::syntax::{self, MojomFile};

pub async fn run<R, W>(
    reader: R,
    mut writer: W,
    clangd_params: Option<ClangdParams>,
) -> anyhow::Result<i32>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader);

    let params = initialize(&mut reader, &mut writer).await?;

    let root_path = match get_root_path(&params) {
        Some(path) => path,
        None => {
            // Use the current directory.
            std::env::current_dir()?
        }
    };
    log::info!("Initialized, path = {:?}", root_path);

    let context = Context {
        parsed_files: HashMap::new(),
    };
    let context = Arc::new(Mutex::new(context));
    let sender = spawn_sender_task(writer);

    let clangd_sender = match clangd_params {
        Some(params) => {
            let clangd = clangd::start(root_path.clone(), params).await?;
            let (clangd_sender, clangd_receiver) = mpsc::channel(64);
            let _clangd_task_handle =
                tokio::spawn(clangd::clangd_task(clangd, clangd_receiver, sender.clone()));
            Some(clangd_sender)
        }
        None => None,
    };

    let (_workspace_task_handle, workspace_message_sender) = {
        let (sender, receiver) = mpsc::channel(64);
        let handle = tokio::spawn(workspace::workspace_task(
            root_path.clone(),
            sender.clone(),
            receiver,
        ));

        (handle, sender)
    };

    let server = Server {
        state: ServerState::Running,
        context,
        reader,
        sender,
        workspace_message_sender,
        clangd_sender,
    };

    let exit_code = server.run().await?;
    log::info!("Exit, status = {}", exit_code);
    Ok(exit_code)
}

async fn initialize<R, W>(
    reader: &mut R,
    writer: &mut W,
) -> anyhow::Result<lsp_types::InitializeParams>
where
    R: AsyncRead + AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let message = rpc::recv_message(reader).await?;
    let (id, params) = match message {
        Message::Request(request) if request.method == request::Initialize::METHOD => {
            let params = match request.params {
                Some(params) => params,
                None => anyhow::bail!("No initialization parameters"),
            };
            let params = serde_json::from_value::<lsp_types::InitializeParams>(params)?;
            (request.id, params)
        }
        _ => {
            anyhow::bail!("Expected initialize message but got {:?}", message);
        }
    };

    let text_document_sync_option = lsp_types::TextDocumentSyncOptions {
        open_close: Some(true),
        change: Some(lsp_types::TextDocumentSyncKind::FULL),
        ..Default::default()
    };
    let text_document_sync = Some(lsp_types::TextDocumentSyncCapability::Options(
        text_document_sync_option,
    ));
    let capabilities = lsp_types::ServerCapabilities {
        text_document_sync,
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        implementation_provider: Some(lsp_types::ImplementationProviderCapability::Simple(true)),
        references_provider: Some(lsp_types::OneOf::Left(true)),
        declaration_provider: Some(lsp_types::DeclarationCapability::Simple(true)),
        workspace_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        ..Default::default()
    };
    let result = lsp_types::InitializeResult {
        capabilities,
        server_info: Some(lsp_types::ServerInfo {
            name: "mojom-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    rpc::Response::new(id).result(result)?.send(writer).await?;

    let message = rpc::recv_message(reader).await?;
    match message {
        Message::Notification(notification)
            if notification.method == notification::Initialized::METHOD =>
        {
            Ok(params)
        }
        _ => anyhow::bail!("Unexpected message: {:?}", message),
    }
}

fn is_chromium_src_dir(path: &PathBuf) -> bool {
    // The root is named `src`.
    if !path.file_name().map(|name| name == "src").unwrap_or(false) {
        return false;
    }

    // Check if the parent directory contains `.gclient`.
    match path.parent() {
        Some(parent) => parent.join(".gclient").is_file(),
        None => false,
    }
}

fn find_chromium_src_dir(mut path: PathBuf) -> PathBuf {
    if is_chromium_src_dir(&path) {
        return path;
    }

    let original = path.clone();
    while path.pop() {
        if is_chromium_src_dir(&path) {
            return path;
        }
    }
    original
}

fn get_root_path(params: &lsp_types::InitializeParams) -> Option<PathBuf> {
    let uri = match params.root_uri {
        Some(ref uri) => uri,
        None => return None,
    };
    let path = match uri.to_file_path() {
        Ok(path) => path,
        Err(_) => return None,
    };

    // Try to find chromium's `src` directory and use it if exists.
    let path = find_chromium_src_dir(path);
    Some(path)
}

fn spawn_sender_task<W>(writer: W) -> RpcSender
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(sender_task(writer, receiver));
    RpcSender { sender }
}

async fn sender_task<W>(mut writer: W, mut receiver: Receiver<Message>) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    while let Some(message) = receiver.recv().await {
        rpc::send_message(&mut writer, &message).await?;
    }
    Ok(())
}

pub(crate) struct Context {
    parsed_files: HashMap<Uri, ParsedMojom>,
}

pub(crate) struct ParsedMojom {
    #[allow(unused)]
    version: Option<i32>,
    ast: MojomAst,
    imports: Vec<ParsedImport>,
}

impl ParsedMojom {
    fn find_definition(&self, position: &lsp_types::Position) -> Option<lsp_types::Location> {
        let ident = self.ast.get_identifier(position);
        self.ast
            .find_definition(ident)
            .or(self.find_definition_in_imports(ident))
    }

    fn find_definition_in_imports(&self, ident: &str) -> Option<lsp_types::Location> {
        self.imports
            .iter()
            .find_map(|import| import.find_definition(&ident))
    }
}

pub(crate) struct ParsedImport {
    uri: Uri,
    module_name: Option<String>,
    symbols: Vec<DocumentSymbol>,
}

impl ParsedImport {
    fn find_definition(&self, ident: &str) -> Option<lsp_types::Location> {
        for symbol in &self.symbols {
            if symbol.name() == ident {
                return Some(lsp_types::Location {
                    uri: self.uri.clone(),
                    range: symbol.range().clone(),
                });
            }
            if let Some(module_name) = &self.module_name {
                let qualified_name = format!("{}.{}", module_name, symbol.name());
                if qualified_name == ident {
                    return Some(lsp_types::Location {
                        uri: self.uri.clone(),
                        range: symbol.range().clone(),
                    });
                }
            }
        }
        None
    }
}

enum ServerState {
    Running,
    ShuttingDown,
    Exit(i32),
}
struct Server<R>
where
    R: AsyncBufRead + Unpin,
{
    state: ServerState,
    context: Arc<Mutex<Context>>,
    reader: R,
    sender: RpcSender,
    workspace_message_sender: Sender<workspace::WorkspaceMessage>,

    clangd_sender: Option<Sender<ClangdMessage>>,
}

impl<R> Server<R>
where
    R: AsyncBufRead + Unpin,
{
    async fn run(mut self) -> anyhow::Result<i32> {
        loop {
            let message = rpc::recv_message(&mut self.reader).await?;
            match message {
                Message::Request(request) => self.handle_request(request).await?,
                Message::Response(response) => self.handle_response(response).await?,
                Message::Notification(notification) => {
                    self.handle_notification(notification).await?
                }
            }

            if let ServerState::Exit(exit_code) = self.state {
                return Ok(exit_code);
            }
        }
    }

    async fn handle_request(&mut self, request: RequestMessage) -> anyhow::Result<()> {
        log::info!("Request: {}({})", request.method, request.id);
        match request.method.as_str() {
            request::Shutdown::METHOD => {
                self.state = ServerState::ShuttingDown;
                self.sender
                    .send_response_message(request.id, Ok(()))
                    .await?;
            }
            request::GotoDefinition::METHOD => {
                let context = Arc::clone(&self.context);
                let params = request.get_params()?;
                let response = goto_definition(context, params).await;
                self.sender
                    .send_response_message(request.id, response)
                    .await?;
            }
            request::GotoImplementation::METHOD => {
                let clangd_sender = match self.clangd_sender.as_ref() {
                    Some(sender) => sender.clone(),
                    None => {
                        self.sender
                            .send_response_message(request.id, Ok(()))
                            .await?;
                        return Ok(());
                    }
                };
                let context = Arc::clone(&self.context);
                let params = request.get_params()?;
                goto_implementation(context, clangd_sender, request.id, params).await?;
            }
            request::References::METHOD => {
                let clangd_sender = match self.clangd_sender.as_ref() {
                    Some(sender) => sender.clone(),
                    None => {
                        self.sender
                            .send_response_message(request.id, Ok(()))
                            .await?;
                        return Ok(());
                    }
                };
                let context = Arc::clone(&self.context);
                let params = request.get_params()?;
                references(context, clangd_sender, request.id, params).await?;
            }
            request::WorkspaceSymbol::METHOD => {
                let params = request.get_params::<lsp_types::WorkspaceSymbolParams>()?;
                let sender = self.sender.clone();
                let workspace_message_sender = self.workspace_message_sender.clone();
                tokio::spawn(async move {
                    let (response_sender, response_receiver) = tokio::sync::oneshot::channel();
                    workspace_message_sender
                        .send(workspace::WorkspaceMessage::FindSymbol(
                            params.query,
                            response_sender,
                        ))
                        .await?;
                    let response = response_receiver.await?;
                    sender
                        .send_response_message(request.id, Ok(response))
                        .await?;
                    anyhow::Ok(())
                });
            }
            _ => {
                log::info!("Unimplemented request: {}", request.method);
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
        log::info!("Notification: {:#?}", notification);
        match notification.method.as_str() {
            notification::Exit::METHOD => {
                let exit_code = match self.state {
                    ServerState::ShuttingDown => 0,
                    _ => 1,
                };
                self.state = ServerState::Exit(exit_code);
            }
            notification::DidOpenTextDocument::METHOD => {
                let context = Arc::clone(&self.context);
                let sender = self.sender.clone();
                let params = notification.get_params()?;
                tokio::spawn(did_open_text_document(context, sender, params));
            }
            notification::DidChangeTextDocument::METHOD => {
                let context = Arc::clone(&self.context);
                let sender = self.sender.clone();
                let params = notification.get_params::<lsp_types::DidChangeTextDocumentParams>()?;
                self.workspace_message_sender
                    .send(workspace::WorkspaceMessage::DidChangeTextDocument(
                        params.text_document.uri.clone(),
                    ))
                    .await?;
                tokio::spawn(did_change_text_document(context, sender, params));
            }
            notification::DidCloseTextDocument::METHOD => {
                let context = Arc::clone(&self.context);
                let params = notification.get_params()?;
                tokio::spawn(did_close_text_document(context, params));
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
}

type ResponseResult<T> = std::result::Result<T, ResponseError>;

async fn goto_definition(
    context: Arc<Mutex<Context>>,
    params: lsp_types::GotoDefinitionParams,
) -> ResponseResult<Option<lsp_types::Location>> {
    let context = context.lock().unwrap();

    let uri = &params.text_document_position_params.text_document.uri;
    let parsed = match context.parsed_files.get(uri) {
        Some(parsed) => parsed,
        None => return Ok(None),
    };

    let position = &params.text_document_position_params.position;
    let response = parsed.find_definition(position);
    Ok(response)
}

async fn goto_implementation(
    context: Arc<Mutex<Context>>,
    clangd_sender: Sender<ClangdMessage>,
    request_id: u64,
    params: request::GotoImplementationParams,
) -> anyhow::Result<()> {
    let context = context.lock().unwrap();

    let uri = &params.text_document_position_params.text_document.uri;
    let position = &params.text_document_position_params.position;
    let symbol = match context
        .parsed_files
        .get(uri)
        .and_then(|parsed| parsed.ast.find_symbol_from_position(position))
    {
        Some(symbol) => symbol,
        None => return Ok(()),
    };

    clangd_sender
        .send(ClangdMessage::GotoImplementation(
            request_id,
            uri.clone(),
            symbol,
        ))
        .await?;
    Ok(())
}

async fn references(
    context: Arc<Mutex<Context>>,
    clangd_sender: Sender<ClangdMessage>,
    request_id: u64,
    params: lsp_types::ReferenceParams,
) -> anyhow::Result<()> {
    let context = context.lock().unwrap();

    let uri = &params.text_document_position.text_document.uri;
    let position = &params.text_document_position.position;
    let symbol = match context
        .parsed_files
        .get(uri)
        .and_then(|parsed| parsed.ast.find_symbol_from_position(position))
    {
        Some(symbol) => symbol,
        None => return Ok(()),
    };

    clangd_sender
        .send(ClangdMessage::References(request_id, uri.clone(), symbol))
        .await?;
    Ok(())
}

async fn did_open_text_document(
    context: Arc<Mutex<Context>>,
    sender: RpcSender,
    params: lsp_types::DidOpenTextDocumentParams,
) -> anyhow::Result<()> {
    let text_document = params.text_document;
    update_mojom(
        context,
        sender,
        text_document.uri,
        text_document.text,
        Some(text_document.version),
    )
    .await?;
    Ok(())
}

async fn did_change_text_document(
    context: Arc<Mutex<Context>>,
    sender: RpcSender,
    mut params: lsp_types::DidChangeTextDocumentParams,
) -> anyhow::Result<()> {
    let uri = params.text_document.uri;
    // Assume the client send the whole text.
    let text = match params.content_changes.pop() {
        Some(event) => event.text,
        None => anyhow::bail!("No text"),
    };
    let version = Some(params.text_document.version);
    update_mojom(context, sender, uri, text, version).await?;
    Ok(())
}

async fn did_close_text_document(
    context: Arc<Mutex<Context>>,
    params: lsp_types::DidCloseTextDocumentParams,
) -> anyhow::Result<()> {
    let mut context = context.lock().unwrap();
    context.parsed_files.remove(&params.text_document.uri);
    Ok(())
}

async fn update_mojom(
    context: Arc<Mutex<Context>>,
    sender: RpcSender,
    uri: Uri,
    text: String,
    version: Option<i32>,
) -> anyhow::Result<()> {
    {
        let mut context = context.lock().unwrap();
        context.parsed_files.remove(&uri);
    }

    let mut diagnostics = Vec::new();
    let parsed = parse_mojom(uri.clone(), text, version, &mut diagnostics).await;

    if let Some(parsed) = parsed {
        let mut context = context.lock().unwrap();
        context.parsed_files.insert(uri.clone(), parsed);
    }

    sender
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

async fn parse_mojom(
    uri: Uri,
    text: String,
    version: Option<i32>,
    diagnostics: &mut Vec<lsp_types::Diagnostic>,
) -> Option<ParsedMojom> {
    let mojom = {
        let mut result = check_mojom_text(&text);
        diagnostics.append(&mut result.diagnostics);
        match result.mojom {
            Some(mojom) => mojom,
            None => {
                return None;
            }
        }
    };

    let imports = parse_imports(&text, &mojom).await;
    let ast = MojomAst::from_mojom(uri, text, mojom);
    let parsed = ParsedMojom {
        version,
        ast,
        imports,
    };
    Some(parsed)
}

async fn parse_imports(text: &str, mojom: &MojomFile) -> Vec<ParsedImport> {
    let imports = mojom.stmts.iter().filter_map(|stmt| match stmt {
        syntax::Statement::Import(import) => Some(import),
        _ => None,
    });
    let mut parsed_imports = Vec::new();
    for import in imports {
        // `import.path` includes double quotes.
        let start = import.path.start + 1;
        let end = import.path.end - 1;
        let import_path = &text[start..end];
        let imported_text = match tokio::fs::read_to_string(import_path).await {
            Ok(text) => text,
            Err(err) => {
                log::debug!("Failed to read imported file: {}: {:?}", import_path, err);
                continue;
            }
        };

        let (imported_mojom, module_name) = {
            let result = check_mojom_text(&imported_text);
            let mojom = match result.mojom {
                Some(mojom) => mojom,
                None => {
                    log::debug!(
                        "Failed to parse imported file: {}: {:?}",
                        import_path,
                        result.diagnostics
                    );
                    continue;
                }
            };

            (mojom, result.module_name)
        };

        // `unwrap()` is safe because opening file succeeded.
        let import_path = Path::new(import_path).canonicalize().unwrap();
        let uri = Uri::from_file_path(import_path).unwrap();
        let imported_ast = MojomAst::from_mojom(uri.clone(), imported_text, imported_mojom);

        let symbols = imported_ast.get_document_symbols();
        parsed_imports.push(ParsedImport {
            uri,
            module_name,
            symbols,
        });
    }
    parsed_imports
}

#[derive(Clone)]
pub(crate) struct RpcSender {
    sender: Sender<Message>,
}

impl RpcSender {
    pub(crate) async fn send_notification_message(
        &self,
        method: &str,
        params: impl Serialize,
    ) -> anyhow::Result<()> {
        let method = method.to_string();
        let params = Some(serde_json::to_value(params)?);
        let notification = NotificationMessage { method, params };
        self.sender
            .send(Message::Notification(notification))
            .await?;
        Ok(())
    }

    pub(crate) async fn send_response_message(
        &self,
        id: u64,
        result: std::result::Result<impl Serialize, ResponseError>,
    ) -> anyhow::Result<()> {
        let response = match result {
            Ok(result) => ResponseMessage {
                id,
                result: Some(serde_json::to_value(result)?),
                error: None,
            },
            Err(err) => ResponseMessage {
                id,
                result: None,
                error: Some(err),
            },
        };
        self.sender.send(Message::Response(response)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::rpc::{recv_message, send_notification, send_request};
    use super::*;

    #[tokio::test]
    async fn test_initialize_and_shutdown() -> anyhow::Result<()> {
        env_logger::init();
        let (client, server) = tokio::io::duplex(64);

        let _client_handle = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(client);
            let mut reader = tokio::io::BufReader::new(reader);
            let workspace_folders = {
                let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata");
                let uri = Uri::from_directory_path(path).unwrap();
                let name = "testdata".to_string();
                Some(vec![lsp_types::WorkspaceFolder { uri, name }])
            };
            let params = lsp_types::InitializeParams {
                workspace_folders,
                ..Default::default()
            };
            send_request(&mut writer, 0, request::Initialize::METHOD, &params).await?;

            let message = recv_message(&mut reader).await?;
            let response = match message {
                Message::Response(response) => response,
                _ => anyhow::bail!("Unexpected message: {:?}", message),
            };
            if let Some(error) = response.error {
                anyhow::bail!("Initialize failed: {:?}", error);
            }

            send_notification(
                &mut writer,
                notification::Initialized::METHOD,
                Some(lsp_types::InitializedParams {}),
            )
            .await?;

            send_request(
                &mut writer,
                1,
                request::Shutdown::METHOD,
                serde_json::Value::Null,
            )
            .await?;

            let message = recv_message(&mut reader).await?;
            let response = match message {
                Message::Response(response) => response,
                _ => anyhow::bail!("Unexpected message: {:?}", message),
            };
            if let Some(error) = response.error {
                anyhow::bail!("Shutdown failed: {:?}", error);
            }

            send_notification(
                &mut writer,
                notification::Exit::METHOD,
                None as Option<serde_json::Value>,
            )
            .await?;

            anyhow::Ok(())
        });

        let (reader, writer) = tokio::io::split(server);
        let exit_code = run(reader, writer, None).await?;
        assert_eq!(exit_code, 0);
        Ok(())
    }
}
