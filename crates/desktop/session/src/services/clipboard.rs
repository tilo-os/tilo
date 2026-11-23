//! The person's clipboard, served to the apps that hold no display of their own: a copy goes
//! onto the selection as the person's own copy, and a paste answers what is on it.
//!
//! An app runs in a process with no way to the display, so the selection is not there to offer or
//! read. The session opens a connection for each app it starts ([`crate::apps`], and the launcher
//! for a Rust app it launches), and answers it here on its own loop: what an app copies is put on
//! the clipboard as any app's copy is ([`Clipboard::copy_offer`]), so the person finds it in their
//! history and on their other devices, and what it pastes is read off the selection, bounded as
//! the clipboard bounds an image taken in. One service answers both languages.

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
    copies: Mutex<Vec<Copy>>,
    asks: Mutex<Vec<Ask>>,
    pending: Mutex<Option<Pending>>,
}

/// One copy an app asked for: the selection it is for, whether histories and device sync must
/// keep it out, and the formats it offers with their bytes.
struct Copy {
    selection: Selection,
    secret: bool,
    formats: Vec<(String, Vec<u8>)>,
}

/// What a paste answers: each requested media type the selection held and its bytes, or nothing.
type Answer = Vec<(String, Vec<u8>)>;

/// Where a paste's answer goes, and the end the connection thread waits on.
type Reply = Sender<Answer>;
type Answered = Receiver<Answer>;

/// A paste an app asked for: the selection, the types it wants, and where the answer goes.
struct Ask {
    selection: Selection,
    types: Vec<String>,
    reply: Reply,
}

/// The selection reads in flight for one ask.
struct Pending {
    reads: Vec<(String, Pull)>,
    done: Vec<(String, Vec<u8>)>,
    ask: Ask,
}

/// The selection a service target names.
fn selection_of(target: tilo_clipboard::service::Target) -> Selection {
    match target {
        tilo_clipboard::service::Target::Clipboard => Selection::Clipboard,
        tilo_clipboard::service::Target::Primary => Selection::Primary,
    }
}

impl Side {
    /// Put what the app copied on the clipboard, and answer its paste, on the loop's pass: a read
    /// of the selection is drained each pass until it is all here, one ask at a time.
    fn follow(&self, clipboard: &Clipboard, compositor: &mut Compositor) {
        let copies = std::mem::take(&mut *self.copies.lock_recover());
        for copy in copies {
            let mut offer = copy.formats;
            if copy.secret {
                // A secret pastes as plain text everywhere, while histories and device sync are
                // told not to keep it.
                offer.push((
                    tilo_clipboard::SECRET_MIME.to_owned(),
                    tilo_clipboard::SECRET_MARKER.to_vec(),
                ));
                offer.push((
                    tilo_clipboard::PASSWORD_MANAGER_HINT_MIME.to_owned(),
                    tilo_clipboard::SECRET_MARKER.to_vec(),
                ));
            }
            clipboard.copy_offer(compositor, copy.selection, offer);
        }
        // The read in flight, if there is one, finishes first.
        let reading = {
            let mut pending = self.pending.lock_recover();
            if let Some(read) = pending.as_mut() {
                let mut remaining = Vec::new();
                for (mime, mut pull) in read.reads.drain(..) {
                    match pull.drain() {
                        Pulled::Pending => remaining.push((mime, pull)),
                        Pulled::Ready(bytes) => read.done.push((mime, bytes)),
                        Pulled::Lost => {}
                    }
                }
                read.reads = remaining;
            }
            let done = pending
                .as_ref()
                .is_some_and(|read| read.reads.is_empty())
                .then(|| pending.take())
                .flatten();
            if let Some(done) = done {
                let _ = done.ask.reply.send(done.done);
            }
            pending.is_some()
        };
        if reading {
            return;
        }
        loop {
            let asking = self.asks.lock_recover().pop();
            let Some(ask) = asking else {
                return;
            };
            let offered = compositor.selection_formats(ask.selection);
            let wanted: Vec<String> = ask
                .types
                .iter()
                .filter(|ty| offered.iter().any(|held| held == *ty))
                .cloned()
                .collect();
            let mut reads = Vec::new();
            for mime in wanted {
                let limit = tilo_clipboard::service::most_of(&mime);
                match compositor.pull_selection_up_to(ask.selection, &mime, limit) {
                    Ok(pull) => reads.push((mime, pull)),
                    Err(error) => tracing::warn!(%error, "an app's paste could not be read"),
                }
            }
            if reads.is_empty() {
                let _ = ask.reply.send(Vec::new());
                continue;
            }
            *self.pending.lock_recover() = Some(Pending {
                reads,
                done: Vec::new(),
                ask,
            });
            return;
        }
    }
}

impl tilo_clipboard::service::ClipboardSide for Side {
    fn copy(
        &self,
        target: tilo_clipboard::service::Target,
        secret: bool,
        formats: &[(String, Vec<u8>)],
    ) -> bool {
        self.copies.lock_recover().push(Copy {
            selection: selection_of(target),
            secret,
            formats: formats.to_vec(),
        });
        (self.wake)();
        true
    }

    fn paste(
        &self,
        target: tilo_clipboard::service::Target,
        types: &[&str],
    ) -> Vec<(String, Vec<u8>)> {
        let (reply, answer): (Reply, Answered) = std::sync::mpsc::channel();
        self.asks.lock_recover().push(Ask {
            selection: selection_of(target),
            types: types.iter().map(|mime| (*mime).to_owned()).collect(),
            reply,
        });
        (self.wake)();
        answer.recv_timeout(WAIT).unwrap_or_default()
    }
}
