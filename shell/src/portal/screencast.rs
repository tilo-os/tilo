//! `org.freedesktop.impl.portal.ScreenCast` backend.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.ScreenCast.xml`.
//!
//! `xdg-desktop-portal` drives a per-session state machine on behalf of
//! a sandboxed app: `CreateSession` allocates it; `SelectSources`
//! records what to capture and is where the consent prompt fires (a
//! valid `restore_token` pre-approves the session and skips it);
//! `Start` re-checks consent (unless restored), provisions a real
//! `PipeWire` node per selected source, and — when the session asked to
//! persist — returns a fresh `restore_token` so the next share can skip the prompt.
//!
//! `OpenPipeWireRemote` is a frontend-interface method in the reference
//! architecture; exposed here too so a qOS-native frontend (and the test
//! harness) can get a real, scoped remote fd directly from the compositor's provisioner.
//!
//! [`ScreenCastStreamProvisioner`] lets tests substitute a
//! [`MockScreenCastProvisioner`] for the real `PipeWire` connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalContext;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::shell::state::{Capability, GRANT_ONCE};

#[cfg(test)]
use super::PortalCaller;
#[cfg(test)]
use super::PromptOutcome;
use super::screencast_pipewire::{PipewireStreamInfo, PipewireStreamRequest};
use super::{CapabilityKind, PortalContext, file_chooser::FileChooserBackend, flow};

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

/// The interface version we advertise. v4 is the first that defines `persist_mode` + `restore_token`, which we implement.
pub const SCREENCAST_VERSION: u32 = 4;

/// Per-session negotiated selection, keyed by the frontend's `session_handle` object path.
#[derive(Clone, Debug, Default)]
struct ScreenCastSession {
    app_id: String,
    /// Bitmask of requested source types (`SelectSources` `types`).
    source_types: u32,
    /// Requested cursor mode.
    cursor_mode: u32,
    /// Whether the app allowed selecting multiple sources.
    multiple: bool,
    /// Requested persistence: 0 none, 1 while-running, 2 until-revoked.
    persist_mode: u32,
    /// True once a valid `restore_token` pre-approved this session, so `Start` skips the consent prompt (persisted-share fast path).
    restored: bool,
}

/// What a `restore_token` maps back to. Issued at `Start` when the app asked to persist; consumed at a later `SelectSources`.
#[derive(Clone, Debug)]
struct RestoreData {
    app_id: String,
    source_types: u32,
    cursor_mode: u32,
    multiple: bool,
    persist_mode: u32,
}

pub struct ScreenCastBackend {
    ctx: PortalContext,
    /// Live sessions keyed by `session_handle` path.
    sessions: Arc<Mutex<HashMap<String, ScreenCastSession>>>,
    /// Issued restore tokens → the selection they restore. In-memory for
    /// the compositor session's lifetime, satisfying `persist_mode` 1
    /// (while-running); durable-until-revoked (mode 2) is a documented follow-up.
    restore: Arc<Mutex<HashMap<String, RestoreData>>>,
}

impl ScreenCastBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self {
            ctx,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            restore: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[interface(name = "org.freedesktop.impl.portal.ScreenCast")]
impl ScreenCastBackend {
    /// Source types the compositor can capture.
    #[zbus(property)]
    async fn available_source_types(&self) -> u32 {
        source_type::ALL
    }

    /// Cursor modes the compositor supports.
    #[zbus(property)]
    async fn available_cursor_modes(&self) -> u32 {
        cursor_mode::ALL
    }

    /// Interface version. Named lowercase to match the spec's `version` property (zbus would otherwise `PascalCase` it to `Version`).
    #[zbus(property, name = "version")]
    async fn version(&self) -> u32 {
        SCREENCAST_VERSION
    }

    /// `CreateSession` — register the session so `SelectSources`/`Start` can
    /// carry the negotiated selection. Consent-neutral: response=0, empty results.
    async fn create_session(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("ScreenCast.CreateSession: app={app_id}");
        if let Ok(mut s) = self.sessions.lock() {
            s.insert(
                session_handle.to_string(),
                ScreenCastSession {
                    app_id,
                    ..Default::default()
                },
            );
        }
        (0, HashMap::new())
    }

    /// `SelectSources` — record what the app wants to capture and surface
    /// the consent prompt. A valid `restore_token` pre-approves the session
    /// (no prompt) — how a persisted share re-attaches without re-asking.
    async fn select_sources(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("ScreenCast.SelectSources: app={app_id}");
        let key = session_handle.to_string();

        // Parse the negotiated options, clamped to what we support.
        let mut types =
            opt_u32(&options, "types").unwrap_or(source_type::MONITOR) & source_type::ALL;
        if types == 0 {
            types = source_type::MONITOR;
        }
        let mut curs =
            opt_u32(&options, "cursor_mode").unwrap_or(cursor_mode::EMBEDDED) & cursor_mode::ALL;
        if curs == 0 {
            curs = cursor_mode::EMBEDDED;
        }
        let mut multiple = opt_bool(&options, "multiple").unwrap_or(false);
        let mut persist_mode = opt_u32(&options, "persist_mode").unwrap_or(0);

        // A restore token that matches this app pre-approves the session and
        // restores its previous selection (source types, cursor mode, multiple, persist mode).
        let restored = if let Some(token) = opt_string(&options, "restore_token") {
            let stored = self
                .restore
                .lock()
                .ok()
                .and_then(|r| r.get(&token).cloned())
                .filter(|d| d.app_id == app_id);
            if let Some(d) = stored {
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

        // Consent: an active `Always` grant, or a valid restore token, skips the prompt. Otherwise ask.
        let allowed = if restored {
            true
        } else {
            let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
            let caller = FileChooserBackend::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
            let cap_template = Capability::Screencast { scope: GRANT_ONCE };
            flow::resolve(&self.ctx, caller, CapabilityKind::Screencast, cap_template)
                .await
                .is_allowed()
        };

        if let Ok(mut s) = self.sessions.lock() {
            let entry = s.entry(key).or_insert_with(|| ScreenCastSession {
                app_id: app_id.clone(),
                ..Default::default()
            });
            entry.app_id = app_id;
            entry.source_types = types;
            entry.cursor_mode = curs;
            entry.multiple = multiple;
            entry.persist_mode = persist_mode;
            entry.restored = restored;
        }

        let code = u32::from(!allowed);
        (code, HashMap::new())
    }

    /// `Start` — provision a real `PipeWire` node per selected source and
    /// return the streams. Issues a fresh `restore_token` when the session asked to persist.
    async fn start(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        session_handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!("ScreenCast.Start: app={app_id}");
        let key = session_handle.to_string();

        // Pull the selection recorded at SelectSources (defaults if the frontend somehow skipped it).
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|s| s.get(&key).cloned())
            .unwrap_or_else(|| ScreenCastSession {
                app_id: app_id.clone(),
                source_types: source_type::MONITOR,
                cursor_mode: cursor_mode::EMBEDDED,
                ..Default::default()
            });

        // Re-check consent unless the session was restored from a token.
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = FileChooserBackend::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        if !session.restored {
            let cap_template = Capability::Screencast { scope: GRANT_ONCE };
            let outcome = flow::resolve(
                &self.ctx,
                caller.clone(),
                CapabilityKind::Screencast,
                cap_template,
            )
            .await;
            if !outcome.is_allowed() {
                return (1, HashMap::new());
            }
        }

        // One stream per selected source type (bounded by `multiple`).
        let (width, height) = size_from_options(&options);
        let types = selected_source_types(session.source_types, session.multiple);
        let mut infos: Vec<PipewireStreamInfo> = Vec::with_capacity(types.len());
        for st in types {
            let req = PipewireStreamRequest {
                caller: caller.clone(),
                width,
                height,
                framerate: 30,
                source_type: st,
                cursor_mode: session.cursor_mode,
            };
            match self.ctx.screencast_provisioner.provision(&req) {
                Ok(info) => infos.push(info),
                Err(e) => {
                    tracing::warn!("screencast: pipewire provision failed: {e}");
                    return (2, HashMap::new());
                }
            }
        }

        // Persist: issue a restore token if the session asked for it.
        let restore_token = if session.persist_mode > 0 {
            let token = uuid::Uuid::new_v4().to_string();
            if let Ok(mut r) = self.restore.lock() {
                r.insert(
                    token.clone(),
                    RestoreData {
                        app_id: session.app_id.clone(),
                        source_types: session.source_types,
                        cursor_mode: session.cursor_mode,
                        multiple: session.multiple,
                        persist_mode: session.persist_mode,
                    },
                );
            }
            Some(token)
        } else {
            None
        };

        start_response(infos, restore_token, session.persist_mode)
    }

    /// `OpenPipeWireRemote` — hand back the fd of a `PipeWire` remote the app
    /// attaches the provisioned nodes to. See the module docs for where this sits relative to the reference frontend.
    async fn open_pipe_wire_remote(
        &self,
        session_handle: ObjectPath<'_>,
        _app_id: String,
        _options: HashMap<String, OwnedValue>,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedFd> {
        let key = session_handle.to_string();
        let known = self
            .sessions
            .lock()
            .ok()
            .is_some_and(|s| s.contains_key(&key));
        if !known {
            return Err(zbus::fdo::Error::AccessDenied(
                "OpenPipeWireRemote for an unknown ScreenCast session".into(),
            ));
        }
        match self.ctx.screencast_provisioner.open_remote_fd() {
            Ok(fd) => Ok(zbus::zvariant::OwnedFd::from(fd)),
            Err(e) => Err(zbus::fdo::Error::Failed(format!(
                "could not open PipeWire remote: {e}"
            ))),
        }
    }
}

/// Read a `(u32, u32)` `size` option, defaulting to 1920x1080.
fn size_from_options(options: &HashMap<String, OwnedValue>) -> (u32, u32) {
    options
        .get("size")
        .and_then(|v| v.try_clone().ok())
        .and_then(|owned| <(u32, u32)>::try_from(owned).ok())
        .unwrap_or((1920, 1080))
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

fn opt_u32(options: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    let v = options.get(key)?.try_clone().ok()?;
    u32::try_from(v).ok()
}

fn opt_bool(options: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    let v = options.get(key)?.try_clone().ok()?;
    bool::try_from(v).ok()
}

fn opt_string(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let v = options.get(key)?.try_clone().ok()?;
    String::try_from(v).ok()
}

/// Build `Start`'s `(response, results)` from the provisioned streams and optional restore token. The `streams` field is `a(ua{sv})`.
fn start_response(
    infos: Vec<PipewireStreamInfo>,
    restore_token: Option<String>,
    persist_mode: u32,
) -> (u32, HashMap<String, OwnedValue>) {
    let mut results: HashMap<String, OwnedValue> = HashMap::new();
    let streams: Vec<(u32, HashMap<String, OwnedValue>)> = infos
        .into_iter()
        .map(super::screencast_pipewire::PipewireStreamInfo::into_stream_tuple)
        .collect();
    if let Ok(o) = OwnedValue::try_from(Value::new(streams)) {
        results.insert("streams".to_string(), o);
    }
    if let Some(token) = restore_token {
        if let Ok(o) = OwnedValue::try_from(Value::new(token)) {
            results.insert("restore_token".to_string(), o);
        }
        if let Ok(o) = OwnedValue::try_from(Value::U32(persist_mode)) {
            results.insert("persist_mode".to_string(), o);
        }
    }
    (0, results)
}

#[cfg(test)]
fn start_response_with_stream(info: PipewireStreamInfo) -> (u32, HashMap<String, OwnedValue>) {
    start_response(vec![info], None, 0)
}

/// Test-only: the outcome→response helper, kept so the "deny path returns response=1" assertion can be exercised without a provisioner.
#[cfg(test)]
fn outcome_to_start_response_for_tests(
    outcome: PromptOutcome,
) -> (u32, HashMap<String, OwnedValue>) {
    if !outcome.is_allowed() {
        return (1, HashMap::new());
    }
    start_response_with_stream(PipewireStreamInfo {
        node_id: 1,
        source_type: 1,
        id: 0,
        size: (1920, 1080),
        position: (0, 0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{
        CallerMap, FilePickerSpawnerHandle, GrantStoreHandle, MockScreenCastProvisioner,
        NullFilePickerSpawner, PromptQueueHandle, PromptResolver,
        ScreenCastStreamProvisionerHandle,
    };
    use crate::shell::state::{Workspace, WorkspaceId};
    use std::sync::Arc;

    fn empty_ws(id: u64) -> Workspace {
        Workspace::test_default(WorkspaceId(id), "sc")
    }

    #[test]
    fn start_allow_returns_response_zero_with_streams_key() {
        let (code, results) = outcome_to_start_response_for_tests(PromptOutcome::AllowOnce);
        assert_eq!(code, 0);
        assert!(results.contains_key("streams"));
    }

    #[test]
    fn start_deny_returns_response_one_no_results() {
        let (code, results) = outcome_to_start_response_for_tests(PromptOutcome::Deny);
        assert_eq!(code, 1);
        assert!(results.is_empty());
    }

    #[test]
    fn start_returns_non_empty_streams_on_allow() {
        let info = PipewireStreamInfo {
            node_id: 99,
            source_type: 1,
            id: 0,
            size: (1280, 720),
            position: (0, 0),
        };
        let (code, results) = start_response_with_stream(info);
        assert_eq!(code, 0);
        let streams_v = results.get("streams").expect("streams");
        let cloned: OwnedValue = streams_v.try_clone().expect("clone");
        let streams: Vec<(u32, HashMap<String, OwnedValue>)> =
            Vec::<(u32, HashMap<String, OwnedValue>)>::try_from(cloned).expect("Vec<(u32, dict)>");
        assert_eq!(streams.len(), 1, "must emit exactly one stream");
        assert_eq!(streams[0].0, 99, "node id must round-trip");
        assert!(streams[0].1.contains_key("size"));
        assert!(streams[0].1.contains_key("source_type"));
        assert!(streams[0].1.contains_key("id"));
    }

    #[test]
    fn start_response_includes_restore_token_when_persisted() {
        let info = PipewireStreamInfo {
            node_id: 3,
            source_type: 1,
            id: 0,
            size: (640, 480),
            position: (0, 0),
        };
        let (code, results) = start_response(vec![info], Some("tok-123".into()), 2);
        assert_eq!(code, 0);
        assert!(results.contains_key("restore_token"));
        assert!(results.contains_key("persist_mode"));
        // No token => no restore_token/persist_mode keys.
        let info2 = PipewireStreamInfo {
            node_id: 4,
            source_type: 1,
            id: 0,
            size: (640, 480),
            position: (0, 0),
        };
        let (_c, results2) = start_response(vec![info2], None, 0);
        assert!(!results2.contains_key("restore_token"));
    }

    #[test]
    fn selected_source_types_respects_multiple_and_mask() {
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
    }

    #[test]
    fn size_from_options_reads_and_defaults() {
        let mut opts: HashMap<String, OwnedValue> = HashMap::new();
        let size: (u32, u32) = (640, 480);
        opts.insert(
            "size".to_string(),
            OwnedValue::try_from(Value::new(size)).unwrap(),
        );
        assert_eq!(size_from_options(&opts), (640, 480));
        assert_eq!(size_from_options(&HashMap::new()), (1920, 1080));
    }

    #[test]
    fn available_types_and_modes_cover_the_spec_bits() {
        assert_eq!(source_type::ALL, 1 | 2 | 4);
        assert_eq!(cursor_mode::ALL, 1 | 2 | 4);
    }

    /// The provisioner records the request the Start path forwards, including the selected source type + size.
    #[test]
    fn screencast_provisioner_records_source_type_and_size() {
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(40));
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let provisioner = Arc::new(MockScreenCastProvisioner::new_with_node(13, "sc"));
        let sc_handle = ScreenCastStreamProvisionerHandle::new(provisioner.clone());
        let map = CallerMap::new();
        let caller = PortalCaller::new(ws_id, "org.example");
        map.force_caller_for_tests(caller);
        let ctx = PortalContext {
            grants: store,
            prompts: queue,
            caller_map: map,
            file_picker: FilePickerSpawnerHandle::new(Arc::new(NullFilePickerSpawner::cancel())),
            screencast_provisioner: sc_handle,
            remote_input_injector: crate::portal::RemoteInputInjectorHandle::null(),
            documents: crate::portal::DocumentStoreHandle::host_paths(),
            print_spooler: crate::portal::PrintSpoolerHandle::null(),
        };
        let req = PipewireStreamRequest {
            caller: PortalCaller::new(ws_id, "org.example"),
            width: 1024,
            height: 768,
            framerate: 30,
            source_type: source_type::WINDOW,
            cursor_mode: cursor_mode::HIDDEN,
        };
        let info = ctx
            .screencast_provisioner
            .provision(&req)
            .expect("provision");
        assert_eq!(info.node_id, 13);
        assert_eq!(info.size, (1024, 768));
        assert_eq!(info.source_type, source_type::WINDOW);
        let seen = provisioner.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].caller.app_id, "org.example");
        assert_eq!(seen[0].source_type, source_type::WINDOW);
    }
}
