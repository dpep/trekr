//! What the client has said that the server has not acted on yet.
//!
//! The server answers one request at a time, on one thread. That is the right
//! shape for an engine whose answers take a millisecond — and the wrong one the
//! moment one takes a second, because every hover queued behind a slow
//! `references` is answered about a cursor that has long since moved. So the
//! channel is read *ahead*: whatever has arrived is buffered here, and a
//! `$/cancelRequest` is pulled out of the stream as soon as it is seen rather
//! than when its turn comes. A handler in a long loop can ask whether its own
//! request has been cancelled, which is the only point at which a cancellation
//! is worth anything.

use lsp_server::{Connection, Message, RequestId};
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::time::Duration;

pub(crate) struct Inbox<'a> {
    connection: &'a Connection,
    pending: RefCell<VecDeque<Message>>,
    cancelled: RefCell<HashSet<RequestId>>,
    closed: RefCell<bool>,
}

/// What waiting for the next message produced.
pub(crate) enum Next {
    Message(Message),
    /// Nothing arrived within the timeout; there is background work to check.
    Idle,
    /// The client hung up.
    Closed,
}

impl<'a> Inbox<'a> {
    pub(crate) fn new(connection: &'a Connection) -> Inbox<'a> {
        Inbox {
            connection,
            pending: RefCell::default(),
            cancelled: RefCell::default(),
            closed: RefCell::new(false),
        }
    }

    /// Move everything already on the channel into the buffer, without waiting.
    pub(crate) fn drain(&self) {
        while let Ok(message) = self.connection.receiver.try_recv() {
            self.accept(message);
        }
    }

    /// Nothing buffered and nothing waiting — the moment for background work.
    pub(crate) fn is_quiet(&self) -> bool {
        self.drain();
        self.pending.borrow().is_empty()
    }

    /// The next message, waiting at most `timeout` when one is given.
    pub(crate) fn next(&self, timeout: Option<Duration>) -> Next {
        self.drain();
        if let Some(message) = self.pending.borrow_mut().pop_front() {
            return Next::Message(message);
        }
        if *self.closed.borrow() {
            return Next::Closed;
        }
        let received = match timeout {
            Some(timeout) => match self.connection.receiver.recv_timeout(timeout) {
                Ok(message) => Some(message),
                Err(error) if error.is_timeout() => return Next::Idle,
                Err(_) => None,
            },
            None => self.connection.receiver.recv().ok(),
        };
        match received {
            Some(message) => {
                self.accept(message);
                // `accept` may have swallowed it as a cancellation.
                match self.pending.borrow_mut().pop_front() {
                    Some(message) => Next::Message(message),
                    None => Next::Idle,
                }
            }
            None => {
                *self.closed.borrow_mut() = true;
                Next::Closed
            }
        }
    }

    /// Has the client withdrawn this request? Reads ahead first, so a handler
    /// polling this sees a cancellation that arrived while it was working.
    pub(crate) fn is_cancelled(&self, id: &RequestId) -> bool {
        self.drain();
        self.cancelled.borrow().contains(id)
    }

    /// Forget a cancellation once its request has been answered, so the set
    /// does not grow for the life of the session.
    pub(crate) fn settle(&self, id: &RequestId) {
        self.cancelled.borrow_mut().remove(id);
    }

    fn accept(&self, message: Message) {
        if let Message::Notification(notification) = &message
            && notification.method == "$/cancelRequest"
        {
            if let Some(id) = cancelled_id(&notification.params) {
                self.cancelled.borrow_mut().insert(id);
            }
            return;
        }
        self.pending.borrow_mut().push_back(message);
    }
}

/// `{"id": 3}` or `{"id": "abc"}` — both are legal request ids.
fn cancelled_id(params: &serde_json::Value) -> Option<RequestId> {
    match params.get("id")? {
        serde_json::Value::Number(n) => Some(RequestId::from(n.as_i64()? as i32)),
        serde_json::Value::String(s) => Some(RequestId::from(s.clone())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancellation_names_either_kind_of_request_id() {
        assert_eq!(
            cancelled_id(&serde_json::json!({"id": 7})),
            Some(RequestId::from(7))
        );
        assert_eq!(
            cancelled_id(&serde_json::json!({"id": "x"})),
            Some(RequestId::from("x".to_string()))
        );
        assert_eq!(cancelled_id(&serde_json::json!({})), None);
    }

    #[test]
    fn a_cancellation_is_seen_before_the_request_it_withdraws_is_reached() {
        let (server, client) = lsp_server::Connection::memory();
        let inbox = Inbox::new(&server);
        let request = |id: i32| {
            Message::Request(lsp_server::Request::new(
                RequestId::from(id),
                "textDocument/hover".into(),
                serde_json::json!({}),
            ))
        };
        client.sender.send(request(1)).unwrap();
        client.sender.send(request(2)).unwrap();
        client
            .sender
            .send(Message::Notification(lsp_server::Notification::new(
                "$/cancelRequest".into(),
                serde_json::json!({"id": 2}),
            )))
            .unwrap();

        let Next::Message(Message::Request(first)) = inbox.next(None) else {
            panic!("the first request comes out first");
        };
        assert_eq!(first.id, RequestId::from(1));
        assert!(inbox.is_cancelled(&RequestId::from(2)));
        assert!(!inbox.is_cancelled(&RequestId::from(1)));
        drop(client);
    }
}
