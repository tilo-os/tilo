//! `org.freedesktop.impl.portal.FileChooser` backend.
//!
//! Spec: `/usr/share/dbus-1/interfaces/org.freedesktop.impl.portal.FileChooser.xml`.
//! Plan: Sandbox Phase 4.3, lines 335-353.
//!
//! Three methods:
//!
//! - `OpenFile(handle, app_id, parent_window, title, options) → (response, results)`
//! - `SaveFile(...)` — same shape
//! - `SaveFiles(...)` — same shape
//!
//! `response` is the portal response code (0 = success, 1 = user cancelled,
//! 2 = other error). `results` is `a{sv}`; for FileChooser the `uris` key
//! is mandatory on success.
//!
//! Every method:
//! 1. Resolves the caller's [`PortalCaller`] via the [`CallerMap`] using
//!    the D-Bus connection's unique-name.
//! 2. Delegates to [`flow::resolve`] with `CapabilityKind::FilesystemRead`
//!    (or `FilesystemReadWrite` for `SaveFile`/`SaveFiles`).
//! 3. On `Allow*`, returns response=0 + a placeholder `uris` array
//!    pointing at a synthetic file under `/work/.portal-tmp/`. The real
//!    file-dialog widget (deferred — see plan §5.1) will replace the
//!    placeholder; v1 ships the *flow* so xdg-desktop-portal's
//!    Request/Response contract is satisfied end-to-end.
//! 4. On `Deny`, returns response=1 + an empty results dict.

use std::collections::HashMap;
use std::path::PathBuf;

use zbus::interface;
use zbus::object_server::SignalContext;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::shell::state::{Capability, FsMode, GrantScope};

use super::{
    flow, CapabilityKind, PortalCaller, PortalContext, PromptOutcome, WorkspaceId,
};

/// D-Bus interface impl. One instance per [`super::serve`] call; held
/// by the zbus `ObjectServer` for the lifetime of the connection.
pub struct FileChooserBackend {
    ctx: PortalContext,
}

impl FileChooserBackend {
    pub fn new(ctx: PortalContext) -> Self {
        Self { ctx }
    }

    /// Resolve the caller to a [`PortalCaller`]. Falls back to a stub
    /// caller when no connection peer-credentials are available (e.g.
    /// in-process p2p test bus) — production wires the bridge accept
    /// path's PID lookup into [`CallerMap`].
    ///
    /// Note that [`CallerMap::lookup`] also honours the
    /// `force_caller_for_tests` override regardless of `sender` —
    /// `None` sender + forced caller still resolves correctly. We
    /// therefore always consult the map (with an empty-string key on
    /// the missing-sender branch) before falling back to the synth.
    pub(super) fn resolve_caller(
        ctx: &PortalContext,
        sender: Option<&OwnedUniqueName>,
        app_id: &str,
    ) -> PortalCaller {
        let key = sender.map(|n| n.as_str().to_string()).unwrap_or_default();
        if let Some(c) = ctx.caller_map.lookup(&key) {
            return c;
        }
        // Fallback: synthesise from the app-id alone, with a zero
        // workspace. Tests poke a real entry into the caller map ahead
        // of the call; production reaches the production-only bridge
        // accept path, which populates the map before any portal request
        // can be issued.
        PortalCaller::new(WorkspaceId(0), app_id)
    }
}

#[interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooserBackend {
    /// `OpenFile` — request one or more existing files.
    async fn open_file(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.OpenFile: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = Self::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::Filesystem {
            // The portal protocol doesn't surface a target path here —
            // the dialog selects one. For the grant model we tag the
            // "any path" workspace-root sentinel so the grant store
            // knows this is a filesystem capability.
            path: PathBuf::from("/"),
            mode: FsMode::Read,
            scope: GrantScope::Once,
        };
        let outcome = flow::resolve(
            &self.ctx,
            caller,
            CapabilityKind::FilesystemRead,
            cap_template,
        )
        .await;
        outcome_to_open_response(outcome, /*save=*/ false)
    }

    /// `SaveFile` — request a destination for one new file.
    async fn save_file(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.SaveFile: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = Self::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::Filesystem {
            path: PathBuf::from("/"),
            mode: FsMode::ReadWrite,
            scope: GrantScope::Once,
        };
        let outcome = flow::resolve(
            &self.ctx,
            caller,
            CapabilityKind::FilesystemReadWrite,
            cap_template,
        )
        .await;
        outcome_to_open_response(outcome, /*save=*/ true)
    }

    /// `SaveFiles` — request a folder + multiple destination filenames.
    async fn save_files(
        &self,
        #[zbus(signal_context)] _ctx: SignalContext<'_>,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        _handle: ObjectPath<'_>,
        app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, HashMap<String, OwnedValue>) {
        tracing::debug!(
            "FileChooser.SaveFiles: app={app_id} title={title} opts={}",
            options.len()
        );
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = Self::resolve_caller(&self.ctx, sender.as_ref(), &app_id);
        let cap_template = Capability::Filesystem {
            path: PathBuf::from("/"),
            mode: FsMode::ReadWrite,
            scope: GrantScope::Once,
        };
        let outcome = flow::resolve(
            &self.ctx,
            caller,
            CapabilityKind::FilesystemReadWrite,
            cap_template,
        )
        .await;
        outcome_to_open_response(outcome, /*save=*/ true)
    }
}

/// Translate a [`PromptOutcome`] into the (response_code, results)
/// pair the FileChooser frontend expects.
///
/// `response_code` matches the freedesktop.org Request convention:
///   0 = OK (with results), 1 = user cancelled, 2 = other failure.
pub(super) fn outcome_to_open_response(
    outcome: PromptOutcome,
    save: bool,
) -> (u32, HashMap<String, OwnedValue>) {
    let mut results: HashMap<String, OwnedValue> = HashMap::new();
    if !outcome.is_allowed() {
        return (1, results);
    }
    // Placeholder URI inside the workspace's `/work` sandbox bind. Until
    // the dialog widget lands (plan §5.1), the deny-path is what
    // mediates real access — we just need to return *some* well-formed
    // `uris` value so the frontend's strict-parser doesn't reject the
    // response. `file:///work/.portal-tmp/<name>` makes it obvious in
    // logs that this is a stub.
    let name = if save { "saved.bin" } else { "selected.bin" };
    let uri = format!("file:///work/.portal-tmp/{name}");
    let uris: Vec<String> = vec![uri];
    let uris_value = Value::new(uris);
    if let Ok(owned) = OwnedValue::try_from(uris_value) {
        results.insert("uris".to_string(), owned);
    }
    if save {
        if let Ok(w) = OwnedValue::try_from(Value::Bool(true)) {
            results.insert("writable".to_string(), w);
        }
    }
    (0, results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::{CallerMap, GrantStoreHandle, PromptQueueHandle, PromptResolver};
    use crate::shell::state::Workspace;
    use std::collections::HashMap;

    fn empty_ws(id: u64) -> Workspace {
        Workspace {
            id: WorkspaceId(id),
            name: "fc".into(),
            strips: vec![],
            roles: vec![],
            workspace_files: vec![],
            collaborators: vec![],
            grants: HashMap::new(),
            shared_surfaces: HashMap::new(),
        }
    }

    #[test]
    fn outcome_allow_returns_response_zero_and_uri() {
        let (code, results) =
            outcome_to_open_response(PromptOutcome::AllowOnce, /*save=*/ false);
        assert_eq!(code, 0);
        assert!(results.contains_key("uris"));
        assert!(!results.contains_key("writable"));
    }

    #[test]
    fn outcome_save_returns_writable_true() {
        let (code, results) =
            outcome_to_open_response(PromptOutcome::AllowAlways, /*save=*/ true);
        assert_eq!(code, 0);
        assert!(results.contains_key("uris"));
        assert!(results.contains_key("writable"));
    }

    #[test]
    fn outcome_deny_returns_response_one_and_no_results() {
        let (code, results) =
            outcome_to_open_response(PromptOutcome::Deny, /*save=*/ false);
        assert_eq!(code, 1);
        assert!(results.is_empty());
    }

    #[test]
    fn resolve_caller_falls_back_to_app_id_when_no_map_entry() {
        let (store, _ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(31));
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let ctx = PortalContext {
            grants: store,
            prompts: queue,
            caller_map: CallerMap::new(),
        };
        let caller = FileChooserBackend::resolve_caller(&ctx, None, "org.example");
        assert_eq!(caller.app_id, "org.example");
        // Zero workspace id flags "no bridge match" — production-side
        // grant lookup will return None and fall through to the prompt
        // path (which is the conservative posture).
        assert_eq!(caller.workspace_id, WorkspaceId(0));
    }

    #[test]
    fn resolve_caller_uses_map_when_unique_name_present() {
        let (store, ws_id) = GrantStoreHandle::test_with_workspace(empty_ws(32));
        let queue = PromptQueueHandle::new(PromptResolver::AutoAllow);
        let map = CallerMap::new();
        map.insert(":1.7", PortalCaller::new(ws_id, "org.real"));
        let ctx = PortalContext {
            grants: store,
            prompts: queue,
            caller_map: map,
        };
        let name: OwnedUniqueName = ":1.7".try_into().unwrap();
        let caller = FileChooserBackend::resolve_caller(&ctx, Some(&name), "ignored");
        assert_eq!(caller.app_id, "org.real");
        assert_eq!(caller.workspace_id, ws_id);
    }
}
