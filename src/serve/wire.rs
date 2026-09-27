//! The LSP wire over stdio, read on the serve loop's own thread.
//!
//! lsp-server's transport reads stdin on a thread of its own, through std's
//! buffered `Stdin`, and parks each parsed message in a rendezvous `send`. At
//! any instant that thread may hold bytes it has taken off the pipe and not yet
//! delivered — which is invisible, and fine, until the process wants to `exec`
//! a successor on the same pipes (`reload.rs`, DEC-050): exec destroys them,
//! and the client's next message is gone. So input is read here, from the raw descriptor, by the
//! thread that also decides when to exec. Every byte taken off the pipe is in
//! [`Reader::unread`] or already parsed, and nothing is read in between.
//!
//! Output keeps a writer thread, so a client slow to read cannot stall the
//! loop; [`Writer::flush`] is the barrier that says everything sent is on the
//! wire.

use lsp_server::Message;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd};
use std::sync::mpsc;
use std::time::Duration;

/// Messages off a descriptor, with what has been read but not yet parsed kept
/// where a caller can see it.
pub(crate) struct Reader {
    /// Read without a userspace buffer, so the bytes `buffer` holds are all the
    /// bytes this process has taken.
    input: File,
    buffer: Vec<u8>,
    /// End of input, or input that is not LSP framing — either way, nothing
    /// more will come.
    closed: bool,
}

impl Reader {
    /// Standard input, preceded by bytes an earlier process read and did not
    /// get to parse.
    pub(crate) fn stdin(unread: Vec<u8>) -> io::Result<Reader> {
        // A duplicate of fd 0, read directly: `Stdin` would buffer ahead.
        let fd = io::stdin().as_fd().try_clone_to_owned()?;
        Ok(Reader::new(File::from(fd), unread))
    }

    pub(crate) fn new(input: File, unread: Vec<u8>) -> Reader {
        Reader {
            input,
            buffer: unread,
            closed: false,
        }
    }

    /// Take whatever has arrived, waiting at most `timeout` (forever when
    /// `None`) for it. False when nothing came.
    pub(crate) fn fill(&mut self, timeout: Option<Duration>) -> bool {
        if self.closed || !readable(&self.input, timeout) {
            return false;
        }
        let mut chunk = vec![0u8; 64 * 1024];
        match self.input.read(&mut chunk) {
            Ok(0) => self.closed = true,
            Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => self.closed = true,
        }
        true
    }

    /// The next whole message in the buffer, if one has fully arrived.
    pub(crate) fn message(&mut self) -> Option<Message> {
        let length = match frame_length(&self.buffer) {
            Ok(Some(length)) => length,
            Ok(None) => return None,
            Err(()) => {
                // Not LSP framing: nothing after it can be trusted to line up,
                // which is what lsp-server's reader concluded too.
                self.closed = true;
                self.buffer.clear();
                return None;
            }
        };
        let frame: Vec<u8> = self.buffer.drain(..length).collect();
        match Message::read(&mut frame.as_slice()) {
            Ok(message) => message,
            Err(_) => {
                self.closed = true;
                self.buffer.clear();
                None
            }
        }
    }

    /// Nothing more will arrive.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed
    }

    /// Bytes read and not yet a whole message — what a successor must be
    /// handed, or they are lost.
    pub(crate) fn unread(&self) -> &[u8] {
        &self.buffer
    }
}

/// How long the frame at the start of `buffer` is, headers and all: `None`
/// until it has all arrived, `Err` when the headers are not LSP's.
fn frame_length(buffer: &[u8]) -> Result<Option<usize>, ()> {
    let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let headers = std::str::from_utf8(&buffer[..end]).map_err(|_| ())?;
    let length = headers
        .split("\r\n")
        .filter_map(|line| line.split_once(": "))
        .find(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
        .ok_or(())?
        .1
        .parse::<usize>()
        .map_err(|_| ())?;
    let total = end + 4 + length;
    Ok((buffer.len() >= total).then_some(total))
}

/// Is there something to read (or the end of input) within `timeout`?
fn readable(input: &File, timeout: Option<Duration>) -> bool {
    let millis = match timeout {
        None => -1,
        // Rounded up, so a sub-millisecond wait is not a busy poll of zero.
        Some(t) => i32::try_from(t.as_micros().div_ceil(1000)).unwrap_or(i32::MAX),
    };
    let mut fd = libc::pollfd {
        fd: input.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `fd` is one valid, initialized pollfd that outlives the call,
    // and nfds is 1 to match. The descriptor is owned by `input`, which is
    // borrowed for the duration, so it cannot be closed underneath poll.
    let ready = unsafe { libc::poll(&mut fd, 1, millis) };
    // EINTR and friends read as "nothing yet"; the caller comes back.
    ready > 0
}

/// Messages to stdout, from a thread of their own.
pub(crate) struct Writer {
    sender: mpsc::Sender<Out>,
    thread: std::thread::JoinHandle<()>,
}

enum Out {
    Message(Message),
    /// Answered once everything queued before it has been written.
    Flush(mpsc::Sender<()>),
}

impl Writer {
    pub(crate) fn stdout() -> Writer {
        let (sender, receiver) = mpsc::channel::<Out>();
        let thread = std::thread::spawn(move || {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            for out in receiver {
                match out {
                    // `write` flushes each message. A failed write is the
                    // client gone; the next `send` reports it.
                    Out::Message(message) => {
                        if message.write(&mut stdout).is_err() {
                            return;
                        }
                    }
                    Out::Flush(done) => {
                        let _ = done.send(());
                    }
                }
            }
        });
        Writer { sender, thread }
    }

    pub(crate) fn send(&self, message: Message) -> anyhow::Result<()> {
        self.sender
            .send(Out::Message(message))
            .map_err(|_| anyhow::anyhow!("the client stopped reading"))
    }

    /// Wait until everything sent so far is on the wire.
    pub(crate) fn flush(&self) {
        let (done, wait) = mpsc::channel();
        if self.sender.send(Out::Flush(done)).is_ok() {
            let _ = wait.recv();
        }
    }

    /// Write what is queued, then stop.
    pub(crate) fn finish(self) {
        drop(self.sender);
        let _ = self.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;

    fn reader_over(bytes: &[u8], unread: Vec<u8>) -> Reader {
        let (read, mut write) = io::pipe().unwrap();
        io::Write::write_all(&mut write, bytes).unwrap();
        drop(write);
        Reader::new(File::from(std::os::fd::OwnedFd::from(read)), unread)
    }

    #[test]
    fn a_partial_frame_is_kept_unread_until_the_rest_arrives() {
        let whole = frame(INITIALIZED);
        let (head, tail) = whole.split_at(whole.len() - 5);
        let mut reader = reader_over(head, Vec::new());
        assert!(reader.fill(Some(Duration::ZERO)));
        assert!(reader.message().is_none());
        assert_eq!(reader.unread(), head, "held, not dropped");

        // A successor handed those bytes finishes the message from the pipe.
        let mut successor = reader_over(tail, reader.unread().to_vec());
        successor.fill(Some(Duration::ZERO));
        let Some(Message::Notification(n)) = successor.message() else {
            panic!("the message completes across the handoff");
        };
        assert_eq!(n.method, "initialized");
        assert!(successor.unread().is_empty());
    }

    #[test]
    fn several_frames_in_one_read_come_out_in_order() {
        let mut bytes = frame(r#"{"jsonrpc":"2.0","id":1,"method":"a","params":{}}"#);
        bytes.extend(frame(
            r#"{"jsonrpc":"2.0","id":2,"method":"b","params":{}}"#,
        ));
        let mut reader = reader_over(&bytes, Vec::new());
        reader.fill(Some(Duration::ZERO));
        let methods: Vec<String> = std::iter::from_fn(|| reader.message())
            .map(|m| match m {
                Message::Request(r) => r.method,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(methods, ["a", "b"]);
    }

    #[test]
    fn the_end_of_input_and_garbage_both_close_the_reader() {
        let mut ended = reader_over(b"", Vec::new());
        assert!(ended.fill(None), "EOF is readable, not a hang");
        assert!(ended.is_closed());

        let mut garbage = reader_over(b"hello\r\n\r\n", Vec::new());
        garbage.fill(Some(Duration::ZERO));
        assert!(garbage.message().is_none());
        assert!(garbage.is_closed());
    }
}
