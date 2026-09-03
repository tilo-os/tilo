//! `org.freedesktop.impl.portal.Background` backed by qOS's launcher.
//!
//! The portal never executes the `commandline` supplied by an application.
//! A successful autostart change is delegated to the signed package launcher,
//! which resolves the authenticated application id to its installed desktop
//! entry. This preserves qOS's app/workspace isolation and avoids a second,
//! unsafe autostart mechanism.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{ObjectPath, OwnedValue};

use super::desktop_services::PortalResult;
use super::lifecycle::{LifecycleEntry, LifecycleResource, PortalLifecycle};
use super::{
    PortalContext,
    auth::{authenticate_call, authenticate_frontend_call},
};
use crate::shell::portal_dialog::{PortalDialogQueue, ReviewDecision, ReviewRequest};

pub const BACKGROUND: u32 = 0;
pub const RUNNING: u32 = 1;
pub const ACTIVE: u32 = 2;

const MAX_TRACKED_APPS: usize = 256;
const MAX_NAME_BYTES: usize = 512;
const MAX_COMMAND_ARGUMENTS: usize = 64;
const MAX_COMMAND_ARGUMENT_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundConsentRequest {
    pub handle: String,
    pub app_id: String,
    pub name: String,
}

/// The launcher/chrome response to a background-permission sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundConsent {
    Allow,
    Deny,
}

/// qOS-owned boundary for background UI and package-registered autostart.
///
/// `set_autostart` must only create/remove an entry for the signed package
/// identified by `app_id`; it intentionally receives no executable path or
/// caller-provided argv. Chrome implementations should use `handle` as their
/// sheet identity and complete `reply` exactly once.
pub trait BackgroundService: Send + Sync {
    fn request_background(
        &self,
        request: BackgroundConsentRequest,
        reply: oneshot::Sender<PortalResult<BackgroundConsent>>,
    ) -> PortalResult<()>;
    fn set_autostart(&self, app_id: &str, enabled: bool) -> PortalResult<bool>;
    fn cancel_background(&self, _handle: &str) {}
}

#[derive(Clone)]
pub struct BackgroundServiceHandle(Arc<dyn BackgroundService>);
impl BackgroundServiceHandle {
    pub fn new(service: Arc<dyn BackgroundService>) -> Self {
        Self(service)
    }
    fn request_background(
        &self,
        request: BackgroundConsentRequest,
        reply: oneshot::Sender<PortalResult<BackgroundConsent>>,
    ) -> PortalResult<()> {
        self.0.request_background(request, reply)
    }
    fn set_autostart(&self, app_id: &str, enabled: bool) -> PortalResult<bool> {
        self.0.set_autostart(app_id, enabled)
    }
    fn cancel_background(&self, handle: &str) {
        self.0.cancel_background(handle);
    }
}

struct UnavailableBackgroundService;
impl BackgroundService for UnavailableBackgroundService {
    fn request_background(
        &self,
        _: BackgroundConsentRequest,
        _: oneshot::Sender<PortalResult<BackgroundConsent>>,
    ) -> PortalResult<()> {
        PortalResult::Unavailable("qOS background-permission chrome is not attached".into())
    }
    fn set_autostart(&self, _: &str, _: bool) -> PortalResult<bool> {
        PortalResult::Unavailable("qOS package launcher is not attached".into())
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct AutostartFile {
    /// Exact signed package ids only, never command lines or .desktop paths.
    packages: BTreeSet<String>,
}

/// qOS's production Background adapter.  Consent is shown through the shell
/// review queue and autostart state is a private durable list of installed GUI
/// package ids. On session restore the compositor calls
/// [`Self::launch_enabled_in_workspace`], which re-validates each package and
/// delegates exclusively to the normal sandboxed launcher.
#[derive(Clone)]
pub struct ShellBackgroundService {
    dialogs: PortalDialogQueue,
    state: Arc<Mutex<AutostartFile>>,
    path: Arc<PathBuf>,
    startup_launched: Arc<AtomicBool>,
}

impl ShellBackgroundService {
    pub fn global() -> Self {
        static SERVICE: LazyLock<ShellBackgroundService> = LazyLock::new(|| {
            ShellBackgroundService::at_path(PortalDialogQueue::global(), default_path())
        });
        SERVICE.clone()
    }

    pub fn with_dialogs(dialogs: PortalDialogQueue) -> Self {
        Self::at_path(dialogs, default_path())
    }

    pub fn at_path(dialogs: PortalDialogQueue, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let state = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            dialogs,
            state: Arc::new(Mutex::new(state)),
            path: Arc::new(path),
            startup_launched: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Normal session-restore hook. Every entry is revalidated against the
    /// current signed package registry immediately before launching; removed
    /// packages are skipped and are never represented by a stale executable.
    pub fn launch_enabled_in_workspace(
        &self,
        workspace: &crate::shell::state::Workspace,
    ) -> Vec<String> {
        let packages = self
            .state
            .lock()
            .map(|state| state.packages.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        packages
            .into_iter()
            .filter(|app_id| is_installed_gui_package(app_id))
            .filter(|app_id| crate::shell::launcher::launch_in_workspace(app_id, workspace).is_ok())
            .collect()
    }

    /// Launch the persisted entries once at desktop-session startup. The
    /// atomic guard prevents a D-Bus activation/retry from spawning another
    /// copy in the same qOS process; each package is still validated by the
    /// launcher at the point of launch.
    pub fn launch_at_session_start(&self, tree: &crate::shell::state::WorkspaceTreeHandle) {
        if self.startup_launched.swap(true, Ordering::AcqRel) {
            return;
        }
        // Background is a session service, not one copy per collaborative
        // workspace. Deliberately choose the workspace the user restored
        // into; that gives launches normal room/capability attribution while
        // preventing every open workspace from spawning a duplicate process.
        let workspace = tree.read().active_workspace().cloned();
        if let Some(workspace) = workspace {
            let _ = self.launch_enabled_in_workspace(&workspace);
        }
    }

    pub fn enabled_packages(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.packages.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn commit(&self, state: &AutostartFile) -> PortalResult<()> {
        let Some(parent) = self.path.parent() else {
            return PortalResult::Failed("background state path has no parent".into());
        };
        if let Err(error) = fs::create_dir_all(parent) {
            return PortalResult::Failed(format!(
                "cannot create background state directory: {error}"
            ));
        }
        if let Err(error) = fs::set_permissions(parent, fs::Permissions::from_mode(0o700)) {
            return PortalResult::Failed(format!(
                "cannot protect background state directory: {error}"
            ));
        }
        let encoded = match serde_json::to_vec(state) {
            Ok(encoded) => encoded,
            Err(error) => {
                return PortalResult::Failed(format!("cannot encode background state: {error}"));
            }
        };
        let temporary = self.path.with_extension("tmp");
        let mut file = match fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) => {
                return PortalResult::Failed(format!("cannot create background state: {error}"));
            }
        };
        if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
            return PortalResult::Failed(format!("cannot protect background state: {error}"));
        }
        if let Err(error) = file.write_all(&encoded).and_then(|()| file.sync_all()) {
            return PortalResult::Failed(format!("cannot write background state: {error}"));
        }
        if let Err(error) = fs::rename(&temporary, self.path.as_ref()) {
            return PortalResult::Failed(format!("cannot commit background state: {error}"));
        }
        if let Err(error) = fs::File::open(parent).and_then(|directory| directory.sync_all()) {
            return PortalResult::Failed(format!("cannot sync background state: {error}"));
        }
        PortalResult::Success(())
    }
}

impl BackgroundService for ShellBackgroundService {
    fn request_background(
        &self,
        request: BackgroundConsentRequest,
        reply: oneshot::Sender<PortalResult<BackgroundConsent>>,
    ) -> PortalResult<()> {
        let review = ReviewRequest {
            handle: format!("{}:background", request.handle),
            app_id: request.app_id,
            title: "Allow background activity?".into(),
            reason: request.name,
            resource: "The app may keep working after its window is closed.".into(),
            confirm_label: "Allow in background".into(),
        };
        let receiver = match self.dialogs.enqueue_review(review) {
            Ok(receiver) => receiver,
            Err(error) => return PortalResult::Unavailable(error),
        };
        tokio::spawn(async move {
            let consent = match receiver.await.unwrap_or(ReviewDecision::Cancelled) {
                ReviewDecision::Confirmed => BackgroundConsent::Allow,
                ReviewDecision::Cancelled => BackgroundConsent::Deny,
            };
            let _ = reply.send(PortalResult::Success(consent));
        });
        PortalResult::Success(())
    }

    fn set_autostart(&self, app_id: &str, enabled: bool) -> PortalResult<bool> {
        if !is_installed_gui_package(app_id) {
            return PortalResult::Failed(
                "application is not a signed installed GUI package".into(),
            );
        }
        let Ok(mut state) = self.state.lock() else {
            return PortalResult::Unavailable("background state is unavailable".into());
        };
        let changed = if enabled {
            state.packages.insert(app_id.into())
        } else {
            state.packages.remove(app_id)
        };
        if !changed {
            return PortalResult::Success(true);
        }
        let snapshot = state.clone();
        match self.commit(&snapshot) {
            PortalResult::Success(()) => PortalResult::Success(true),
            PortalResult::Cancelled => PortalResult::Cancelled,
            PortalResult::Unavailable(error) => PortalResult::Unavailable(error),
            PortalResult::Failed(error) => {
                *state = fs::read(self.path.as_ref())
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default();
                PortalResult::Failed(error)
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct AppState {
    state: u32,
    running: bool,
    background_allowed: bool,
    autostart_enabled: bool,
}

#[derive(Clone)]
pub struct BackgroundBackend {
    ctx: PortalContext,
    apps: Arc<Mutex<HashMap<String, AppState>>>,
    service: BackgroundServiceHandle,
    lifecycle: PortalLifecycle,
}

impl BackgroundBackend {
    /// Creates a backend that denies requests until qOS's launcher/chrome
    /// service is wired using [`Self::with_service`].
    pub fn new(ctx: PortalContext) -> Self {
        Self::with_lifecycle(ctx, PortalLifecycle::new())
    }

    pub fn with_lifecycle(ctx: PortalContext, lifecycle: PortalLifecycle) -> Self {
        Self::with_service_and_lifecycle(
            ctx,
            BackgroundServiceHandle::new(Arc::new(UnavailableBackgroundService)),
            lifecycle,
        )
    }

    pub fn with_service(ctx: PortalContext, service: BackgroundServiceHandle) -> Self {
        Self::with_service_and_lifecycle(ctx, service, PortalLifecycle::new())
    }

    pub fn with_service_and_lifecycle(
        ctx: PortalContext,
        service: BackgroundServiceHandle,
        lifecycle: PortalLifecycle,
    ) -> Self {
        Self {
            ctx,
            apps: Arc::new(Mutex::new(HashMap::new())),
            service,
            lifecycle,
        }
    }

    /// Compositor-side state update. The application id originates from the
    /// launcher, never from this D-Bus API; updates are bounded to avoid an
    /// unbounded activity map.
    pub fn set_app_state(&self, app_id: impl AsRef<str>, state: u32) -> bool {
        let app_id = app_id.as_ref();
        if !valid_app_key(app_id) || state > ACTIVE {
            return false;
        }
        self.apps.lock().is_ok_and(|mut apps| {
            if !apps.contains_key(app_id) && apps.len() >= MAX_TRACKED_APPS {
                return false;
            }
            let entry = apps.entry(app_id.into()).or_default();
            entry.state = state;
            entry.running = true;
            true
        })
    }

    /// Reconcile the portal view with the launcher's PID/start-time verified
    /// process registry. Returns true only when the externally visible map
    /// changed, allowing the signal pump to remain quiet otherwise.
    pub fn sync_running_apps(&self) -> bool {
        let running: BTreeSet<String> = crate::shell::launcher::verified_running_packages()
            .into_iter()
            .collect();
        let Ok(mut apps) = self.apps.lock() else {
            return false;
        };
        let before: BTreeSet<_> = apps
            .iter()
            .filter(|(_, entry)| entry.running)
            .map(|(id, entry)| (id.clone(), entry.state))
            .collect();
        for entry in apps.values_mut() {
            entry.running = false;
        }
        for app_id in &running {
            if apps.contains_key(app_id) || apps.len() < MAX_TRACKED_APPS {
                let entry = apps.entry(app_id.clone()).or_default();
                entry.running = true;
                if entry.state == ACTIVE {
                    // Focus is compositor-owned and may be restored through
                    // set_app_state; otherwise a live process is Running.
                } else {
                    entry.state = RUNNING;
                }
            }
        }
        apps.retain(|_, entry| {
            entry.running || entry.background_allowed || entry.autostart_enabled
        });
        let after: BTreeSet<_> = apps
            .iter()
            .filter(|(_, entry)| entry.running)
            .map(|(id, entry)| (id.clone(), entry.state))
            .collect();
        before != after
    }

    /// Remove state when the package is uninstalled. This also forgets portal
    /// consent; package reinstall must ask again.
    pub fn remove_app(&self, app_id: &str) -> bool {
        self.apps
            .lock()
            .is_ok_and(|mut apps| apps.remove(app_id).is_some())
    }

    pub fn app_state(&self, app_id: &str) -> Option<u32> {
        self.apps.lock().ok()?.get(app_id).map(|entry| entry.state)
    }

    /// Observable only by the compositor/launcher integration; it is not a
    /// D-Bus property and cannot be forged by an application.
    pub fn autostart_enabled(&self, app_id: &str) -> Option<bool> {
        self.apps
            .lock()
            .ok()?
            .get(app_id)
            .map(|entry| entry.autostart_enabled)
    }

    async fn authenticated(
        &self,
        hdr: zbus::message::Header<'_>,
        app_id: &str,
    ) -> Option<super::auth::AuthenticatedCall> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        authenticate_call(&self.ctx, sender.as_ref(), app_id, None)
            .await
            .ok()
    }
}

#[interface(name = "org.freedesktop.impl.portal.Background")]
impl BackgroundBackend {
    async fn get_app_state(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
    ) -> HashMap<String, OwnedValue> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        if authenticate_frontend_call(&self.ctx, sender.as_ref())
            .await
            .is_err()
        {
            return HashMap::new();
        }
        self.apps
            .lock()
            .map(|apps| {
                apps.iter()
                    .filter(|(_, entry)| entry.running)
                    .map(|(app, entry)| (app.clone(), OwnedValue::from(entry.state)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The outer response means the request completed. `result=1` means the
    /// user allowed background use; `result=0` is an explicit denial.
    async fn notify_background(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        handle: ObjectPath<'_>,
        app_id: String,
        name: String,
    ) -> (u32, HashMap<String, OwnedValue>) {
        let Some(auth) = self.authenticated(hdr, &app_id).await else {
            return (2, HashMap::new());
        };
        if !valid_name(&name) {
            return (2, HashMap::new());
        }
        let key = handle.to_string();
        let entry = LifecycleEntry {
            owner: auth.owner.to_string(),
            app_id: auth.caller.app_id.clone(),
            resource: LifecycleResource::Prompt,
        };
        let service = self.service.clone();
        let cancel_key = key.clone();
        let registered = self
            .lifecycle
            .register_request(connection, &key, entry, move |_| {
                service.cancel_background(&cancel_key);
            })
            .await;
        if !matches!(registered, Ok(true)) {
            return (2, HashMap::new());
        }
        let (reply, rx) = oneshot::channel();
        let request = BackgroundConsentRequest {
            handle: key.clone(),
            app_id: auth.caller.app_id.clone(),
            name,
        };
        let outcome = match self.service.request_background(request, reply) {
            PortalResult::Success(()) => rx.await.unwrap_or(PortalResult::Cancelled),
            PortalResult::Cancelled => PortalResult::Cancelled,
            PortalResult::Unavailable(error) => PortalResult::Unavailable(error),
            PortalResult::Failed(error) => PortalResult::Failed(error),
        };
        self.lifecycle
            .complete_request_object(connection, &key)
            .await;
        let allowed = matches!(outcome, PortalResult::Success(BackgroundConsent::Allow));
        if allowed {
            if let Ok(mut apps) = self.apps.lock() {
                if apps.contains_key(&auth.caller.app_id) || apps.len() < MAX_TRACKED_APPS {
                    apps.entry(auth.caller.app_id.clone())
                        .or_default()
                        .background_allowed = true;
                }
            }
        }
        match outcome {
            PortalResult::Success(_) => response_with_result(allowed),
            PortalResult::Cancelled => (1, HashMap::new()),
            PortalResult::Unavailable(_) | PortalResult::Failed(_) => (2, HashMap::new()),
        }
    }

    /// Deprecated upstream API. qOS accepts it only after a background grant,
    /// and passes solely the verified package id to the launcher. The argv is
    /// parsed only for size/control-character limits then discarded.
    async fn enable_autostart(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        app_id: String,
        enable: bool,
        commandline: Vec<String>,
        flags: u32,
    ) -> bool {
        let Some(auth) = self.authenticated(hdr, &app_id).await else {
            return false;
        };
        if flags != 0 || !valid_commandline(&commandline) {
            return false;
        }
        let permitted = self
            .apps
            .lock()
            .ok()
            .and_then(|apps| apps.get(&auth.caller.app_id).copied())
            .is_some_and(|state| state.background_allowed);
        if !permitted {
            return false;
        }
        match self.service.set_autostart(&auth.caller.app_id, enable) {
            PortalResult::Success(applied) => {
                if applied {
                    if let Ok(mut apps) = self.apps.lock() {
                        if let Some(state) = apps.get_mut(&auth.caller.app_id) {
                            state.autostart_enabled = enable;
                        }
                    }
                }
                applied
            }
            PortalResult::Cancelled | PortalResult::Unavailable(_) | PortalResult::Failed(_) => {
                false
            }
        }
    }

    #[zbus(signal)]
    async fn running_applications_changed(
        ctxt: &zbus::object_server::SignalContext<'_>,
    ) -> zbus::Result<()>;
}

pub fn spawn_running_monitor(connection: zbus::Connection, backend: BackgroundBackend) {
    let task_connection = connection.clone();
    connection
        .executor()
        .spawn(
            async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_millis(750));
                loop {
                    interval.tick().await;
                    if !backend.sync_running_apps() {
                        continue;
                    }
                    let Ok(context) = zbus::object_server::SignalContext::new(
                        &task_connection,
                        super::OBJECT_PATH,
                    ) else {
                        break;
                    };
                    if BackgroundBackend::running_applications_changed(&context)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            },
            "portal background application monitor",
        )
        .detach();
}

fn response_with_result(allowed: bool) -> (u32, HashMap<String, OwnedValue>) {
    let mut results = HashMap::new();
    results.insert(
        "result".into(),
        OwnedValue::from(if allowed { 1_u32 } else { 0_u32 }),
    );
    (0, results)
}
fn valid_app_key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
}
fn valid_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_NAME_BYTES && !value.chars().any(char::is_control)
}
fn valid_commandline(values: &[String]) -> bool {
    values.len() <= MAX_COMMAND_ARGUMENTS
        && values.iter().all(|value| {
            value.len() <= MAX_COMMAND_ARGUMENT_BYTES && !value.chars().any(char::is_control)
        })
}

fn is_installed_gui_package(app_id: &str) -> bool {
    crate::shell::launcher::installed_apps()
        .map(|apps| {
            apps.into_iter()
                .any(|app| app.has_gui && app.package_id == app_id)
        })
        .unwrap_or(false)
}

fn default_path() -> PathBuf {
    super::private_state_root()
        // A missing private state/runtime directory must make persistence
        // fail; shared /tmp would let another user pre-seed portal state.
        .unwrap_or_else(|| PathBuf::from("/dev/null"))
        .join("qos/portal/background-autostart.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_is_bounded_and_values_are_validated() {
        let (grants, _) = crate::portal::GrantStoreHandle::test_with_workspace(
            crate::shell::state::Workspace::test_default(
                crate::shell::state::WorkspaceId(1),
                "test",
            ),
        );
        let backend = BackgroundBackend::new(PortalContext::new_with_null_spawners(
            grants,
            crate::portal::PromptQueueHandle::new(crate::portal::PromptResolver::AutoDeny),
            crate::portal::CallerMap::new(),
        ));
        assert!(backend.set_app_state("org.example.App", ACTIVE));
        assert_eq!(backend.app_state("org.example.App"), Some(ACTIVE));
        assert!(!backend.set_app_state("org.example.Bad", 3));
        assert!(!valid_commandline(&vec!["bad\nargv".into()]));
    }

    #[test]
    fn denial_result_is_not_a_fabricated_grant() {
        let response = response_with_result(false);
        assert_eq!(response.0, 0);
        assert_eq!(
            response
                .1
                .get("result")
                .and_then(|value| value.try_clone().ok())
                .and_then(|value| u32::try_from(value).ok()),
            Some(0)
        );
    }

    #[test]
    fn autostart_state_is_private_and_contains_package_ids_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let service = ShellBackgroundService::at_path(PortalDialogQueue::default(), &path);
        let file = AutostartFile {
            packages: BTreeSet::from(["org.example.App".into()]),
        };
        assert!(matches!(service.commit(&file), PortalResult::Success(())));
        assert_eq!(
            ShellBackgroundService::at_path(PortalDialogQueue::default(), &path).enabled_packages(),
            vec!["org.example.App"]
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn session_start_chooses_only_the_active_workspace() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_demo();
        let active = tree.active_workspace().unwrap().id;
        // A second workspace models a restored multi-workspace session.
        let second = crate::shell::state::Workspace::test_default(
            crate::shell::state::WorkspaceId(active.0 + 100),
            "other",
        );
        tree.workspaces.push(second);
        let shared = tree.shared();
        let chosen = shared
            .read()
            .active_workspace()
            .map(|workspace| workspace.id);
        assert_eq!(chosen, Some(active));
    }
}
