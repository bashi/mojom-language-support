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
use std::sync::{Arc, Mutex};

use lsp_types::notification::{self, Notification};
use lsp_types::request::{self, Request};
use lsp_types::Url;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, Command};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::asyncrpc::{recv_message, send_notification, send_request};
use super::mojomast::DocumentSymbol;
use super::protocol::{Message, ResponseMessage};

pub struct ClangdParams {
    pub clangd_path: PathBuf,
    pub out_dir: PathBuf,
    pub compile_commands_dir: Option<PathBuf>,
}

pub(crate) struct Clangd {
    rt: Runtime,
    inner: Arc<Mutex<Inner>>,
}

impl Clangd {
    pub(crate) fn terminate(self) -> anyhow::Result<()> {
        let inner = self.inner;
        self.rt.block_on(async {
            let mut inner = inner.lock().unwrap();
            inner.child.kill().await?;
            Ok::<(), anyhow::Error>(())
        })?;
        Ok(())
    }

    pub(crate) fn goto_implementation(
        &mut self,
        params: request::GotoImplementationParams,
        target_symbol: DocumentSymbol,
    ) -> anyhow::Result<Option<request::GotoImplementationResponse>> {
        let inner = self.inner.clone();
        self.rt.block_on(async {
            let mut inner = inner.lock().unwrap();
            inner.goto_implementation(params, target_symbol).await
        })
    }
}

struct CppBindingHeader {
    text_document: lsp_types::TextDocumentIdentifier,
    symbols: Vec<lsp_types::SymbolInformation>,
}

impl CppBindingHeader {
    fn create_goto_implementation_params(
        &self,
        target_symbol: &DocumentSymbol,
    ) -> Option<request::GotoImplementationParams> {
        let symbol = match self.symbols.iter().find(|symbol| {
            target_symbol.is_lsp_kind(symbol.kind) && target_symbol.name() == symbol.name
        }) {
            Some(symbol) => symbol,
            None => return None,
        };

        let text_document = self.text_document.clone();
        let position = symbol.location.range.start;

        Some(request::GotoImplementationParams {
            text_document_position_params: lsp_types::TextDocumentPositionParams {
                text_document,
                position,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
    }
}

struct CppBindings {
    mojom_uri: Url,
    non_blink: CppBindingHeader,
    blink: CppBindingHeader,
}

struct Inner {
    root_path: PathBuf,
    gen_path: PathBuf,

    child: Child,
    stdin: ChildStdin,
    next_message_id: u64,

    receiver: Receiver<Message>,

    bindings: Option<CppBindings>,
}

impl Inner {
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
        let non_blink = self.open_header(base_path.clone() + ".h").await?;
        let blink = self.open_header(base_path + "-blink.h").await?;
        self.bindings = Some(CppBindings {
            mojom_uri,
            non_blink,
            blink,
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
        self.send_notification(
            notification::DidOpenTextDocument::METHOD,
            &lsp_types::DidOpenTextDocumentParams {
                text_document: text_document_item.clone(),
            },
        )
        .await?;

        // Get symbols.
        let text_document = lsp_types::TextDocumentIdentifier::new(uri.clone());
        self.send_request(
            request::DocumentSymbolRequest::METHOD,
            &lsp_types::DocumentSymbolParams {
                text_document: text_document.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        )
        .await?;
        let response = self
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
        let cached = match self.bindings.take() {
            Some(cached) => cached,
            None => return Ok(()),
        };

        self.send_notification(
            notification::DidCloseTextDocument::METHOD,
            &lsp_types::DidCloseTextDocumentParams {
                text_document: cached.non_blink.text_document,
            },
        )
        .await?;
        self.send_notification(
            notification::DidCloseTextDocument::METHOD,
            &lsp_types::DidCloseTextDocumentParams {
                text_document: cached.blink.text_document,
            },
        )
        .await?;

        Ok(())
    }

    async fn get_implementation_from_clangd(
        &mut self,
        params: request::GotoImplementationParams,
    ) -> anyhow::Result<Option<Vec<lsp_types::Location>>> {
        self.send_request(request::GotoImplementation::METHOD, &params)
            .await?;
        let response = match self
            .recv_response_or_null::<request::GotoImplementationResponse>()
            .await?
        {
            Some(response) => response,
            None => return Ok(None),
        };

        // Filter mojom-generated implementations.
        let is_mojom_generated = |uri: &Url| {
            let path = uri.path();
            path.contains("/gen/") && path.contains("mojom")
        };
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

    async fn goto_implementation(
        &mut self,
        params: request::GotoImplementationParams,
        target_symbol: DocumentSymbol,
    ) -> anyhow::Result<Option<request::GotoImplementationResponse>> {
        self.ensure_binding_headers(&params.text_document_position_params.text_document.uri)
            .await?;
        // Get non-blink implementation.
        let bindings = &self.bindings.as_ref().unwrap().non_blink;
        let locations = match bindings.create_goto_implementation_params(&target_symbol) {
            Some(params) => self.get_implementation_from_clangd(params).await?,
            None => return Ok(None),
        };

        // Get blink implementation.
        let bindings = &self.bindings.as_ref().unwrap().blink;
        let blink_locations = match bindings.create_goto_implementation_params(&target_symbol) {
            Some(params) => self.get_implementation_from_clangd(params).await?,
            None => None,
        };

        // Merge locations.
        let locations = match (locations, blink_locations) {
            (Some(mut a), Some(mut b)) => {
                a.append(&mut b);
                a
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return Ok(None),
        };

        let response = request::GotoImplementationResponse::Array(locations);
        Ok(Some(response))
    }

    async fn send_request<P>(&mut self, method: &str, params: &P) -> anyhow::Result<()>
    where
        P: Serialize,
    {
        let id = self.next_message_id;
        self.next_message_id = self.next_message_id.wrapping_add(1);
        send_request(&mut self.stdin, id, method, params).await?;
        Ok(())
    }

    async fn send_notification<P>(&mut self, method: &str, params: &P) -> anyhow::Result<()>
    where
        P: Serialize,
    {
        send_notification(&mut self.stdin, method, params).await?;
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

pub(crate) fn start(root_path: PathBuf, params: ClangdParams) -> anyhow::Result<Clangd> {
    let rt = Runtime::new()?;
    let gen_path = if params.out_dir.is_absolute() {
        params.out_dir.clone().join("gen")
    } else {
        root_path.join(&params.out_dir).join("gen")
    };
    let inner = rt.block_on(start_impl(root_path, gen_path, params))?;
    let inner = Arc::new(Mutex::new(inner));
    let clangd = Clangd { rt, inner };

    Ok(clangd)
}

async fn start_impl(
    root_path: PathBuf,
    gen_path: PathBuf,
    params: ClangdParams,
) -> anyhow::Result<Inner> {
    use std::process::Stdio;
    let mut command = Command::new(&params.clangd_path);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = params.compile_commands_dir.as_ref() {
        command.arg(format!("--compile-commands-dir={}", dir.display()));
    }
    let mut child = command.spawn()?;

    let stderr = child.stderr.take().unwrap();
    tokio::spawn(stderr_task(stderr));

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    initialize(&params, &mut reader, &mut stdin).await?;

    let (sender, receiver) = mpsc::channel(64);
    tokio::spawn(reader_task(reader, sender));

    Ok(Inner {
        root_path,
        gen_path,
        child,
        stdin,
        next_message_id: 1,
        receiver,
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
    send_request(writer, 0, request::Initialize::METHOD, &params).await?;

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
        &lsp_types::InitializedParams {},
    )
    .await?;

    Ok(())
}

async fn stderr_task(stderr: ChildStderr) -> anyhow::Result<()> {
    let mut lines = BufReader::new(stderr).lines();
    while let Some(line) = lines.next_line().await? {
        let first = match line.chars().next() {
            Some(ch) => ch,
            None => continue,
        };
        match first {
            'E' => log::error!("{}", line),
            'W' => log::warn!("{}", line),
            'I' => log::info!("{}", line),
            'D' => log::debug!("{}", line),
            'T' => log::trace!("{}", line),
            _ => log::debug!("{}", line),
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
