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

use std::path::{Path, PathBuf};

use lsp_types::notification::{self, Notification};
use lsp_types::request::{self, Request};
use lsp_types::Url;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, Command};
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::document_symbol::{DocumentSymbol, InterfaceSymbol, MethodSymbol};
use super::protocol::{ErrorCodes, Message, ResponseError, ResponseMessage};
use super::rpc::{recv_message, send_notification, send_request};
use super::server::RpcSender;

pub struct ClangdParams {
    pub clangd_path: PathBuf,
    pub compile_commands_dir: Option<PathBuf>,
    pub log_level: Option<log::Level>,
}

struct CppBindingHeader {
    text_document: lsp_types::TextDocumentIdentifier,
    symbols: Vec<lsp_types::SymbolInformation>,
}

impl CppBindingHeader {
    async fn goto_implementation<W>(
        &self,
        transport: &mut Transport<W>,
        target_symbol: &DocumentSymbol,
    ) -> anyhow::Result<Option<Vec<lsp_types::Location>>>
    where
        W: AsyncWrite + Unpin,
    {
        let text_document_position_params =
            match self.create_text_document_position_params(target_symbol) {
                Some(params) => params,
                None => return Ok(None),
            };

        let params = request::GotoImplementationParams {
            text_document_position_params,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        transport
            .send_request(request::GotoImplementation::METHOD, params)
            .await?;
        let response = match transport
            .recv_response_or_null::<request::GotoImplementationResponse>()
            .await?
        {
            Some(response) => response,
            None => return Ok(None),
        };

        // Filter mojom-generated implementations.
        let locations = match response {
            request::GotoImplementationResponse::Array(mut locations) => {
                locations.retain(|location| !is_mojom_generated(&location.uri));
                locations
            }
            request::GotoImplementationResponse::Link(mut _links) => {
                unimplemented!();
            }
            request::GotoImplementationResponse::Scalar(location) => {
                if is_mojom_generated(&location.uri) {
                    return Ok(None);
                }
                vec![location]
            }
        };
        Ok(Some(locations))
    }

    async fn find_references<W>(
        &self,
        transport: &mut Transport<W>,
        target_symbol: &DocumentSymbol,
    ) -> anyhow::Result<Option<Vec<lsp_types::Location>>>
    where
        W: AsyncWrite + Unpin,
    {
        let text_document_position = match self.create_text_document_position_params(target_symbol)
        {
            Some(position) => position,
            None => return Ok(None),
        };
        let params = lsp_types::ReferenceParams {
            text_document_position,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: lsp_types::ReferenceContext {
                include_declaration: false,
            },
        };

        transport
            .send_request(request::References::METHOD, params)
            .await?;
        let mut response = match transport
            .recv_response_or_null::<Vec<lsp_types::Location>>()
            .await?
        {
            Some(response) => response,
            None => return Ok(None),
        };

        // Filter out mojom generated files.
        response.retain(|location| !is_mojom_generated(&location.uri));
        Ok(Some(response))
    }

    fn create_text_document_position_params(
        &self,
        target_symbol: &DocumentSymbol,
    ) -> Option<lsp_types::TextDocumentPositionParams> {
        let location = match self.find_symbol_location(target_symbol) {
            Some(location) => location,
            None => return None,
        };

        let text_document = self.text_document.clone();
        let position = location.range.start;

        Some(lsp_types::TextDocumentPositionParams {
            text_document,
            position,
        })
    }

    fn find_interface_symbol(
        &self,
        interface: &InterfaceSymbol,
    ) -> Option<lsp_types::SymbolInformation> {
        self.symbols
            .iter()
            .find(|symbol| {
                symbol.kind == lsp_types::SymbolKind::CLASS && symbol.name == interface.name
            })
            .cloned()
    }

    fn find_method_symbol(
        &self,
        method: &MethodSymbol,
    ) -> Option<(lsp_types::SymbolInformation, lsp_types::SymbolInformation)> {
        let mut stack: Vec<&lsp_types::SymbolInformation> = Vec::new();
        for symbol in self.symbols.iter() {
            while let Some(outer) = stack.last() {
                if outer.location.range.end >= symbol.location.range.start {
                    break;
                }
                stack.pop();
            }

            if symbol.kind == lsp_types::SymbolKind::CLASS {
                stack.push(symbol);
                continue;
            }

            if symbol.kind != lsp_types::SymbolKind::METHOD || symbol.name != method.name {
                continue;
            }

            if let Some(interface) = stack.last() {
                if interface.name == method.interface_name {
                    return Some(((*interface).clone(), symbol.clone()));
                }
            }
        }
        None
    }

    fn find_symbol_location(&self, target_symbol: &DocumentSymbol) -> Option<lsp_types::Location> {
        match target_symbol {
            DocumentSymbol::Interface(interface) => self
                .find_interface_symbol(&interface)
                .map(|symbol| symbol.location),
            DocumentSymbol::Method(method) => {
                let method = match self.find_method_symbol(&method) {
                    Some((_interface, method)) => method,
                    None => return None,
                };
                let mut location = method.location.clone();
                // `location` starts with the return type, not the name of the method,
                // but clangd requires the location for the name of the method.
                // Assume that the return type of generated methods is always `void`.
                location.range.start.character += "void ".len() as u32;

                Some(location)
            }
            _ => None,
        }
    }
}

struct CppBindings {
    mojom_uri: Url,
    bindings: Vec<CppBindingHeader>,
}

impl CppBindings {
    async fn close<W>(self, transport: &mut Transport<W>) -> anyhow::Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        for binding in self.bindings {
            transport
                .send_notification(
                    notification::DidCloseTextDocument::METHOD,
                    Some(lsp_types::DidCloseTextDocumentParams {
                        text_document: binding.text_document,
                    }),
                )
                .await?;
        }
        Ok(())
    }

    async fn goto_implementation<W>(
        &self,
        transport: &mut Transport<W>,
        target_symbol: &DocumentSymbol,
    ) -> anyhow::Result<Option<request::GotoImplementationResponse>>
    where
        W: AsyncWrite + Unpin,
    {
        let mut all_locations = Vec::new();
        for binding in self.bindings.iter() {
            if let Some(mut locations) = binding
                .goto_implementation(transport, target_symbol)
                .await?
            {
                all_locations.append(&mut locations);
            }
        }

        if all_locations.is_empty() {
            Ok(None)
        } else {
            let response = request::GotoImplementationResponse::Array(all_locations);
            Ok(Some(response))
        }
    }

    async fn find_references<W>(
        &self,
        transport: &mut Transport<W>,
        target_symbol: DocumentSymbol,
    ) -> anyhow::Result<Option<Vec<lsp_types::Location>>>
    where
        W: AsyncWrite + Unpin,
    {
        // Use Proxy symbols for references.
        let target_symbol = target_symbol.to_proxy_symbol();

        let mut all_locations = Vec::new();
        for binding in self.bindings.iter() {
            if let Some(mut locations) = binding.find_references(transport, &target_symbol).await? {
                all_locations.append(&mut locations);
            }
        }

        if all_locations.is_empty() {
            Ok(None)
        } else {
            Ok(Some(all_locations))
        }
    }
}

struct Transport<W> {
    writer: W,
    next_message_id: u64,
    receiver: Receiver<Message>,
}

impl<W> Transport<W>
where
    W: AsyncWrite + Unpin,
{
    async fn send_request<P>(&mut self, method: &str, params: P) -> anyhow::Result<()>
    where
        P: Serialize,
    {
        let id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);
        send_request(&mut self.writer, id, method, params).await?;
        Ok(())
    }

    async fn send_notification<P>(&mut self, method: &str, params: Option<P>) -> anyhow::Result<()>
    where
        P: Serialize,
    {
        send_notification(&mut self.writer, method, params).await?;
        Ok(())
    }

    async fn recv_response_message(&mut self) -> anyhow::Result<ResponseMessage> {
        while let Some(message) = self.receiver.recv().await {
            match message {
                Message::Request(request) => {
                    return Err(anyhow::anyhow!("Unexpected request: {:?}", request));
                }
                Message::Response(response) => {
                    return Ok(response);
                }
                Message::Notification(notification) => {
                    // Ignore notifications for now.
                    log::trace!("{:?}", notification);
                }
            }
        }
        anyhow::bail!("Transport error");
    }

    async fn recv_response_or_null<R: DeserializeOwned>(&mut self) -> anyhow::Result<Option<R>> {
        let message = self.recv_response_message().await?;
        if let Some(error) = message.error {
            anyhow::bail!("Request failed: {:?}", error);
        }
        let result = match message.result {
            Some(result) => result,
            None => anyhow::bail!("No result"),
        };
        if result.is_null() {
            return Ok(None);
        }
        let response = serde_json::from_value(result)?;
        Ok(Some(response))
    }
}

pub(crate) struct Clangd {
    #[allow(unused)]
    child: Child,

    root_path: PathBuf,
    gen_path: PathBuf,

    transport: Transport<ChildStdin>,

    bindings: Option<CppBindings>,
}

impl Clangd {
    async fn ensure_binding_headers(&mut self, mojom_uri: &Url) -> anyhow::Result<()> {
        if let Some(cached) = self.bindings.as_ref() {
            if &cached.mojom_uri == mojom_uri {
                return Ok(());
            }
            self.drop_binding_headers().await?;
        }

        let base_path = {
            let root_path = self.root_path.to_str().unwrap();
            let out_path = self.gen_path.to_str().unwrap();
            mojom_uri.path().replace(root_path, out_path)
        };

        let mojom_uri = mojom_uri.clone();
        let mut bindings = Vec::new();
        if let Ok(non_blink) = self.open_header(base_path.clone() + ".h").await {
            bindings.push(non_blink);
        }
        if let Ok(blink) = self.open_header(base_path + "-blink.h").await {
            bindings.push(blink);
        }
        self.bindings = Some(CppBindings {
            mojom_uri,
            bindings,
        });
        Ok(())
    }

    async fn open_header(&mut self, path: impl AsRef<Path>) -> anyhow::Result<CppBindingHeader> {
        let uri = Url::from_file_path(path)
            .map_err(|err| anyhow::anyhow!("Failed to convert file: {:?}", err))?;

        // Open document.
        let text_document_item = {
            let path = uri
                .to_file_path()
                .map_err(|err| anyhow::anyhow!("{:?}", err))?;
            let text = tokio::fs::read_to_string(path).await?;
            lsp_types::TextDocumentItem {
                uri: uri.clone(),
                language_id: "cpp".to_string(),
                version: 1,
                text,
            }
        };
        self.transport
            .send_notification(
                notification::DidOpenTextDocument::METHOD,
                Some(lsp_types::DidOpenTextDocumentParams {
                    text_document: text_document_item.clone(),
                }),
            )
            .await?;

        // Get symbols.
        let text_document = lsp_types::TextDocumentIdentifier::new(uri.clone());
        self.transport
            .send_request(
                request::DocumentSymbolRequest::METHOD,
                lsp_types::DocumentSymbolParams {
                    text_document: text_document.clone(),
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
            .await?;
        let response = self
            .transport
            .recv_response_or_null::<lsp_types::DocumentSymbolResponse>()
            .await?;
        let symbols = match response {
            Some(lsp_types::DocumentSymbolResponse::Flat(symbols)) => symbols,
            Some(lsp_types::DocumentSymbolResponse::Nested(_)) => unimplemented!(),
            None => vec![],
        };

        Ok(CppBindingHeader {
            text_document,
            symbols,
        })
    }

    async fn drop_binding_headers(&mut self) -> anyhow::Result<()> {
        let bindings = match self.bindings.take() {
            Some(bindings) => bindings,
            None => return Ok(()),
        };
        bindings.close(&mut self.transport).await?;
        Ok(())
    }

    pub(crate) async fn goto_implementation(
        &mut self,
        uri: &Url,
        target_symbol: DocumentSymbol,
    ) -> anyhow::Result<Option<request::GotoImplementationResponse>> {
        self.ensure_binding_headers(uri).await?;
        self.bindings
            .as_ref()
            .unwrap()
            .goto_implementation(&mut self.transport, &target_symbol)
            .await
    }

    pub(crate) async fn find_references(
        &mut self,
        uri: &Url,
        target_symbol: DocumentSymbol,
    ) -> anyhow::Result<Option<Vec<lsp_types::Location>>> {
        self.ensure_binding_headers(uri).await?;
        self.bindings
            .as_ref()
            .unwrap()
            .find_references(&mut self.transport, target_symbol)
            .await
    }
}

pub(crate) async fn start(
    root_path: PathBuf,
    gen_path: PathBuf,
    mut params: ClangdParams,
) -> anyhow::Result<Clangd> {
    use std::process::Stdio;

    let mut command = Command::new(&params.clangd_path);
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    if params.log_level.is_some() {
        command.stderr(Stdio::piped());
    } else {
        command.stderr(Stdio::null());
    }
    if let Some(dir) = params.compile_commands_dir.as_ref() {
        command.arg(format!("--compile-commands-dir={}", dir.display()));
    }
    let mut child = command.spawn()?;

    if let Some(log_level) = params.log_level.take() {
        let stderr = child.stderr.take().unwrap();
        tokio::spawn(stderr_task(stderr, log_level));
    }

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    initialize(&params, &mut reader, &mut stdin).await?;

    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(reader_task(reader, sender));

    let transport = Transport {
        writer: stdin,
        next_message_id: 1,
        receiver,
    };

    Ok(Clangd {
        child,
        root_path,
        gen_path,
        transport,
        bindings: None,
    })
}

async fn initialize<R, W>(
    params: &ClangdParams,
    reader: &mut R,
    writer: &mut W,
) -> anyhow::Result<()>
where
    R: AsyncRead + AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let workspace_dir = match params.compile_commands_dir.as_ref() {
        Some(dir) => dir.clone(),
        None => std::env::current_dir()?,
    };
    let uri = Url::from_directory_path(&workspace_dir)
        .map_err(|err| anyhow::anyhow!("Failed to locate workspace folder: {:?}", err))?;
    let workspace_folder = lsp_types::WorkspaceFolder {
        uri,
        name: "chromium".to_string(),
    };

    let params = lsp_types::InitializeParams {
        process_id: Some(std::process::id()),
        workspace_folders: Some(vec![workspace_folder]),
        ..Default::default()
    };
    send_request(writer, 0, request::Initialize::METHOD, params).await?;

    let message = recv_message(reader).await?;
    let init_response = match message {
        Message::Response(response) => response,
        _ => anyhow::bail!("Unexpected message: {:?}", message),
    };

    if let Some(error) = init_response.error {
        anyhow::bail!("{} failed: {:?}", request::Initialize::METHOD, error);
    }

    send_notification(
        writer,
        notification::Initialized::METHOD,
        Some(&lsp_types::InitializedParams {}),
    )
    .await?;

    Ok(())
}

async fn stderr_task(stderr: ChildStderr, log_level: log::Level) -> anyhow::Result<()> {
    let mut lines = BufReader::new(stderr).lines();
    while let Some(line) = lines.next_line().await? {
        let first = match line.chars().next() {
            Some(ch) => ch,
            None => continue,
        };
        match first {
            'E' => {
                if log_level <= log::Level::Error {
                    log::error!("{}", line);
                }
            }
            'W' => {
                if log_level <= log::Level::Warn {
                    log::warn!("{}", line);
                }
            }
            'I' => {
                if log_level <= log::Level::Info {
                    log::info!("{}", line);
                }
            }
            'D' => {
                if log_level <= log::Level::Debug {
                    log::debug!("{}", line);
                }
            }
            'T' => {
                if log_level <= log::Level::Trace {
                    log::trace!("{}", line);
                }
            }
            _ => (),
        }
    }
    Ok(())
}

async fn reader_task<R>(mut reader: R, sender: Sender<Message>) -> anyhow::Result<()>
where
    R: AsyncRead + AsyncBufRead + Unpin,
{
    while let Ok(message) = recv_message(&mut reader).await {
        sender.send(message).await?;
    }
    Ok(())
}

fn is_mojom_generated(uri: &Url) -> bool {
    let path = uri.path();
    path.contains("/gen/") && path.contains("mojom")
}

#[derive(Debug)]
pub(crate) enum ClangdMessage {
    GotoImplementation(u64, Url, DocumentSymbol),
    References(u64, Url, DocumentSymbol),
}

pub(crate) async fn clangd_task(
    mut clangd: Clangd,
    mut receiver: Receiver<ClangdMessage>,
    rpc_sender: RpcSender,
) -> anyhow::Result<()> {
    while let Some(message) = receiver.recv().await {
        match message {
            ClangdMessage::GotoImplementation(id, uri, target_symbol) => {
                let response = clangd
                    .goto_implementation(&uri, target_symbol)
                    .await
                    .map_err(|err| ResponseError::new(ErrorCodes::InternalError, err.to_string()));
                rpc_sender.send_response(id, response).await?;
            }
            ClangdMessage::References(id, uri, target_symbol) => {
                let response = clangd
                    .find_references(&uri, target_symbol)
                    .await
                    .map_err(|err| ResponseError::new(ErrorCodes::InternalError, err.to_string()));
                rpc_sender.send_response(id, response).await?;
            }
        }
    }
    Ok(())
}
