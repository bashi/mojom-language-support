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

use serde::Serialize;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};

use super::protocol::{Header, JsonRpcNotificationMessage, JsonRpcRequestMessage, Message};

pub(crate) async fn send_request<W, P>(
    writer: &mut W,
    id: u64,
    method: &str,
    params: &P,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    P: Serialize,
{
    let params = serde_json::to_value(params)?;
    let message = JsonRpcRequestMessage {
        jsonrpc: "2.0",
        id,
        method,
        params,
    };
    send_message(writer, &message).await
}

pub(crate) async fn send_notification<W, P>(
    writer: &mut W,
    method: &str,
    params: &P,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    P: Serialize,
{
    let params = serde_json::to_value(params)?;
    let message = JsonRpcNotificationMessage {
        jsonrpc: "2.0",
        method,
        params,
    };
    send_message(writer, &message).await
}

pub(crate) async fn send_message<W, M>(writer: &mut W, message: &M) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    M: Serialize,
{
    let message = serde_json::to_string(message)?;
    let content_length = message.len();

    let header = format!("Content-Length: {}\r\n\r\n", content_length);
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(message.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

pub(crate) async fn recv_message<R>(reader: &mut R) -> anyhow::Result<Message>
where
    R: AsyncRead + AsyncBufRead + Unpin,
{
    let header = read_header_part(reader).await?;
    let mut message_body = vec![0; header.content_length];
    reader.read_exact(&mut message_body).await?;
    serde_json::from_slice::<Message>(&message_body)
        .map_err(|err| anyhow::anyhow!("Failed to parse message body: {:?}", err))
}

async fn read_header_part<R>(reader: &mut R) -> anyhow::Result<Header>
where
    R: AsyncRead + AsyncBufRead + Unpin,
{
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            anyhow::bail!("No header");
        }
        if line == "\r\n" {
            break;
        }

        let (name, value) = match line.split_once(':') {
            Some((name, value)) => (name, value),
            None => anyhow::bail!("Invalid header field: {}", line),
        };

        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                anyhow::bail!("Multiple Content-Length fields");
            }
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }

    let content_length = match content_length {
        Some(content_length) => content_length,
        None => anyhow::bail!("Missing Content-Length header field"),
    };

    Ok(Header { content_length })
}
