//! `org.freedesktop.impl.portal.Clipboard`: the clipboard of a remote
//! desktop (or an active input capture) whose app asked for it before the
//! session started.
//!
//! The clipboard is never allowed on its own: it rides on the session the
//! person allowed, and goes with it. What the remote app offers becomes
//! the desktop's selection ([`Clipboard::offer`]): every offered type is
//! pulled at once through one-shot, bounded transfers. What the desktop
//! holds is read for the app on request ([`Clipboard::read`]), and the
//! session tells the portal when its selection changes
//! ([`Clipboards::selection_changed`]), which the apps hear as
//! `SelectionOwnerChanged`.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue};

use crate::input_capture::InputCaptures;
use crate::remote_desktop::Remotes;
use crate::session::Sessions;
use crate::{OBJECT_PATH, Pending, Portal, option};

const MAX_MIME_TYPES: usize = 16;
const MAX_MIME_BYTES: usize = 128;
const MAX_TRANSFER_BYTES: usize = 4 * 1024 * 1024;

/// The desktop's selection, as the session holds it.
pub trait Clipboard: Send + Sync {
    /// Whether there is a selection to share at all.
    fn available(&self) -> bool;

    /// Own the selection, offering `mimes`; their bytes follow through
    /// [`Self::write`].
    ///
    /// # Errors
    /// The selection could not be taken.
    fn offer(&self, mimes: &[String]) -> Result<(), String>;

    /// The bytes of offered type `mime`.
    ///
    /// # Errors
    /// The type is not offered, or the selection moved on.
    fn write(&self, mime: &str, bytes: Vec<u8>) -> Result<(), String>;

    /// The selection's bytes as `mime`.
    fn read(&self, mime: &str) -> Pending<Result<Vec<u8>, String>>;

    /// Drop a selection the portal offered: its session ended.
    fn clear(&self);
}

/// A session without a selection to share.
pub struct NoClipboard;

impl Clipboard for NoClipboard {
    fn available(&self) -> bool {
        false
    }

    fn offer(&self, _mimes: &[String]) -> Result<(), String> {
        Err("this session shares no clipboard".into())
    }

    fn write(&self, _mime: &str, _bytes: Vec<u8>) -> Result<(), String> {
        Err("this session shares no clipboard".into())
    }

    fn read(&self, _mime: &str) -> Pending<Result<Vec<u8>, String>> {
        Box::pin(async { Err("this session shares no clipboard".into()) })
    }

    fn clear(&self) {}
}

struct PendingWrite {
    session: String,
    mime: String,
    reader: UnixStream,
}

#[derive(Default)]
struct State {
    offers: HashMap<String, Vec<String>>,
    /// Transfers asked for, by serial: the session and type.
    asked: BTreeMap<u32, (String, String)>,
    pending: HashMap<u32, PendingWrite>,
    next_serial: u32,
    /// The session whose offer is the desktop's selection.
    owner: Option<String>,
}

/// The sessions' clipboards.
#[derive(Clone)]
pub struct Clipboards {
    state: Arc<Mutex<State>>,
    sessions: Sessions,
    remotes: Remotes,
    captures: InputCaptures,
    clipboard: Arc<Mutex<Arc<dyn Clipboard>>>,
}

impl Clipboards {
    pub(crate) fn new(sessions: &Sessions, remotes: Remotes, captures: InputCaptures) -> Self {
        let clipboards = Self {
            state: Arc::default(),
            sessions: sessions.clone(),
            remotes,
            captures,
            clipboard: Arc::new(Mutex::new(Arc::new(NoClipboard))),
        };
        let ended = clipboards.clone();
        sessions.on_end(move |path| ended.session_ended(path));
        clipboards
    }

    pub(crate) fn set(&self, clipboard: Arc<dyn Clipboard>) {
        *self.clipboard.lock_recover() = clipboard;
    }

    fn clipboard(&self) -> Arc<dyn Clipboard> {
        Arc::clone(&self.clipboard.lock_recover())
    }

    /// Let go of what session `path` offered: it ended, so a revoked app's
    /// bytes never stay on the desktop.
    fn session_ended(&self, path: &str) {
        let mut state = self.state.lock_recover();
        state.offers.remove(path);
        state.asked.retain(|_, (session, _)| session != path);
        state.pending.retain(|_, write| write.session != path);
        let owned = state.owner.as_deref() == Some(path);
        if owned {
            state.owner = None;
        }
        drop(state);
        if owned {
            self.clipboard().clear();
        }
    }

    /// Whether session `path` holds the clipboard for frontend `owner`.
    fn live(&self, path: &str, owner: &str) -> bool {
        self.sessions
            .get(path)
            .is_some_and(|session| session.owner == owner)
            && (self.remotes.clipboard(path) || self.captures.clipboard(path))
    }

    /// The desktop's selection changed to one offering `mimes`: the apps
    /// holding the clipboard hear of it. A change away from a portal
    /// session's offer ends that offer.
    pub fn selection_changed(&self, mimes: Vec<String>) {
        self.state.lock_recover().owner = None;
        let mut sessions = self.remotes.clipboard_sessions();
        sessions.extend(self.captures.clipboard_sessions());
        let Some(connection) = self.sessions.connection() else {
            return;
        };
        self.sessions.spawn(async move {
            let Ok(emitter) = SignalEmitter::new(&connection, OBJECT_PATH) else {
                return;
            };
            for session in sessions {
                let Ok(session) = ObjectPath::try_from(session.as_str()) else {
                    continue;
                };
                let mut options = HashMap::new();
                if let Ok(value) = OwnedValue::try_from(zbus::zvariant::Value::new(mimes.clone())) {
                    options.insert("mime_types".to_owned(), value);
                }
                options.insert("session_is_owner".to_owned(), OwnedValue::from(false));
                let _ = ClipboardPortal::selection_owner_changed(&emitter, session, options).await;
            }
        });
    }

    /// Ask session `path`'s app for its bytes as `mime`; the serial.
    fn ask_transfer(&self, path: &str, mime: &str) -> Option<u32> {
        let mut state = self.state.lock_recover();
        if !state
            .offers
            .get(path)?
            .iter()
            .any(|offered| offered == mime)
        {
            return None;
        }
        let serial = state.next_serial.max(1);
        state.next_serial = serial.wrapping_add(1).max(1);
        state
            .asked
            .insert(serial, (path.to_owned(), mime.to_owned()));
        drop(state);
        if let Some(connection) = self.sessions.connection() {
            let (path, mime) = (path.to_owned(), mime.to_owned());
            self.sessions.spawn(async move {
                let (Ok(emitter), Ok(session)) = (
                    SignalEmitter::new(&connection, OBJECT_PATH),
                    ObjectPath::try_from(path.as_str()),
                ) else {
                    return;
                };
                let _ = ClipboardPortal::selection_transfer(&emitter, session, mime, serial).await;
            });
        }
        Some(serial)
    }
}

fn valid_mime(mime: &str) -> bool {
    !mime.is_empty()
        && mime.len() <= MAX_MIME_BYTES
        && mime.contains('/')
        && !mime.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn mimes(options: &HashMap<String, OwnedValue>) -> Option<Vec<String>> {
    let mut values = option::<Vec<String>>(options, "mime_types")?;
    if values.is_empty() || values.len() > MAX_MIME_TYPES || !values.iter().all(|m| valid_mime(m)) {
        return None;
    }
    values.sort();
    values.dedup();
    Some(values)
}

/// The app's end of a socket handing it `bytes`.
fn fd_for_bytes(bytes: Vec<u8>) -> zbus::fdo::Result<OwnedFd> {
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(zbus::fdo::Error::Failed(
            "clipboard payload too large".into(),
        ));
    }
    let (reader, mut writer) =
        UnixStream::pair().map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
    std::thread::spawn(move || {
        let _ = writer.write_all(&bytes);
    });
    Ok(OwnedFd::from(std::os::fd::OwnedFd::from(reader)))
}

/// What the app wrote, bounded in size and time.
fn bounded_read(reader: &UnixStream) -> Result<Vec<u8>, String> {
    reader
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    reader
        .take(u64::try_from(MAX_TRANSFER_BYTES.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err("clipboard payload too large".into());
    }
    Ok(bytes)
}

/// The `org.freedesktop.impl.portal.Clipboard` interface.
pub struct ClipboardPortal {
    portal: Portal,
}

impl ClipboardPortal {
    #[must_use]
    pub const fn new(portal: Portal) -> Self {
        Self { portal }
    }

    fn clipboards(&self) -> &Clipboards {
        &self.portal.clipboards
    }

    /// The frontend connection of a call on a session holding the
    /// clipboard.
    async fn holder(
        &self,
        header: &zbus::message::Header<'_>,
        session: &ObjectPath<'_>,
    ) -> zbus::fdo::Result<OwnedUniqueName> {
        if !self.clipboards().clipboard().available() {
            return Err(zbus::fdo::Error::Failed("no clipboard to share".into()));
        }
        let (owner, _) = self
            .portal
            .authenticate_session(header, session.as_str(), "")
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal caller".into()))?;
        if !self.clipboards().live(session.as_str(), owner.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied(
                "the session does not hold the clipboard".into(),
            ));
        }
        Ok(owner)
    }
}

#[interface(name = "org.freedesktop.impl.portal.Clipboard")]
impl ClipboardPortal {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn request_clipboard(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        if !self.clipboards().clipboard().available() {
            return Err(zbus::fdo::Error::Failed("no clipboard to share".into()));
        }
        let path = session.as_str();
        self.portal
            .authenticate_session(&header, path, "")
            .await
            .map_err(|_| zbus::fdo::Error::AccessDenied("untrusted portal caller".into()))?;
        let asked = self.clipboards().remotes.request_clipboard(path)
            || self.clipboards().captures.request_clipboard(path);
        if asked {
            Ok(())
        } else {
            Err(zbus::fdo::Error::AccessDenied(
                "the clipboard is asked for before the session starts".into(),
            ))
        }
    }

    async fn set_selection(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        self.holder(&header, &session).await?;
        let mimes = mimes(&options)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("invalid MIME types".into()))?;
        self.clipboards()
            .clipboard()
            .offer(&mimes)
            .map_err(zbus::fdo::Error::Failed)?;
        let path = session.as_str();
        {
            let mut state = self.clipboards().state.lock_recover();
            state.offers.insert(path.to_owned(), mimes.clone());
            state.owner = Some(path.to_owned());
        }
        for mime in &mimes {
            self.clipboards().ask_transfer(path, mime);
        }
        Ok(())
    }

    async fn selection_write(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        serial: u32,
    ) -> zbus::fdo::Result<OwnedFd> {
        self.holder(&header, &session).await?;
        // A serial gives one writer once: no later write replaces bytes
        // already handed over.
        let asked = self.clipboards().state.lock_recover().asked.remove(&serial);
        let (path, mime) = asked
            .filter(|(path, _)| path == session.as_str())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("unknown transfer".into()))?;
        let (reader, writer) =
            UnixStream::pair().map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        self.clipboards().state.lock_recover().pending.insert(
            serial,
            PendingWrite {
                session: path,
                mime,
                reader,
            },
        );
        Ok(OwnedFd::from(std::os::fd::OwnedFd::from(writer)))
    }

    async fn selection_write_done(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        serial: u32,
        success: bool,
    ) -> zbus::fdo::Result<()> {
        self.holder(&header, &session).await?;
        let pending = self
            .clipboards()
            .state
            .lock_recover()
            .pending
            .remove(&serial)
            .filter(|pending| pending.session == session.as_str())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("unknown transfer".into()))?;
        if !success {
            return Ok(());
        }
        let bytes = tokio::task::spawn_blocking(move || bounded_read(&pending.reader))
            .await
            .map_err(|_| zbus::fdo::Error::Failed("the transfer did not finish".into()))?
            .map_err(zbus::fdo::Error::Failed)?;
        let still_owner =
            self.clipboards().state.lock_recover().owner.as_deref() == Some(session.as_str());
        if still_owner {
            self.clipboards()
                .clipboard()
                .write(&pending.mime, bytes)
                .map_err(zbus::fdo::Error::Failed)?;
        }
        Ok(())
    }

    async fn selection_read(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        mime_type: String,
    ) -> zbus::fdo::Result<OwnedFd> {
        self.holder(&header, &session).await?;
        if !valid_mime(&mime_type) {
            return Err(zbus::fdo::Error::InvalidArgs("invalid MIME type".into()));
        }
        let bytes = self
            .clipboards()
            .clipboard()
            .read(&mime_type)
            .await
            .map_err(zbus::fdo::Error::Failed)?;
        fd_for_bytes(bytes)
    }

    #[zbus(signal)]
    async fn selection_owner_changed(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn selection_transfer(
        emitter: &SignalEmitter<'_>,
        session: ObjectPath<'_>,
        mime_type: String,
        serial: u32,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[test]
    fn types_and_payloads_are_bounded() {
        assert!(valid_mime("text/plain"));
        assert!(!valid_mime("text/plain\n"));
        assert!(!valid_mime("not-a-mime"));
        let (reader, mut writer) = UnixStream::pair().expect("pair");
        let writing = std::thread::spawn(move || {
            let _ = writer.write_all(&vec![0; MAX_TRANSFER_BYTES + 1]);
        });
        assert!(bounded_read(&reader).is_err());
        writing.join().expect("written");
    }

    struct Cleared(AtomicBool);

    impl Clipboard for Cleared {
        fn available(&self) -> bool {
            true
        }
        fn offer(&self, _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn write(&self, _: &str, _: Vec<u8>) -> Result<(), String> {
            Ok(())
        }
        fn read(&self, _: &str) -> Pending<Result<Vec<u8>, String>> {
            Box::pin(async { Ok(Vec::new()) })
        }
        fn clear(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn an_ended_session_takes_its_offer_off_the_desktop() {
        let sessions = Sessions::default();
        let clipboards = Clipboards::new(&sessions, Remotes::default(), InputCaptures::default());
        let cleared = Arc::new(Cleared(AtomicBool::new(false)));
        clipboards.set(Arc::clone(&cleared) as Arc<dyn Clipboard>);
        let path = "/org/freedesktop/portal/desktop/session/1_2/r";
        {
            let mut state = clipboards.state.lock_recover();
            state.offers.insert(path.into(), vec!["image/png".into()]);
            state.owner = Some(path.into());
        }
        assert!(clipboards.ask_transfer(path, "image/png").is_some());
        assert!(
            clipboards.ask_transfer(path, "text/plain").is_none(),
            "not offered"
        );
        clipboards.session_ended(path);
        let state = clipboards.state.lock_recover();
        assert!(state.offers.is_empty() && state.asked.is_empty() && state.owner.is_none());
        drop(state);
        assert!(cleared.0.load(Ordering::SeqCst));
    }
}
