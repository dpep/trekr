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
        // Whitespace between messages carries nothing; dropped here, a run of
        // it is not scanned again at every read.
        let blank = self
            .buffer
            .iter()
            .take_while(|b| b.is_ascii_whitespace())
            .count();
        self.buffer.drain(..blank);
        let parsed = match frame(&self.buffer) {
            Ok(Some((length, body))) => {
                let parsed = serde_json::from_slice::<Message>(&self.buffer[body])
                    .map_err(|e| format!("a message body that is not JSON-RPC: {e}"));
                self.buffer.drain(..length);
                parsed
            }
            Ok(None) => return None,
            Err(why) => Err(why),
        };
        parsed
            .map_err(|why| {
                // Nothing after it can be trusted to line up, which is what
                // lsp-server's reader concluded too.
                eprintln!("trekr: closing the connection: {why}");
                self.closed = true;
                self.buffer.clear();
            })
            .ok()
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

/// The longest body read: a `didOpen` of the largest file trekr reads, with
/// room for JSON's escaping. A longer one is refused, not buffered toward.
const MAX_MESSAGE: usize = 4 * crate::scan::MAX_SOURCE as usize;

/// The longest header section read, blank line included. A client sends
/// `Content-Length` and perhaps `Content-Type`, under a hundred bytes; 8 KiB
/// is the limit HTTP servers commonly put on one header line (nginx's
/// buffer, Apache's `LimitRequestFieldSize`). Past it, framing is refused
/// rather than waited for, and each read rescans at most this much.
const MAX_HEADERS: usize = 8 * 1024;

/// The frame at the start of `buffer`: its length, whitespace before it
/// included, and where its body lies. `None` until it has all arrived, `Err`
/// saying what is wrong when its headers are not LSP's.
///
/// Laxer than the spec (DEC-810): header names in any case, space or none
/// around the `:`, `\n` line ends, and whitespace between messages — Spoom's
/// client writes `Content-Length:93` and a `\n` after each body, which Sorbet
/// reads. The length
/// is still bytes, as the spec says: a client counting characters is cut
/// short on a non-ASCII body, and refused for it rather than guessed at.
fn frame(buffer: &[u8]) -> Result<Option<(usize, std::ops::Range<usize>)>, String> {
    let mut at = buffer
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(buffer.len());
    let headers_end = buffer.len().min(at.saturating_add(MAX_HEADERS));
    let mut length = None;
    loop {
        let Some(newline) = buffer[at..headers_end].iter().position(|&b| b == b'\n') else {
            if headers_end == buffer.len() {
                return Ok(None);
            }
            return Err(format!(
                "a header section longer than the {MAX_HEADERS} bytes headers may be"
            ));
        };
        let line = &buffer[at..at + newline];
        at += newline + 1;
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        let line = String::from_utf8_lossy(line);
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!("a header line with no `:`: {line:?}"));
        };
        if name.trim().eq_ignore_ascii_case("Content-Length") {
            let value = value.trim();
            let Ok(parsed) = value.parse::<usize>() else {
                return Err(format!(
                    "a Content-Length that is not a byte count: {value:?}"
                ));
            };
            if length.is_some_and(|before| before != parsed) {
                return Err("two Content-Length headers that disagree".into());
            }
            length = Some(parsed);
        }
    }
    let Some(length) = length else {
        return Err("a message with no Content-Length header".into());
    };
    if length > MAX_MESSAGE {
        return Err(format!(
            "a Content-Length of {length} bytes, past the {MAX_MESSAGE} a message may be"
        ));
    }
    let end = at
        .checked_add(length)
        .ok_or("a Content-Length past addressable memory")?;
    Ok((buffer.len() >= end).then_some((end, at..end)))
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
        let thread = crate::stack::spawn(move || {
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

    /// The methods read off `bytes`, and whether the reader then closed.
    fn methods_in(bytes: &[u8]) -> (Vec<String>, bool) {
        let mut reader = reader_over(bytes, Vec::new());
        reader.fill(Some(Duration::ZERO));
        let methods = std::iter::from_fn(|| reader.message())
            .map(|m| match m {
                Message::Notification(n) => n.method,
                other => panic!("{other:?}"),
            })
            .collect();
        (methods, reader.is_closed())
    }

    #[test]
    fn spooms_framing_is_read_no_space_after_the_colon_and_a_newline_after_the_body() {
        let one = format!(
            "Content-Length:{}\r\n\r\n{INITIALIZED}\n",
            INITIALIZED.len()
        );
        let (methods, closed) = methods_in(format!("{one}{one}").as_bytes());
        assert_eq!(methods, ["initialized", "initialized"]);
        assert!(!closed);
    }

    #[test]
    fn header_names_in_any_case_and_space_around_the_colon_are_read() {
        let n = INITIALIZED.len();
        for header in [
            format!("content-length: {n}"),
            format!("CONTENT-LENGTH:\t{n}"),
            format!("Content-Length : {n} "),
            format!(
                "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length:{n}"
            ),
        ] {
            let bytes = format!("{header}\r\n\r\n{INITIALIZED}");
            assert_eq!(
                methods_in(bytes.as_bytes()).0,
                ["initialized"],
                "{header:?}"
            );
        }
    }

    #[test]
    fn bare_newline_line_ends_and_whitespace_between_messages_are_read() {
        let n = INITIALIZED.len();
        let bytes = format!(
            "Content-Length: {n}\n\n{INITIALIZED}\r\n \r\n\tContent-Length: {n}\r\n\n{INITIALIZED}\n\n"
        );
        let (methods, closed) = methods_in(bytes.as_bytes());
        assert_eq!(methods, ["initialized", "initialized"]);
        assert!(!closed, "trailing whitespace waits for the next message");
    }

    #[test]
    fn a_missing_or_garbage_content_length_is_refused_and_says_what_was_wrong() {
        for (headers, why) in [
            (
                "Content-Type: application/json",
                "a message with no Content-Length header",
            ),
            ("Content-Length: abc", "not a byte count: \"abc\""),
            ("Content-Length: -1", "not a byte count: \"-1\""),
            ("Content-Length:", "not a byte count: \"\""),
            ("hello", "a header line with no `:`: \"hello\""),
            (
                "Content-Length: 2\r\nContent-Length: 3",
                "two Content-Length headers that disagree",
            ),
        ] {
            let bytes = format!("{headers}\r\n\r\n{{}}");
            let refused = super::frame(bytes.as_bytes()).expect_err(headers);
            assert!(refused.contains(why), "{headers:?}: {refused}");
            assert_eq!(methods_in(bytes.as_bytes()), (vec![], true), "{headers:?}");
        }
    }

    #[test]
    fn a_content_length_past_what_a_message_may_be_is_refused_not_waited_for() {
        for length in [u64::MAX, 1 << 40, MAX_MESSAGE as u64 + 1] {
            let bytes = format!("Content-Length: {length}\r\n\r\n{{}}");
            let refused = super::frame(bytes.as_bytes()).expect_err("refused");
            assert!(refused.contains("past the"), "{length}: {refused}");
            assert_eq!(methods_in(bytes.as_bytes()), (vec![], true), "{length}");
        }
        let at_most = format!("Content-Length: {MAX_MESSAGE}\r\n\r\n{{}}");
        assert_eq!(
            super::frame(at_most.as_bytes()),
            Ok(None),
            "waits for the rest"
        );
    }

    #[test]
    fn a_header_section_past_its_cap_is_refused_not_scanned_toward() {
        // No newline yet: refused once the cap is passed, not waited on.
        let mut line = b"X-Pad: ".to_vec();
        line.resize(MAX_HEADERS + 1, b'a');
        let refused = super::frame(&line).expect_err("refused");
        assert!(refused.contains("header section longer than"), "{refused}");
        assert_eq!(
            super::frame(&line[..MAX_HEADERS]),
            Ok(None),
            "at the cap, waits"
        );
        assert_eq!(methods_in(&line), (vec![], true));

        // Lines that each end, but add up past it, are refused too.
        let many = "X-Pad: a\r\n".repeat(MAX_HEADERS / 10 + 1);
        let bytes = format!("{many}Content-Length: 2\r\n\r\n{{}}");
        assert!(super::frame(bytes.as_bytes()).is_err());

        // Long, but under it: read.
        let pad = "a".repeat(MAX_HEADERS - 200);
        let n = INITIALIZED.len();
        let bytes = format!("X-Pad: {pad}\r\nContent-Length: {n}\r\n\r\n{INITIALIZED}");
        assert_eq!(
            methods_in(bytes.as_bytes()),
            (vec!["initialized".into()], false)
        );
    }

    #[test]
    fn whitespace_between_messages_is_dropped_not_held() {
        let blank = " \r\n".repeat(MAX_HEADERS);
        let mut reader = reader_over(blank.as_bytes(), Vec::new());
        reader.fill(Some(Duration::ZERO));
        assert!(reader.message().is_none());
        assert!(reader.unread().is_empty(), "nothing to rescan or hand on");
        assert!(!reader.is_closed());
    }

    #[test]
    fn content_length_counts_bytes_not_characters() {
        let body = r#"{"jsonrpc":"2.0","method":"é","params":{}}"#;
        let bytes = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        assert_eq!(methods_in(bytes.as_bytes()), (vec!["é".to_string()], false));
        // A client counting characters cuts the body short: refused, not
        // stretched to fit.
        let chars = format!("Content-Length: {}\r\n\r\n{body}", body.chars().count());
        assert_eq!(methods_in(chars.as_bytes()), (vec![], true));
    }
}
