//! Injectable, shell-owned operations used by application-facing portals.
//!
//! Portal D-Bus objects deliberately contain no privileged host operations.
//! They decode the wire contract and delegate to this small service boundary;
//! the compositor supplies real implementations once its UI is running while
//! tests can use deterministic recording implementations.  The default is
//! always an explicit cancellation/unavailable result, never fabricated data.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// A portal operation result.  Backends map these directly to the portal
/// response codes: success=0, cancelled=1, unavailable/failed=2.
#[derive(Clone, Debug, PartialEq)]
pub enum PortalResult<T> {
    Success(T),
    Cancelled,
    Unavailable(String),
    Failed(String),
}

impl<T> PortalResult<T> {
    pub const fn response_code(&self) -> u32 {
        match self {
            Self::Success(_) => 0,
            Self::Cancelled => 1,
            Self::Unavailable(_) | Self::Failed(_) => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountInformation {
    pub id: String,
    pub name: String,
    /// A document-portal URI, never a private host path.
    pub image_uri: Option<String>,
}

pub trait AccountService: Send + Sync {
    fn get(&self, app_id: &str, reason: Option<&str>) -> PortalResult<AccountInformation>;
}

#[derive(Clone)]
pub struct AccountServiceHandle(Arc<dyn AccountService>);

impl AccountServiceHandle {
    pub fn new(service: Arc<dyn AccountService>) -> Self {
        Self(service)
    }
    pub fn get(&self, app_id: &str, reason: Option<&str>) -> PortalResult<AccountInformation> {
        self.0.get(app_id, reason)
    }
}

struct NoAccount;
impl AccountService for NoAccount {
    fn get(&self, _: &str, _: Option<&str>) -> PortalResult<AccountInformation> {
        PortalResult::Unavailable("Tilo identity sharing is not available in this session".into())
    }
}

struct WorkspaceAccount {
    grants: super::GrantStoreHandle,
}

impl AccountService for WorkspaceAccount {
    fn get(&self, app_id: &str, _: Option<&str>) -> PortalResult<AccountInformation> {
        let Some(caller) = self.grants.resolve_live_app(app_id) else {
            return PortalResult::Failed("application instance is no longer active".into());
        };
        let tree = self.grants.tree_handle();
        let room = {
            let tree = tree.read();
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id == caller.workspace_id)
                .map(|workspace| workspace.room.to_string())
        };
        let Some(room) = room else {
            return PortalResult::Failed("application workspace is no longer active".into());
        };
        match crate::identity_registry::selected_identity_profile(&room) {
            Ok(Some(profile)) => PortalResult::Success(AccountInformation {
                id: profile.principal_id(),
                name: profile.display_name,
                // The identity registry contains a private signing-record
                // path, not an avatar. It must never escape through Account.
                image_uri: None,
            }),
            Ok(None) => PortalResult::Unavailable("this workspace has no selected identity".into()),
            Err(error) => PortalResult::Failed(format!("identity registry unavailable: {error}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationChoiceRequest {
    pub handle: String,
    pub app_id: String,
    pub choices: Vec<String>,
    pub last_choice: Option<String>,
    pub content_type: Option<String>,
    pub uri: Option<String>,
    pub filename: Option<String>,
    pub activation_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationChoice {
    pub choice: String,
    pub activation_token: Option<String>,
}

pub trait AppChooserService: Send + Sync {
    /// Queue a chooser request without blocking the D-Bus dispatcher.  The
    /// service owns the reply until the user chooses or cancels, which lets
    /// `UpdateChoices` change the live sheet meanwhile.
    fn enqueue(
        &self,
        request: ApplicationChoiceRequest,
        reply: oneshot::Sender<PortalResult<ApplicationChoice>>,
    ) -> PortalResult<()>;
    fn update_choices(&self, handle: &str, choices: &[String]) -> PortalResult<()>;

    /// Withdraw a live sheet because its implementation-side Request object
    /// was closed. Implementations must release the reply sender so the
    /// waiting D-Bus call resolves as cancelled rather than hanging.
    fn cancel(&self, _handle: &str) {}
}

#[derive(Clone)]
pub struct AppChooserServiceHandle(Arc<dyn AppChooserService>);
impl AppChooserServiceHandle {
    pub fn new(service: Arc<dyn AppChooserService>) -> Self {
        Self(service)
    }
    pub fn enqueue(
        &self,
        request: ApplicationChoiceRequest,
        reply: oneshot::Sender<PortalResult<ApplicationChoice>>,
    ) -> PortalResult<()> {
        self.0.enqueue(request, reply)
    }
    pub fn update_choices(&self, handle: &str, choices: &[String]) -> PortalResult<()> {
        self.0.update_choices(handle, choices)
    }
    pub fn cancel(&self, handle: &str) {
        self.0.cancel(handle);
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherInstallRequest {
    pub app_id: String,
    pub name: String,
    pub launcher_type: u32,
    pub target: Option<String>,
    pub editable_name: bool,
    pub editable_icon: bool,
    /// Tilo-owned icon descriptor chosen by the reviewed portal sheet.
    pub icon_kind: String,
    /// Validated bytes for a supplied icon. Tilo-owned icon choices leave
    /// this empty; caller-owned paths and FDs never cross this boundary.
    pub icon_bytes: Option<Vec<u8>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherInstallChoice {
    pub name: String,
}
pub trait DynamicLauncherService: Send + Sync {
    fn prepare_install(
        &self,
        request: &LauncherInstallRequest,
    ) -> PortalResult<LauncherInstallChoice>;
    fn request_install_token(&self, app_id: &str) -> PortalResult<()>;
}
#[derive(Clone)]
pub struct DynamicLauncherServiceHandle(Arc<dyn DynamicLauncherService>);
impl DynamicLauncherServiceHandle {
    pub fn new(service: Arc<dyn DynamicLauncherService>) -> Self {
        Self(service)
    }
    pub fn prepare_install(
        &self,
        request: &LauncherInstallRequest,
    ) -> PortalResult<LauncherInstallChoice> {
        self.0.prepare_install(request)
    }
    pub fn request_install_token(&self, app_id: &str) -> PortalResult<()> {
        self.0.request_install_token(app_id)
    }
}
struct NoDynamicLauncher;
impl DynamicLauncherService for NoDynamicLauncher {
    fn prepare_install(&self, _: &LauncherInstallRequest) -> PortalResult<LauncherInstallChoice> {
        PortalResult::Unavailable("launcher installation is not attached".into())
    }
    fn request_install_token(&self, _: &str) -> PortalResult<()> {
        PortalResult::Cancelled
    }
}

/// Tilo-owned dynamic launcher library.  These are deliberately *not*
/// `.desktop` files: a portal caller can never add an executable command to
/// the host.  The application catalogue can consume these records as web
/// shortcuts, while activation goes through the normal URI policy resolver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DynamicLauncherRecord {
    pub id: String,
    pub owner_app_id: String,
    pub name: String,
    pub launcher_type: u32,
    pub target: Option<String>,
    #[serde(default = "default_dynamic_launcher_icon_kind")]
    pub icon_kind: String,
    /// The checked caller icon, retained as data rather than as a path. This
    /// lets launcher UI render the reviewed image after a restart without
    /// giving it ambient access to the creating application's files.
    #[serde(default)]
    pub icon_bytes: Option<Vec<u8>>,
}

fn default_dynamic_launcher_icon_kind() -> String {
    "supplied".into()
}

#[derive(Clone)]
pub struct ShellDynamicLauncherService {
    records: Arc<Mutex<Vec<DynamicLauncherRecord>>>,
    path: Arc<PathBuf>,
}

impl ShellDynamicLauncherService {
    pub fn for_session() -> Self {
        Self::at_path(portal_state_path("dynamic-launchers.json"))
    }

    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let records = tilo_files::native::atomic::json::load_or_default(&path);
        Self {
            records: Arc::new(Mutex::new(records)),
            path: Arc::new(path),
        }
    }

    pub fn records(&self) -> Vec<DynamicLauncherRecord> {
        self.records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default()
    }

    fn persist(&self, records: &[DynamicLauncherRecord]) -> Result<(), String> {
        write_private_json(self.path.as_ref(), records)
    }
}

/// Snapshot the reviewed shortcuts installed through DynamicLauncher.  The
/// application palette re-reads the private file instead of keeping a second
/// cache, so a newly accepted shortcut is visible on its next query and a
/// corrupt/stale file simply contributes no rows.
pub fn dynamic_launcher_records() -> Vec<DynamicLauncherRecord> {
    ShellDynamicLauncherService::for_session()
        .records()
        .into_iter()
        .filter(|record| {
            valid_launcher_request(&LauncherInstallRequest {
                app_id: record.owner_app_id.clone(),
                name: record.name.clone(),
                launcher_type: record.launcher_type,
                target: record.target.clone(),
                editable_name: false,
                editable_icon: false,
                icon_kind: record.icon_kind.clone(),
                icon_bytes: record.icon_bytes.clone(),
            }) && record.icon_bytes.as_deref().is_none_or(valid_icon_payload)
        })
        .collect()
}

/// Cheap file generation for the reviewed launcher library.  This is used to
/// decide whether a retained palette projection must re-read the JSON; normal
/// query edits only stat the private file and never clone its records.
#[must_use]
pub fn dynamic_launcher_generation() -> u128 {
    let path = portal_state_path("dynamic-launchers.json");
    let Ok(metadata) = fs::metadata(path) else {
        return 0;
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    modified ^ u128::from(metadata.len())
}

/// Pair one parsed dynamic-launcher library with the file generation that was
/// observed for that same read. A writer racing the read yields a different
/// generation and causes a retained worker result to be discarded rather than
/// relabelled as current.
pub fn dynamic_launcher_snapshot() -> (u128, Vec<DynamicLauncherRecord>) {
    let before = dynamic_launcher_generation();
    let records = dynamic_launcher_records();
    let after = dynamic_launcher_generation();
    // `before` is the generation of the bytes we intended to read; a changed
    // post-read generation is deliberately returned so consumers reject this
    // possibly mixed snapshot and schedule a fresh worker.
    (if before == after { before } else { 0 }, records)
}

impl DynamicLauncherService for ShellDynamicLauncherService {
    fn prepare_install(
        &self,
        request: &LauncherInstallRequest,
    ) -> PortalResult<LauncherInstallChoice> {
        if !valid_launcher_request(request) {
            return PortalResult::Failed("invalid dynamic launcher request".into());
        }
        let target_key = request.target.as_deref().unwrap_or(&request.name);
        let id = format!(
            "portal-{}",
            blake3::hash(
                format!("{}\0{}\0{}", request.app_id, request.name, target_key).as_bytes()
            )
            .to_hex()
        );
        let record = DynamicLauncherRecord {
            id,
            owner_app_id: request.app_id.clone(),
            name: request.name.trim().to_string(),
            launcher_type: request.launcher_type,
            target: request.target.clone(),
            icon_kind: request.icon_kind.clone(),
            icon_bytes: request.icon_bytes.clone(),
        };
        let Ok(mut records) = self.records.lock() else {
            return PortalResult::Unavailable("Tilo launcher library is unavailable".into());
        };
        let before = records.clone();
        records.retain(|existing| existing.id != record.id);
        records.push(record);
        records.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        if let Err(error) = self.persist(&records) {
            *records = before;
            return PortalResult::Failed(format!("could not save launcher: {error}"));
        }
        PortalResult::Success(LauncherInstallChoice {
            name: request.name.trim().to_string(),
        })
    }

    fn request_install_token(&self, _app_id: &str) -> PortalResult<()> {
        // Tilo completes installation during the reviewed PrepareInstall call;
        // there is no bearer token that another process could replay.
        PortalResult::Success(())
    }
}

fn valid_launcher_request(request: &LauncherInstallRequest) -> bool {
    let name = request.name.trim();
    !name.is_empty()
        && name.len() <= 255
        && !name.chars().any(char::is_control)
        && matches!(request.launcher_type, 1 | 2)
        && matches!(
            request.icon_kind.as_str(),
            "supplied" | "tilo-monogram" | "tilo-web"
        )
        && match request.icon_kind.as_str() {
            "supplied" => request
                .icon_bytes
                .as_ref()
                .is_some_and(|bytes| !bytes.is_empty() && bytes.len() <= 2 * 1024 * 1024),
            "tilo-monogram" | "tilo-web" => request.icon_bytes.is_none(),
            _ => false,
        }
        && match (request.launcher_type, request.target.as_deref()) {
            (1, None) => true,
            (2, Some(target)) => valid_web_target(target),
            _ => false,
        }
}

fn valid_web_target(target: &str) -> bool {
    target.len() <= 4096
        && target.trim() == target
        && !target.chars().any(char::is_control)
        && target
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmailRequest {
    pub app_id: String,
    pub workspace_id: crate::shell::state::WorkspaceId,
    pub addresses: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub attachments: Vec<String>,
    /// Canonical document-portal exports corresponding one-for-one with
    /// `attachments`. These never enter the mailto URI; they are used only to
    /// give the selected sandboxed mail handler an ephemeral read bind.
    pub attachment_paths: Vec<PathBuf>,
    pub activation_token: Option<String>,
}
pub trait EmailService: Send + Sync {
    fn compose(&self, request: &EmailRequest) -> PortalResult<()>;
}
#[derive(Clone)]
pub struct EmailServiceHandle(Arc<dyn EmailService>);
impl EmailServiceHandle {
    pub fn new(service: Arc<dyn EmailService>) -> Self {
        Self(service)
    }
    pub fn compose(&self, request: &EmailRequest) -> PortalResult<()> {
        self.0.compose(request)
    }
}
struct NoEmail;
impl EmailService for NoEmail {
    fn compose(&self, _: &EmailRequest) -> PortalResult<()> {
        PortalResult::Unavailable("no Tilo mail provider is configured".into())
    }
}

/// Opens the configured `mailto` provider using Tilo's normal signed-package
/// URI launcher. It never shells out. Attachments remain percent-encoded
/// document-portal `file:` URIs; the mail provider receives no raw host path
/// or decoded pathname to accidentally log or reinterpret.
#[derive(Clone)]
pub struct ShellEmailService {
    tree: crate::shell::state::WorkspaceTreeHandle,
}

impl ShellEmailService {
    pub fn new(tree: crate::shell::state::WorkspaceTreeHandle) -> Self {
        Self { tree }
    }
}

impl EmailService for ShellEmailService {
    fn compose(&self, request: &EmailRequest) -> PortalResult<()> {
        let Some((room, mut capabilities, defaults, user_id)) =
            self.tree.try_read().and_then(|tree| {
                let workspace = tree
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == request.workspace_id)?;
                Some((
                    workspace.room,
                    workspace.sandbox.clone(),
                    workspace.default_apps.clone(),
                    crate::shell::state::local_user_id_for_room(&workspace.room),
                ))
            })
        else {
            return PortalResult::Unavailable(
                "the requesting Tilo workspace is no longer available".into(),
            );
        };
        let Ok(resource) = desktop_policy::ResourceKind::scheme("mailto") else {
            return PortalResult::Unavailable("mailto is not a valid Tilo resource".into());
        };
        let Some(handler) = crate::shell::launcher::app_for_resource_with_policy(
            &resource,
            &user_id,
            Some(&defaults),
        ) else {
            return PortalResult::Unavailable("no Tilo mail provider is configured".into());
        };
        if !crate::shell::launcher::revalidate_resource_claim(&handler.package_id, &resource) {
            return PortalResult::Failed(
                "the configured mail provider is no longer available".into(),
            );
        }
        add_mail_attachment_grants(
            &mut capabilities,
            &handler.package_id,
            &request.attachment_paths,
        );
        let uri = mailto_uri(request);
        match crate::shell::launcher::launch_uri_in_room(
            &handler.package_id,
            room,
            capabilities,
            &uri,
        ) {
            Ok(_) => PortalResult::Success(()),
            Err(error) => PortalResult::Failed(format!("could not open mail provider: {error}")),
        }
    }
}

fn add_mail_attachment_grants(
    capabilities: &mut Vec<crate::shell::state::CapOverride>,
    handler_app_id: &str,
    attachment_paths: &[PathBuf],
) {
    use crate::shell::state::{Access, CapOverride, Expiry, Scope, Subject};
    capabilities.extend(attachment_paths.iter().map(|path| {
        CapOverride::grant(
            Subject::app(handler_app_id),
            Scope::FsPath(path.to_string_lossy().into_owned()),
            Access::Read,
        )
        .with_id(format!("portal-email:{}", uuid::Uuid::new_v4()))
        .expiring(Expiry::AfterUse)
    }));
}

fn mailto_uri(request: &EmailRequest) -> String {
    let mut query = Vec::new();
    push_mail_field(&mut query, "cc", &request.cc.join(","));
    push_mail_field(&mut query, "bcc", &request.bcc.join(","));
    if let Some(subject) = request.subject.as_deref() {
        push_mail_field(&mut query, "subject", subject);
    }
    if let Some(body) = request.body.as_deref() {
        push_mail_field(&mut query, "body", body);
    }
    for attachment in &request.attachments {
        // `attachment` is Tilo's mail-provider handoff field. It carries the
        // original document-portal URI as one opaque query value, rather than
        // turning an attachment into an ambient host filesystem path.
        push_mail_field(&mut query, "attachment", attachment);
    }
    let mut uri = format!("mailto:{}", percent_encode(&request.addresses.join(",")));
    if !query.is_empty() {
        uri.push('?');
        uri.push_str(&query.join("&"));
    }
    uri
}

fn push_mail_field(query: &mut Vec<String>, key: &str, value: &str) {
    if !value.is_empty() {
        query.push(format!("{key}={}", percent_encode(value)));
    }
}

fn percent_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, QUERY_VALUE).to_string()
}

/// Everything but the RFC 3986 unreserved characters and `@`.
const QUERY_VALUE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'@');

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenshotTarget {
    Screen,
    Window,
    Area,
    ActiveWindow,
}
impl ScreenshotTarget {
    pub const fn bit(self) -> u32 {
        match self {
            Self::Screen => 1,
            Self::Window => 2,
            Self::Area => 4,
            Self::ActiveWindow => 8,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenshotRequest {
    /// Implementation-side Request object path. Used to withdraw a live
    /// trusted selector when the frontend sends Request.Close.
    pub request_handle: String,
    /// Authenticated launcher attribution.  The native bridge uses it to
    /// publish the finished artifact into this app's document-store subtree.
    pub caller: super::PortalCaller,
    pub app_id: String,
    pub interactive: bool,
    pub target: Option<ScreenshotTarget>,
}
pub trait ScreenshotService: Send + Sync {
    fn available_targets(&self) -> u32;
    fn screenshot(&self, request: &ScreenshotRequest) -> PortalResult<String>;
    fn pick_color(&self, request_handle: &str, app_id: &str) -> PortalResult<(f64, f64, f64)>;
    fn cancel(&self, _request_handle: &str) {}
    /// Cancel a live colour picker without affecting an interactive screenshot
    /// selector that happens to use a different request handle.
    fn cancel_color(&self, request_handle: &str) {
        self.cancel(request_handle);
    }
}
#[derive(Clone)]
pub struct ScreenshotServiceHandle(Arc<dyn ScreenshotService>);
impl ScreenshotServiceHandle {
    pub fn new(service: Arc<dyn ScreenshotService>) -> Self {
        Self(service)
    }
    pub fn available_targets(&self) -> u32 {
        self.0.available_targets()
    }
    pub fn screenshot(&self, request: &ScreenshotRequest) -> PortalResult<String> {
        self.0.screenshot(request)
    }
    pub fn pick_color(&self, request_handle: &str, app_id: &str) -> PortalResult<(f64, f64, f64)> {
        self.0.pick_color(request_handle, app_id)
    }
    pub fn cancel(&self, request_handle: &str) {
        self.0.cancel(request_handle);
    }
    pub fn cancel_color(&self, request_handle: &str) {
        self.0.cancel_color(request_handle);
    }
}
struct NoScreenshot;
impl ScreenshotService for NoScreenshot {
    fn available_targets(&self) -> u32 {
        0
    }
    fn screenshot(&self, _: &ScreenshotRequest) -> PortalResult<String> {
        PortalResult::Unavailable("screenshot capture is not attached".into())
    }
    fn pick_color(&self, _: &str, _: &str) -> PortalResult<(f64, f64, f64)> {
        PortalResult::Unavailable("color picker is not attached".into())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PortalSetting {
    Unsigned(u32),
    Int(i32),
    Double(f64),
    Bool(bool),
    Color(f64, f64, f64),
    Text(String),
}
impl PortalSetting {
    pub fn to_owned_value(&self) -> zbus::zvariant::OwnedValue {
        match self {
            Self::Unsigned(v) => zbus::zvariant::OwnedValue::from(*v),
            Self::Int(v) => zbus::zvariant::OwnedValue::from(*v),
            Self::Double(v) => zbus::zvariant::OwnedValue::from(*v),
            Self::Bool(v) => zbus::zvariant::OwnedValue::from(*v),
            Self::Color(r, g, b) => {
                zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::new((*r, *g, *b)))
                    .unwrap_or_else(|_| zbus::zvariant::OwnedValue::from(0_u32))
            }
            Self::Text(v) => {
                zbus::zvariant::OwnedValue::from(zbus::zvariant::Str::from(v.as_str()))
            }
        }
    }
}
struct SettingsService {
    values: Mutex<BTreeMap<(String, String), PortalSetting>>,
    changes: tokio::sync::broadcast::Sender<(String, String, PortalSetting)>,
}

#[derive(Clone)]
pub struct SettingsServiceHandle(Arc<SettingsService>);
impl Default for SettingsServiceHandle {
    fn default() -> Self {
        // Every key exists from the first call: the dark default the shell
        // starts with, until the appearance monitor reads the settings file.
        let settings = super::appearance::LegacyAppearance::from_settings_text("", false)
            .portal_settings()
            .into_iter()
            .map(|(namespace, key, value)| ((namespace.to_owned(), key.to_owned()), value))
            .collect();
        let (changes, _) = tokio::sync::broadcast::channel(64);
        Self(Arc::new(SettingsService {
            values: Mutex::new(settings),
            changes,
        }))
    }
}
impl SettingsServiceHandle {
    pub fn get(&self, namespace: &str, key: &str) -> Option<PortalSetting> {
        self.0
            .values
            .lock()
            .ok()?
            .get(&(namespace.into(), key.into()))
            .cloned()
    }
    pub fn all(&self) -> BTreeMap<(String, String), PortalSetting> {
        self.0.values.lock().map(|v| v.clone()).unwrap_or_default()
    }
    pub fn set(&self, namespace: impl Into<String>, key: impl Into<String>, value: PortalSetting) {
        let namespace = namespace.into();
        let key = key.into();
        if let Ok(mut settings) = self.0.values.lock()
            && settings.get(&(namespace.clone(), key.clone())) != Some(&value)
        {
            settings.insert((namespace.clone(), key.clone()), value.clone());
            let _ = self.0.changes.send((namespace, key, value));
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<(String, String, PortalSetting)> {
        self.0.changes.subscribe()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallpaperRequest {
    pub app_id: String,
    /// Local workspace id authenticated from the launching process. This is
    /// used only to choose a local registry key; it is never replicated.
    pub workspace_id: String,
    pub uri: String,
    pub show_preview: bool,
    pub set_on: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedWallpaper {
    cache_file: String,
}
pub trait WallpaperService: Send + Sync {
    fn stage_wallpaper(&self, request: &WallpaperRequest) -> PortalResult<StagedWallpaper>;
    fn apply_staged_wallpaper(
        &self,
        request: &WallpaperRequest,
        staged: &StagedWallpaper,
    ) -> PortalResult<()>;
    fn discard_staged_wallpaper(&self, request: &WallpaperRequest, staged: &StagedWallpaper);
}
#[derive(Clone)]
pub struct WallpaperServiceHandle(Arc<dyn WallpaperService>);
impl WallpaperServiceHandle {
    pub fn new(service: Arc<dyn WallpaperService>) -> Self {
        Self(service)
    }
    pub fn stage_wallpaper(&self, request: &WallpaperRequest) -> PortalResult<StagedWallpaper> {
        self.0.stage_wallpaper(request)
    }
    pub fn apply_staged_wallpaper(
        &self,
        request: &WallpaperRequest,
        staged: &StagedWallpaper,
    ) -> PortalResult<()> {
        self.0.apply_staged_wallpaper(request, staged)
    }
    pub fn discard_staged_wallpaper(&self, request: &WallpaperRequest, staged: &StagedWallpaper) {
        self.0.discard_staged_wallpaper(request, staged);
    }
}
struct NoWallpaper;
impl WallpaperService for NoWallpaper {
    fn stage_wallpaper(&self, _: &WallpaperRequest) -> PortalResult<StagedWallpaper> {
        PortalResult::Unavailable("wallpaper service is not attached".into())
    }
    fn apply_staged_wallpaper(
        &self,
        _: &WallpaperRequest,
        _: &StagedWallpaper,
    ) -> PortalResult<()> {
        PortalResult::Unavailable("wallpaper service is not attached".into())
    }
    fn discard_staged_wallpaper(&self, _: &WallpaperRequest, _: &StagedWallpaper) {}
}

/// The `workspace_id` of a wallpaper shown on every workspace that has none of
/// its own.
pub const EVERY_WORKSPACE: &str = "*";

/// Which workspaces a newly applied wallpaper covers. A workspace is a unit of
/// collaboration, so a picture of its own tells at a glance which shared
/// context is on screen; the Appearance pane stores the choice as
/// `appearance.wallpaper_scope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallpaperScope {
    /// Only the workspace the request came from.
    Workspace,
    /// Every workspace, replacing any workspace's own picture.
    All,
}

impl WallpaperScope {
    /// Parse the persisted value; anything but `"all"` is this workspace.
    pub fn parse(value: Option<&str>) -> Self {
        if value == Some("all") {
            Self::All
        } else {
            Self::Workspace
        }
    }

    /// The scope the user chose in Settings.
    pub fn configured() -> Self {
        Self::parse(settings_store::str_at(
            &settings_store::load(),
            "appearance",
            "wallpaper_scope",
        ))
    }
}

/// Record `cache_file` as the wallpaper for `targets`, on `workspace_id` alone
/// or on every workspace.
pub fn place_wallpaper(
    records: &mut Vec<WallpaperRecord>,
    workspace_id: &str,
    scope: WallpaperScope,
    targets: &[&str],
    cache_file: &str,
) {
    let owner = match scope {
        WallpaperScope::Workspace => workspace_id,
        WallpaperScope::All => EVERY_WORKSPACE,
    };
    records.retain(|old| {
        let replaced = match scope {
            WallpaperScope::Workspace => old.workspace_id == workspace_id,
            WallpaperScope::All => true,
        };
        !(replaced && targets.iter().any(|target| old.target == *target))
    });
    records.extend(targets.iter().map(|target| WallpaperRecord {
        workspace_id: owner.to_string(),
        cache_file: cache_file.to_string(),
        target: (*target).into(),
    }));
}

/// The record a workspace shows for `target`: its own, else the one for every
/// workspace.
pub fn wallpaper_record_for<'a>(
    records: &'a [WallpaperRecord],
    workspace_id: &str,
    target: &str,
) -> Option<&'a WallpaperRecord> {
    let find = |owner: &str| {
        records
            .iter()
            .find(|record| record.workspace_id == owner && record.target == target)
    };
    find(workspace_id).or_else(|| find(EVERY_WORKSPACE))
}

/// Local-only wallpaper selection.  Collaborative workspace state must never
/// replicate a private file/document reference to peers.  The renderer reads
/// this private registry when drawing a workspace backdrop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallpaperRecord {
    pub workspace_id: String,
    /// Basename of a normalised PNG inside Tilo's private wallpaper cache.
    /// Never persist the caller's document URI: it may contain identifying
    /// document names and is valid only for the original portal grant.
    pub cache_file: String,
    /// One of `background` or `lockscreen`. `set-on=both` creates two
    /// records referencing the same cached image.
    pub target: String,
}

#[derive(Clone)]
pub struct ShellWallpaperService {
    tree: crate::shell::state::WorkspaceTreeHandle,
    records: Arc<Mutex<Vec<WallpaperRecord>>>,
    path: Arc<PathBuf>,
}

impl ShellWallpaperService {
    pub fn new(tree: crate::shell::state::WorkspaceTreeHandle) -> Self {
        Self::at_path(tree, portal_state_path("wallpapers.json"))
    }

    pub fn at_path(
        tree: crate::shell::state::WorkspaceTreeHandle,
        path: impl Into<PathBuf>,
    ) -> Self {
        let path = path.into();
        let records = tilo_files::native::atomic::json::load_or_default(&path);
        Self {
            tree,
            records: Arc::new(Mutex::new(records)),
            path: Arc::new(path),
        }
    }

    pub fn records(&self) -> Vec<WallpaperRecord> {
        self.records
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default()
    }

    fn cache_dir(&self) -> PathBuf {
        wallpaper_cache_dir(self.path.as_ref())
    }
}

impl WallpaperService for ShellWallpaperService {
    fn stage_wallpaper(&self, request: &WallpaperRequest) -> PortalResult<StagedWallpaper> {
        if !matches!(
            request.set_on.as_str(),
            "background" | "lockscreen" | "both"
        ) {
            return PortalResult::Failed("invalid wallpaper source".into());
        }
        let Some(workspace_id) = self.tree.try_read().and_then(|tree| {
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id.0.to_string() == request.workspace_id)
                .map(|workspace| workspace.id.0.to_string())
        }) else {
            return PortalResult::Failed("requesting workspace is no longer active".into());
        };
        let imported = match import_wallpaper(&request.uri, &self.cache_dir()) {
            Ok(file) => file,
            Err(error) => {
                return PortalResult::Failed(format!("could not import wallpaper: {error}"));
            }
        };
        if request.show_preview {
            set_wallpaper_preview(&workspace_id, &imported);
        }
        PortalResult::Success(StagedWallpaper {
            cache_file: imported,
        })
    }

    fn apply_staged_wallpaper(
        &self,
        request: &WallpaperRequest,
        staged: &StagedWallpaper,
    ) -> PortalResult<()> {
        let Some(workspace_id) = self.tree.try_read().and_then(|tree| {
            tree.desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.id.0.to_string() == request.workspace_id)
                .map(|workspace| workspace.id.0.to_string())
        }) else {
            self.discard_staged_wallpaper(request, staged);
            return PortalResult::Failed("requesting workspace is no longer active".into());
        };
        let targets: &[&str] = match request.set_on.as_str() {
            "background" => &["background"],
            "lockscreen" => &["lockscreen"],
            "both" => &["background", "lockscreen"],
            _ => return PortalResult::Failed("invalid wallpaper target".into()),
        };
        let scope = WallpaperScope::configured();
        let Ok(mut records) = self.records.lock() else {
            self.discard_staged_wallpaper(request, staged);
            return PortalResult::Unavailable("Tilo wallpaper registry is unavailable".into());
        };
        let before = records.clone();
        place_wallpaper(
            &mut records,
            &workspace_id,
            scope,
            targets,
            &staged.cache_file,
        );
        if let Err(error) = write_private_json(self.path.as_ref(), &*records) {
            *records = before;
            // We still hold `records`; do cleanup inline rather than calling
            // `discard_staged_wallpaper` and trying to lock the same mutex.
            clear_wallpaper_preview(&request.workspace_id, &staged.cache_file);
            if !records
                .iter()
                .any(|record| record.cache_file == staged.cache_file)
            {
                let _ = fs::remove_file(self.cache_dir().join(&staged.cache_file));
            }
            return PortalResult::Failed(format!("could not save wallpaper: {error}"));
        }
        cleanup_wallpaper_cache(&self.cache_dir(), &records);
        clear_wallpaper_preview(&workspace_id, &staged.cache_file);
        PortalResult::Success(())
    }

    fn discard_staged_wallpaper(&self, request: &WallpaperRequest, staged: &StagedWallpaper) {
        clear_wallpaper_preview(&request.workspace_id, &staged.cache_file);
        let in_use = self.records.lock().is_ok_and(|records| {
            records
                .iter()
                .any(|record| record.cache_file == staged.cache_file)
        });
        if !in_use {
            let _ = fs::remove_file(self.cache_dir().join(&staged.cache_file));
        }
    }
}

const MAX_WALLPAPER_SOURCE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_WALLPAPER_PIXELS: u64 = 16 * 1024 * 1024;

/// Resolve an xdg-document-portal export. The portal protocol uses ordinary
/// `file://` URIs; safety comes from requiring the canonical path to sit
/// below the per-user document-portal mount, not from inventing a new scheme.
pub(crate) fn document_portal_path_from_uri(uri: &str) -> Result<PathBuf, String> {
    document_portal_path_from_uri_at(uri, &document_portal_root())
}

fn document_portal_root() -> PathBuf {
    tilo_paths::xdg::runtime_dir()
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", nix_uid())))
        .join("doc")
}

fn nix_uid() -> u32 {
    // `id -u` is intentionally not spawned. Linux exposes the effective uid
    // through procfs; absent procfs the standard runtime-dir path below will
    // simply fail closed during canonicalisation.
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

fn document_portal_path_from_uri_at(uri: &str, root: &Path) -> Result<PathBuf, String> {
    if uri.len() > 8192 || uri.trim() != uri || uri.chars().any(char::is_control) {
        return Err("malformed document URI".into());
    }
    let rest = uri.strip_prefix("file://").ok_or_else(|| {
        "wallpaper must be a file URI exported by the document portal".to_string()
    })?;
    let encoded_path = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost/")
            .map(|path| &path[..])
            .ok_or_else(|| "file URI has a remote authority".to_string())?
    };
    let bytes = percent_decode_uri_path(encoded_path)?;
    if bytes.first() != Some(&b'/') || bytes.contains(&0) {
        return Err("file URI path is not absolute".into());
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("file URI may not contain parent traversal".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "document portal is unavailable".to_string())?;
    let canonical = path
        .canonicalize()
        .map_err(|_| "exported document no longer exists".to_string())?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err("file URI is not a document-portal export".into());
    }
    Ok(canonical)
}

fn percent_decode_uri_path(value: &str) -> Result<Vec<u8>, String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes
                .get(index + 1)
                .ok_or_else(|| "truncated URI escape".to_string())?;
            let low = *bytes
                .get(index + 2)
                .ok_or_else(|| "truncated URI escape".to_string())?;
            let nibble = |byte: u8| match byte {
                b'0'..=b'9' => Some(byte - b'0'),
                b'a'..=b'f' => Some(byte - b'a' + 10),
                b'A'..=b'F' => Some(byte - b'A' + 10),
                _ => None,
            };
            let high = nibble(high).ok_or_else(|| "invalid URI escape".to_string())?;
            let low = nibble(low).ok_or_else(|| "invalid URI escape".to_string())?;
            output.push((high << 4) | low);
            index += 3;
        } else {
            if !bytes[index].is_ascii() {
                return Err("file URI must percent-encode non-ASCII bytes".into());
            }
            output.push(bytes[index]);
            index += 1;
        }
    }
    Ok(output)
}

fn wallpaper_cache_dir(state_path: &Path) -> PathBuf {
    state_path
        .parent()
        .unwrap_or_else(|| Path::new("/dev/null"))
        .join("wallpaper-cache")
}

#[derive(Clone, Default)]
struct WallpaperPreview {
    workspace_id: String,
    cache_file: String,
}

fn wallpaper_preview() -> &'static Mutex<Option<WallpaperPreview>> {
    static PREVIEW: std::sync::OnceLock<Mutex<Option<WallpaperPreview>>> =
        std::sync::OnceLock::new();
    PREVIEW.get_or_init(|| Mutex::new(None))
}

fn set_wallpaper_preview(workspace_id: &str, cache_file: &str) {
    if let Ok(mut preview) = wallpaper_preview().lock() {
        *preview = Some(WallpaperPreview {
            workspace_id: workspace_id.into(),
            cache_file: cache_file.into(),
        });
    }
}

fn clear_wallpaper_preview(workspace_id: &str, cache_file: &str) {
    if let Ok(mut preview) = wallpaper_preview().lock()
        && preview.as_ref().is_some_and(|preview| {
            preview.workspace_id == workspace_id && preview.cache_file == cache_file
        })
    {
        *preview = None;
    }
}

/// An active, uncommitted wallpaper preview. It exists only while the shell
/// consent sheet is visible, and contains a private cache basename rather
/// than the caller's document URI.
pub(crate) fn wallpaper_preview_file_for_workspace(workspace_id: &str) -> Option<PathBuf> {
    let preview = wallpaper_preview().lock().ok()?.clone()?;
    (preview.workspace_id == workspace_id && safe_wallpaper_cache_name(&preview.cache_file))
        .then(|| wallpaper_cache_dir(&wallpaper_state_path()).join(preview.cache_file))
}

pub(crate) fn wallpaper_cache_dir_for_renderer(state_path: &Path) -> PathBuf {
    wallpaper_cache_dir(state_path)
}

fn import_wallpaper(uri: &str, cache: &Path) -> Result<String, String> {
    let source = document_portal_path_from_uri(uri)?;
    let metadata = fs::metadata(&source).map_err(|error| error.to_string())?;
    if metadata.len() > MAX_WALLPAPER_SOURCE_BYTES {
        return Err("image exceeds the 32 MiB import limit".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    OpenOptions::new()
        .read(true)
        .open(&source)
        .map_err(|error| error.to_string())?
        .take(MAX_WALLPAPER_SOURCE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_WALLPAPER_SOURCE_BYTES {
        return Err("image exceeds the 32 MiB import limit".into());
    }
    let normalized = normalise_wallpaper(&bytes)?;
    let file = format!("{}.png", blake3::hash(&normalized).to_hex());
    let destination = cache.join(&file);
    if !destination.exists() {
        write_private_bytes(&destination, &normalized)?;
    }
    Ok(file)
}

fn normalise_wallpaper(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader = image::ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|error| format!("could not identify image: {error}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(MAX_WALLPAPER_PIXELS.saturating_mul(8));
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| format!("could not decode PNG, JPEG, or WebP image: {error}"))?;
    let (width, height) = (image.width(), image.height());
    let pixel_count = u64::from(width) * u64::from(height);
    if pixel_count == 0 || pixel_count > MAX_WALLPAPER_PIXELS {
        return Err("image dimensions are unsupported".into());
    }
    let rgba = image.into_rgba8().into_raw();
    ui::png::encode_rgba8(width, height, &rgba).map_err(|error| error.to_string())
}

fn write_private_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    tilo_files::native::atomic::write_private(path, bytes).map_err(|error| error.to_string())
}

fn cleanup_wallpaper_cache(cache: &Path, records: &[WallpaperRecord]) {
    let live: std::collections::BTreeSet<&str> = records
        .iter()
        .map(|record| record.cache_file.as_str())
        .collect();
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_str().is_none_or(|name| !live.contains(name)) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn safe_wallpaper_cache_name(name: &str) -> bool {
    name.len() == 68
        && name.ends_with(".png")
        && name.as_bytes()[..64].iter().all(u8::is_ascii_hexdigit)
}

fn portal_state_path(file: &str) -> PathBuf {
    let base = super::private_state_root().unwrap_or_else(|| PathBuf::from("/dev/null"));
    base.join("tilo/portal").join(file)
}

pub(crate) fn wallpaper_state_path() -> PathBuf {
    portal_state_path("wallpapers.json")
}

fn write_private_json(path: &Path, value: &(impl Serialize + ?Sized)) -> Result<(), String> {
    tilo_files::native::atomic::json::write_private(path, value).map_err(|error| error.to_string())
}

/// All application-facing desktop capabilities.  This is cloneable by design:
/// each zbus object owns a snapshot of handles while their implementations
/// remain shared with the shell.
#[derive(Clone)]
pub struct DesktopPortalServices {
    pub account: AccountServiceHandle,
    pub app_chooser: AppChooserServiceHandle,
    pub dynamic_launcher: DynamicLauncherServiceHandle,
    pub email: EmailServiceHandle,
    pub screenshot: ScreenshotServiceHandle,
    pub settings: SettingsServiceHandle,
    pub wallpaper: WallpaperServiceHandle,
}
impl Default for DesktopPortalServices {
    fn default() -> Self {
        Self {
            account: AccountServiceHandle::new(Arc::new(NoAccount)),
            app_chooser: AppChooserServiceHandle::new(Arc::new(
                crate::shell::portal_dialog::ShellAppChooserService::global(),
            )),
            dynamic_launcher: DynamicLauncherServiceHandle::new(Arc::new(NoDynamicLauncher)),
            email: EmailServiceHandle::new(Arc::new(NoEmail)),
            screenshot: ScreenshotServiceHandle::new(Arc::new(NoScreenshot)),
            settings: SettingsServiceHandle::default(),
            wallpaper: WallpaperServiceHandle::new(Arc::new(NoWallpaper)),
        }
    }
}

impl DesktopPortalServices {
    /// Services that can operate directly from durable shell state. UI-backed
    /// adapters are attached separately by `OverlayHost` when its controller
    /// is available.
    pub fn for_session(tree: crate::shell::state::WorkspaceTreeHandle) -> Self {
        let mut services = Self::default();
        services.account = AccountServiceHandle::new(Arc::new(WorkspaceAccount {
            grants: super::GrantStoreHandle::new(tree.clone()),
        }));
        services.dynamic_launcher =
            DynamicLauncherServiceHandle::new(Arc::new(ShellDynamicLauncherService::for_session()));
        services.email = EmailServiceHandle::new(Arc::new(ShellEmailService::new(tree.clone())));
        services.wallpaper =
            WallpaperServiceHandle::new(Arc::new(ShellWallpaperService::new(tree)));
        services
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Access, Expiry, Scope, Subject};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn mail_handoff_encodes_fields_and_grants_attachments_once() {
        // Phase `mailto_encoding_cannot_create_an_extra_field`.
        {
            let uri = mailto_uri(&EmailRequest {
                addresses: vec!["a@example.test".into()],
                subject: Some("hello&bcc=other@example.test".into()),
                body: Some("line one\nline two".into()),
                ..EmailRequest::default()
            });
            assert_eq!(
                uri,
                "mailto:a@example.test?subject=hello%26bcc%3Dother@example.test&body=line%20one%0Aline%20two"
            );
        }
        // Phase `mail_attachment_is_one_opaque_uri_query_value`.
        {
            let uri = mailto_uri(&EmailRequest {
                attachments: vec!["file:///run/user/1000/doc/token/a%20%26%20b.pdf".into()],
                ..EmailRequest::default()
            });
            assert_eq!(
                uri,
                "mailto:?attachment=file%3A%2F%2F%2Frun%2Fuser%2F1000%2Fdoc%2Ftoken%2Fa%2520%2526%2520b.pdf"
            );
            assert!(!uri.contains("&b.pdf"));
        }
        // Phase `mail_attachments_become_one_shot_read_grants_for_the_handler_only`.
        {
            let mut capabilities = Vec::new();
            add_mail_attachment_grants(
                &mut capabilities,
                "so.tilo.mail",
                &[PathBuf::from("/run/user/1000/doc/abc/report.pdf")],
            );
            assert_eq!(capabilities.len(), 1);
            let grant = &capabilities[0];
            assert_eq!(grant.subject, Subject::app("so.tilo.mail"));
            assert_eq!(
                grant.scope,
                Scope::FsPath("/run/user/1000/doc/abc/report.pdf".into())
            );
            assert_eq!(grant.access, Access::Read);
            assert_eq!(grant.expiry, Expiry::AfterUse);
            assert!(grant.id.starts_with("portal-email:"));
        }
    }

    #[test]
    fn launcher_and_document_data_is_durable_and_confined() {
        // Phase `dynamic_library_is_non_executable_and_durable`.
        {
            let dir = tempfile::tempdir().expect("temporary state directory");
            let path = dir.path().join("launchers.json");
            let service = ShellDynamicLauncherService::at_path(&path);
            let request = LauncherInstallRequest {
                app_id: "org.example.Caller".into(),
                name: "Example site".into(),
                launcher_type: 2,
                target: Some("https://example.test/".into()),
                editable_name: true,
                editable_icon: false,
                icon_kind: "tilo-web".into(),
                icon_bytes: None,
            };
            assert!(matches!(
                service.prepare_install(&request),
                PortalResult::Success(_)
            ));
            let records = ShellDynamicLauncherService::at_path(&path).records();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].target.as_deref(), Some("https://example.test/"));
            assert_eq!(records[0].icon_kind, "tilo-web");
            assert!(records[0].icon_bytes.is_none());
            assert!(!std::fs::read_to_string(&path).unwrap().contains("Exec="));
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        // Phase `supplied_launcher_icon_is_durable_data_not_a_path`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("launchers.json");
            let service = ShellDynamicLauncherService::at_path(&path);
            let icon = b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>".to_vec();
            let request = LauncherInstallRequest {
                app_id: "org.example.Caller".into(),
                name: "Reviewed launcher".into(),
                launcher_type: 1,
                target: None,
                editable_name: false,
                editable_icon: false,
                icon_kind: "supplied".into(),
                icon_bytes: Some(icon.clone()),
            };
            assert!(matches!(
                service.prepare_install(&request),
                PortalResult::Success(_)
            ));
            let restored = ShellDynamicLauncherService::at_path(path).records();
            assert_eq!(restored[0].icon_bytes.as_deref(), Some(icon.as_slice()));
            assert!(
                !std::fs::read_to_string(dir.path().join("launchers.json"))
                    .unwrap()
                    .contains("/home/")
            );
        }
        // Phase `document_uri_is_confined_to_the_document_portal_mount`.
        {
            let root = tempfile::tempdir().unwrap();
            let exported = root.path().join("grant/a b.png");
            fs::create_dir_all(exported.parent().unwrap()).unwrap();
            fs::write(&exported, b"png").unwrap();
            let uri = format!("file://{}", exported.display()).replace(' ', "%20");
            assert_eq!(
                document_portal_path_from_uri_at(&uri, root.path()).unwrap(),
                exported.canonicalize().unwrap()
            );
            assert!(document_portal_path_from_uri_at("file:///etc/passwd", root.path()).is_err());
            assert!(document_portal_path_from_uri_at("file://remote/path", root.path()).is_err());
        }
    }

    #[test]
    fn wallpaper_imports_are_normalised_private_and_scoped() {
        // Phase `imported_wallpaper_is_normalised_and_private`.
        {
            use image::ImageEncoder as _;
            let mut original = Vec::new();
            image::codecs::png::PngEncoder::new(&mut original)
                .write_image(&[12, 34, 56], 1, 1, image::ExtendedColorType::Rgb8)
                .unwrap();
            let normalized = normalise_wallpaper(&original).unwrap();
            let decoder =
                image::codecs::png::PngDecoder::new(std::io::Cursor::new(&normalized)).unwrap();
            assert_eq!(
                image::ImageDecoder::color_type(&decoder),
                image::ColorType::Rgba8
            );
            assert_eq!(
                ui::png::decode_rgba8(&normalized).unwrap().rgba,
                [12, 34, 56, 255]
            );

            let cache = tempfile::tempdir().unwrap();
            let file = cache.path().join("private.png");
            write_private_bytes(&file, b"private").unwrap();
            assert_eq!(fs::metadata(file).unwrap().permissions().mode() & 0o077, 0);
        }
        // Phase `wallpaper_preview_is_scoped_and_cancelled_without_a_persistent_uri`.
        {
            let file = format!("{}.png", "a".repeat(64));
            set_wallpaper_preview("preview-workspace", &file);
            assert!(wallpaper_preview_file_for_workspace("preview-workspace").is_some());
            assert!(wallpaper_preview_file_for_workspace("other-workspace").is_none());
            clear_wallpaper_preview("preview-workspace", &file);
            assert!(wallpaper_preview_file_for_workspace("preview-workspace").is_none());
        }
        // Phase `wallpaper_normaliser_accepts_common_desktop_formats`.
        {
            let source = image::DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(1, 1, vec![12, 34, 56, 255]).unwrap(),
            );
            for format in [image::ImageFormat::Jpeg, image::ImageFormat::WebP] {
                let mut encoded = std::io::Cursor::new(Vec::new());
                source.write_to(&mut encoded, format).unwrap();
                let normalized = normalise_wallpaper(encoded.get_ref()).unwrap();
                assert!(normalized.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]));
            }
        }
    }

    #[test]
    fn a_wallpaper_for_one_workspace_leaves_the_others_alone() {
        let hash = |c: char| format!("{}.png", c.to_string().repeat(64));
        let mut records = Vec::new();
        place_wallpaper(
            &mut records,
            "a",
            WallpaperScope::All,
            &["background"],
            &hash('1'),
        );
        place_wallpaper(
            &mut records,
            "b",
            WallpaperScope::Workspace,
            &["background"],
            &hash('2'),
        );
        assert_eq!(
            wallpaper_record_for(&records, "b", "background").map(|r| r.cache_file.clone()),
            Some(hash('2')),
            "a workspace's own picture tells you which shared context you are in"
        );
        assert_eq!(
            wallpaper_record_for(&records, "a", "background").map(|r| r.cache_file.clone()),
            Some(hash('1')),
            "the rest show the picture chosen for every workspace"
        );
        assert!(wallpaper_record_for(&records, "a", "lockscreen").is_none());

        place_wallpaper(
            &mut records,
            "c",
            WallpaperScope::All,
            &["background"],
            &hash('3'),
        );
        assert_eq!(
            records.len(),
            1,
            "every workspace replaces the per-workspace pictures"
        );
        assert_eq!(
            wallpaper_record_for(&records, "b", "background").map(|r| r.cache_file.clone()),
            Some(hash('3'))
        );
        assert_eq!(WallpaperScope::parse(Some("all")), WallpaperScope::All);
        assert_eq!(WallpaperScope::parse(None), WallpaperScope::Workspace);
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u16, u16)> {
    if !bytes.starts_with(&[0xff, 0xd8]) {
        return None;
    }
    let mut index: usize = 2;
    while index.saturating_add(9) <= bytes.len() {
        if bytes[index] != 0xff {
            return None;
        }
        while bytes.get(index) == Some(&0xff) {
            index += 1;
        }
        let marker = *bytes.get(index)?;
        index += 1;
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([
            *bytes.get(index)?,
            *bytes.get(index + 1)?,
        ]));
        if length < 7 || index.saturating_add(length) > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            return Some((
                u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]),
                u16::from_be_bytes([bytes[index + 3], bytes[index + 4]]),
            ));
        }
        index += length;
    }
    None
}
pub(crate) fn valid_icon_payload(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 {
        return false;
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return (1..=512).contains(&width) && (1..=512).contains(&height);
    }
    if let Some((width, height)) = jpeg_dimensions(&bytes) {
        return (1..=512).contains(&u32::from(width)) && (1..=512).contains(&u32::from(height));
    }
    if bytes.len() > 256 * 1024 {
        return false;
    }
    let Ok(svg) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let lower = svg.to_ascii_lowercase();
    lower.trim_start().starts_with("<svg")
        && ![
            "<script",
            "<foreignobject",
            "href=\"http:",
            "href='http:",
            "href=\"file:",
            "href='file:",
            "href=\"data:",
            "href='data:",
            "url(http",
            "url(file",
            "url(data",
        ]
        .iter()
        .any(|forbidden| lower.contains(forbidden))
}
