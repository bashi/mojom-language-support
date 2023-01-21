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

use std::path::PathBuf;

use lsp_types::notification::{self, Notification};
use lsp_types::request::{self, Request};
use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, BufReader};
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::clangd::{self, ClangdParams};
use super::protocol::{Message, NotificationMessage, ResponseError, ResponseMessage};
use super::rpc;
use super::workspace::{self, WorkspaceMessage};

pub async fn run<R, W>(
    reader: R,
    mut writer: W,
    out_path: PathBuf,
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

    let gen_path = if out_path.is_absolute() {
        out_path.join("gen")
    } else {
        root_path.join(&out_path).join("gen")
    };

    let rpc_sender = spawn_sender_task(writer);

    let clangd_sender = match clangd_params {
        Some(params) => {
            let clangd = clangd::start(root_path.clone(), gen_path.clone(), params).await?;
            let (clangd_sender, clangd_receiver) = mpsc::channel(64);
            let _clangd_task_handle = tokio::spawn(clangd::clangd_task(
                clangd,
                clangd_receiver,
                rpc_sender.clone(),
            ));
            Some(clangd_sender)
        }
        None => None,
    };

    let (_workspace_task_handle, workspace_message_sender) = {
        let rpc_sender = rpc_sender.clone();
        let (sender, receiver) = mpsc::channel(64);
        let handle = tokio::spawn(workspace::workspace_task(
            root_path,
            gen_path,
            rpc_sender,
            clangd_sender,
            sender.clone(),
            receiver,
        ));

        (handle, sender)
    };

    let server = Server {
        state: ServerState::Running,
        reader,
        rpc_sender,
        workspace_message_sender,
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
        declaration_provider: Some(lsp_types::DeclarationCapability::Simple(true)),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        implementation_provider: Some(lsp_types::ImplementationProviderCapability::Simple(true)),
        references_provider: Some(lsp_types::OneOf::Left(true)),
        text_document_sync,
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

enum ServerState {
    Running,
    ShuttingDown,
}

struct Server<R>
where
    R: AsyncBufRead + Unpin,
{
    state: ServerState,
    reader: R,
    rpc_sender: RpcSender,
    workspace_message_sender: Sender<WorkspaceMessage>,
}

impl<R> Server<R>
where
    R: AsyncBufRead + Unpin,
{
    async fn run(mut self) -> anyhow::Result<i32> {
        loop {
            let message = rpc::recv_message(&mut self.reader).await?;
            match message {
                Message::Request(request) => {
                    log::info!("Request: {}({})", request.method, request.id);
                    match request.method.as_str() {
                        request::Shutdown::METHOD => {
                            self.state = ServerState::ShuttingDown;
                            self.rpc_sender
                                .send_success_response(request.id, ())
                                .await?;
                        }
                        _ => {
                            self.workspace_message_sender
                                .send(WorkspaceMessage::RpcRequest(request))
                                .await?;
                        }
                    }
                }
                Message::Response(response) => {
                    log::info!("Response({})", response.id);
                    self.workspace_message_sender
                        .send(WorkspaceMessage::RpcResponse(response))
                        .await?;
                }
                Message::Notification(notification) => {
                    log::info!("Notification: {}", notification.method);
                    match notification.method.as_str() {
                        notification::Exit::METHOD => {
                            let exit_code = match self.state {
                                ServerState::ShuttingDown => 0,
                                _ => 1,
                            };
                            return Ok(exit_code);
                        }
                        _ => {
                            self.workspace_message_sender
                                .send(WorkspaceMessage::RpcNotification(notification))
                                .await?;
                        }
                    }
                }
            }
        }
    }
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

    pub(crate) async fn send_response(
        &self,
        id: u64,
        response: std::result::Result<impl Serialize, ResponseError>,
    ) -> anyhow::Result<()> {
        match response {
            Ok(response) => self.send_success_response(id, response).await?,
            Err(err) => self.send_error_response(id, err).await?,
        }
        Ok(())
    }

    pub(crate) async fn send_success_response(
        &self,
        id: u64,
        response: impl Serialize,
    ) -> anyhow::Result<()> {
        let response = ResponseMessage {
            id,
            result: Some(serde_json::to_value(response)?),
            error: None,
        };
        self.sender.send(Message::Response(response)).await?;
        Ok(())
    }

    pub(crate) async fn send_null_response(&self, id: u64) -> anyhow::Result<()> {
        let response = ResponseMessage {
            id,
            result: Some(serde_json::Value::Null),
            error: None,
        };
        self.sender.send(Message::Response(response)).await?;
        Ok(())
    }

    pub(crate) async fn send_error_response(
        &self,
        id: u64,
        err: ResponseError,
    ) -> anyhow::Result<()> {
        let response = ResponseMessage {
            id,
            result: None,
            error: Some(err),
        };
        self.sender.send(Message::Response(response)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use lsp_types::Url as Uri;
    use serde::de::DeserializeOwned;
    use tokio::io::{BufReader, DuplexStream, ReadHalf, WriteHalf};
    use tokio::task::JoinHandle;

    use super::super::rpc::{recv_message, send_notification, send_request};
    use super::*;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata")
    }

    fn workspace_uri(path: impl AsRef<Path>) -> Uri {
        Uri::from_file_path(workspace_root().join(path).canonicalize().unwrap()).unwrap()
    }

    struct Server {
        handle: JoinHandle<anyhow::Result<i32>>,
    }

    impl Server {
        fn new(stream: DuplexStream) -> Self {
            let handle = tokio::spawn(async move {
                let out_path = workspace_root().join("out").join("Default");
                let (reader, writer) = tokio::io::split(stream);
                let exit_code = run(reader, writer, out_path, None).await?;
                anyhow::Ok(exit_code)
            });
            Server { handle }
        }

        async fn exit(self) -> anyhow::Result<i32> {
            self.handle.await?
        }
    }

    struct Client {
        reader: BufReader<ReadHalf<DuplexStream>>,
        writer: WriteHalf<DuplexStream>,
        next_request_id: u64,
    }

    impl Client {
        fn new(stream: DuplexStream) -> Self {
            let (reader, writer) = tokio::io::split(stream);
            let reader = tokio::io::BufReader::new(reader);
            Client {
                reader,
                writer,
                next_request_id: 0,
            }
        }

        async fn send_request(
            &mut self,
            method: &'static str,
            params: impl Serialize,
        ) -> anyhow::Result<()> {
            let id = self.next_request_id;
            self.next_request_id += 1;
            send_request(&mut self.writer, id, method, params).await?;
            Ok(())
        }

        async fn send_notification(
            &mut self,
            method: &'static str,
            params: Option<impl Serialize>,
        ) -> anyhow::Result<()> {
            send_notification(&mut self.writer, method, params).await?;
            Ok(())
        }

        async fn recv_message(&mut self) -> anyhow::Result<Message> {
            recv_message(&mut self.reader).await
        }

        async fn try_recv_response<R: DeserializeOwned>(&mut self) -> anyhow::Result<R> {
            let message = self.recv_message().await?;
            let response = match message {
                Message::Response(response) => response,
                _ => anyhow::bail!("Unexpected message: {:?}", message),
            };
            if let Some(error) = response.error {
                anyhow::bail!("Shutdown failed: {:?}", error);
            }
            let result = match response.result {
                Some(result) => result,
                None => anyhow::bail!("No result"),
            };
            let response = serde_json::from_value(result)?;
            Ok(response)
        }

        async fn try_recv_notification<N: notification::Notification>(
            &mut self,
        ) -> anyhow::Result<N::Params> {
            let message = self.recv_message().await?;
            let notification = match message {
                Message::Notification(notification) => notification,
                _ => anyhow::bail!("Unexpected message: {:?}", message),
            };
            if notification.method != N::METHOD {
                anyhow::bail!(
                    "Expected {} but got {} notification",
                    N::METHOD,
                    notification.method
                );
            }

            let params = match notification.params {
                Some(params) => params,
                None => anyhow::bail!("No parameter for {}", N::METHOD),
            };
            let params = serde_json::from_value(params)?;
            Ok(params)
        }

        async fn initialize(&mut self) -> anyhow::Result<()> {
            let root_uri = Some(workspace_uri("."));
            let workspace_folders = {
                let uri = root_uri.clone().unwrap();
                let name = "testdata".to_string();
                Some(vec![lsp_types::WorkspaceFolder { uri, name }])
            };
            let params = lsp_types::InitializeParams {
                workspace_folders,
                root_uri,
                ..Default::default()
            };
            self.send_request(request::Initialize::METHOD, &params)
                .await?;

            let message = self.recv_message().await?;
            let response = match message {
                Message::Response(response) => response,
                _ => anyhow::bail!("Unexpected message: {:?}", message),
            };
            if let Some(error) = response.error {
                anyhow::bail!("Initialize failed: {:?}", error);
            }

            self.send_notification(
                notification::Initialized::METHOD,
                Some(lsp_types::InitializedParams {}),
            )
            .await?;
            Ok(())
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            self.send_request(request::Shutdown::METHOD, serde_json::Value::Null)
                .await?;

            let message = self.recv_message().await?;
            let response = match message {
                Message::Response(response) => response,
                _ => anyhow::bail!("Unexpected message: {:?}", message),
            };
            if let Some(error) = response.error {
                anyhow::bail!("Shutdown failed: {:?}", error);
            }

            self.send_notification(
                notification::Exit::METHOD,
                None as Option<serde_json::Value>,
            )
            .await?;
            Ok(())
        }

        async fn open(&mut self, path: impl AsRef<Path>) -> anyhow::Result<()> {
            let uri = workspace_uri(path.as_ref());
            let text = tokio::fs::read_to_string(uri.to_file_path().unwrap()).await?;
            let params = lsp_types::DidOpenTextDocumentParams {
                text_document: lsp_types::TextDocumentItem {
                    uri,
                    language_id: "mojom".to_string(),
                    version: 0,
                    text,
                },
            };
            self.send_notification(notification::DidOpenTextDocument::METHOD, Some(params))
                .await?;

            let notification = self
                .try_recv_notification::<notification::PublishDiagnostics>()
                .await?;
            if notification.diagnostics.len() > 0 {
                log::warn!(
                    "{:?} has diagnostics: {:#?}",
                    path.as_ref(),
                    notification.diagnostics
                );
            }
            Ok(())
        }
    }

    fn create_server_and_client() -> (Server, Client) {
        static START: std::sync::Once = std::sync::Once::new();
        START.call_once(|| {
            let _ = env_logger::try_init();
        });
        let (server, client) = tokio::io::duplex(64);
        let server = Server::new(server);
        let client = Client::new(client);
        (server, client)
    }

    #[tokio::test]
    async fn test_initialize_and_shutdown() -> anyhow::Result<()> {
        let (server, mut client) = create_server_and_client();
        client.initialize().await?;
        client.shutdown().await?;
        let exit_code = server.exit().await?;
        assert_eq!(exit_code, 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_goto_definition() -> anyhow::Result<()> {
        let (server, mut client) = create_server_and_client();
        client.initialize().await?;

        client.open("my_interface.mojom").await?;

        let uri = workspace_uri("my_interface.mojom");
        let text_document = lsp_types::TextDocumentIdentifier { uri };
        let position = lsp_types::Position {
            line: 6,
            character: 23,
        };
        let text_document_position_params = lsp_types::TextDocumentPositionParams {
            text_document,
            position,
        };
        let params = lsp_types::GotoDefinitionParams {
            text_document_position_params,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        client
            .send_request(request::GotoDefinition::METHOD, params)
            .await?;

        let response = client
            .try_recv_response::<lsp_types::GotoDefinitionResponse>()
            .await;
        assert!(response.is_ok());

        client.shutdown().await?;
        server.exit().await?;
        Ok(())
    }
}
