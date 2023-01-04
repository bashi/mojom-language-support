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

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Message {
    Request(RequestMessage),
    Response(ResponseMessage),
    Notification(NotificationMessage),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RequestMessage {
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RequestMessage {
    pub(crate) fn get_params<P>(&self) -> anyhow::Result<P>
    where
        P: DeserializeOwned,
    {
        let params = match self.params.as_ref() {
            Some(params) => params.clone(),
            None => anyhow::bail!("No parameters for {}", self.method),
        };

        let params = serde_json::from_value(params)?;
        Ok(params)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ResponseMessage {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ResponseError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ResponseError {
    pub(crate) fn new(code: ErrorCodes, message: String) -> ResponseError {
        ResponseError {
            code: code.into(),
            message: message,
            data: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NotificationMessage {
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl NotificationMessage {
    pub(crate) fn get_params<P>(&self) -> anyhow::Result<P>
    where
        P: DeserializeOwned,
    {
        let params = match self.params.as_ref() {
            Some(params) => params.clone(),
            None => anyhow::bail!("No parameters for {}", self.method),
        };

        let params = serde_json::from_value(params)?;
        Ok(params)
    }
}

#[allow(unused)]
pub(crate) enum ErrorCodes {
    // Defined by JSON RPC
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    InternalError,
    #[allow(non_camel_case_types)]
    serverErrorStart,
    #[allow(non_camel_case_types)]
    serverErrorEnd,
    ServerNotInitialized,
    UnknownErrorCode,

    // Defined by the protocol
    RequestCancelled,
    ContentModified,
}

impl From<ErrorCodes> for i32 {
    fn from(code: ErrorCodes) -> i32 {
        match code {
            ErrorCodes::ParseError => -32700,
            ErrorCodes::InvalidRequest => -32600,
            ErrorCodes::MethodNotFound => -32601,
            ErrorCodes::InvalidParams => -32602,
            ErrorCodes::InternalError => -32603,
            ErrorCodes::serverErrorStart => -32099,
            ErrorCodes::serverErrorEnd => -32000,
            ErrorCodes::ServerNotInitialized => -32002,
            ErrorCodes::UnknownErrorCode => -32001,
            ErrorCodes::RequestCancelled => -32800,
            ErrorCodes::ContentModified => -32801,
        }
    }
}

// https://microsoft.github.io/language-server-protocol/specification#header-part
#[derive(Debug)]
pub(crate) struct Header {
    pub content_length: usize,
}

#[derive(Serialize)]
pub(crate) struct JsonRpcRequestMessage<'a> {
    pub jsonrpc: &'a str,
    pub id: u64,
    pub method: &'a str,
    pub params: Value,
}

#[derive(Serialize)]
pub(crate) struct JsonRpcResponseMessage<'a> {
    pub jsonrpc: &'a str,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Serialize)]
pub(crate) struct JsonRpcNotificationMessage<'a> {
    pub jsonrpc: &'a str,
    pub method: &'a str,
    pub params: Option<Value>,
}
