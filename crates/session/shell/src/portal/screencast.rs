//! `org.freedesktop.impl.portal.ScreenCast` backend.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.ScreenCast.xml`.
//!
//! `xdg-desktop-portal` drives a per-session state machine on behalf of
//! a sandboxed app: `CreateSession` allocates it; `SelectSources`
//! records what to capture and is where the consent prompt fires (a
//! valid standard `restore_data` record pre-approves the session and skips it);
//! `Start` re-checks consent (unless restored), provisions a real
//! `PipeWire` node per selected source, and — when the session asked to
//! persist — returns fresh standard `restore_data` so the next share can skip
//! the prompt.
//!
//! [`ScreenCastStreamProvisioner`](crate::portal::screencast_pipewire::ScreenCastStreamProvisioner) lets tests substitute a
//! [`MockScreenCastProvisioner`](crate::portal::screencast_pipewire::MockScreenCastProvisioner) for the `PipeWire` connection.

#![allow(
    clippy::used_underscore_binding,
    reason = "the D-Bus method signatures below require unused positional parameters (handle, \
              options, ...); zbus's #[interface] macro deserialises them into these named \
              parameters and forwards them by identifier into its generated dispatcher, so the \
              lint fires on macro-generated code a per-function #[allow] cannot reach"
)]

use super::reply::{Ended, OrFail, Reply, Results, answer};
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use zbus::interface;
#[cfg(target_os = "linux")]
use zbus::object_server::SignalEmitter;
#[cfg(target_os = "linux")]
use zbus::zvariant::ObjectPath;
use zbus::zvariant::{OwnedValue, Value};

#[cfg(target_os = "linux")]
use crate::shell::state::{Capability, GRANT_ONCE};

use super::PortalCaller;
#[cfg(test)]
use super::PromptOutcome;
#[cfg(target_os = "linux")]
use super::auth::authenticate_call;
#[cfg(target_os = "linux")]
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
use super::restore::{RestoreRecord, RestoreStore, restore_data, restore_key};
use super::screencast_pipewire::{
    PipewireStreamInfo, PipewireStreamRequest, ScreenCastStreamProvisionerHandle,
};
#[cfg(target_os = "linux")]
use super::{CapabilityKind, PortalContext, flow};

#[cfg(target_os = "linux")]
const MAX_LIVE_SESSIONS: usize = 64;

/// Source-type bitmask (`ScreenCast.AvailableSourceTypes`).
pub mod source_type {
    pub const MONITOR: u32 = 1;
    pub const WINDOW: u32 = 2;
    pub const VIRTUAL: u32 = 4;
    /// Everything the compositor can capture.
    pub const ALL: u32 = MONITOR | WINDOW | VIRTUAL;
}

/// Cursor-embedding bitmask (`ScreenCast.AvailableCursorModes`).
pub mod cursor_mode {
    pub const HIDDEN: u32 = 1;
    pub const EMBEDDED: u32 = 2;
    pub const METADATA: u32 = 4;
    /// Every mode the compositor supports.
    pub const ALL: u32 = HIDDEN | EMBEDDED | METADATA;
}

/// The interface version we advertise. v4 defines `persist_mode` +
/// implementation-owned `restore_data`, which we implement.
pub const SCREENCAST_VERSION: u32 = 4;

/// Per-session negotiated selection, keyed by the frontend's `session_handle` object path.
#[derive(Clone, Debug, Default)]
struct ScreenCastSession {
    owner: String,
    app_id: String,
    caller: Option<PortalCaller>,
    /// Bitmask of requested source types (`SelectSources` `types`).
    source_types: u32,
    /// Requested cursor mode.
    cursor_mode: u32,
    /// Whether the app allowed selecting multiple sources.
    multiple: bool,
    /// Requested persistence: 0 none, 1 while-running, 2 until-revoked.
    persist_mode: u32,
    /// True once valid standard `restore_data` pre-approved this session, so
    /// `Start` skips the consent prompt (persisted-share fast path).
    restored: bool,
    /// Consent is requested exactly once, at Start (unless restored).
    consented: bool,
    /// Serializes concurrent Start calls across the async prompt.
    starting: bool,
    /// PipeWire nodes owned by this session, released on lifecycle close.
    streams: Vec<u32>,
}

/// What the opaque payload inside a standard `restore_data` tuple maps back
/// to. Issued at `Start` when the app asked to persist; consumed at a later
/// `SelectSources`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct RestoreData {
    app_id: String,
    source_types: u32,
    cursor_mode: u32,
    multiple: bool,
    persist_mode: u32,
    issued_unix_seconds: u64,
}

impl RestoreRecord for RestoreData {
    fn app_id(&self) -> &str {
        &self.app_id
    }
    fn persist_mode(&self) -> u32 {
        self.persist_mode
    }
    fn issued_unix_seconds(&self) -> u64 {
        self.issued_unix_seconds
    }
    fn grant_in_range(&self) -> bool {
        self.source_types & !source_type::ALL == 0 && self.cursor_mode & !cursor_mode::ALL == 0
    }
}

#[cfg(target_os = "linux")]
pub struct ScreenCastBackend {
    ctx: PortalContext,
    lifecycle: PortalLifecycle,
    /// Live sessions keyed by `session_handle` path.
    sessions: Arc<Mutex<HashMap<String, ScreenCastSession>>>,
    /// Issued opaque restore payloads → the selection they restore. Mode 1 is session
    /// memory; mode 2 is an app-bound private, atomically-written local file.
    restore: Arc<Mutex<RestoreStore<RestoreData>>>,
}

/// Lifecycle hook retained by `serve`: closing a frontend Session object
/// promptly drops its registry entry and stops every producer node.
#[derive(Clone)]
pub struct ScreenCastSessionHandle {
    sessions: Arc<Mutex<HashMap<String, ScreenCastSession>>>,
    provisioner: ScreenCastStreamProvisionerHandle,
}

impl ScreenCastSessionHandle {
    pub fn close(&self, session_handle: &str) -> bool {
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(session_handle));
        let Some(session) = session else {
            return false;
        };
        for node in session.streams {
            self.provisioner.release(node);
        }
        true
    }

    pub fn clear(&self) {
        let nodes = self
            .sessions
            .lock()
            .map(|mut sessions| {
                sessions
                    .drain()
                    .flat_map(|(_, session)| session.streams)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for node in nodes {
            self.provisioner.release(node);
        }
    }
}

#[cfg(target_os = "linux")]
impl ScreenCastBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }

    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self {
            ctx,
            lifecycle,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            restore: Arc::new(Mutex::new(RestoreStore::load("screencast-restore.json"))),
        }
    }

    pub fn session_handle(&self) -> ScreenCastSessionHandle {
        ScreenCastSessionHandle {
            sessions: self.sessions.clone(),
            provisioner: self.ctx.screencast_provisioner.clone(),
        }
    }

    fn reset_starting(&self, key: &str) {
        if let Ok(mut sessions) = self.sessions.lock()
            && let Some(session) = sessions.get_mut(key)
        {
            session.starting = false;
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
#[cfg(target_os = "linux")]
impl ScreenCastBackend {
    /// Source types the compositor can capture.
    #[zbus(property)]
    #[allow(
        clippy::unused_self,
        reason = "zbus's #[interface] macro dispatches D-Bus property getters as instance methods, even though this constant value doesn't depend on self"
    )]
    fn available_source_types(&self) -> u32 {
        self.ctx
            .screencast_provisioner
            .is_available()
            .then(|| self.ctx.screencast_provisioner.available_source_types())
            .unwrap_or(0)
    }

    /// Cursor modes the compositor supports.
    #[zbus(property)]
    #[allow(
        clippy::unused_self,
        reason = "zbus's #[interface] macro dispatches D-Bus property getters as instance methods, even though this constant value doesn't depend on self"
    )]
    fn available_cursor_modes(&self) -> u32 {
        self.ctx
            .screencast_provisioner
            .is_available()
            .then(|| self.ctx.screencast_provisioner.available_cursor_modes())
            .unwrap_or(0)
    }

    /// Interface version. Named lowercase to match the spec's `version` property (zbus would otherwise `PascalCase` it to `Version`).
    #[zbus(property, name = "version")]
    #[allow(
        clippy::unused_self,
        reason = "zbus's #[interface] macro dispatches D-Bus property getters as instance methods, even though this constant value doesn't depend on self"
    )]
    fn version(&self) -> u32 {
        SCREENCAST_VERSION
    }

    /// `CreateSession` — register the session so `SelectSources`/`Start` can
    /// carry the negotiated selection. Consent-neutral: response=0, empty results.
    async fn create_session(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            tracing::debug!("ScreenCast.CreateSession: app={app_id}");
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, None)
                .await
                .or_fail()?;
            let key = session_handle.to_string();
            {
                let mut sessions = self.sessions.lock().or_fail()?;
                if sessions.contains_key(&key) || sessions.len() >= MAX_LIVE_SESSIONS {
                    return Err(Ended::Failed);
                }
                sessions.insert(
                    key.clone(),
                    ScreenCastSession {
                        owner: auth.owner.to_string(),
                        app_id: app_id.clone(),
                        caller: Some(auth.caller.clone()),
                        ..Default::default()
                    },
                );
            }
            let cleanup = self.session_handle();
            let cleanup_path = key.clone();
            let entry = LifecycleEntry {
                owner: auth.owner.to_string(),
                app_id,
                resource: LifecycleResource::ScreenCast,
            };
            let registered = self
                .lifecycle
                .register_session(connection, &key, entry, move |_| {
                    cleanup.close(&cleanup_path);
                })
                .await;
            if !matches!(registered, Ok(true)) {
                let _ = self.session_handle().close(&key);
                return Err(Ended::Failed);
            }
            Ok(Results::new())
        })
        .await
    }

    /// `SelectSources` — record what the app wants to capture and surface
    /// the consent prompt. A valid `restore_token` pre-approves the session
    /// (no prompt) — how a persisted share re-attaches without re-asking.
    async fn select_sources(
        &self,
        #[zbus(signal_emitter)] _ctx: SignalEmitter<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            tracing::debug!("ScreenCast.SelectSources: app={app_id}");
            let key = session_handle.to_string();
            let auth = authenticate_call(&self.ctx, &hdr, &app_id, None)
                .await
                .or_fail()?;
            let existing = self
                .sessions
                .lock()
                .ok()
                .and_then(|sessions| sessions.get(&key).cloned())
                .or_fail()?;
            if existing.owner != auth.owner.as_str()
                || existing.app_id != app_id
                || existing.caller.as_ref() != Some(&auth.caller)
            {
                return Err(Ended::Failed);
            }

            // Parse the negotiated options, clamped to what we support.
            let producer_available = self.ctx.screencast_provisioner.is_available();
            let supported_types = producer_available
                .then(|| self.ctx.screencast_provisioner.available_source_types())
                .unwrap_or(0);
            let supported_cursors = producer_available
                .then(|| self.ctx.screencast_provisioner.available_cursor_modes())
                .unwrap_or(0);
            let requested_types =
                crate::portal::option::<u32>(&options, "types").unwrap_or_else(|| {
                    // Absent options mean "choose a supported default", never
                    // silently substitute a monitor for an explicitly unsupported
                    // window/virtual request.
                    first_supported_bit(supported_types).unwrap_or(source_type::MONITOR)
                });
            let mut types = requested_types & supported_types;
            if types == 0 {
                return Err(Ended::Failed);
            }
            let requested_cursors = crate::portal::option::<u32>(&options, "cursor_mode")
                .unwrap_or_else(|| {
                    first_supported_bit(supported_cursors).unwrap_or(cursor_mode::HIDDEN)
                });
            let mut curs = requested_cursors & supported_cursors;
            if curs == 0 {
                return Err(Ended::Failed);
            }
            let mut multiple = crate::portal::option::<bool>(&options, "multiple").unwrap_or(false);
            let mut persist_mode =
                crate::portal::option::<u32>(&options, "persist_mode").unwrap_or(0);

            // `restore_data` is a standard `(suv)` tuple. Its payload is opaque
            // to xdg-desktop-portal; Tilo uses a random private-store key so an
            // application cannot forge a durable grant by manufacturing fields.
            // A matching record pre-approves the session and
            // restores its previous selection (source types, cursor mode, multiple, persist mode).
            let restored = if let Some(token) = restore_key(&options) {
                let stored = self
                    .restore
                    .lock()
                    .ok()
                    .and_then(|r| r.get(&token))
                    .filter(|d| d.app_id == app_id);
                if let Some(d) = stored {
                    if d.source_types & !supported_types != 0
                        || d.cursor_mode & !supported_cursors != 0
                    {
                        return Err(Ended::Failed);
                    }
                    types = d.source_types;
                    curs = d.cursor_mode;
                    multiple = d.multiple;
                    persist_mode = d.persist_mode;
                    true
                } else {
                    false
                }
            } else {
                false
            };

            let mut sessions = self.sessions.lock().or_fail()?;
            let entry = sessions.get_mut(&key).or_fail()?;
            if entry.consented || entry.starting || !entry.streams.is_empty() {
                return Err(Ended::Failed);
            }
            entry.source_types = types;
            entry.cursor_mode = curs;
            entry.multiple = multiple;
            entry.persist_mode = persist_mode.min(2);
            entry.restored = restored;
            drop(sessions);
            // Source selection is consent-neutral. Start owns the sole prompt so
            // users never see two consecutive dialogs for one screen share.
            Ok(Results::new())
        })
        .await
    }

    /// `Start` — provision a real `PipeWire` node per selected source and
    /// return the streams. Issues a fresh `restore_token` when the session asked to persist.
    async fn start(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> Reply {
        answer(async move {
            tracing::debug!("ScreenCast.Start: app={app_id}");
            let key = session_handle.to_string();

            let auth = authenticate_call(&self.ctx, &hdr, &app_id, Some(&parent_window))
                .await
                .or_fail()?;

            // `Request.Close` must be able to cancel a consent sheet while this
            // method is awaiting the shell-owned prompt resolver.
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancellation = cancelled.clone();
            let request_entry = LifecycleEntry {
                owner: auth.owner.to_string(),
                app_id: auth.caller.app_id.clone(),
                resource: LifecycleResource::Prompt,
            };
            let request_path = handle.to_string();
            let cancel = move |_: &LifecycleEntry| cancellation.store(true, Ordering::Release);
            self.lifecycle
                .with_request(connection, &request_path, request_entry, cancel, async {
                    // SelectSources is mandatory. Never synthesize a session from fields
                    // supplied by an otherwise authenticated Start call.
                    let Some(session) =
                        self.sessions.lock().ok().and_then(|s| s.get(&key).cloned())
                    else {
                        return Err(Ended::Failed);
                    };
                    if session.owner != auth.owner.as_str()
                        || session.app_id != app_id
                        || session.caller.as_ref() != Some(&auth.caller)
                        || session.source_types == 0
                        || session.starting
                        || !session.streams.is_empty()
                    {
                        return Err(Ended::Failed);
                    }
                    let marked_starting = self.sessions.lock().ok().is_some_and(|mut sessions| {
                        sessions.get_mut(&key).is_some_and(|entry| {
                            if entry.starting || !entry.streams.is_empty() {
                                false
                            } else {
                                entry.starting = true;
                                true
                            }
                        })
                    });
                    if !marked_starting {
                        return Err(Ended::Failed);
                    }

                    // The sole consent prompt for this handshake lives here. A valid
                    // restore token is the only fast path.
                    if !session.restored && !session.consented {
                        let cap_template = Capability::Screencast { scope: GRANT_ONCE };
                        let outcome = flow::resolve(
                            &self.ctx,
                            auth.caller.clone(),
                            CapabilityKind::Screencast,
                            cap_template,
                        )
                        .await;
                        if cancelled.load(Ordering::Acquire) || !outcome.is_allowed() {
                            self.reset_starting(&key);
                            return Err(Ended::Cancelled);
                        }
                    }

                    // One stream per selected source type (bounded by `multiple`).
                    let (width, height) = size_from_options(&options);
                    let types = selected_source_types(session.source_types, session.multiple);
                    let mut infos: Vec<PipewireStreamInfo> = Vec::with_capacity(types.len());
                    for st in types {
                        let req = PipewireStreamRequest {
                            caller: auth.caller.clone(),
                            width,
                            height,
                            framerate: 30,
                            source_type: st,
                            cursor_mode: session.cursor_mode,
                        };
                        match self
                            .ctx
                            .screencast_provisioner
                            .provision_blocking(req)
                            .await
                        {
                            Ok(info) => infos.push(info),
                            Err(e) => {
                                for info in &infos {
                                    self.ctx.screencast_provisioner.release(info.node_id);
                                }
                                self.reset_starting(&key);
                                tracing::warn!("screencast: pipewire provision failed: {e}");
                                return Err(Ended::Failed);
                            }
                        }
                    }

                    if cancelled.load(Ordering::Acquire) {
                        for info in &infos {
                            self.ctx.screencast_provisioner.release(info.node_id);
                        }
                        self.reset_starting(&key);
                        return Err(Ended::Cancelled);
                    }

                    // Persist: issue an opaque key carried in standard `restore_data` if
                    // the session asked for it.
                    let restore_key =
                        (session.persist_mode > 0).then(|| uuid::Uuid::new_v4().to_string());

                    let updated = self.sessions.lock().ok().is_some_and(|mut sessions| {
                        let Some(entry) = sessions.get_mut(&key) else {
                            return false;
                        };
                        entry.consented = true;
                        entry.starting = false;
                        entry.streams = infos.iter().map(|info| info.node_id).collect();
                        true
                    });
                    if !updated {
                        for info in &infos {
                            self.ctx.screencast_provisioner.release(info.node_id);
                        }
                        return Err(Ended::Failed);
                    }

                    if let Some(token) = &restore_key {
                        let data = RestoreData {
                            app_id: session.app_id.clone(),
                            source_types: session.source_types,
                            cursor_mode: session.cursor_mode,
                            multiple: session.multiple,
                            persist_mode: session.persist_mode,
                            issued_unix_seconds: tilo_clock::unix_now(),
                        };
                        let stored = self.restore.lock().map_err(|_| ()).and_then(|mut restore| {
                            restore.insert(token.clone(), data).map_err(|_| ())
                        });
                        if stored.is_err() {
                            for info in &infos {
                                self.ctx.screencast_provisioner.release(info.node_id);
                            }
                            self.reset_starting(&key);
                            return Err(Ended::Failed);
                        }
                    }

                    Ok(start_response(infos, restore_key, session.persist_mode))
                })
                .await
        })
        .await
    }
}

/// Read a `(u32, u32)` `size` option, defaulting to 1920x1080.
fn size_from_options(options: &HashMap<String, OwnedValue>) -> (u32, u32) {
    crate::portal::option::<(u32, u32)>(options, "size").unwrap_or((1920, 1080))
}

/// Expand a source-type bitmask into the concrete streams to provision.
/// Always yields at least one (monitor); when `multiple` is false, only the first (monitor > window > virtual preference) is kept.
fn selected_source_types(mask: u32, multiple: bool) -> Vec<u32> {
    let mut out = Vec::new();
    for bit in [
        source_type::MONITOR,
        source_type::WINDOW,
        source_type::VIRTUAL,
    ] {
        if mask & bit != 0 {
            out.push(bit);
        }
    }
    if out.is_empty() {
        out.push(source_type::MONITOR);
    }
    if !multiple {
        out.truncate(1);
    }
    out
}

/// Return the least-significant supported portal bit. Portal source and cursor
/// bitmasks deliberately use independent single-bit flags, so this chooses a
/// deterministic default without inventing an unsupported capability.
#[cfg(target_os = "linux")]
fn first_supported_bit(mask: u32) -> Option<u32> {
    (mask != 0).then_some(mask & mask.wrapping_neg())
}

/// Build `Start`'s results from the provisioned streams and optional restore data. The `streams` field is `a(ua{sv})`.
fn start_response(
    infos: Vec<PipewireStreamInfo>,
    restore_key: Option<String>,
    persist_mode: u32,
) -> Results {
    let mut results = Results::new();
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = infos
        .into_iter()
        .map(super::screencast_pipewire::PipewireStreamInfo::into_stream_tuple)
        .collect();
    if let Ok(o) = OwnedValue::try_from(Value::new(streams)) {
        results.insert("streams".to_string(), o);
    }
    if let Some(key) = restore_key {
        if let Some(data) = restore_data(key) {
            results.insert("restore_data".to_string(), data);
        }
        if let Ok(o) = OwnedValue::try_from(Value::U32(persist_mode)) {
            results.insert("persist_mode".to_string(), o);
        }
    }
    results
}

#[cfg(test)]
fn start_response_with_stream(info: PipewireStreamInfo) -> Results {
    start_response(vec![info], None, 0)
}

/// Test-only: the outcome→response helper, kept so the "deny path returns response=1" assertion can be exercised without a provisioner.
#[cfg(test)]
fn outcome_to_start_response_for_tests(outcome: PromptOutcome) -> Reply {
    if !outcome.is_allowed() {
        return crate::portal::reply::reply(Err(Ended::Cancelled));
    }
    crate::portal::reply::reply(Ok(start_response_with_stream(PipewireStreamInfo {
        node_id: 1,
        source_type: 1,
        id: 0,
        size: (1920, 1080),
        position: (0, 0),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::screencast_pipewire::{
        MockScreenCastProvisioner, ScreenCastStreamProvisionerHandle,
    };
    use crate::shell::state::WorkspaceId;
    use std::sync::Arc;

    #[test]
    fn screencast_start_responses_carry_streams_and_restore_data() {
        // Phase `start_allow_returns_response_zero_with_streams_key`.
        {
            let (code, results) = outcome_to_start_response_for_tests(PromptOutcome::AllowOnce);
            assert_eq!(code, 0);
            assert!(results.contains_key("streams"));
        }
        // Phase `start_deny_returns_response_one_no_results`.
        {
            let (code, results) = outcome_to_start_response_for_tests(PromptOutcome::Deny);
            assert_eq!(code, 1);
            assert!(results.is_empty());
        }
        // Phase `start_returns_non_empty_streams_on_allow`.
        {
            let info = PipewireStreamInfo {
                node_id: 99,
                source_type: 1,
                id: 0,
                size: (1280, 720),
                position: (0, 0),
            };
            let results = start_response_with_stream(info);
            let streams_v = results.get("streams").expect("streams");
            let cloned: OwnedValue = streams_v.try_clone().expect("clone");
            let streams: Vec<(u32, HashMap<String, OwnedValue>)> =
                Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(cloned)
                    .expect("Vec<(u32, dict)>");
            assert_eq!(streams.len(), 1, "must emit exactly one stream");
            assert_eq!(streams[0].0, 99, "node id must round-trip");
            assert!(streams[0].1.contains_key("size"));
            assert!(streams[0].1.contains_key("source_type"));
            assert!(streams[0].1.contains_key("id"));
        }
        // Phase `start_response_includes_standard_restore_data_when_persisted`.
        {
            let info = PipewireStreamInfo {
                node_id: 3,
                source_type: 1,
                id: 0,
                size: (640, 480),
                position: (0, 0),
            };
            let results = start_response(vec![info], Some("tok-123".into()), 2);
            assert!(results.contains_key("restore_data"));
            assert!(results.contains_key("persist_mode"));
            // No restore data => no restore_data/persist_mode keys.
            let info2 = PipewireStreamInfo {
                node_id: 4,
                source_type: 1,
                id: 0,
                size: (640, 480),
                position: (0, 0),
            };
            let results2 = start_response(vec![info2], None, 0);
            assert!(!results2.contains_key("restore_data"));
        }
    }

    #[test]
    fn screencast_restore_authority_is_app_bound_durable_and_revocable() {
        // Phase `standard_restore_data_round_trips_only_tilo_uuid_payloads`.
        {
            let key = uuid::Uuid::new_v4().to_string();
            let mut options = HashMap::new();
            options.insert("restore_data".into(), restore_data(key.clone()).unwrap());
            let raw = options["restore_data"].try_clone().unwrap();
            let parsed: (String, u32, OwnedValue) = raw.try_into().unwrap();
            assert_eq!(parsed.0, "tilo");
            assert_eq!(parsed.1, 1);
            assert_eq!(Value::from(parsed.2).downcast::<String>().unwrap(), key);
            let key = uuid::Uuid::new_v4().to_string();
            options.insert("restore_data".into(), restore_data(key.clone()).unwrap());
            assert_eq!(restore_key(&options).as_deref(), Some(key.as_str()));
            options.insert(
                "restore_data".into(),
                OwnedValue::from(zbus::zvariant::Str::from("not-a-tuple")),
            );
            assert_eq!(restore_key(&options), None);
        }
        // Phase `durable_restore_survives_a_store_restart_and_stays_app_bound`.
        {
            let root = std::env::temp_dir().join(format!("tilo-sc-{}", uuid::Uuid::new_v4()));
            let path = root.join("restore.json");
            let token = uuid::Uuid::new_v4().to_string();
            let mut store = RestoreStore::load_at(Some(path.clone()));
            store
                .insert(token.clone(), durable_record("org.example.share"))
                .expect("durable write");
            drop(store);
            let after_restart = RestoreStore::<RestoreData>::load_at(Some(path));
            assert_eq!(
                after_restart.get(&token).map(|record| record.app_id),
                Some("org.example.share".into())
            );
            let _ = std::fs::remove_dir_all(root);
        }
        // Phase `corrupt_durable_restore_is_quarantined_and_never_restored`.
        {
            let root = std::env::temp_dir().join(format!("tilo-sc-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).expect("root");
            let path = root.join("restore.json");
            std::fs::write(&path, b"not json").expect("corrupt fixture");
            let store = RestoreStore::<RestoreData>::load_at(Some(path.clone()));
            assert!(store.durable_is_empty());
            assert!(!path.exists(), "corrupt data must not remain authoritative");
            assert!(
                std::fs::read_dir(&root)
                    .expect("root entries")
                    .any(|entry| entry
                        .expect("entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with("restore.corrupt-")),
                "corrupt artifact is retained for diagnosis rather than overwritten"
            );
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn screencast_options_select_sources_sizes_and_provisioning() {
        // Phase `selected_source_types_respects_multiple_and_mask`.
        // Single-source: only the first (monitor preferred).
        assert_eq!(
            selected_source_types(source_type::MONITOR | source_type::WINDOW, false),
            vec![source_type::MONITOR]
        );
        // Multiple: every requested type.
        assert_eq!(
            selected_source_types(source_type::WINDOW | source_type::VIRTUAL, true),
            vec![source_type::WINDOW, source_type::VIRTUAL]
        );
        // Empty mask defaults to monitor.
        assert_eq!(selected_source_types(0, true), vec![source_type::MONITOR]);
        // Phase `size_from_options_reads_and_defaults`.
        {
            let mut opts: HashMap<String, OwnedValue> = HashMap::new();
            let size: (u32, u32) = (640, 480);
            opts.insert(
                "size".to_string(),
                OwnedValue::try_from(Value::new(size)).unwrap(),
            );
            assert_eq!(size_from_options(&opts), (640, 480));
            assert_eq!(size_from_options(&HashMap::new()), (1920, 1080));
        }
        // Phase `available_types_and_modes_cover_the_spec_bits`.
        assert_eq!(source_type::ALL, 1 | 2 | 4);
        assert_eq!(cursor_mode::ALL, 1 | 2 | 4);
        // Phase `screencast_provisioner_records_source_type_and_size`.
        // The provisioner records the request the Start path forwards, including the selected source type + size.
        {
            let ws_id = WorkspaceId(40);
            let provisioner = Arc::new(MockScreenCastProvisioner::new_with_node(13, "sc"));
            let sc_handle = ScreenCastStreamProvisionerHandle::new(provisioner.clone());
            let req = PipewireStreamRequest {
                caller: PortalCaller::new(ws_id, "org.example"),
                width: 1024,
                height: 768,
                framerate: 30,
                source_type: source_type::WINDOW,
                cursor_mode: cursor_mode::HIDDEN,
            };
            let info = sc_handle.provision(&req).expect("provision");
            assert_eq!(info.node_id, 13);
            assert_eq!(info.size, (1024, 768));
            assert_eq!(info.source_type, source_type::WINDOW);
            let seen = provisioner.requests();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].caller.app_id, "org.example");
            assert_eq!(seen[0].source_type, source_type::WINDOW);
        }
    }

    fn durable_record(app_id: &str) -> RestoreData {
        RestoreData {
            app_id: app_id.into(),
            source_types: source_type::MONITOR,
            cursor_mode: cursor_mode::EMBEDDED,
            multiple: false,
            persist_mode: 2,
            issued_unix_seconds: 42,
        }
    }
}
