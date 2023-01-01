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

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use serde_json::Value;

use super::protocol::{
    read_message, ErrorCodes, Message, NotificationMessage, RequestMessage, ResponseError,
    ResponseMessage,
};

use super::clangd::{self, ClangdParams};
use super::diagnostic::{start_diagnostics_thread, DiagnosticsThread};
use super::messagesender::{start_message_sender_thread, MessageSender};

#[derive(PartialEq)]
enum State {
    Initialized,
    ShuttingDown(u64),
    Terminated,
}

struct ServerContext {
    state: State,
    // A handler to send messages on the main thread.
    msg_sender: MessageSender,
    // A handler to the diagnostics thread.
    diag: DiagnosticsThread,
    // Set when `exit` notification is received.
    exit_code: Option<i32>,
}

impl ServerContext {
    fn new(msg_sender: MessageSender, diag: DiagnosticsThread) -> ServerContext {
        ServerContext {
            state: State::Initialized,
            msg_sender,
            diag,
            exit_code: None,
        }
    }
}

// Requests

fn get_request_params<P: serde::de::DeserializeOwned>(
    params: Option<Value>,
) -> std::result::Result<P, ResponseError> {
    let params = match params {
        Some(params) => params,
        None => {
            return Err(ResponseError::new(
                ErrorCodes::InvalidParams,
                "No parameters".to_string(),
            ))
        }
    };

    serde_json::from_value::<P>(params)
        .map_err(|err| ResponseError::new(ErrorCodes::InvalidRequest, err.to_string()))
}

fn handle_request(ctx: &mut ServerContext, msg: RequestMessage) -> anyhow::Result<()> {
    let id = msg.id;
    let method = msg.method.as_str();
    log::info!("[recv({})] Request: {}", id, method);

    // Workaround for Eglot. It sends "exit" as a request, not as a notification.
    if method == "exit" {
        exit_notification(ctx);
        return Ok(());
    }

    use lsp_types::request::*;
    let res = match method {
        Initialize::METHOD => initialize_request(),
        Shutdown::METHOD => shutdown_request(ctx, msg.id),
        GotoDefinition::METHOD => get_request_params(msg.params)
            .and_then(|params| goto_definition_request(&mut ctx.diag, params)),
        GotoImplementation::METHOD => get_request_params(msg.params)
            .and_then(|params| goto_implementation_request(ctx, params)),
        References::METHOD => {
            get_request_params(msg.params).and_then(|params| find_references_request(ctx, params))
        }
        _ => unimplemented_request(id, method),
    };
    match res {
        Ok(res) => {
            ctx.msg_sender.send_success_response(id, res);
        }
        Err(err) => ctx.msg_sender.send_error_response(id, err),
    };
    Ok(())
}

fn handle_response(ctx: &mut ServerContext, msg: ResponseMessage) -> anyhow::Result<()> {
    match ctx.state {
        State::ShuttingDown(request_id) if request_id == msg.id => {
            ctx.state = State::Terminated;
        }
        _ => {
            log::debug!("Unimplemented response: {:?}", msg);
        }
    }
    Ok(())
}

type RequestResult = std::result::Result<Value, ResponseError>;

fn unimplemented_request(id: u64, method_name: &str) -> RequestResult {
    let msg = format!(
        "Unimplemented request: id = {} method = {}",
        id, method_name
    );
    let err = ResponseError::new(ErrorCodes::InternalError, msg);
    Err(err)
}

fn initialize_request() -> RequestResult {
    // The server was already initialized.
    let error_message = "Unexpected initialize message".to_owned();
    Err(ResponseError::new(
        ErrorCodes::ServerNotInitialized,
        error_message,
    ))
}

fn shutdown_request(ctx: &mut ServerContext, request_id: u64) -> RequestResult {
    ctx.state = State::ShuttingDown(request_id);
    Ok(Value::Null)
}

fn goto_definition_request(
    diag: &mut DiagnosticsThread,
    params: lsp_types::TextDocumentPositionParams,
) -> RequestResult {
    if let Some(loc) = diag.goto_definition(params.text_document.uri, params.position) {
        let res = serde_json::to_value(loc).unwrap();
        return Ok(res);
    }
    return Ok(Value::Null);
}

fn goto_implementation_request(
    ctx: &mut ServerContext,
    params: lsp_types::request::GotoImplementationParams,
) -> RequestResult {
    let response = ctx.diag.goto_implementation(params);
    response_to_request_result(response)
}

fn find_references_request(
    ctx: &mut ServerContext,
    params: lsp_types::ReferenceParams,
) -> RequestResult {
    let response = ctx.diag.find_references(params);
    response_to_request_result(response)
}

fn response_to_request_result<R>(response: anyhow::Result<Option<R>>) -> RequestResult
where
    R: serde::Serialize,
{
    match response {
        Ok(Some(response)) => Ok(serde_json::to_value(response).unwrap()),
        Ok(None) => Ok(Value::Null),
        Err(err) => Err(ResponseError::new(
            ErrorCodes::InternalError,
            err.to_string(),
        )),
    }
}

// Notifications

fn get_params<P: serde::de::DeserializeOwned>(params: Option<Value>) -> anyhow::Result<P> {
    let params = match params {
        Some(params) => params,
        None => anyhow::bail!("No parameters"),
    };
    serde_json::from_value::<P>(params).map_err(|err| err.into())
}

fn handle_notification(ctx: &mut ServerContext, msg: NotificationMessage) -> anyhow::Result<()> {
    log::info!("[recv] Notification: {}", msg.method);

    use lsp_types::notification::*;
    match msg.method.as_str() {
        Exit::METHOD => exit_notification(ctx),
        DidOpenTextDocument::METHOD => {
            get_params(msg.params).map(|params| did_open_text_document(ctx, params))?;
        }
        DidChangeTextDocument::METHOD => {
            get_params(msg.params).map(|params| did_change_text_document(ctx, params))?;
        }
        DidCloseTextDocument::METHOD => {
            get_params(msg.params).map(|params| did_close_text_document(ctx, params))?;
        }
        // Accept following notifications but do nothing.
        DidChangeConfiguration::METHOD => (),
        WillSaveTextDocument::METHOD => (),
        DidSaveTextDocument::METHOD => (),
        LogTrace::METHOD => (),
        SetTrace::METHOD => (),
        _ => {
            log::warn!("Received unimplemented notification: {:#?}", msg);
        }
    }
    Ok(())
}

fn exit_notification(ctx: &mut ServerContext) {
    // https://microsoft.github.io/language-server-protocol/specification#exit
    let exit_code = match ctx.state {
        State::ShuttingDown(_) | State::Terminated => 0,
        _ => 1,
    };
    ctx.exit_code = Some(exit_code);
}

fn did_open_text_document(ctx: &mut ServerContext, params: lsp_types::DidOpenTextDocumentParams) {
    ctx.diag
        .check(params.text_document.uri, params.text_document.text);
}

fn did_change_text_document(
    ctx: &mut ServerContext,
    params: lsp_types::DidChangeTextDocumentParams,
) {
    let uri = params.text_document.uri.clone();
    let content = params
        .content_changes
        .iter()
        .map(|i| i.text.to_owned())
        .collect::<Vec<_>>();
    let text = content.join("");
    ctx.diag.check(uri, text);
}

fn did_close_text_document(ctx: &mut ServerContext, params: lsp_types::DidCloseTextDocumentParams) {
    ctx.diag.did_close_text_document(params);
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

pub struct ServerStartParams<R, W>
where
    R: Read,
    W: Write + Send + 'static,
{
    pub reader: R,
    pub writer: W,
    pub clangd_params: Option<ClangdParams>,
}

// Returns exit code.
pub fn start<R, W>(mut options: ServerStartParams<R, W>) -> anyhow::Result<i32>
where
    R: Read,
    W: Write + Send + 'static,
{
    let mut reader = BufReader::new(options.reader);
    let mut writer = BufWriter::new(options.writer);

    let params = super::initialization::initialize(&mut reader, &mut writer)?;

    let root_path = get_root_path(&params).unwrap_or(PathBuf::new());
    if root_path.exists() {
        std::env::set_current_dir(&root_path)?;
    }

    let clangd = match options.clangd_params.take() {
        Some(params) => Some(clangd::start_wrapper(root_path.clone(), params)?),
        None => None,
    };

    let msg_sender_thread = start_message_sender_thread(writer);
    let diag = start_diagnostics_thread(root_path, msg_sender_thread.get_sender(), clangd);

    let mut ctx = ServerContext::new(msg_sender_thread.get_sender(), diag);
    loop {
        let message = read_message(&mut reader)?;
        match message {
            Message::Request(request) => handle_request(&mut ctx, request)?,
            Message::Response(response) => handle_response(&mut ctx, response)?,
            Message::Notification(notification) => handle_notification(&mut ctx, notification)?,
        };

        if let Some(exit_code) = ctx.exit_code {
            log::info!("[exit] {}", exit_code);
            return Ok(exit_code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{self, read_message, write_notification, write_request};
    use super::*;

    use lsp_types::notification::*;
    use lsp_types::request::*;
    use pipe::pipe;

    #[test]
    fn test_server_init() {
        let (reader, mut writer) = pipe();

        let params: lsp_types::InitializeParams = Default::default();
        let params = serde_json::to_value(&params).unwrap();

        let (r, w) = pipe();
        let options = ServerStartParams {
            reader,
            writer: w,
            clangd_params: None,
        };
        let handle = std::thread::spawn(move || {
            let status = start(options);
            status
        });

        write_request(
            &mut writer,
            1,
            lsp_types::request::Initialize::METHOD,
            params,
        )
        .unwrap();

        let mut r = BufReader::new(r);
        let msg = read_message(&mut r).unwrap();
        match msg {
            protocol::Message::Response(msg) => {
                assert_eq!(1, msg.id);
            }
            _ => unreachable!(),
        }

        write_notification(
            &mut writer,
            lsp_types::notification::Initialized::METHOD,
            Some(serde_json::Value::Null),
        )
        .unwrap();

        write_request(
            &mut writer,
            2,
            lsp_types::request::Shutdown::METHOD,
            serde_json::Value::Null,
        )
        .unwrap();

        let msg = read_message(&mut r).unwrap();
        match msg {
            protocol::Message::Response(msg) => {
                assert_eq!(2, msg.id);
            }
            _ => unreachable!(),
        }

        write_notification(
            &mut writer,
            lsp_types::notification::Exit::METHOD,
            Some(serde_json::Value::Null),
        )
        .unwrap();

        drop(writer);
        drop(r);

        let status = handle.join().unwrap();
        assert!(status.is_ok());
    }
}
