//! Personal Privacy & Security pane. Device posture is read from the real
//! kernel/session state; workspace grants remain a read-only cross-workspace
//! summary because their authority and mutation UI belong to the contextual
//! Workspace Settings surface.
//!
//! The compositor writes every workspace's granted capabilities to
//! `~/.config/tilo/state.json` (its `WorkspaceTree`); the FUSE mount and the
//! launcher sandbox enforce exactly that list. This pane reads that same file
//! and renders the live grants — which app in which workspace holds camera,
//! microphone, screen-sharing, file, or network access, and for how long —
//! using the grants enforced by the compositor.
//!
//! It is deliberately **read-only**. Granting happens at the compositor's
//! consent prompt (an app reaching outside what it owns) and revoking happens
//! in the workspace that owns the grant; this pane does not write
//! `state.json`, so it ships no buttons that would look live but change
//! nothing. When there are no grants — a fresh session, or one where no app
//! has been approved for anything — it shows an empty state.

use ui::tokens::SpacingStep;

use crate::i18n::{t, t_args};
use crate::panes::model::{ModelPane, PaneModel, Shared};
use crate::widgets::{
    El, StatusTone, caption, card, cards, error_row, gap, rows, semantic_status_pill, value_row,
};
use collab_types::caps::{CapOverride, Effect, Expiry, Principal, Scope, Subject, Verb};
use serde::Deserialize;
use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource;
use ui::widget::{Button, Row, Spacer, Text, Widget};

use crate::host::PaneCx;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

/// Where Privacy & Security sends the person to change what it reports.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum PermissionsEffect {
    /// Open the privacy settings of the workspace that owns the grants.
    #[effect(job, label = "Open workspace privacy")]
    OpenWorkspacePrivacy,
}

/// Answer `effect` by opening the tool it names.
#[expect(
    clippy::needless_pass_by_value,
    reason = "every area answers through the effect vocabulary's one signature"
)]
pub(crate) fn answer(
    app: &SettingsApp,
    effect: PermissionsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        PermissionsEffect::OpenWorkspacePrivacy => {
            if !app.host().reaches_devices() {
                return Err("only a window someone uses opens workspace settings".into());
            }
            Ok(cx.thread(
                move |_| {
                    os_actions::open_tool(os_actions::ToolIntent::WorkspaceSettings {
                        route: os_actions::WorkspaceSettingsRoute::PrivacySecurity,
                    })
                },
                |_: &mut SettingsApp, opened: Result<(), String>, _| {
                    opened.map(|()| Effected::done())
                },
            ))
        }
    }
}

struct PrivacySnapshot {
    privacy: os_actions::PrivacyStatus,
    security: DeviceSecurity,
    workspace_state: Option<String>,
}

#[cfg(test)]
impl PrivacySnapshot {
    fn unavailable() -> Self {
        Self {
            privacy: os_actions::PrivacyStatus::default(),
            security: DeviceSecurity {
                disk_encryption: "perm-unavailable",
                secure_boot: "perm-unavailable",
                firewall: Err("Device security could not be observed.".into()),
            },
            workspace_state: None,
        }
    }
}

trait PrivacyControl: Send + Sync {
    fn read(&self) -> Result<PrivacySnapshot, String>;
}

struct DevicePrivacy;
impl PrivacyControl for DevicePrivacy {
    fn read(&self) -> Result<PrivacySnapshot, String> {
        Ok(PrivacySnapshot {
            privacy: os_actions::get_privacy_status_checked()?,
            security: device_security(),
            workspace_state: load_text(),
        })
    }
}

pub(crate) struct PrivacyArea {
    cx: PaneCx,
    control: Option<Arc<dyn PrivacyControl>>,
    model: OnceCell<Shared<PrivacyModel>>,
}

impl PrivacyArea {
    pub(crate) fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            control: cx
                .reaches_devices()
                .then(|| Arc::new(DevicePrivacy) as Arc<dyn PrivacyControl>),
            model: OnceCell::new(),
        }
    }

    pub(crate) fn pane(&self) -> Box<dyn Widget> {
        let model = self.model.get_or_init(|| {
            Rc::new(RefCell::new(PrivacyModel::new(
                self.cx.clone(),
                self.control.clone(),
            )))
        });
        Box::new(ModelPane::new(Rc::clone(model)))
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().set_visible(visible);
        }
    }

    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_read();
        }
    }

    #[cfg(test)]
    fn with_control(cx: &PaneCx, control: Arc<dyn PrivacyControl>) -> Self {
        Self {
            cx: cx.clone(),
            control: Some(control),
            model: OnceCell::new(),
        }
    }
}

struct PrivacyModel {
    cx: PaneCx,
    snapshot: Option<Result<PrivacySnapshot, String>>,
    reading: Option<std::sync::mpsc::Receiver<Result<PrivacySnapshot, String>>>,
    changed: bool,
    control: Option<Arc<dyn PrivacyControl>>,
    visible: bool,
    refresh_at: Option<ui::time::Instant>,
}

impl PrivacyModel {
    fn new(cx: PaneCx, control: Option<Arc<dyn PrivacyControl>>) -> Self {
        let mut model = Self {
            cx,
            snapshot: None,
            reading: None,
            changed: false,
            control: None,
            visible: true,
            refresh_at: None,
        };
        model.control = control;
        model.start_read();
        model
    }

    fn set_visible(&mut self, visible: bool) {
        // Selecting the same pane can reopen it from the narrow category list.
        // It is still an explicit observation request, not a retained relayout.
        self.visible = visible;
        self.refresh_at = None;
        if visible {
            self.start_read();
        }
    }

    fn start_read(&mut self) {
        if self.reading.is_some() {
            return;
        }
        let Some(control) = self.control.clone() else {
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let wake = ui::WakeHandle::new();
        self.refresh_at = None;
        self.changed = true;
        match ui::background::Background::current().spawn("privacy_snapshot", move || {
            if sender.send(control.read()).is_ok() {
                wake.wake();
            }
        }) {
            Ok(()) => self.reading = Some(receiver),
            Err(error) => {
                self.snapshot = Some(Err(error.to_string()));
                self.refresh_at = self
                    .visible
                    .then(|| ui::time::now() + Duration::from_secs(1));
            }
        }
    }

    fn poll_read(&mut self) {
        let Some(receiver) = &self.reading else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(t("perm-read-disconnected")),
        };
        self.reading = None;
        self.snapshot = Some(result);
        self.refresh_at = self
            .visible
            .then(|| ui::time::now() + Duration::from_secs(1));
        self.changed = true;
    }
}

impl PaneModel for PrivacyModel {
    fn compose(&self) -> Box<dyn Widget> {
        match &self.snapshot {
            Some(Ok(snapshot)) if self.reading.is_some() => cards(vec![
                caption(&t("perm-read-refreshing")),
                build_observed_view(&self.cx, snapshot),
            ]),
            Some(Ok(snapshot)) => build_observed_view(&self.cx, snapshot),
            Some(Err(error)) => cards(vec![card(
                &t("perm-this-device"),
                rows(vec![caption(&t_args(
                    "perm-read-error",
                    &[("error", error.as_str().into())],
                ))]),
            )]),
            None => cards(vec![card(
                &t("perm-this-device"),
                rows(vec![caption(&t(if self.reading.is_some() {
                    "perm-read-loading"
                } else {
                    "perm-read-preview"
                }))]),
            )]),
        }
    }
    fn poll(&mut self) -> bool {
        self.poll_read();
        if self.visible && self.refresh_at.is_some_and(|at| ui::time::now() >= at) {
            self.start_read();
        }
        std::mem::take(&mut self.changed)
    }
    fn name(&self) -> &'static str {
        "PrivacyPane"
    }
    fn next_wake(&self) -> Option<ui::time::Instant> {
        self.visible.then_some(self.refresh_at).flatten()
    }
}

/// The compositor's persisted workspace tree, beside `settings.json`.
fn state_path() -> Option<PathBuf> {
    settings_store::config_path(std::path::Path::new("state.json"))
}

fn load_text() -> Option<String> {
    std::fs::read_to_string(state_path()?).ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DeviceSecurity {
    disk_encryption: &'static str,
    secure_boot: &'static str,
    /// The firewall in force, or why that could not be read.
    firewall: Result<&'static str, String>,
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    command_output(program, args).ok()
}

/// `program args…`'s trimmed standard output, or why it gave none.
fn command_output(program: &str, args: &[&str]) -> Result<String, String> {
    let out = bounded_command::run(program, args, None, 1024 * 1024, Duration::from_secs(5))
        .map_err(|error| format!("{program}: {error}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let said = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|text| !text.is_empty())
            .map_or_else(|| format!("exited with {}", out.status), str::to_string);
        Err(format!("{program}: {said}"))
    }
}

/// `lsblk` reports every ancestor of `/`; any `crypt` layer means the live
/// root filesystem is encrypted. An unavailable probe stays unknown rather
/// than guessing from a partition name.
fn disk_encryption_status() -> &'static str {
    let Some(source) = command_stdout("findmnt", &["-no", "SOURCE", "/"]) else {
        return "perm-unavailable";
    };
    let Some(chain) = command_stdout("lsblk", &["-s", "-n", "-o", "TYPE", &source]) else {
        return "perm-unavailable";
    };
    if chain.lines().any(|line| line.trim() == "crypt") {
        "perm-encrypted"
    } else {
        "perm-not-encrypted"
    }
}

fn secure_boot_status() -> &'static str {
    let Ok(entries) = std::fs::read_dir("/sys/firmware/efi/efivars") else {
        return "perm-unavailable";
    };
    let Some(path) = entries.flatten().map(|e| e.path()).find(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("SecureBoot-"))
    }) else {
        return "perm-disabled";
    };
    // EFI variable files start with four attribute bytes, followed by the
    // one-byte SecureBoot value.
    match std::fs::read(path).ok().and_then(|v| v.get(4).copied()) {
        Some(1) => "perm-enabled",
        Some(_) => "perm-disabled",
        None => "perm-unavailable",
    }
}

/// firewalld when it runs, otherwise the nftables ruleset. "Not active" is
/// only said when the ruleset was read and is empty; a ruleset that could
/// not be read (it needs privileges) is reported as unreadable.
fn firewall_status() -> Result<&'static str, String> {
    firewall_from(command_output("firewall-cmd", &["--state"]), || {
        command_output("nft", &["list", "ruleset"])
    })
}

fn firewall_from(
    firewalld: Result<String, String>,
    nft: impl FnOnce() -> Result<String, String>,
) -> Result<&'static str, String> {
    if firewalld.as_deref() == Ok("running") {
        return Ok("perm-firewall-firewalld");
    }
    match nft() {
        Ok(ruleset) if !ruleset.trim().is_empty() => Ok("perm-firewall-nftables"),
        Ok(_) => Ok("perm-firewall-off"),
        Err(nft_error) => Err(match firewalld {
            Err(firewalld_error) => format!("{firewalld_error}; {nft_error}"),
            Ok(_) => nft_error,
        }),
    }
}

fn device_security() -> DeviceSecurity {
    DeviceSecurity {
        disk_encryption: disk_encryption_status(),
        secure_boot: secure_boot_status(),
        firewall: firewall_status(),
    }
}

fn kill_label(state: os_actions::KillState, present: bool) -> String {
    use os_actions::KillState;
    match state {
        KillState::HardwareBlocked => t("perm-kill-hw"),
        KillState::SoftwareBlocked => t("perm-kill-sw"),
        KillState::Allowed => t("perm-kill-allowed"),
        KillState::Unavailable if present => t("perm-kill-present"),
        KillState::Unavailable => t("perm-kill-missing"),
    }
}

/// Minimal projection of `state.json`: only the fields this pane reads. Serde
/// ignores every other workspace field, and each `capabilities` entry is the
/// real [`CapOverride`] the compositor serialised, so the parse tracks the
/// enforced shape rather than a copy of it.
#[derive(Deserialize, Default)]
struct TreeView {
    #[serde(default)]
    workspaces: Vec<WsView>,
}

#[derive(Deserialize, Default)]
struct WsView {
    #[serde(default)]
    name: String,
    #[serde(default)]
    capabilities: Vec<CapOverride>,
}

/// Which tint a capability reads in — kept separate from the rendered `Color`
/// so the projection is testable without a `Theme`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tint {
    Files,
    Camera,
    Microphone,
    Screen,
    Remote,
    Network,
    AppAction,
}

impl Tint {
    fn tone(self) -> StatusTone {
        match self {
            Tint::Files | Tint::Screen | Tint::AppAction => StatusTone::Info,
            Tint::Camera | Tint::Microphone | Tint::Remote => StatusTone::Warning,
            Tint::Network => StatusTone::Success,
        }
    }
}

/// One capability an app holds, projected into display terms.
struct CapView {
    kind: &'static str,
    detail: String,
    expiry: &'static str,
    tint: Tint,
}

/// One app's grants within a workspace.
struct AppGrants {
    app: String,
    caps: Vec<CapView>,
}

/// One workspace and the app grants it holds.
struct WsGrants {
    name: String,
    apps: Vec<AppGrants>,
}

/// Human-readable expiry, mirroring the compositor prompt's three lifetimes.
fn expiry_label(e: Expiry) -> &'static str {
    match e {
        Expiry::Never => "Always",
        Expiry::AtUnix(_) => "Temporary",
        Expiry::EndOfSession => "Until log out",
        Expiry::AfterUse => "Once",
    }
}

fn expiry_shown(english: &str) -> String {
    t(match english {
        "Temporary" => "perm-expiry-temp",
        "Until log out" => "perm-expiry-logout",
        "Once" => "perm-expiry-once",
        _ => "perm-expiry-always",
    })
}

fn kind_shown(english: &str) -> String {
    t(match english {
        "Files" => "perm-kind-files",
        "Camera" => "perm-kind-camera",
        "Microphone" => "perm-kind-mic",
        "Screen sharing" => "perm-kind-screen",
        "Remote control" => "perm-kind-remote",
        "Network" => "perm-kind-network",
        _ => "perm-kind-action",
    })
}

/// Project one stored capability into a display capability, or `None` when it
/// is not an app grant this pane renders (role/room/resource overrides and
/// restrictions are enforcement plumbing, not app permissions).
fn cap_view(cap: &CapOverride) -> Option<CapView> {
    if cap.effect != Effect::Grant {
        return None;
    }
    let expiry = expiry_label(cap.expiry);
    let (kind, detail, tint) = match &cap.scope {
        Scope::FsPath(p) => {
            let verb = if cap.verb.implies(Verb::Write) {
                t("perm-verb-rw")
            } else {
                t("perm-verb-ro")
            };
            ("Files", format!("{verb}: {p}"), Tint::Files)
        }
        Scope::Device(d) if d == "camera" => ("Camera", t("perm-capture-video"), Tint::Camera),
        Scope::Device(d) if d == "microphone" => {
            ("Microphone", t("perm-capture-audio"), Tint::Microphone)
        }
        Scope::Device(d) if d == "screencast" => {
            ("Screen sharing", t("perm-stream-workspace"), Tint::Screen)
        }
        Scope::Device(d) if d == "remote-desktop" => {
            ("Remote control", t("perm-inject-input"), Tint::Remote)
        }
        Scope::NetHost(h) => {
            let detail = if h == "*" {
                t("perm-any-host")
            } else {
                h.clone()
            };
            ("Network", detail, Tint::Network)
        }
        // Another app's action this app may use: the consent sheet grants
        // these; the workspace's own Settings lists them under App actions.
        Scope::Action { app, action } => (
            "App action",
            t_args(
                "perm-action-detail",
                &[
                    ("action", action.as_str().into()),
                    ("app", app.as_str().into()),
                ],
            ),
            Tint::AppAction,
        ),
        _ => return None,
    };
    Some(CapView {
        kind,
        detail,
        expiry,
        tint,
    })
}

/// Project the whole state document into per-workspace, per-app grants,
/// keeping only workspaces that actually hold app grants. Stable ordering
/// (workspaces as stored; apps and their caps sorted) so the pane is
/// deterministic.
fn project(state_json: &str) -> Result<Vec<WsGrants>, serde_json::Error> {
    let tree: TreeView = serde_json::from_str(state_json)?;
    let mut out = Vec::new();
    for ws in tree.workspaces {
        // Bucket this workspace's app grants by app id.
        let mut by_app: std::collections::BTreeMap<String, Vec<CapView>> =
            std::collections::BTreeMap::new();
        for cap in &ws.capabilities {
            let Subject::Principal(Principal::App(app)) = &cap.subject else {
                continue;
            };
            if let Some(view) = cap_view(cap) {
                by_app.entry(app.clone()).or_default().push(view);
            }
        }
        if by_app.is_empty() {
            continue;
        }
        let apps = by_app
            .into_iter()
            .map(|(app, mut caps)| {
                caps.sort_by(|a, b| a.kind.cmp(b.kind).then(a.detail.cmp(&b.detail)));
                AppGrants { app, caps }
            })
            .collect();
        out.push(WsGrants {
            name: ws.name,
            apps,
        });
    }
    Ok(out)
}

/// Total number of app grants across every workspace — drives populated vs.
/// empty and the summary count.
fn grant_count(all: &[WsGrants]) -> usize {
    all.iter()
        .flat_map(|w| w.apps.iter())
        .map(|a| a.caps.len())
        .sum()
}

/// One capability line: a tinted kind pill, the detail, and a trailing
/// lifetime chip. Read-only — no controls.
fn cap_row(view: &CapView) -> El {
    let kind = kind_shown(view.kind);
    let expiry = expiry_shown(view.expiry);
    Box::new(
        Row::new()
            .push(semantic_status_pill(&kind, view.tint.tone()))
            .push(Box::new(
                Text::new(&view.detail)
                    .with_role(ui::TextRole::Caption)
                    .dimmed(),
            ))
            .push(Box::new(Spacer::new()))
            .push(semantic_status_pill(&expiry, StatusTone::Neutral)),
    )
}

/// One app's block within a workspace card: the app id, then its grants.
fn app_block(grants: &AppGrants) -> El {
    let mut items: Vec<El> = Vec::new();
    items.push(Box::new(
        Text::new(&grants.app).with_role(ui::TextRole::Body),
    ));
    for cap in &grants.caps {
        items.push(cap_row(cap));
    }
    rows(items)
}

/// Include the hosted disk-unlock model beside the device security summary.
pub(crate) fn build_with_disk_unlock(privacy: El, disk_unlock: El) -> Box<dyn Widget> {
    crate::widgets::rows(vec![privacy, disk_unlock])
}

fn privacy_hardware_card(privacy: &os_actions::PrivacyStatus) -> El {
    let microphone_present = privacy.microphone_muted.is_some();
    let mute = match privacy.microphone_muted {
        Some(true) => t("perm-muted"),
        Some(false) => t("perm-listening"),
        None => t("perm-unavailable"),
    };
    card(
        &t("perm-privacy-hardware"),
        rows(vec![
            value_row(
                &t("perm-camera"),
                &kill_label(privacy.camera_kill, privacy.camera_present),
            ),
            value_row(
                &t("perm-microphone"),
                &kill_label(privacy.microphone_kill, microphone_present),
            ),
            value_row(&t("perm-mic-mute"), &mute),
        ]),
    )
}

fn active_privacy_use_card(privacy: &os_actions::PrivacyStatus) -> El {
    let usage_rows: Vec<El> = if privacy.active_media.is_empty() {
        vec![caption(&t("perm-no-active-use"))]
    } else {
        privacy
            .active_media
            .iter()
            .map(|usage| {
                let status = match usage.kind {
                    os_actions::MediaKind::Camera => t("perm-using-camera"),
                    os_actions::MediaKind::Microphone => t("perm-using-mic"),
                };
                value_row(&usage.label, &status)
            })
            .collect()
    };
    card(&t("perm-active-use"), rows(usage_rows))
}

fn device_security_card(security: &DeviceSecurity) -> El {
    card(
        &t("perm-device-security"),
        rows(vec![
            value_row(&t("perm-disk"), &t(security.disk_encryption)),
            value_row(&t("perm-secure-boot"), &t(security.secure_boot)),
            match &security.firewall {
                Ok(firewall) => value_row(&t("perm-firewall"), &t(firewall)),
                Err(error) => error_row(
                    &t("perm-firewall"),
                    &t_args("perm-firewall-error", &[("error", error.as_str().into())]),
                ),
            },
            caption(&t("perm-security-caption")),
        ]),
    )
}

/// Establish the boundary before mixing the two related, but independently
/// owned, kinds of access below. System Settings reports this device's state;
/// collaborators, roles, and app access belong to the workspace that owns
/// them. Keeping the distinction in the first visible card prevents this
/// cross-workspace overview from looking like an editor for every workspace.
fn scope_card() -> El {
    card(
        &t("perm-this-device"),
        rows(vec![Box::new(
            Text::new(&t("perm-this-device-body")).with_role(ui::TextRole::Caption),
        )]),
    )
}

/// Intro card: what these grants are and where they come from.
fn workspace_grants_intro_card(cx: &PaneCx) -> El {
    let open = t("perm-open-workspace");
    card(
        &t("perm-workspace-intro-title"),
        rows(vec![
            Box::new(Text::new(&t("perm-workspace-intro-body")).with_role(ui::TextRole::Caption)),
            Box::new(cx.bind_invocation(
                Button::new("open-workspace-privacy", &open),
                PermissionsEffect::OpenWorkspacePrivacy,
            )),
        ]),
    )
}

/// Empty state — a successfully loaded workspace record contains no
/// app grants.
fn no_workspace_grants_card() -> El {
    card(
        &t("perm-no-grants-title"),
        rows(vec![
            Box::new(Text::new(&t("perm-no-grants-body")).with_role(ui::TextRole::Caption)),
            gap(SpacingStep::S1),
            caption(&t("perm-no-grants-caption")),
        ]),
    )
}

/// Loading workspace state is distinct from successfully reading a workspace
/// that has no app grants. A privacy surface must never turn an I/O or parse
/// failure into a claim that access is absent.
fn workspace_grants_unavailable_card() -> El {
    card(
        &t("perm-unavailable-title"),
        rows(vec![
            Box::new(Text::new(&t("perm-unavailable-body")).with_role(ui::TextRole::Caption)),
            caption(&t("perm-unavailable-caption")),
        ]),
    )
}

/// A quiet summary line above the per-workspace cards, followed by one card
/// per workspace with a grant.
fn push_workspace_grant_cards(panel: &mut Vec<El>, all: &[WsGrants], total: usize) {
    let ws_n = all.len();
    let grants = i64::try_from(total).unwrap_or(i64::MAX);
    let workspaces = i64::try_from(ws_n).unwrap_or(i64::MAX);
    let summary = t_args(
        "perm-grants-summary",
        &[("grants", grants.into()), ("workspaces", workspaces.into())],
    );
    panel.push(Box::new(
        Text::new(&summary)
            .with_role(ui::TextRole::Caption)
            .dimmed(),
    ));

    for ws in all {
        let mut body: Vec<El> = Vec::new();
        let last = ws.apps.len().saturating_sub(1);
        for (i, app) in ws.apps.iter().enumerate() {
            body.push(app_block(app));
            if i != last {
                body.push(gap(SpacingStep::S3));
            }
        }
        let unnamed = t("perm-workspace-unnamed");
        let title = if ws.name.trim().is_empty() {
            unnamed
        } else {
            ws.name.clone()
        };
        panel.push(card(&title, rows(body)));
    }
}

/// Where the compositor enforces the grants.
fn enforcement_card() -> El {
    card(
        &t("perm-enforcement-title"),
        rows(vec![caption(&t("perm-enforcement-caption"))]),
    )
}

/// Pure snapshot composition, exercised against an
/// explicit state document in tests without touching the real config dir.
fn build_observed_view(cx: &PaneCx, snapshot: &PrivacySnapshot) -> Box<dyn Widget> {
    let state_json = snapshot.workspace_state.as_deref();
    let workspace_access = state_json.map(project).transpose();

    let mut panel: Vec<El> = Vec::new();

    let privacy = &snapshot.privacy;
    panel.push(scope_card());
    panel.push(privacy_hardware_card(privacy));
    panel.push(active_privacy_use_card(privacy));
    panel.push(device_security_card(&snapshot.security));
    panel.push(workspace_grants_intro_card(cx));

    match workspace_access {
        Ok(Some(all)) if all.is_empty() => panel.push(no_workspace_grants_card()),
        Ok(Some(all)) => {
            let total = grant_count(&all);
            push_workspace_grant_cards(&mut panel, &all, total);
        }
        Ok(None) | Err(_) => panel.push(workspace_grants_unavailable_card()),
    }

    panel.push(enforcement_card());

    cards(panel)
}

#[cfg(test)]
fn build_view(cx: &PaneCx, state_json: Option<&str>) -> Box<dyn Widget> {
    let mut snapshot = PrivacySnapshot::unavailable();
    snapshot.workspace_state = state_json.map(str::to_owned);
    build_observed_view(cx, &snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct HeldPrivacy {
        entered: std::sync::mpsc::Sender<std::thread::ThreadId>,
        gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        refusal: Option<String>,
        reads: std::sync::atomic::AtomicUsize,
    }
    impl PrivacyControl for HeldPrivacy {
        fn read(&self) -> Result<PrivacySnapshot, String> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.entered.send(std::thread::current().id()).unwrap();
            self.gate
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            if let Some(error) = &self.refusal {
                return Err(error.clone());
            }
            let mut snapshot = PrivacySnapshot::unavailable();
            snapshot.security.disk_encryption = "perm-encrypted";
            snapshot.workspace_state = Some(r#"{"workspaces":[]}"#.into());
            Ok(snapshot)
        }
    }

    #[test]
    fn mounted_privacy_open_does_not_wait_for_device_services_on_ui() {
        let (entered, entries) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let control: Arc<dyn PrivacyControl> = Arc::new(HeldPrivacy {
            entered,
            gate: std::sync::Mutex::new(gate),
            refusal: None,
            reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        app.with_surface::<SettingsApp, _>(|settings| {
            *settings.privacy_mut() = PrivacyArea::with_control(&settings.host().clone(), control);
        })
        .unwrap();
        let helper = std::thread::spawn(move || {
            let thread = entries.recv_timeout(Duration::from_secs(3)).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            release.send(()).unwrap();
            thread
        });
        let address = crate::effects::open_pane_address(crate::categories::Category::Permissions);
        let start = std::time::Instant::now();
        app.call_on(&address.resource, &address.action, serde_json::json!({}))
            .unwrap();
        app.settle();
        let elapsed = start.elapsed();
        let reader = helper.join().unwrap();
        assert!(
            elapsed < Duration::from_millis(150),
            "mounted Privacy open blocks device reads on UI: {elapsed:?}, reader={reader:?}"
        );
        assert_ne!(reader, std::thread::current().id());
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| {
                    let mut labels = Vec::new();
                    text_labels(h.root_widget(), &mut labels);
                    labels.contains(&t("perm-encrypted"))
                        && labels.contains(&t("perm-no-grants-title"))
                })
        );
    }

    #[test]
    fn privacy_snapshot_wakes_its_owner_and_stays_retained_on_layout() {
        use ui::headless::{Framed, viewport};
        let mut scope = ui::WakeScope::new();
        let (woke, wakes) = std::sync::mpsc::channel();
        scope
            .bind(move |_| {
                let _ = woke.send(());
            })
            .unwrap();
        let _entry = scope.enter();
        let (entered, entries) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let control = Arc::new(HeldPrivacy {
            entered,
            gate: std::sync::Mutex::new(gate),
            refusal: None,
            reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let area = PrivacyArea::with_control(&cx(), control.clone());
        let _ = area.pane();
        let pane = ModelPane::new(Rc::clone(area.model.get().unwrap()));
        entries.recv_timeout(Duration::from_secs(3)).unwrap();
        let mut app = Framed::new(
            ui::State::new(),
            pane,
            viewport((1200.0, 900.0)),
            ui::Theme::dark(),
        );
        let mut labels = Vec::new();
        text_labels(Framed::harness(&app).root_widget(), &mut labels);
        assert!(labels.contains(&t("perm-read-loading")));
        assert!(!labels.contains(&t("perm-encrypted")));
        assert!(!labels.contains(&t("perm-no-active-use")));
        let schedule = Framed::harness(&app).frame_schedule();
        assert!(!schedule.needs_animation_frame);
        assert!(schedule.next_wake_deadline.is_none());
        release.send(()).unwrap();
        wakes.recv_timeout(Duration::from_secs(3)).unwrap();
        // Ordinary app polling can adopt a closed pane without stealing its composition signal.
        area.poll();
        Framed::harness_mut(&mut app)
            .relayout_at(viewport((1100.0, 900.0)))
            .unwrap();
        let mut labels = Vec::new();
        text_labels(Framed::harness(&app).root_widget(), &mut labels);
        assert!(labels.contains(&t("perm-encrypted")));
        assert!(labels.contains(&t("perm-no-grants-title")));
        for width in [1000.0, 1200.0] {
            Framed::harness_mut(&mut app)
                .relayout_at(viewport((width, 900.0)))
                .unwrap();
        }
        assert_eq!(control.reads.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(area.model.get().unwrap().borrow().reading.is_none());
    }

    #[test]
    fn privacy_snapshot_failure_never_claims_observed_device_posture() {
        use ui::headless::{Framed, viewport};
        let (entered, entries) = std::sync::mpsc::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let area = PrivacyArea::with_control(
            &cx(),
            Arc::new(HeldPrivacy {
                entered,
                gate: std::sync::Mutex::new(gate),
                refusal: Some("fixture privacy service failed".into()),
                reads: std::sync::atomic::AtomicUsize::new(0),
            }),
        );
        let mut app = Framed::new(
            ui::State::new(),
            {
                let _ = area.pane();
                ModelPane::new(Rc::clone(area.model.get().unwrap()))
            },
            viewport((1200.0, 900.0)),
            ui::Theme::dark(),
        );
        entries.recv_timeout(Duration::from_secs(3)).unwrap();
        release.send(()).unwrap();
        assert!(
            Framed::harness_mut(&mut app).wait_until(ui::time::Duration::from_secs(3), |h| {
                let mut labels = Vec::new();
                text_labels(h.root_widget(), &mut labels);
                labels
                    .iter()
                    .any(|label| label.contains("fixture privacy service failed"))
            })
        );
        let mut labels = Vec::new();
        text_labels(Framed::harness(&app).root_widget(), &mut labels);
        for claim in [
            "perm-encrypted",
            "perm-no-active-use",
            "perm-no-grants-title",
        ] {
            assert!(!labels.contains(&t(claim)));
        }
    }

    struct ChangingPrivacy {
        entered: std::sync::mpsc::Sender<()>,
        results: std::sync::Mutex<std::sync::mpsc::Receiver<Result<PrivacySnapshot, String>>>,
    }
    impl PrivacyControl for ChangingPrivacy {
        fn read(&self) -> Result<PrivacySnapshot, String> {
            self.entered.send(()).unwrap();
            self.results
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
        }
    }

    fn changed_snapshot(active: bool) -> PrivacySnapshot {
        let mut snapshot = PrivacySnapshot::unavailable();
        snapshot.workspace_state = Some(if active {
            sample_state()
        } else {
            r#"{"workspaces":[]}"#.into()
        });
        if active {
            snapshot
                .privacy
                .active_media
                .push(os_actions::ActiveMediaUse {
                    client_id: 7,
                    pid: 42,
                    app_id: "so.tilo.camera".into(),
                    label: "Observed camera".into(),
                    binary: "/usr/bin/camera".into(),
                    kind: os_actions::MediaKind::Camera,
                });
        }
        snapshot
    }

    fn open_privacy(app: &mut ui::framework::testing::TestApp<crate::SettingsShell>) {
        let address = crate::effects::open_pane_address(crate::categories::Category::Permissions);
        app.call_on(&address.resource, &address.action, serde_json::json!({}))
            .unwrap();
        app.settle();
    }

    fn await_privacy_label(
        app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
        label: &str,
    ) {
        assert!(
            app.harness()
                .wait_until(ui::time::Duration::from_secs(3), |h| {
                    let mut labels = Vec::new();
                    text_labels(h.root_widget(), &mut labels);
                    labels.iter().any(|value| value == label)
                }),
            "actual mounted Privacy snapshot never adopted {label}"
        );
    }

    #[test]
    fn mounted_privacy_reopen_and_visible_refresh_adopt_capture_and_grant_changes() {
        let (entered, entries) = std::sync::mpsc::channel();
        let (results, observed) = std::sync::mpsc::channel();
        let control = Arc::new(ChangingPrivacy {
            entered,
            results: std::sync::Mutex::new(observed),
        });
        let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
        app.with_surface::<SettingsApp, _>(|settings| {
            *settings.privacy_mut() = PrivacyArea::with_control(&settings.host().clone(), control);
        })
        .unwrap();
        results.send(Ok(changed_snapshot(false))).unwrap();
        open_privacy(&mut app);
        entries.recv_timeout(Duration::from_secs(3)).unwrap();
        await_privacy_label(&mut app, &t("perm-no-active-use"));
        let other = crate::effects::open_pane_address(crate::categories::Category::Appearance);
        app.call_on(&other.resource, &other.action, serde_json::json!({}))
            .unwrap();
        app.settle();
        std::thread::sleep(Duration::from_millis(1100));
        app.settle();
        assert!(
            entries.try_recv().is_err(),
            "a hidden Privacy pane must not keep reading"
        );
        open_privacy(&mut app);
        entries
            .recv_timeout(Duration::from_secs(1))
            .expect("reopening must actually reread current device and workspace state");
        let mut labels = Vec::new();
        text_labels(app.harness().root_widget(), &mut labels);
        assert!(labels.contains(&t("perm-read-refreshing")));
        for _ in 0..3 {
            app.settle();
        }
        assert!(
            entries.try_recv().is_err(),
            "refreshing must retain exactly one in-flight read"
        );
        results.send(Ok(changed_snapshot(true))).unwrap();
        await_privacy_label(&mut app, "Observed camera");
        await_privacy_label(&mut app, "Studio");
        // Ordinary mounted idle scheduling must refresh again while the pane stays visible.
        results.send(Ok(changed_snapshot(false))).unwrap();
        await_privacy_label(&mut app, &t("perm-no-active-use"));
        entries.recv_timeout(Duration::from_secs(1)).unwrap();
        await_privacy_label(&mut app, &t("perm-no-grants-title"));
        // A real refresh failure must replace the successful observation, never claim no capture.
        results
            .send(Err("current capture graph unavailable".into()))
            .unwrap();
        await_privacy_label(
            &mut app,
            &t_args(
                "perm-read-error",
                &[("error", "current capture graph unavailable".into())],
            ),
        );
        let mut labels = Vec::new();
        text_labels(app.harness().root_widget(), &mut labels);
        assert!(!labels.contains(&t("perm-no-active-use")));
        assert!(!labels.contains(&t("perm-no-grants-title")));
    }

    fn cx() -> PaneCx {
        PaneCx::in_file(std::env::temp_dir().join("tilo-settings-permissions-unused.json"))
    }
    use collab_types::caps::{CapOverride, Scope, Subject, Verb};
    use std::collections::HashMap;

    /// "Not active" is only said about a ruleset that was read and is empty.
    /// A ruleset this session may not read (nft needs privileges) and no
    /// running firewalld is not a firewall that is off.
    #[test]
    fn an_unreadable_firewall_is_not_reported_as_off() {
        let denied = || Err("nft: Operation not permitted (you must be root)".to_string());
        assert_eq!(
            firewall_from(Ok("running".into()), denied).map(t),
            Ok(t("perm-firewall-firewalld"))
        );
        assert_eq!(
            firewall_from(Err("firewall-cmd: not running".into()), denied).map(t),
            Err(
                "firewall-cmd: not running; nft: Operation not permitted (you must be root)".into()
            )
        );
        assert_eq!(
            firewall_from(
                Err("firewall-cmd: not running".into()),
                || Ok(String::new())
            )
            .map(t),
            Ok(t("perm-firewall-off"))
        );
        assert_eq!(
            firewall_from(Err("firewall-cmd: not running".into()), || {
                Ok("table inet filter {}".into())
            })
            .map(t),
            Ok(t("perm-firewall-nftables"))
        );
    }
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    fn count_named(w: &dyn Widget, name: &str) -> usize {
        let mut n = usize::from(w.name() == name);
        for c in w.children() {
            n += count_named(c, name);
        }
        n
    }

    fn button_labels(w: &dyn Widget, labels: &mut Vec<String>) {
        if w.name() == "Button"
            && let Some(label) = w.accessibility().and_then(|node| node.name)
        {
            labels.push(label);
        }
        for c in w.children() {
            button_labels(c, labels);
        }
    }

    fn text_labels(w: &dyn Widget, labels: &mut Vec<String>) {
        if w.name() == "Text"
            && let Some(label) = w.accessibility().and_then(|node| node.name)
        {
            labels.push(label);
        }
        for c in w.children() {
            text_labels(c, labels);
        }
    }

    /// A real state.json fragment: one workspace, one app holding a camera
    /// grant and a read-only file grant. Built by serialising the actual
    /// `CapOverride` type so the fixture matches the compositor's on-disk
    /// shape exactly.
    fn sample_state() -> String {
        let cam = CapOverride::grant(
            Subject::app("so.tilo.camera"),
            Scope::Device("camera".into()),
            Verb::Read,
        );
        let file = CapOverride::grant(
            Subject::app("so.tilo.camera"),
            Scope::FsPath("/var/workspace/Photos".into()),
            Verb::Read,
        );
        let caps = serde_json::to_string(&vec![cam, file]).unwrap();
        format!(r#"{{"workspaces":[{{"name":"Studio","capabilities":{caps}}}]}}"#)
    }

    fn build_layout_of(pane: &mut Box<dyn Widget>) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = pane.build_layout(&mut taffy, &theme, &mut node_map);
    }

    /// Read-only pane, owner routing, scope kinds and one pill per grant.
    #[test]
    fn permissions_pane_is_read_only_and_routes_workspace_access() {
        // Case `is_read_only_except_for_one_live_context_route`.
        {
            // The pane still cannot mutate state.json. Its only button asks the
            // shell to reveal the active workspace's authoritative controls.
            for json in [None, Some(sample_state().as_str())] {
                let mut pane = build_view(&cx(), json);
                build_layout_of(&mut pane);
                assert_eq!(count_named(pane.as_ref(), "Button"), 1);
            }
        }

        // Case `workspace_access_is_explicitly_routed_to_its_owner`.
        {
            // System Settings never pretends it can edit a workspace. The one
            // live control is an explicit route to the current workspace's
            // authoritative access surface.
            let mut pane = build_view(&cx(), Some(&sample_state()));
            build_layout_of(&mut pane);
            let mut labels = Vec::new();
            button_labels(pane.as_ref(), &mut labels);
            assert_eq!(labels, vec!["Open workspace access"]);
        }

        // Case `cap_view_maps_each_scope_to_its_kind`.
        {
            let g = |scope, verb| {
                cap_view(&CapOverride::grant(Subject::app("a"), scope, verb)).map(|v| v.kind)
            };
            assert_eq!(
                g(Scope::Device("camera".into()), Verb::Read),
                Some("Camera")
            );
            assert_eq!(
                g(Scope::Device("microphone".into()), Verb::Read),
                Some("Microphone")
            );
            assert_eq!(
                g(Scope::Device("screencast".into()), Verb::Read),
                Some("Screen sharing")
            );
            assert_eq!(
                g(Scope::Device("remote-desktop".into()), Verb::Write),
                Some("Remote control")
            );
            assert_eq!(g(Scope::NetHost("*".into()), Verb::Write), Some("Network"));
            let action = cap_view(&CapOverride::grant(
                Subject::app("so.tilo.shortcuts"),
                Scope::Action {
                    app: "so.tilo.agent".into(),
                    action: "freeform".into(),
                },
                Verb::Write,
            ))
            .unwrap();
            assert_eq!(action.kind, "App action");
            assert!(action.detail.contains("freeform") && action.detail.contains("so.tilo.agent"));
            // Read vs write is reflected in the file detail.
            let rw = cap_view(&CapOverride::grant(
                Subject::app("a"),
                Scope::FsPath("/w".into()),
                Verb::Write,
            ))
            .unwrap();
            assert!(rw.detail.contains("read & write"));
            // A restriction (not a grant) renders nothing.
            assert!(
                cap_view(&CapOverride::new(
                    Subject::app("a"),
                    Scope::Device("camera".into()),
                    Effect::Restrict,
                    Verb::Read,
                ))
                .is_none()
            );
        }

        // Case `populated_pane_renders_a_pill_per_grant`.
        {
            // Two grants → the workspace card carries a kind pill + a lifetime chip
            // for each (4 badges), built from the real projection.
            let mut pane = build_view(&cx(), Some(&sample_state()));
            build_layout_of(&mut pane);
            assert_eq!(
                count_named(pane.as_ref(), "Badge"),
                4,
                "two pills per grant"
            );
            // Intro + one workspace + enforcement cards are all present.
            assert!(count_named(pane.as_ref(), "Section") >= 3);
        }
    }

    /// Real grants, missing/empty/malformed state and omitted workspaces.
    #[test]
    fn permission_state_is_reported_truthfully() {
        // Case `projection_reads_the_real_grants_from_state_json`.
        {
            // The projection is exactly what `build_view` renders from, so
            // asserting on it asserts on the production path.
            let all = project(&sample_state()).expect("sample state parses");
            assert_eq!(all.len(), 1, "one workspace holds grants");
            assert_eq!(all[0].name, "Studio");
            assert_eq!(all[0].apps.len(), 1, "one app");
            assert_eq!(all[0].apps[0].app, "so.tilo.camera");
            assert_eq!(all[0].apps[0].caps.len(), 2, "camera + file grant");
            assert_eq!(grant_count(&all), 2);
            // The file grant carries the path + mode; the camera grant its kind.
            let kinds: Vec<&str> = all[0].apps[0].caps.iter().map(|c| c.kind).collect();
            assert!(kinds.contains(&"Camera") && kinds.contains(&"Files"));
            let file = all[0].apps[0]
                .caps
                .iter()
                .find(|c| c.kind == "Files")
                .unwrap();
            assert!(file.detail.contains("/var/workspace/Photos"));
            assert!(file.detail.contains("read-only"));
        }

        // Case `missing_state_never_claims_that_no_grants_exist`.
        {
            // No state document means the panel cannot make a claim about grants.
            let mut pane = build_view(&cx(), None);
            build_layout_of(&mut pane);
            assert_eq!(
                count_named(pane.as_ref(), "Badge"),
                0,
                "no grant pills when empty"
            );
            let mut labels = Vec::new();
            text_labels(pane.as_ref(), &mut labels);
            assert!(labels.contains(&"Workspace access can't be checked".to_string()));
            assert!(
                !labels
                    .iter()
                    .any(|label| label.contains("Nothing here means nothing is granted"))
            );
        }

        // Case `valid_empty_state_is_the_only_path_that_claims_no_grants`.
        {
            let mut pane = build_view(&cx(), Some(r#"{"workspaces":[]}"#));
            build_layout_of(&mut pane);
            let mut labels = Vec::new();
            text_labels(pane.as_ref(), &mut labels);
            assert!(labels.contains(&"No workspace access".to_string()));
            assert!(!labels.contains(&"Workspace access can't be checked".to_string()));
        }

        // Case `workspace_without_app_grants_is_omitted`.
        {
            // A workspace whose only capability is room membership (a person
            // subject) holds no *app* grant and must not render a card.
            let membership =
                CapOverride::membership(Subject::person("alice"), "room-1", Verb::Write);
            let caps = serde_json::to_string(&vec![membership]).unwrap();
            let json = format!(r#"{{"workspaces":[{{"name":"Empty","capabilities":{caps}}}]}}"#);
            assert!(
                project(&json).expect("valid workspace state").is_empty(),
                "no app grants → no workspace card"
            );
        }

        // Case `malformed_document_is_reported_as_unavailable_not_empty`.
        {
            assert!(project("not json {").is_err());
            let mut pane = build_view(&cx(), Some("not json {"));
            build_layout_of(&mut pane);
            assert_eq!(count_named(pane.as_ref(), "Badge"), 0);
            let mut labels = Vec::new();
            text_labels(pane.as_ref(), &mut labels);
            assert!(labels.contains(&"Workspace access can't be checked".to_string()));
        }
    }
}
