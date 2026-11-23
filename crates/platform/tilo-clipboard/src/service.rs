//! The clipboard as a service: an app's copies and pastes over the connection its session
//! opened for it, answered by the process that holds the person's selection.
//!
//! An app runs in a process with no display of its own, so the selection is not there to offer
//! or read: the session holds it, and this is the wire between the two. Every message is one
//! length-prefixed text header and, for anything that carries bytes, one length-prefixed body
//! after it — the framing every socket in the tree uses ([`wire_frame`]).
//!
//! A side is whatever answers on the session's end ([`ClipboardSide`]): the session's own
//! clipboard, so what an app copies is the person's copy (it lands in their history and on every
//! app's clipboard) and what it pastes is what they copied anywhere.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use wire_frame::Framing;

use crate::{MAX_CLIPBOARD_IMAGE_INPUT_BYTES, MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES};

/// A request's header: `copy <mime>`, `paste <mime> …`, or `quit`.
const HEADER: Framing = Framing::little_endian(4096);
/// The bytes a copy offers or a paste answers: an image as copied anywhere, held to the bound
/// the clipboard itself sets for one taken in.
const BODY: Framing = Framing::little_endian(MAX_CLIPBOARD_IMAGE_INPUT_BYTES);
/// The longest text one copy or paste carries.
const TEXT: usize = MAX_CLIPBOARD_PLAIN_FALLBACK_BYTES;

/// What answers on the session's end of an app's clipboard connection: the selection the
/// person's device holds.
pub trait ClipboardSide: Send + Sync {
    /// Offer `bytes` of `mime` as the person's copy. False when the selection could not take it.
    fn copy(&self, mime: &str, bytes: &[u8]) -> bool;

    /// What the selection holds of the first of `types` it offers, as its media type and bytes.
    /// None when it holds none of them.
    fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)>;
}

/// Serve an app's clipboard requests on `stream` until it closes the connection.
pub fn serve(stream: UnixStream, side: &dyn ClipboardSide) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = stream;
    loop {
        let Ok(Some(header)) = HEADER.read(&mut reader) else {
            return;
        };
        let Ok(text) = String::from_utf8(header) else {
            return;
        };
        let mut words = text.split_whitespace();
        match words.next() {
            Some("copy") => {
                let Some(mime) = words.next() else {
                    return;
                };
                let Ok(Some(bytes)) = BODY.read(&mut reader) else {
                    return;
                };
                let answer: &[u8] = if side.copy(mime, &bytes) {
                    b"ok"
                } else {
                    b"no"
                };
                if HEADER.write(&mut writer, answer).is_err() || writer.flush().is_err() {
                    return;
                }
            }
            Some("paste") => {
                let types: Vec<&str> = words.collect();
                let wrote = match side.paste(&types) {
                    Some((mime, bytes)) => {
                        let header = format!("paste {mime} {}", bytes.len());
                        HEADER
                            .write(&mut writer, header.as_bytes())
                            .and_then(|()| BODY.write(&mut writer, &bytes))
                    }
                    None => HEADER.write(&mut writer, b"none"),
                };
                if wrote.is_err() || writer.flush().is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// The most a copy may offer of `mime`: a text is held to what text is, an image to what an image
/// taken in may be.
#[must_use]
pub fn most_of(mime: &str) -> usize {
    if mime.starts_with("text/") {
        TEXT
    } else {
        MAX_CLIPBOARD_IMAGE_INPUT_BYTES
    }
}

/// How long an app waits for the session's answer: a paste waits on the app that owns the
/// selection, which answers when it gets round to it, and never for good.
const WAIT: Duration = Duration::from_secs(30);

/// An app's end of the clipboard connection: what its runtime copies and pastes through.
pub struct Connected {
    writer: Mutex<UnixStream>,
    reader: Mutex<UnixStream>,
}

fn held<T>(lock: Result<T, std::sync::PoisonError<T>>) -> T {
    lock.unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Connected {
    /// The clipboard over `stream`, the connection the session opened for the app; none when the
    /// connection cannot be read or written.
    #[must_use]
    pub fn connect(stream: UnixStream) -> Option<Self> {
        let reader = stream.try_clone().ok()?;
        let _ = reader.set_read_timeout(Some(WAIT));
        let _ = reader.set_write_timeout(Some(WAIT));
        Some(Self {
            reader: Mutex::new(reader),
            writer: Mutex::new(stream),
        })
    }

    /// Offer `bytes` of `mime` as the person's copy: false when the session did not take it.
    #[must_use]
    pub fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
        if bytes.len() > most_of(mime) {
            return false;
        }
        let mut writer: MutexGuard<'_, UnixStream> = held(self.writer.lock());
        let sent = HEADER
            .write(&mut *writer, format!("copy {mime}").as_bytes())
            .and_then(|()| BODY.write(&mut *writer, bytes));
        if sent.is_err() || writer.flush().is_err() {
            return false;
        }
        drop(writer);
        let mut reader = held(self.reader.lock());
        matches!(HEADER.read(&mut *reader), Ok(Some(answer)) if answer.as_slice() == b"ok")
    }

    /// What the selection holds of the first of `types` it offers, as its media type and bytes.
    #[must_use]
    pub fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        let mut writer = held(self.writer.lock());
        let asked = format!("paste {}", types.join(" "));
        if HEADER.write(&mut *writer, asked.as_bytes()).is_err() || writer.flush().is_err() {
            return None;
        }
        drop(writer);
        let mut reader = held(self.reader.lock());
        let header = HEADER.read(&mut *reader).ok()??;
        let header = String::from_utf8(header).ok()?;
        let mut words = header.split_whitespace();
        if words.next() != Some("paste") {
            return None;
        }
        let mime = words.next()?.to_owned();
        let limit: usize = words.next()?.parse().ok()?;
        let bytes = BODY.read(&mut *reader).ok()??;
        drop(reader);
        // The header bounds what is read: a body of another length is not the paste it named.
        (bytes.len() == limit).then_some((mime, bytes))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// What the person's selection holds, as the session would answer it.
    #[derive(Default)]
    struct Selection(Mutex<Option<(String, Vec<u8>)>>);

    impl ClipboardSide for Selection {
        fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
            *held(self.0.lock()) = Some((mime.to_owned(), bytes.to_vec()));
            true
        }

        fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
            let held_copy = held(self.0.lock()).clone()?;
            types.iter().any(|t| *t == held_copy.0).then_some(held_copy)
        }
    }

    #[test]
    fn what_an_app_copies_the_selection_holds_and_what_it_pastes_comes_back() {
        let (app, session) = UnixStream::pair().expect("a socket pair");
        let selection = Arc::new(Selection::default());
        let serving = Arc::clone(&selection);
        let server = std::thread::spawn(move || serve(session, &*serving));
        let clipboard = Connected::connect(app).expect("the connection is usable");

        assert!(clipboard.copy("text/plain", b"Lisbon in May"));
        assert_eq!(
            held(selection.0.lock()).clone(),
            Some(("text/plain".to_owned(), b"Lisbon in May".to_vec()))
        );
        assert_eq!(
            clipboard.paste(&["text/plain"]),
            Some(("text/plain".to_owned(), b"Lisbon in May".to_vec()))
        );

        let image = vec![0x89, b'P', b'N', b'G', 0, 1, 2, 3];
        assert!(clipboard.copy("image/png", &image));
        assert_eq!(
            clipboard.paste(&["image/png", "image/jpeg"]),
            Some(("image/png".to_owned(), image)),
            "an image an app copied pastes as the image"
        );
        assert_eq!(
            clipboard.paste(&["text/plain"]),
            None,
            "a type the selection does not hold pastes nothing"
        );
        drop(clipboard);
        server.join().expect("the server stops when the app closes");
    }

    #[test]
    fn a_copy_the_selection_refuses_answers_no() {
        struct Refusing;
        impl ClipboardSide for Refusing {
            fn copy(&self, _: &str, _: &[u8]) -> bool {
                false
            }

            fn paste(&self, _: &[&str]) -> Option<(String, Vec<u8>)> {
                None
            }
        }
        let (app, session) = UnixStream::pair().expect("a socket pair");
        let refusing: Arc<dyn ClipboardSide> = Arc::new(Refusing);
        let server = std::thread::spawn(move || serve(session, &*refusing));
        let clipboard = Connected::connect(app).expect("the connection is usable");
        assert!(!clipboard.copy("text/plain", b"words"));
        assert_eq!(clipboard.paste(&["text/plain"]), None);
        drop(clipboard);
        server.join().expect("the server stops");
    }

    #[test]
    fn a_copy_larger_than_the_clipboard_holds_is_refused_before_it_is_sent() {
        let (app, session) = UnixStream::pair().expect("a socket pair");
        // Nothing answers on the other end: the bound is the client's, checked before anything is
        // written, and nothing here waits for a reply.
        drop(session);
        let clipboard = Connected::connect(app).expect("the connection is usable");
        let oversized = vec![0u8; TEXT + 1];
        assert!(!clipboard.copy("text/plain", &oversized));
        assert!(!clipboard.copy("image/png", &vec![0u8; MAX_CLIPBOARD_IMAGE_INPUT_BYTES + 1]));
    }
}
