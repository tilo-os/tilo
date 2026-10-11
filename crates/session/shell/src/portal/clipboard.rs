//! Authenticated clipboard portal, attached only to opted-in RemoteDesktop sessions.
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::os::fd::OwnedFd as StdOwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedFd, OwnedValue};

const MAX_MIME_TYPES: usize = 16;
const MAX_MIME_BYTES: usize = 128;
const MAX_TRANSFER_BYTES: usize = 4 * 1024 * 1024;

/// Session authority shared by RemoteDesktop and InputCapture.  Clipboard is
/// never independently authorized: a provider binds it to the owning portal
/// session and decides when that session's capture/control phase is active.
pub trait ClipboardSessionProvider: Send + Sync {
    /// Bind Clipboard to `session` before `Start`. Only the authenticated
    /// frontend connection that created the session may do so.
    fn request_clipboard(&self, session: &str, owner: &str) -> bool;
    /// Who owns `session` while its clipboard is live: requested before
    /// `Start`, started, and not paused by its portal.
    fn clipboard_owner(&self, session: &str) -> Option<String>;
    /// Every session whose clipboard is live. Names are implementation-side
    /// session paths, not app input.
    fn clipboard_sessions(&self) -> Vec<String>;
}

/// Compositor selection bridge. Production can back this with Smithay's live
/// selection store; the memory implementation covers bounded text/image data.
pub trait ClipboardBroker: Send + Sync {
    /// Begin a new remote-owned offer. Implementations must revoke stale
    /// content before accepting replacement bytes.
    fn begin_offer(&self, mimes: &[String]) -> Result<(), String>;
    fn read(&self, mime: &str) -> Result<Vec<u8>, String>;
    fn write(&self, mime: &str, bytes: Vec<u8>) -> Result<(), String>;
    fn clear(&self) -> Result<(), String>;
    /// Return a locally-owned selection change, if any. The portal only
    /// forwards metadata; payloads remain behind `SelectionRead` and the
    /// normal active-session gate.
    fn selection_change(&self) -> Option<Vec<String>> {
        None
    }
    /// Whether this backend is connected to a live compositor selection.
    fn available(&self) -> bool {
        true
    }
}
#[derive(Clone)]
pub struct ClipboardBrokerHandle {
    inner: Arc<dyn ClipboardBroker>,
    selection_wake: Option<crate::idle_wake::IdleWake>,
}
impl ClipboardBrokerHandle {
    pub fn new(broker: Arc<dyn ClipboardBroker>) -> Self {
        Self {
            inner: broker,
            selection_wake: None,
        }
    }

    pub(crate) fn with_selection_wake(mut self, wake: crate::idle_wake::IdleWake) -> Self {
        self.selection_wake = Some(wake);
        self
    }

    fn begin_offer(&self, mimes: &[String]) -> Result<(), String> {
        self.inner.begin_offer(mimes)
    }
    fn read(&self, mime: &str) -> Result<Vec<u8>, String> {
        self.inner.read(mime)
    }
    fn write(&self, mime: &str, bytes: Vec<u8>) -> Result<(), String> {
        self.inner.write(mime, bytes)
    }
    fn clear(&self) -> Result<(), String> {
        self.inner.clear()
    }
    fn selection_change(&self) -> Option<Vec<String>> {
        self.inner.selection_change()
    }
    fn selection_wake(&self) -> Option<crate::idle_wake::IdleWake> {
        self.selection_wake.clone()
    }
    pub fn available(&self) -> bool {
        self.inner.available()
    }
    pub fn unavailable() -> Self {
        Self::new(Arc::new(NullBroker))
    }
}
#[derive(Default)]
pub struct MemoryClipboardBroker {
    values: Mutex<BTreeMap<String, Vec<u8>>>,
}

/// Production bridge into the compositor's focus-gated selection mirror.
/// It preserves arbitrary bounded MIME payloads (including PNG/JPEG) and the
/// compositor publishes them only through normal focused `wl_data_device`.
#[cfg(target_os = "linux")]
pub struct CompositorClipboardBroker {
    selection: crate::compositor::selection::SharedSelection,
    seen_revision: Mutex<u64>,
    pub(crate) wake: crate::idle_wake::IdleWake,
}

#[cfg(target_os = "linux")]
impl CompositorClipboardBroker {
    pub fn new(selection: crate::compositor::selection::SharedSelection) -> Self {
        let wake = crate::idle_wake::IdleWake::default();
        crate::compositor::selection::watch_clipboard_revision(&selection, wake.clone());
        Self {
            selection,
            seen_revision: Mutex::new(0),
            wake,
        }
    }
}

#[cfg(target_os = "linux")]
impl ClipboardBroker for CompositorClipboardBroker {
    fn begin_offer(&self, mimes: &[String]) -> Result<(), String> {
        crate::compositor::selection::begin_mirror_offer(
            &self.selection,
            crate::compositor::selection::Target::Clipboard,
            mimes,
        )
    }
    fn read(&self, mime: &str) -> Result<Vec<u8>, String> {
        crate::compositor::selection::mirror_bytes(
            &self.selection,
            crate::compositor::selection::Target::Clipboard,
            mime,
        )
        .ok_or_else(|| "the compositor clipboard does not offer that MIME type".into())
    }

    fn write(&self, mime: &str, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.len() > MAX_TRANSFER_BYTES {
            return Err("clipboard payload exceeds policy limit".into());
        }
        crate::compositor::selection::set_mirror_bytes(
            &self.selection,
            crate::compositor::selection::Target::Clipboard,
            mime,
            bytes,
        )
    }
    fn clear(&self) -> Result<(), String> {
        crate::compositor::selection::clear_mirror(
            &self.selection,
            crate::compositor::selection::Target::Clipboard,
        );
        Ok(())
    }
    fn selection_change(&self) -> Option<Vec<String>> {
        let snapshot = crate::compositor::selection::selection_snapshot(
            &self.selection,
            crate::compositor::selection::Target::Clipboard,
        );
        let mut seen = self.seen_revision.lock().ok()?;
        if snapshot.revision == 0 || snapshot.revision == *seen {
            return None;
        }
        *seen = snapshot.revision;
        // Remote-owned writes already travel in the opposite direction; do
        // not reflect them back to their source. For local/client selections,
        // wait until a pull produced at least one payload, except a clear
        // (empty MIME list), which must be forwarded immediately.
        (!snapshot.compositor_owned && (snapshot.has_payload || snapshot.mime_types.is_empty()))
            .then_some(snapshot.mime_types)
    }
}
impl MemoryClipboardBroker {
    pub fn set(&self, mime: impl Into<String>, bytes: Vec<u8>) -> Result<(), String> {
        self.write(&mime.into(), bytes)
    }
}
impl ClipboardBroker for MemoryClipboardBroker {
    fn begin_offer(&self, _mimes: &[String]) -> Result<(), String> {
        self.values
            .lock()
            .map_err(|_| "clipboard broker unavailable".to_string())?
            .clear();
        Ok(())
    }
    fn read(&self, mime: &str) -> Result<Vec<u8>, String> {
        self.values
            .lock()
            .map_err(|_| "clipboard broker unavailable".to_string())?
            .get(mime)
            .cloned()
            .ok_or_else(|| "requested MIME type is unavailable".into())
    }
    fn write(&self, mime: &str, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.len() > MAX_TRANSFER_BYTES {
            return Err("clipboard payload exceeds policy limit".into());
        }
        self.values
            .lock()
            .map_err(|_| "clipboard broker unavailable".to_string())?
            .insert(mime.into(), bytes);
        Ok(())
    }
    fn clear(&self) -> Result<(), String> {
        self.values
            .lock()
            .map_err(|_| "clipboard broker unavailable".to_string())?
            .clear();
        Ok(())
    }
}
struct NullBroker;
impl ClipboardBroker for NullBroker {
    fn begin_offer(&self, _: &[String]) -> Result<(), String> {
        Err("the Clipboard portal is unavailable outside a live Tilo compositor".into())
    }
    fn read(&self, _: &str) -> Result<Vec<u8>, String> {
        Err("compositor clipboard broker is not installed".into())
    }
    fn write(&self, _: &str, _: Vec<u8>) -> Result<(), String> {
        Err("compositor clipboard broker is not installed".into())
    }
    fn clear(&self) -> Result<(), String> {
        Ok(())
    }
    fn available(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardTransfer {
    pub session: String,
    pub mime: String,
    pub serial: u32,
}
struct PendingWrite {
    session: String,
    mime: String,
    reader: UnixStream,
}
#[derive(Default)]
struct ClipboardState {
    offers: HashMap<String, Vec<String>>,
    pending: HashMap<u32, PendingWrite>,
    transfers: Vec<ClipboardTransfer>,
    next_serial: u32,
    /// At most one remote session owns the real desktop clipboard.  It is
    /// cleared immediately if that session is closed/revoked.
    active_selection: Option<String>,
}

#[derive(Clone)]
pub struct ClipboardBackend {
    providers: Vec<Arc<dyn ClipboardSessionProvider>>,
    broker: ClipboardBrokerHandle,
    state: Arc<Mutex<ClipboardState>>,
    /// Transfers queued for emission.
    wake: crate::idle_wake::IdleWake,
    /// Session close, so a revoked remote clipboard is cleared without polling.
    session_wake: crate::idle_wake::IdleWake,
}

/// Emit deferred transfer requests without blocking the compositor or D-Bus
/// method dispatcher. The task exits naturally when the connection closes.
pub fn spawn_signal_emitter(connection: zbus::Connection, backend: ClipboardBackend) {
    tokio::spawn(async move {
        let Ok(context) = zbus::object_server::SignalEmitter::new(&connection, super::OBJECT_PATH)
        else {
            return;
        };
        loop {
            backend.prune_revoked();
            if let Some(mime_types) = backend.broker.selection_change() {
                for session in backend.live_sessions() {
                    let Ok(session) = ObjectPath::try_from(session) else {
                        continue;
                    };
                    let mut options = HashMap::new();
                    if let Ok(value) =
                        OwnedValue::try_from(zbus::zvariant::Value::new(mime_types.clone()))
                    {
                        options.insert("mime_types".into(), value);
                    }
                    if ClipboardBackend::selection_owner_changed(&context, session, options)
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            for transfer in backend.take_transfers() {
                let Ok(session) = ObjectPath::try_from(transfer.session) else {
                    continue;
                };
                if ClipboardBackend::selection_transfer(
                    &context,
                    session,
                    transfer.mime,
                    transfer.serial,
                )
                .await
                .is_err()
                {
                    return;
                }
            }
            backend.parked().await;
        }
    });
}
impl Default for ClipboardBackend {
    fn default() -> Self {
        Self::new()
    }
}
impl ClipboardBackend {
    /// No session providers means fail closed.
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
            broker: ClipboardBrokerHandle::new(Arc::new(NullBroker)),
            state: Arc::new(Mutex::new(ClipboardState {
                next_serial: 1,
                ..Default::default()
            })),
            wake: crate::idle_wake::IdleWake::default(),
            session_wake: crate::idle_wake::IdleWake::default(),
        }
    }
    pub fn with_session_providers(
        providers: Vec<Arc<dyn ClipboardSessionProvider>>,
        broker: ClipboardBrokerHandle,
    ) -> Self {
        Self {
            providers,
            broker,
            state: Arc::new(Mutex::new(ClipboardState {
                next_serial: 1,
                ..Default::default()
            })),
            wake: crate::idle_wake::IdleWake::default(),
            session_wake: crate::idle_wake::IdleWake::default(),
        }
    }

    /// Share the portal lifecycle wake so session close prunes without a timer.
    #[must_use]
    pub(crate) fn with_session_wake(mut self, wake: crate::idle_wake::IdleWake) -> Self {
        self.session_wake = wake;
        self
    }

    async fn parked(&self) {
        if let Some(selection) = self.broker.selection_wake() {
            tokio::select! {
                () = self.wake.wait() => {}
                () = self.session_wake.wait() => {}
                () = selection.wait() => {}
            }
        } else {
            tokio::select! {
                () = self.wake.wait() => {}
                () = self.session_wake.wait() => {}
            }
        }
    }
    fn owner(hdr: &zbus::message::Header<'_>) -> zbus::fdo::Result<OwnedUniqueName> {
        hdr.sender()
            .cloned()
            .map(OwnedUniqueName::from)
            .ok_or_else(|| {
                zbus::fdo::Error::AccessDenied(
                    "clipboard backend requires a portal frontend sender".into(),
                )
            })
    }
    fn requested(
        &self,
        session: &ObjectPath<'_>,
        owner: &OwnedUniqueName,
    ) -> zbus::fdo::Result<()> {
        self.any_provider(
            session,
            owner,
            |provider, session, owner| provider.request_clipboard(session, owner),
            "clipboard must be requested by this remote session before Start",
        )
    }
    fn active(&self, session: &ObjectPath<'_>, owner: &OwnedUniqueName) -> zbus::fdo::Result<()> {
        self.any_provider(
            session,
            owner,
            |provider, session, owner| provider.clipboard_owner(session).as_deref() == Some(owner),
            "clipboard is not active for this owned session",
        )
    }
    /// Succeed when some provider `grants` the clipboard to `owner`'s
    /// `session`; refuse with `refusal` otherwise.
    fn any_provider(
        &self,
        session: &ObjectPath<'_>,
        owner: &OwnedUniqueName,
        grants: fn(&dyn ClipboardSessionProvider, &str, &str) -> bool,
        refusal: &str,
    ) -> zbus::fdo::Result<()> {
        let session = session.to_string();
        self.providers
            .iter()
            .any(|provider| grants(provider.as_ref(), &session, owner.as_str()))
            .then_some(())
            .ok_or_else(|| zbus::fdo::Error::AccessDenied(refusal.into()))
    }
    fn available(&self) -> zbus::fdo::Result<()> {
        self.broker.available().then_some(()).ok_or_else(|| {
            zbus::fdo::Error::Failed(
                "Clipboard is unavailable because this is not a live Tilo compositor session"
                    .into(),
            )
        })
    }

    /// Drop all state owned by RemoteDesktop sessions that no longer exist.
    /// The compositor selection is cleared too, so revocation cannot leave a
    /// previous remote application's bytes visible to a focused local app.
    pub fn prune_revoked(&self) {
        let clear = self.state.lock().ok().is_some_and(|mut state| {
            let live = |session: &str| {
                self.providers
                    .iter()
                    .any(|provider| provider.clipboard_owner(session).is_some())
            };
            state.offers.retain(|session, _| live(session));
            state.pending.retain(|_, write| live(&write.session));
            state.transfers.retain(|transfer| live(&transfer.session));
            if state
                .active_selection
                .as_ref()
                .is_some_and(|session| !live(session))
            {
                state.active_selection = None;
                true
            } else {
                false
            }
        });
        if clear {
            let _ = self.broker.clear();
        }
    }
    fn live_sessions(&self) -> Vec<String> {
        let mut sessions = self
            .providers
            .iter()
            .flat_map(|provider| provider.clipboard_sessions())
            .collect::<Vec<_>>();
        sessions.sort();
        sessions.dedup();
        sessions
    }
    /// Compositor code calls this when it needs a format owned by a remote app;
    /// a signal pump emits the returned events as `SelectionTransfer`.
    pub fn request_transfer(&self, session: &str, mime: &str) -> Option<ClipboardTransfer> {
        self.prune_revoked();
        let mut state = self.state.lock().ok()?;
        if !state
            .offers
            .get(session)?
            .iter()
            .any(|offered| offered == mime)
        {
            return None;
        }
        let serial = state.next_serial.max(1);
        state.next_serial = serial.wrapping_add(1).max(1);
        let transfer = ClipboardTransfer {
            session: session.into(),
            mime: mime.into(),
            serial,
        };
        state.transfers.push(transfer.clone());
        drop(state);
        self.wake.poke();
        Some(transfer)
    }
    pub fn take_transfers(&self) -> Vec<ClipboardTransfer> {
        self.state
            .lock()
            .map(|mut state| std::mem::take(&mut state.transfers))
            .unwrap_or_default()
    }
}
fn valid_mime(mime: &str) -> bool {
    !mime.is_empty()
        && mime.len() <= MAX_MIME_BYTES
        && mime.contains('/')
        && !mime.chars().any(|c| c.is_control() || c.is_whitespace())
}
fn mimes(options: &HashMap<String, OwnedValue>) -> Result<Vec<String>, zbus::fdo::Error> {
    let values = options
        .get("mime_types")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| Vec::<String>::try_from(v).ok())
        .ok_or_else(|| zbus::fdo::Error::InvalidArgs("mime_types must be a string array".into()))?;
    if values.is_empty()
        || values.len() > MAX_MIME_TYPES
        || values.iter().any(|mime| !valid_mime(mime))
    {
        return Err(zbus::fdo::Error::InvalidArgs(
            "invalid or excessive MIME type list".into(),
        ));
    }
    let mut unique = values;
    unique.sort();
    unique.dedup();
    Ok(unique)
}
fn fd_for_bytes(bytes: Vec<u8>) -> Result<OwnedFd, zbus::fdo::Error> {
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(zbus::fdo::Error::Failed(
            "clipboard payload exceeds policy limit".into(),
        ));
    }
    let (reader, mut writer) =
        UnixStream::pair().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    std::thread::spawn(move || {
        let _ = writer.write_all(&bytes);
    });
    Ok(OwnedFd::from(StdOwnedFd::from(reader)))
}
fn bounded_read(mut reader: UnixStream) -> Result<Vec<u8>, zbus::fdo::Error> {
    reader
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut reader)
        .take((MAX_TRANSFER_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    if bytes.len() > MAX_TRANSFER_BYTES {
        return Err(zbus::fdo::Error::Failed(
            "clipboard payload exceeds policy limit".into(),
        ));
    }
    Ok(bytes)
}

#[interface(name = "org.freedesktop.impl.portal.Clipboard")]
impl ClipboardBackend {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }
    fn request_clipboard(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        self.available()?;
        self.requested(&session, &Self::owner(&hdr)?)
    }
    fn set_selection(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<()> {
        let owner = Self::owner(&hdr)?;
        self.available()?;
        self.active(&session, &owner)?;
        let mimes = mimes(&options)?;
        self.broker
            .begin_offer(&mimes)
            .map_err(zbus::fdo::Error::Failed)?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| zbus::fdo::Error::Failed("clipboard state unavailable".into()))?;
            state.offers.insert(session.to_string(), mimes.clone());
            state.active_selection = Some(session.to_string());
        }
        // Pull every offered format through bounded, one-shot transfer FDs.
        // This is asynchronous (the signal pump emits them) and lets a PNG or
        // JPEG become a regular focus-gated Wayland selection without a
        // privileged data-control client.
        for mime in &mimes {
            let _ = self.request_transfer(&session.to_string(), mime);
        }
        Ok(())
    }
    fn selection_write(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        serial: u32,
    ) -> zbus::fdo::Result<OwnedFd> {
        let owner = Self::owner(&hdr)?;
        self.available()?;
        self.active(&session, &owner)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("clipboard state unavailable".into()))?;
        let index = state
            .transfers
            .iter()
            .position(|t| t.session == session.to_string() && t.serial == serial)
            .ok_or_else(|| {
                zbus::fdo::Error::InvalidArgs("unknown clipboard transfer serial".into())
            })?;
        // A serial grants precisely one writer FD. Reusing it could replace a
        // pending transfer after the frontend has already supplied bytes.
        let transfer = state.transfers.remove(index);
        let (reader, writer) =
            UnixStream::pair().map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        state.pending.insert(
            serial,
            PendingWrite {
                session: transfer.session,
                mime: transfer.mime,
                reader,
            },
        );
        Ok(OwnedFd::from(StdOwnedFd::from(writer)))
    }
    async fn selection_write_done(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        serial: u32,
        success: bool,
    ) -> zbus::fdo::Result<()> {
        let owner = Self::owner(&hdr)?;
        self.available()?;
        self.active(&session, &owner)?;
        let pending = self
            .state
            .lock()
            .map_err(|_| zbus::fdo::Error::Failed("clipboard state unavailable".into()))?
            .pending
            .remove(&serial)
            .ok_or_else(|| {
                zbus::fdo::Error::InvalidArgs("unknown clipboard transfer serial".into())
            })?;
        if pending.session != session.to_string() {
            return Err(zbus::fdo::Error::AccessDenied(
                "transfer belongs to another session".into(),
            ));
        }
        if success {
            let bytes = tokio::task::spawn_blocking(move || bounded_read(pending.reader))
                .await
                .map_err(|_| zbus::fdo::Error::Failed("clipboard reader task failed".into()))??;
            self.broker
                .write(&pending.mime, bytes)
                .map_err(zbus::fdo::Error::Failed)?
        }
        Ok(())
    }
    fn selection_read(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        session: ObjectPath<'_>,
        mime: String,
    ) -> zbus::fdo::Result<OwnedFd> {
        let owner = Self::owner(&hdr)?;
        self.available()?;
        self.active(&session, &owner)?;
        if !valid_mime(&mime) {
            return Err(zbus::fdo::Error::InvalidArgs("invalid MIME type".into()));
        }
        fd_for_bytes(self.broker.read(&mime).map_err(zbus::fdo::Error::Failed)?)
    }
    #[zbus(signal)]
    async fn selection_owner_changed(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        session: ObjectPath<'_>,
        options: HashMap<String, OwnedValue>,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn selection_transfer(
        ctxt: &zbus::object_server::SignalEmitter<'_>,
        session: ObjectPath<'_>,
        mime: String,
        serial: u32,
    ) -> zbus::Result<()>;
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestProvider {
        session: String,
        live: Arc<AtomicBool>,
    }

    impl ClipboardSessionProvider for TestProvider {
        fn request_clipboard(&self, _: &str, _: &str) -> bool {
            false
        }
        fn clipboard_owner(&self, session: &str) -> Option<String> {
            (self.live.load(Ordering::Acquire) && session == self.session)
                .then(|| ":1.1".to_owned())
        }
        fn clipboard_sessions(&self) -> Vec<String> {
            self.clipboard_owner(&self.session)
                .map(|_| self.session.clone())
                .into_iter()
                .collect()
        }
    }

    #[test]
    fn clipboard_broker_bounds_mimes_and_payloads() {
        // Phase `mime_policy_rejects_hostile_values`.
        assert!(valid_mime("text/plain"));
        assert!(!valid_mime("text/plain\n"));
        assert!(!valid_mime("not-a-mime"));
        // Phase `broker_enforces_payload_cap`.
        {
            let broker = MemoryClipboardBroker::default();
            assert!(
                broker
                    .write("text/plain", vec![0; MAX_TRANSFER_BYTES + 1])
                    .is_err()
            );
        }
        // Phase `bounded_reader_rejects_an_oversize_stream`.
        {
            let (reader, mut writer) = UnixStream::pair().unwrap();
            let writer_thread = std::thread::spawn(move || {
                writer.write_all(&vec![0; MAX_TRANSFER_BYTES + 1]).unwrap();
            });
            assert!(bounded_read(reader).is_err());
            writer_thread.join().unwrap();
        }
    }
    #[test]
    fn revoking_a_provider_clears_its_selection_and_pending_state() {
        let session = "/org/freedesktop/portal/desktop/session/tilo/test".to_string();
        let live = Arc::new(AtomicBool::new(true));
        let broker = Arc::new(MemoryClipboardBroker::default());
        broker
            .set("image/png", vec![0x89, b'P', b'N', b'G'])
            .unwrap();
        let backend = ClipboardBackend::with_session_providers(
            vec![Arc::new(TestProvider {
                session: session.clone(),
                live: live.clone(),
            })],
            ClipboardBrokerHandle::new(broker.clone()),
        );
        {
            let mut state = backend.state.lock().unwrap();
            state
                .offers
                .insert(session.clone(), vec!["image/png".into()]);
            state.active_selection = Some(session.clone());
            state.transfers.push(ClipboardTransfer {
                session,
                mime: "image/png".into(),
                serial: 1,
            });
        }
        live.store(false, Ordering::Release);
        backend.prune_revoked();
        let state = backend.state.lock().unwrap();
        assert!(state.offers.is_empty());
        assert!(state.transfers.is_empty());
        assert!(state.active_selection.is_none());
        drop(state);
        assert!(broker.read("image/png").is_err());
    }
}
