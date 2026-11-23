//! The person's clipboard, served to the tilolang apps that hold no display of their own: a copy
//! goes onto the selection as the person's own copy, and a paste answers what is on it.
//!
//! An app's instance runs in a process with no way to the display, so the selection is not there
//! to offer or read. The session opens a connection for each instance it starts
//! ([`crate::apps`]), and answers it here on its own loop: what an app copies is put on the
//! clipboard as any app's copy is ([`Clipboard::copy_bytes`]), so the person finds it in their
//! history and on their other devices, and what it pastes is read off the selection, bounded as
//! the clipboard bounds an image taken in.

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tilo_compositor::clipboard_history::{Pull, Pulled};
use tilo_compositor::{Compositor, Selection};
use tilo_lock::MutexRecover as _;

use crate::clipboard::Clipboard;

/// How long an app's paste waits for the answer the loop gives it: a read waits on the app that
/// owns the selection, which answers when it gets round to it.
const WAIT: Duration = Duration::from_secs(30);

/// The apps connected now, each held weakly: a connection whose app went ends its side, and the
/// next pass drops it.
type Sides = Arc<Mutex<Vec<Weak<Side>>>>;

/// The clipboard service, served until the session ends.
pub(crate) struct Served {
    sides: Sides,
}

impl Served {
    /// Put what the apps copied on the person's clipboard, and answer what they paste, each pass.
    pub(crate) fn follow(&self, clipboard: &Clipboard, compositor: &mut Compositor) {
        let mut sides = self.sides.lock_recover();
        sides.retain(|weak| match weak.upgrade() {
            Some(side) => {
                side.follow(clipboard, compositor);
                true
            }
            // The app's connection ended with its instance: nothing answers it now.
            None => false,
        });
    }
}

/// Bind the clipboard socket under the launcher's services root and serve it.
///
/// # Errors
/// The services root or the socket cannot be made, or the acceptor cannot start.
pub(crate) fn serve(wake: tilo_service::Wake) -> Result<Served, String> {
    launcher::services::ensure_services_root()
        .map_err(|error| format!("create the services directory: {error}"))?;
    serve_at(
        &launcher::services::socket_for(launcher::services::CLIPBOARD_SERVICE),
        wake,
    )
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the clipboard service accepts and serves on its own threads"
)]
fn serve_at(path: &Path, wake: tilo_service::Wake) -> Result<Served, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| format!("remove the stale clipboard socket: {error}"))?;
    }
    let listener = tilo_files::native::socket::bind(path)
        .map_err(|error| format!("bind the clipboard socket {}: {error}", path.display()))?;
    let sides = Arc::new(Mutex::new(Vec::new()));
    let accepting = Arc::clone(&sides);
    std::thread::Builder::new()
        .name("tilo-clipboard".into())
        .spawn(move || accept(&listener, &accepting, &wake))
        .map_err(|error| format!("start the clipboard service: {error}"))?;
    tracing::info!(path = %path.display(), "serving the clipboard to the apps");
    Ok(Served { sides })
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the clipboard service accepts and serves on its own threads"
)]
fn accept(listener: &UnixListener, sides: &Sides, wake: &tilo_service::Wake) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let side = Arc::new(Side {
                    wake: Arc::clone(wake),
                    copies: Mutex::new(Vec::new()),
                    asks: Mutex::new(Vec::new()),
                    pending: Mutex::new(None),
                });
                sides.lock_recover().push(Arc::downgrade(&side));
                if let Err(error) = std::thread::Builder::new()
                    .name("tilo-clipboard-app".into())
                    .spawn(move || tilo_clipboard::service::serve(stream, &*side))
                {
                    tracing::warn!(%error, "a clipboard connection could not be served");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::warn!(%error, "the clipboard service's accept failed");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// One app's connection: what it copied since the loop last took it, what it asked to paste, and
/// the read of the selection in flight for it.
pub(crate) struct Side {
    wake: tilo_service::Wake,
    copies: Mutex<Vec<(String, Vec<u8>)>>,
    asks: Mutex<Vec<Ask>>,
    pending: Mutex<Option<Pending>>,
}

/// What a paste answers: the media type the selection held and its bytes, or nothing.
type Answer = Option<(String, Vec<u8>)>;

/// Where a paste's answer goes, and the end the connection thread waits on.
type Reply = Sender<Answer>;
type Answered = Receiver<Answer>;

/// A paste an app asked for: the types it wants, foremost first, and where the answer goes.
struct Ask {
    types: Vec<String>,
    reply: Reply,
}

/// The selection read in flight for one ask.
struct Pending {
    mime: String,
    pull: Pull,
    ask: Ask,
}

impl Side {
    /// Put what the app copied on the clipboard, and answer its paste, on the loop's pass: a read
    /// of the selection is drained each pass until it is all here, one ask at a time.
    fn follow(&self, clipboard: &Clipboard, compositor: &mut Compositor) {
        let copies = std::mem::take(&mut *self.copies.lock_recover());
        for (mime, bytes) in copies {
            clipboard.copy_bytes(compositor, &mime, bytes);
        }
        // The read in flight, if there is one, finishes first.
        let reading = {
            let mut pending = self.pending.lock_recover();
            let outcome = pending.as_mut().map(|read| read.pull.drain());
            let reading = match outcome {
                None | Some(Pulled::Pending) => pending.is_some(),
                Some(Pulled::Ready(bytes)) => {
                    if let Some(done) = pending.take() {
                        let _ = done.ask.reply.send(Some((done.mime, bytes)));
                    }
                    false
                }
                Some(Pulled::Lost) => {
                    if let Some(done) = pending.take() {
                        let _ = done.ask.reply.send(None);
                    }
                    false
                }
            };
            drop(pending);
            reading
        };
        if reading {
            return;
        }
        loop {
            let asking = self.asks.lock_recover().pop();
            let Some(ask) = asking else {
                return;
            };
            let offered = compositor.selection_formats(Selection::Clipboard);
            let Some(mime) = ask
                .types
                .iter()
                .find(|wanted| offered.iter().any(|held| &held == wanted))
                .cloned()
            else {
                let _ = ask.reply.send(None);
                continue;
            };
            let limit = tilo_clipboard::service::most_of(&mime);
            match compositor.pull_selection_up_to(Selection::Clipboard, &mime, limit) {
                Ok(pull) => {
                    *self.pending.lock_recover() = Some(Pending { mime, pull, ask });
                    return;
                }
                Err(error) => {
                    tracing::warn!(%error, "an app's paste could not be read");
                    let _ = ask.reply.send(None);
                }
            }
        }
    }
}

impl tilo_clipboard::service::ClipboardSide for Side {
    fn copy(&self, mime: &str, bytes: &[u8]) -> bool {
        self.copies
            .lock_recover()
            .push((mime.to_owned(), bytes.to_vec()));
        (self.wake)();
        true
    }

    fn paste(&self, types: &[&str]) -> Option<(String, Vec<u8>)> {
        let (reply, answer): (Reply, Answered) = std::sync::mpsc::channel();
        self.asks.lock_recover().push(Ask {
            types: types.iter().map(|mime| (*mime).to_owned()).collect(),
            reply,
        });
        (self.wake)();
        answer.recv_timeout(WAIT).ok().flatten()
    }
}
