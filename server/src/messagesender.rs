use std::io::Write;
use std::sync::mpsc::{channel, Sender};
use std::thread;

use serde_json::Value;

use super::protocol::{self, NotificationMessage, ResponseError};

struct SuccessResponse {
    id: u64,
    result: Value,
}

struct ErrorResponse {
    id: u64,
    err: ResponseError,
}

enum SendingMessage {
    SuccessResponse(SuccessResponse),
    ErrorResponse(ErrorResponse),
    Notification(NotificationMessage),
}

#[derive(Clone)]
pub(crate) struct MessageSender {
    sender: Sender<SendingMessage>,
}

// TODO: Make sure using unwrap() makes sense.
impl MessageSender {
    pub(crate) fn send_success_response(&self, id: u64, res: Value) {
        let msg = SendingMessage::SuccessResponse(SuccessResponse {
            id: id,
            result: res,
        });
        self.sender.send(msg).unwrap();
    }

    pub(crate) fn send_error_response(&self, id: u64, err: ResponseError) {
        let msg = SendingMessage::ErrorResponse(ErrorResponse { id: id, err: err });
        self.sender.send(msg).unwrap();
    }

    pub(crate) fn send_notification(&self, notif: NotificationMessage) {
        let msg = SendingMessage::Notification(notif);
        self.sender.send(msg).unwrap();
    }
}

pub(crate) struct MessageSenderThread {
    sender: Sender<SendingMessage>,
    handle: thread::JoinHandle<()>,
}

impl MessageSenderThread {
    #[allow(unused)]
    pub(crate) fn join(self) {
        self.handle.join().unwrap();
    }

    pub(crate) fn get_sender(&self) -> MessageSender {
        MessageSender {
            sender: self.sender.clone(),
        }
    }
}

pub(crate) fn start_message_sender_thread<W: Write + Send + 'static>(
    mut writer: W,
) -> MessageSenderThread {
    let (sender, receiver) = channel();
    let handle = thread::spawn(move || loop {
        let msg = receiver.recv();
        // Terminate the thread when recv() failed.
        let msg = if let Ok(msg) = msg { msg } else { break };

        match msg {
            SendingMessage::SuccessResponse(res) => {
                protocol::write_success_response(&mut writer, res.id, res.result).unwrap();
            }
            SendingMessage::ErrorResponse(res) => {
                protocol::write_error_response(&mut writer, res.id, res.err).unwrap();
            }
            SendingMessage::Notification(notif) => {
                protocol::write_notification(&mut writer, &notif.method, notif.params).unwrap();
            }
        };
    });

    MessageSenderThread {
        sender: sender,
        handle: handle,
    }
}
