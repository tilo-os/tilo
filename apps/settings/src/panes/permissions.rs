//! Personal Privacy & Security pane. Device posture is read from the real
//! kernel/session state; workspace grants remain a read-only cross-workspace
//! summary because their authority and mutation UI belong to the contextual
//! Workspace Settings surface.

use ui::tokens::Rhythm;

use crate::i18n::{t, t_args};
use collab_types::caps::{Access, CapOverride, DeviceCap, Effect, Scope};
use provider_settings_ui::widgets::{
    El, Item, SettingsSection, StatusTone, block, error_row, note, page, row, rows, section,
    thing_row, untitled, value_row,
};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tilo_action_policy::Grantee;
use tilo_action_policy::Lifetime;
use ui::LocalState;
use ui::framework::{EffectAnswer, EffectCx, Effected, ViewCx};

use ui::tilo_resource;
use ui::widget::{Button, Column, Element, Row, Toggle};

use crate::area::AreaContext;
use crate::device_read::Reading;
use crate::effects::SettingsEffect;
use crate::resource::Settings;
use crate::settings_app::SettingsApp;

crate::effects::effect_family!(PermissionsEffect => answer);

crate::area::pane_area! {
    PermissionsArea;
    crate::area::serves!(Permissions);
    fn poll(&self) {
        PermissionsArea::poll(self);
    }
}

pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&PermissionsArea> {
    app.areas().find()
}

#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum PermissionsEffect {
    /// Open the privacy settings of the workspace that owns the grants.
    #[effect(kind = "outside")]
    #[effect(job)]
    OpenWorkspacePrivacy,
    /// Let an application use the saved passwords and keys again, or stop it.
    #[effect(kind = "outside")]
    #[effect(job, idempotent)]
    SetSecretsAccess {
        #[param("The application identity the secret store recorded")]
        app: String,
        #[param("Whether the application may use saved secrets")]
        allowed: bool,
    },
    /// Remove an application's saved secrets and its resource.
    #[effect(kind = "outside")]
    #[effect(job)]
    ForgetSecrets {
        #[param("The application identity the secret store recorded")]
        app: String,
    },
    /// Take back an answer the person gave an app about a device of theirs.
    #[effect(kind = "outside")]
    #[effect(job, idempotent)]
    RevokeDeviceAnswer {
        #[param("The answer, as the session publishes it")]
        resource: String,
    },
}

pub(crate) fn answer(
    app: &SettingsApp,
    effect: PermissionsEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    match effect {
        PermissionsEffect::SetSecretsAccess { app: id, allowed } => {
            let verb = if allowed { "allow" } else { "revoke" };
            secret_command(app, cx, vec!["apps".into(), verb.into(), id])
        }
        PermissionsEffect::ForgetSecrets { app: id } => {
            secret_command(app, cx, vec!["apps".into(), "forget".into(), id])
        }
        PermissionsEffect::RevokeDeviceAnswer { resource } => {
            let reader = Arc::clone(&area(app).ok_or_else(crate::area::missing)?.reader);
            Ok(app.job(
                cx,
                move || reader.revoke_device_answer(&resource),
                |app: &SettingsApp, done: Result<(), String>, _| {
                    area(app)
                        .ok_or_else(crate::area::missing)?
                        .reading
                        .refresh();
                    done.map(|()| Effected::done())
                },
            ))
        }
        PermissionsEffect::OpenWorkspacePrivacy => {
            if !app.reaches_devices() {
                return Err("only a window someone uses opens workspace settings".into());
            }
            Ok(app.job(
                cx,
                move || {
                    os_actions::open_tool(
                        os_actions::ToolIntent::WorkspaceSettings {
                            route: os_actions::WorkspaceSettingsRoute::PrivacySecurity,
                        },
                        None,
                    )
                },
                |_: &SettingsApp, opened: Result<(), String>, _| opened.map(|()| Effected::done()),
            ))
        }
    }
}

const SECRET_SERVICE: &str = "/usr/libexec/tilo/tilo-secret-service";

fn secret_command(
    app: &SettingsApp,
    cx: &mut EffectCx<Settings>,
    args: Vec<String>,
) -> EffectAnswer {
    let reader = Arc::clone(
        &crate::panes::permissions::area(app)
            .ok_or_else(crate::area::missing)?
            .reader,
    );
    Ok(app.job(
        cx,
        move || reader.secret_command(&args),
        |app: &SettingsApp, done: Result<(), String>, _| {
            crate::panes::permissions::area(app)
                .ok_or_else(crate::area::missing)?
                .reading
                .refresh();
            done.map(|()| Effected::done())
        },
    ))
}

trait PermissionsReader: Send + Sync {
    fn privacy(&self) -> os_actions::PrivacyStatus;
    fn device_security(&self) -> DeviceSecurity;
    fn secret_apps(&self) -> Result<Vec<SecretApp>, String>;
    fn secret_command(&self, args: &[String]) -> Result<(), String>;
    fn workspace_state(&self) -> Option<String>;
    /// The answers the person kept about the devices of their machines, as the session publishes
    /// them; empty when no session publishes them.
    fn device_answers(&self) -> Vec<DeviceAnswer>;
    fn revoke_device_answer(&self, resource: &str) -> Result<(), String>;
}

/// The resource the session publishes its device answers under.
const PORTAL_RESOURCE: &str = "portal";

/// One answer the person kept about a device of theirs, as the session publishes it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct DeviceAnswer {
    /// The resource that takes it back.
    resource: String,
    app: String,
    /// The device's capability, as `DeviceCap` names it.
    cap: String,
    answer: tilo_action_policy::Answer,
    lifetime: Lifetime,
    device_label: String,
    this_device: bool,
}

fn device_answers_of(reading: &str) -> Vec<DeviceAnswer> {
    #[derive(Deserialize)]
    struct Portal {
        #[serde(default)]
        consents: Vec<DeviceAnswer>,
    }
    serde_json::from_str::<Portal>(reading)
        .map(|portal| portal.consents)
        .unwrap_or_default()
}

struct SystemPermissions;

impl PermissionsReader for SystemPermissions {
    fn privacy(&self) -> os_actions::PrivacyStatus {
        crate::device_read::off_ui_thread();
        os_actions::get_privacy_status()
    }

    fn device_security(&self) -> DeviceSecurity {
        crate::device_read::off_ui_thread();
        device_security()
    }

    fn secret_apps(&self) -> Result<Vec<SecretApp>, String> {
        crate::device_read::off_ui_thread();
        secret_apps()
    }

    fn secret_command(&self, args: &[String]) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        command_output(SECRET_SERVICE, &args).map(|_| ())
    }

    fn workspace_state(&self) -> Option<String> {
        crate::device_read::off_ui_thread();
        load_text()
    }

    fn device_answers(&self) -> Vec<DeviceAnswer> {
        crate::device_read::off_ui_thread();
        ui::collaboration::SessionResource::open(PORTAL_RESOURCE)
            .ok()
            .and_then(|portal| portal.latest())
            .and_then(Result::ok)
            .map(|reading| device_answers_of(&reading))
            .unwrap_or_default()
    }

    fn revoke_device_answer(&self, resource: &str) -> Result<(), String> {
        ui::collaboration::SessionResource::invoke_without_watching(
            resource,
            "forget",
            &serde_json::Map::new(),
        )
    }
}

struct SamplePermissions {
    secrets: Mutex<Vec<SecretApp>>,
    device_answers: Mutex<Vec<DeviceAnswer>>,
}

fn sample_answer(
    app: &str,
    cap: &str,
    device: &str,
    here: bool,
    lifetime: Lifetime,
) -> DeviceAnswer {
    DeviceAnswer {
        resource: format!("portal:consent:{device}\u{1f}{app}\u{1f}{cap}"),
        app: app.into(),
        cap: cap.into(),
        answer: tilo_action_policy::Answer::Allow,
        lifetime,
        device_label: device.into(),
        this_device: here,
    }
}

impl Default for SamplePermissions {
    fn default() -> Self {
        Self {
            device_answers: Mutex::new(vec![
                sample_answer(
                    "so.tilo.recorder",
                    "camera",
                    "Laptop",
                    true,
                    Lifetime::UntilRevoked,
                ),
                sample_answer(
                    "so.tilo.recorder",
                    "microphone",
                    "Laptop",
                    true,
                    Lifetime::UntilRevoked,
                ),
                sample_answer(
                    "so.tilo.meet",
                    "screen",
                    "Phone",
                    false,
                    Lifetime::Until(1_900_000_000_000),
                ),
            ]),
            secrets: Mutex::new(vec![
                SecretApp {
                    id: "binary:/usr/bin/chromium".into(),
                    name: "chromium".into(),
                    access: "allowed".into(),
                    uses: 42,
                    items: 3,
                },
                SecretApp {
                    id: "sandbox:so.tilo.mail".into(),
                    name: "so.tilo.mail".into(),
                    access: "allowed".into(),
                    uses: 7,
                    items: 1,
                },
            ]),
        }
    }
}

impl PermissionsReader for SamplePermissions {
    fn privacy(&self) -> os_actions::PrivacyStatus {
        crate::device_read::off_ui_thread();
        os_actions::PrivacyStatus {
            microphone_muted: Some(false),
            microphone_kill: os_actions::KillState::Allowed,
            camera_present: true,
            camera_kill: os_actions::KillState::Allowed,
            active_media: Vec::new(),
            ..os_actions::PrivacyStatus::default()
        }
    }

    fn device_security(&self) -> DeviceSecurity {
        crate::device_read::off_ui_thread();
        DeviceSecurity {
            disk_encryption: "perm-encrypted",
            secure_boot: "perm-enabled",
            firewall: Ok("perm-firewall-firewalld"),
        }
    }

    fn secret_apps(&self) -> Result<Vec<SecretApp>, String> {
        crate::device_read::off_ui_thread();
        Ok(self
            .secrets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    fn secret_command(&self, args: &[String]) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        let unexpected = || format!("{SECRET_SERVICE}: unexpected arguments {args:?}");
        let [apps, verb, id] = args else {
            return Err(unexpected());
        };
        let access = match (apps.as_str(), verb.as_str()) {
            ("apps", "allow") => Some("allowed"),
            ("apps", "revoke") => Some("revoked"),
            ("apps", "forget") => None,
            _ => return Err(unexpected()),
        };
        let mut secrets = self.secrets.lock().unwrap_or_else(PoisonError::into_inner);
        if !secrets.iter().any(|app| app.id == *id) {
            return Err(format!("{SECRET_SERVICE}: no application `{id}`"));
        }
        match access {
            Some(access) => secrets
                .iter_mut()
                .filter(|app| app.id == *id)
                .for_each(|app| app.access = access.into()),
            None => secrets.retain(|app| app.id != *id),
        }
        drop(secrets);
        Ok(())
    }

    fn device_answers(&self) -> Vec<DeviceAnswer> {
        self.device_answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn revoke_device_answer(&self, resource: &str) -> Result<(), String> {
        self.device_answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|answer| answer.resource != resource);
        Ok(())
    }

    fn workspace_state(&self) -> Option<String> {
        crate::device_read::off_ui_thread();
        let caps = [(
            "so.tilo.terminal",
            Scope::Device(DeviceCap::DesktopControl),
            Access::Write,
        )]
        .map(|(app, scope, access)| {
            CapOverride::grant(Grantee::person("local"), scope, access).through(app)
        });
        let caps = serde_json::to_string(&caps).ok()?;
        Some(format!(
            r#"{{"workspaces":[{{"name":"Home","sandbox":{caps}}}]}}"#
        ))
    }
}

#[derive(Clone)]
struct PermissionsReading {
    privacy: os_actions::PrivacyStatus,
    security: DeviceSecurity,
    secrets: Result<Vec<SecretApp>, String>,
    device_answers: Vec<DeviceAnswer>,
    workspace_access: Option<Vec<WsGrants>>,
}

impl PermissionsReading {
    fn read(reader: &dyn PermissionsReader) -> Self {
        Self {
            privacy: reader.privacy(),
            security: reader.device_security(),
            secrets: reader.secret_apps(),
            device_answers: reader.device_answers(),
            workspace_access: workspace_access(reader.workspace_state().as_deref()),
        }
    }
}

pub struct PermissionsArea {
    reader: Arc<dyn PermissionsReader>,
    reading: Reading<PermissionsReading>,
    shown: LocalState<u64>,
    seen: std::cell::Cell<u64>,
}

impl PermissionsArea {
    pub fn new(cx: &AreaContext) -> Self {
        let reader: Arc<dyn PermissionsReader> = if cx.devices {
            Arc::new(SystemPermissions)
        } else {
            Arc::new(SamplePermissions::default())
        };
        Self::over(reader)
    }

    fn over(reader: Arc<dyn PermissionsReader>) -> Self {
        let read = Arc::clone(&reader);
        let reading = Reading::new(move || PermissionsReading::read(read.as_ref()));
        Self {
            reader,
            seen: std::cell::Cell::new(reading.revision()),
            reading,
            shown: LocalState::new(0),
        }
    }

    fn poll(&self) {
        let revision = self.reading.revision();
        if self.seen.replace(revision) != revision {
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct SecretApp {
    id: String,
    name: String,
    access: String,
    #[serde(default)]
    uses: u64,
    #[serde(default)]
    items: u32,
}

fn parse_secret_apps(json: &str) -> Result<Vec<SecretApp>, String> {
    serde_json::from_str(json).map_err(|error| error.to_string())
}

fn secret_apps() -> Result<Vec<SecretApp>, String> {
    parse_secret_apps(&command_output(
        SECRET_SERVICE,
        &["apps", "list", "--json"],
    )?)
}

fn secrets_card(cx: &ViewCx<Settings>, apps: &Result<Vec<SecretApp>, String>) -> SettingsSection {
    let title = t("perm-secrets-title");
    let mut body: Vec<Item> = vec![note(&t("perm-secrets-intro"))];
    match apps {
        Err(_) => body.push(note(&t("perm-secrets-unavailable"))),
        Ok(apps) if apps.is_empty() => body.push(note(&t("perm-secrets-none"))),
        Ok(apps) => {
            for app in apps {
                let allowed = app.access == "allowed";
                let detail = t_args(
                    "perm-secrets-detail",
                    &[
                        ("id", ui::i18n::ArgValue::Text(app.id.clone())),
                        ("items", ui::i18n::ArgValue::Number(f64::from(app.items))),
                        (
                            "uses",
                            ui::i18n::ArgValue::Number(f64::from(
                                u32::try_from(app.uses).unwrap_or(u32::MAX),
                            )),
                        ),
                    ],
                );
                let id = app.id.clone();
                let access = cx.actions().bind_effect(
                    Toggle::from_state(&ui::LocalState::new(allowed))
                        .with_id(&format!("secrets-access:{}", app.id))
                        .with_accessible_name(&t("perm-secrets-access")),
                    crate::effects::effect(PermissionsEffect::SET_SECRETS_ACCESS)
                        .map(move |allowed: bool| (id.clone(), allowed)),
                );
                let menu = ui::widget::RowMenu::new(
                    format!("secrets-more:{}", app.id),
                    &t_args("settings-row-more", &[("name", app.name.clone().into())]),
                )
                .destructive(
                    t("perm-secrets-forget"),
                    cx.actions()
                        .effect(&SettingsEffect::from(PermissionsEffect::ForgetSecrets {
                            app: app.id.clone(),
                        })),
                );
                body.push(row(Box::new(thing_row(
                    &format!("secrets:{}", app.id),
                    &app_name(&app.name),
                    &[detail],
                    Some(Box::new(
                        Row::new()
                            .no_wrap()
                            .with_rhythm(Rhythm::Related)
                            .with_align_items(ui::taffy::AlignItems::Center)
                            .push(Box::new(access))
                            .push(Box::new(menu)),
                    )),
                ))));
            }
        }
    }
    section(&title, rows(body))
}

/// The answers the person kept about the devices of their machines, each with what takes it back.
fn device_answers_card(cx: &ViewCx<Settings>, answers: &[DeviceAnswer]) -> SettingsSection {
    let mut body: Vec<Item> = vec![note(&t("perm-device-answers-intro"))];
    if answers.is_empty() {
        body.push(note(&t("perm-device-answers-none")));
    }
    for answer in answers {
        let what = match answer.cap.as_str() {
            "camera" => t("perm-kind-camera"),
            "microphone" => t("perm-kind-mic"),
            "screen" => t("perm-kind-screen"),
            "input" => t("perm-kind-remote"),
            other => other.to_owned(),
        };
        let mut details = vec![
            what,
            if answer.this_device {
                t("perm-device-answer-here")
            } else {
                t_args(
                    "perm-device-answer-on",
                    &[("device", answer.device_label.clone().into())],
                )
            },
        ];
        if answer.answer != tilo_action_policy::Answer::Allow {
            details.push(t("perm-device-answer-refused"));
        }
        details.push(ui::format::lifetime(answer.lifetime));
        let revoke = cx.actions().bind_effect_invocation(
            Button::new(
                &format!("device-answer-revoke:{}", answer.resource),
                &t("perm-device-answer-revoke"),
            ),
            &SettingsEffect::from(PermissionsEffect::RevokeDeviceAnswer {
                resource: answer.resource.clone(),
            }),
        );
        body.push(row(Box::new(thing_row(
            &format!("device-answer:{}", answer.resource),
            &app_name(&answer.app),
            &details,
            Some(Box::new(revoke)),
        ))));
    }
    section(&t("perm-device-answers-title"), rows(body))
}

fn state_path() -> Option<PathBuf> {
    settings_store::config_path(std::path::Path::new("state.json"))
}

fn load_text() -> Option<String> {
    std::fs::read_to_string(state_path()?).ok()
}

/// The device's protection, as message keys: it is read off the UI thread,
/// where the window's locale is not, and said on it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DeviceSecurity {
    disk_encryption: &'static str,
    secure_boot: &'static str,
    firewall: Result<&'static str, String>,
}

fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    command_output(program, args).ok()
}

fn command_output(program: &str, args: &[&str]) -> Result<String, String> {
    crate::device_read::off_ui_thread();
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
    match std::fs::read(path).ok().and_then(|v| v.get(4).copied()) {
        Some(1) => "perm-enabled",
        Some(_) => "perm-disabled",
        None => "perm-unavailable",
    }
}

/// The firewall in force: firewalld when it runs, otherwise the nftables ruleset. "Not active" is
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
    sandbox: Vec<CapOverride>,
}

/// Which tint a capability reads in — kept separate from the rendered `Color`
/// so the projection is testable without a `Theme`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tint {
    Files,
    Remote,
    Network,
}

impl Tint {
    fn icon(self) -> ui::icons::Icon {
        use ui::icons::Icon;
        match self {
            Tint::Files => Icon::Folder,
            Tint::Remote => Icon::MousePointer2,
            Tint::Network => Icon::Globe,
        }
    }

    fn tone(self) -> StatusTone {
        match self {
            Tint::Files => StatusTone::Info,
            Tint::Remote => StatusTone::Warning,
            Tint::Network => StatusTone::Success,
        }
    }
}

#[derive(Clone)]
struct CapView {
    kind: &'static str,
    detail: String,
    lifetime: Lifetime,
    tint: Tint,
}

#[derive(Clone)]
struct AppGrants {
    app: String,
    caps: Vec<CapView>,
}

#[derive(Clone)]
struct WsGrants {
    name: String,
    apps: Vec<AppGrants>,
}

/// What the app `id` is called: never its id.
fn app_name(id: &str) -> String {
    package_store::display_name_for_package_id(id)
}

fn kind_shown(english: &str) -> String {
    t(match english {
        "Files" => "perm-kind-files",
        "Desktop control" => "perm-kind-desktop-control",
        _ => "perm-kind-network",
    })
}

/// Project one stored capability into a display capability, or `None` when it
/// is not an app grant this pane renders (role/workspace/resource overrides and
/// restrictions are enforcement plumbing, not app permissions, and a workspace
/// never allows the person's camera, microphone, screen or input: their own
/// answers do, listed under their devices).
fn cap_view(cap: &CapOverride) -> Option<CapView> {
    if cap.effect != Effect::Grant {
        return None;
    }
    let (kind, detail, tint) = match &cap.scope {
        Scope::FsPath(p) => {
            let verb = if cap.access == Access::Write {
                t("perm-verb-rw")
            } else {
                t("perm-verb-ro")
            };
            ("Files", format!("{verb}: {p}"), Tint::Files)
        }
        Scope::Device(DeviceCap::DesktopControl) => {
            ("Desktop control", t("perm-desktop-control"), Tint::Remote)
        }
        Scope::NetHost(h) => {
            let detail = if h == "*" {
                t("perm-any-host")
            } else {
                h.clone()
            };
            ("Network", detail, Tint::Network)
        }
        _ => return None,
    };
    Some(CapView {
        kind,
        detail,
        lifetime: cap.lifetime,
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
        let mut by_app: std::collections::BTreeMap<String, Vec<CapView>> =
            std::collections::BTreeMap::new();
        for cap in &ws.sandbox {
            let Some(app) = &cap.app else {
                continue;
            };
            if let Some(view) = cap_view(cap) {
                by_app
                    .entry(app.as_str().to_owned())
                    .or_default()
                    .push(view);
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

fn grant_count(all: &[WsGrants]) -> usize {
    all.iter()
        .flat_map(|w| w.apps.iter())
        .map(|a| a.caps.len())
        .sum()
}

fn app_block(grants: &AppGrants) -> Vec<Item> {
    let rows: Vec<El> = grants
        .caps
        .iter()
        .enumerate()
        .map(|(index, view)| {
            let kind = kind_shown(view.kind);
            Box::new(
                ui::widget::ListRow::new(format!("grants:{}:{index}", grants.app), &view.detail)
                    .icon(view.tint.icon())
                    .tags(&[(
                        kind.as_str(),
                        ui::widget::ChipStyle::Tinted(view.tint.tone()),
                    )])
                    .value(ui::format::lifetime(view.lifetime)),
            ) as El
        })
        .collect();
    vec![block(Box::new(ui::widget::Section::new(rows).with_header(
        ui::widget::SectionHeader::group(app_name(&grants.app)),
    )))]
}

pub(crate) fn view(cx: &ViewCx<Settings>, app: &std::rc::Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    held.reading.refresh();
    let reading = held.reading.clone();
    let grants = cx.switch(&held.shown, move |cx, _| {
        let shown = reading.get();
        let workspace_access = shown.as_ref().map_or(Grants::Reading, |reading| {
            Grants::Read(reading.workspace_access.as_deref())
        });
        panel(cx, workspace_access, shown.as_ref())
    });
    let mut column = Column::new()
        .with_no_padding()
        .with_rhythm(Rhythm::Group)
        .push(grants);
    if let Some(disk_unlock) = crate::panes::disk_unlock::view(cx, app) {
        column = column.push(disk_unlock);
    }
    Some(Box::new(column))
}

fn privacy_hardware_card(privacy: &os_actions::PrivacyStatus) -> SettingsSection {
    let microphone_present = privacy.microphone_muted.is_some();
    let mute = match privacy.microphone_muted {
        Some(true) => t("perm-muted"),
        Some(false) => t("perm-listening"),
        None => t("perm-unavailable"),
    };
    section(
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

fn active_privacy_use_card(privacy: &os_actions::PrivacyStatus) -> SettingsSection {
    let usage_rows: Vec<Item> = if privacy.active_media.is_empty() {
        vec![note(&t("perm-no-active-use"))]
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
    section(&t("perm-active-use"), rows(usage_rows))
}

fn device_security_card(security: &DeviceSecurity) -> SettingsSection {
    section(
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
        ]),
    )
}

fn scope_card() -> SettingsSection {
    section(
        &t("perm-this-device"),
        rows(vec![note(&t("perm-this-device-body"))]),
    )
}

fn workspace_grants_intro_card(cx: &ViewCx<Settings>) -> SettingsSection {
    let open = t("perm-open-workspace");
    section(
        &t("perm-workspace-intro-title"),
        rows(vec![
            note(&t("perm-workspace-intro-body")),
            row(Box::new(cx.actions().bind_effect_invocation(
                Button::new("open-workspace-privacy", &open),
                &SettingsEffect::from(PermissionsEffect::OpenWorkspacePrivacy),
            ))),
        ]),
    )
}

fn no_workspace_grants_card() -> SettingsSection {
    section(
        &t("perm-no-grants-title"),
        rows(vec![note(&t("perm-no-grants-body"))]),
    )
}

/// Loading workspace state is distinct from successfully reading a workspace
/// that has no app grants. A privacy surface must never turn an I/O or parse
/// failure into a claim that access is absent.
fn workspace_grants_unavailable_card() -> SettingsSection {
    section(
        &t("perm-unavailable-title"),
        rows(vec![
            note(&t("perm-unavailable-body")),
            note(&t("perm-unavailable-caption")),
        ]),
    )
}

fn push_workspace_grant_cards(panel: &mut Vec<SettingsSection>, all: &[WsGrants], total: usize) {
    let ws_n = all.len();
    let grants = i64::try_from(total).unwrap_or(i64::MAX);
    let workspaces = i64::try_from(ws_n).unwrap_or(i64::MAX);
    let summary = t_args(
        "perm-grants-summary",
        &[("grants", grants.into()), ("workspaces", workspaces.into())],
    );
    panel.push(untitled(rows(vec![note(&summary)])));

    for ws in all {
        let body: Vec<Item> = ws.apps.iter().flat_map(app_block).collect();
        let unnamed = t("perm-workspace-unnamed");
        let title = if ws.name.trim().is_empty() {
            unnamed
        } else {
            ws.name.clone()
        };
        panel.push(section(&title, rows(body)));
    }
}

/// The workspace grants a state document holds; `None` when there is no
/// document or it does not parse, which the pane never shows as "no grants".
fn workspace_access(state_json: Option<&str>) -> Option<Vec<WsGrants>> {
    project(state_json?).ok()
}

fn reading_card(title: &str) -> SettingsSection {
    section(&t(title), rows(vec![note(&t("perm-reading"))]))
}

#[derive(Clone, Copy)]
enum Grants<'a> {
    Reading,
    Read(Option<&'a [WsGrants]>),
}

fn panel(
    cx: &ViewCx<Settings>,
    workspace_access: Grants<'_>,
    reading: Option<&PermissionsReading>,
) -> Element {
    let mut panel: Vec<SettingsSection> = vec![scope_card()];
    if let Some(reading) = reading {
        panel.push(privacy_hardware_card(&reading.privacy));
        panel.push(device_answers_card(cx, &reading.device_answers));
        panel.push(active_privacy_use_card(&reading.privacy));
        panel.push(device_security_card(&reading.security));
        panel.push(secrets_card(cx, &reading.secrets));
    } else {
        panel.push(reading_card("perm-privacy-hardware"));
        panel.push(reading_card("perm-device-answers-title"));
        panel.push(reading_card("perm-active-use"));
        panel.push(reading_card("perm-device-security"));
        panel.push(reading_card("perm-secrets-title"));
    }
    panel.push(workspace_grants_intro_card(cx));

    match workspace_access {
        Grants::Read(Some([])) => panel.push(no_workspace_grants_card()),
        Grants::Read(Some(all)) => {
            let total = grant_count(all);
            push_workspace_grant_cards(&mut panel, all, total);
        }
        Grants::Read(None) => panel.push(workspace_grants_unavailable_card()),
        Grants::Reading => {}
    }

    page(panel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::count_named;
    use collab_types::caps::{Access, CapOverride, Scope};
    use tilo_action_policy::Grantee;
    use ui::prelude::TestApp;
    use ui::widget::Widget;

    use crate::resource::SettingsShell;

    struct Fixed {
        secrets: Result<Vec<SecretApp>, String>,
        state: Option<String>,
    }

    impl Fixed {
        fn state(state: Option<&str>) -> Self {
            Self {
                secrets: Ok(Vec::new()),
                state: state.map(str::to_owned),
            }
        }
    }

    impl PermissionsReader for Fixed {
        fn privacy(&self) -> os_actions::PrivacyStatus {
            SamplePermissions::default().privacy()
        }
        fn device_security(&self) -> DeviceSecurity {
            SamplePermissions::default().device_security()
        }
        fn secret_apps(&self) -> Result<Vec<SecretApp>, String> {
            self.secrets.clone()
        }
        fn secret_command(&self, _: &[String]) -> Result<(), String> {
            Ok(())
        }
        fn workspace_state(&self) -> Option<String> {
            self.state.clone()
        }
        fn device_answers(&self) -> Vec<DeviceAnswer> {
            Vec::new()
        }
        fn revoke_device_answer(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn window_over(reader: Arc<dyn PermissionsReader>) -> TestApp<SettingsShell> {
        ui::i18n::I18nManager::init("en-US");
        crate::area::replace_in_next_window(move |_| PermissionsArea::over(reader));
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.harness().resize((1280, 4096)).unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::Permissions);
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| !frame.contains(&t("perm-reading")))),
            "the device's reading is shown"
        );
        app
    }

    fn with_id<'a>(w: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
        if w.test_id() == Some(id)
            || w.accessibility()
                .and_then(|node| node.semantic_key)
                .as_deref()
                == Some(id)
        {
            return Some(w);
        }
        w.children()
            .into_iter()
            .find_map(|child| with_id(child, id))
    }

    fn said(app: &mut TestApp<SettingsShell>) -> Vec<String> {
        let mut said = Vec::new();
        text_labels(app.harness().root_widget(), &mut said);
        row_speech(app.harness().root_widget(), &mut said);
        said
    }

    tilo_test_env::scenario! {
        fn device_security_readings_are_parsed_as_the_services_print_them;

        phase the_secret_store_listing_is_read_as_the_service_prints_it;
        {
            let json = r#"[{"id":"binary:/usr/bin/chromium","name":"chromium","access":"allowed","first_seen":1,"last_used":2,"uses":9,"items":3},
                       {"id":"local-process:/home/a/t","name":"t (not part of the system)","access":"revoked","uses":1,"items":0}]"#;
            let apps = parse_secret_apps(json).unwrap();
            assert_eq!(apps.len(), 2);
            assert_eq!(apps[0].items, 3);
            assert_eq!(apps[1].access, "revoked");
            assert!(parse_secret_apps("not json").is_err());
            assert_eq!(parse_secret_apps("[]").unwrap(), Vec::new());
        }

        phase an_unreadable_firewall_is_not_reported_as_off;
        // "Not active" is only said about a ruleset that was read and is empty.
        // A ruleset this session may not read (nft needs privileges) and no
        // running firewalld is not a firewall that is off.
        {
            let denied = || Err("nft: Operation not permitted (you must be root)".to_string());
            assert_eq!(
                firewall_from(Ok("running".into()), denied),
                Ok("perm-firewall-firewalld")
            );
            assert_eq!(
                firewall_from(Err("firewall-cmd: not running".into()), denied),
                Err(
                    "firewall-cmd: not running; nft: Operation not permitted (you must be root)".into()
                )
            );
            assert_eq!(
                firewall_from(
                    Err("firewall-cmd: not running".into()),
                    || Ok(String::new())
                ),
                Ok("perm-firewall-off")
            );
            assert_eq!(
                firewall_from(Err("firewall-cmd: not running".into()), || {
                    Ok("table inet filter {}".into())
                }),
                Ok("perm-firewall-nftables")
            );
        }
    }

    #[test]
    fn each_recorded_app_gets_a_stop_or_allow_control_and_a_forget_control() {
        let apps = vec![
            SecretApp {
                id: "binary:/usr/bin/chromium".into(),
                name: "chromium".into(),
                access: "allowed".into(),
                uses: 2,
                items: 1,
            },
            SecretApp {
                id: "sandbox:so.tilo.notes".into(),
                name: "so.tilo.notes".into(),
                access: "revoked".into(),
                uses: 0,
                items: 0,
            },
        ];
        let mut app = window_over(Arc::new(Fixed {
            secrets: Ok(apps),
            ..Fixed::state(None)
        }));
        let root = app.harness().root_widget();
        let checked = |id: &str| {
            with_id(root, &format!("secrets-access:{id}"))
                .and_then(Widget::accessibility)
                .and_then(|node| node.state.checked)
        };
        assert_eq!(checked("binary:/usr/bin/chromium"), Some(true));
        assert_eq!(checked("sandbox:so.tilo.notes"), Some(false));
        let mut labels = Vec::new();
        for id in ["binary:/usr/bin/chromium", "sandbox:so.tilo.notes"] {
            button_labels(
                with_id(root, &format!("secrets:{id}")).expect("the app's row"),
                &mut labels,
            );
        }
        assert_eq!(
            labels,
            vec![
                t_args("settings-row-more", &[("name", "chromium".into())]),
                t_args("settings-row-more", &[("name", "so.tilo.notes".into())]),
            ]
        );
        // A store that cannot be reached is said so, never shown as empty.
        let mut unreachable = window_over(Arc::new(Fixed {
            secrets: Err("no bus".into()),
            ..Fixed::state(None)
        }));
        let said = said(&mut unreachable);
        assert!(said.contains(&t("perm-secrets-unavailable")), "{said:#?}");
    }

    #[test]
    fn each_device_answer_names_its_device_and_is_taken_back_from_here() {
        let sample = SamplePermissions::default();
        let answers = sample.device_answers();
        let mut app = window_over(Arc::new(sample));
        let joined = said(&mut app).join(" | ");
        assert!(joined.contains(&t("perm-device-answer-here")), "{joined}");
        assert!(
            joined.contains(&t_args(
                "perm-device-answer-on",
                &[("device", "Phone".into())]
            )),
            "an answer kept on another device names it: {joined}"
        );
        let phone = answers
            .iter()
            .find(|answer| !answer.this_device)
            .expect("the phone's answer");
        let revoke = format!("device-answer-revoke:{}", phone.resource);
        app.harness().click_named(&revoke).unwrap();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| with_id(
                    harness.root_widget(),
                    &revoke
                )
                .is_none()),
            "the answer taken back is gone"
        );
        assert!(
            with_id(
                app.harness().root_widget(),
                &format!("device-answer-revoke:{}", answers[0].resource)
            )
            .is_some(),
            "the others stay"
        );
    }

    struct StalledReader(Arc<crate::stall::Stall>);

    impl StalledReader {
        fn wait(&self) {
            self.0.wait();
        }
    }

    impl PermissionsReader for StalledReader {
        fn privacy(&self) -> os_actions::PrivacyStatus {
            self.wait();
            SamplePermissions::default().privacy()
        }
        fn device_security(&self) -> DeviceSecurity {
            self.wait();
            SamplePermissions::default().device_security()
        }
        fn secret_apps(&self) -> Result<Vec<SecretApp>, String> {
            self.wait();
            SamplePermissions::default().secret_apps()
        }
        fn secret_command(&self, args: &[String]) -> Result<(), String> {
            self.wait();
            SamplePermissions::default().secret_command(args)
        }
        fn workspace_state(&self) -> Option<String> {
            self.wait();
            SamplePermissions::default().workspace_state()
        }
        fn device_answers(&self) -> Vec<DeviceAnswer> {
            self.wait();
            SamplePermissions::default().device_answers()
        }
        fn revoke_device_answer(&self, resource: &str) -> Result<(), String> {
            self.wait();
            SamplePermissions::default().revoke_device_answer(resource)
        }
    }

    #[test]
    fn a_stalled_device_read_never_holds_privacy_and_security() {
        ui::i18n::I18nManager::init("en-US");
        let check = crate::stall::Stall::new();
        let stalled = Arc::clone(&check);
        crate::area::replace_in_next_window(move |_| {
            PermissionsArea::over(Arc::new(StalledReader(stalled)))
        });
        let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
        app.harness().resize((1280, 4096)).unwrap();

        crate::effects::show_pane(&mut app, crate::categories::Category::Permissions);
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        check.assert_held("opening the pane");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains(&t("perm-reading")), "{frame}");
        assert!(!frame.contains(&t("perm-firewall-firewalld")), "{frame}");
        assert!(!frame.contains("binary:/usr/bin/chromium"), "{frame}");
        assert!(!frame.contains(&t("perm-unavailable-title")), "{frame}");
        assert!(!frame.contains(&t("perm-desktop-control")), "{frame}");

        check.release();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains(&t("perm-firewall-firewalld"))
                        && frame.contains("binary:/usr/bin/chromium")
                        && frame.contains(&t("perm-desktop-control"))
                        && !frame.contains(&t("perm-reading")))),
            "what the device reports is shown"
        );
    }
    /// A real state.json fragment: one workspace, one app holding a camera
    /// grant and a read-only file grant. Built by serialising the actual
    /// `CapOverride` type so the fixture matches the compositor's on-disk
    /// shape exactly.
    fn sample_state() -> String {
        let control = CapOverride::grant(
            Grantee::person("local"),
            Scope::Device(DeviceCap::DesktopControl),
            Access::Write,
        )
        .through("so.tilo.camera");
        let file = CapOverride::grant(
            Grantee::person("local"),
            Scope::FsPath("/var/workspace/Photos".into()),
            Access::Read,
        )
        .through("so.tilo.camera");
        let caps = serde_json::to_string(&vec![control, file]).unwrap();
        format!(r#"{{"workspaces":[{{"name":"Studio","sandbox":{caps}}}]}}"#)
    }

    fn button_labels(w: &dyn Widget, labels: &mut Vec<String>) {
        let menu = w
            .as_any()
            .is_some_and(<dyn std::any::Any>::is::<ui::widget::RowMenu>);
        if (w.name() == "Button" || menu)
            && let Some(label) = w.accessibility().and_then(|node| node.name)
        {
            labels.push(label);
        }
        for c in w.children() {
            button_labels(c, labels);
        }
    }

    fn count_grant_rows(w: &dyn Widget, rows: &mut usize) {
        if w.name() == "ListRow"
            && w.accessibility().is_some_and(|node| {
                node.semantic_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with("grants:"))
            })
        {
            *rows += 1;
        }
        for c in w.children() {
            count_grant_rows(c, rows);
        }
    }

    fn row_speech(w: &dyn Widget, said: &mut Vec<String>) {
        if w.name() == "ListRow"
            && let Some(node) = w.accessibility()
        {
            said.push(format!(
                "{} {}",
                node.name.unwrap_or_default(),
                node.value.unwrap_or_default()
            ));
        }
        for c in w.children() {
            row_speech(c, said);
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

    #[test]
    fn permissions_pane_is_read_only_and_routes_workspace_access() {
        {
            // System Settings never pretends it can edit a workspace: its
            // grants carry no control, and the one live control is an
            // explicit route to the current workspace's authoritative
            // access surface.
            let mut app = window_over(Arc::new(Fixed::state(Some(&sample_state()))));
            let root = app.harness().root_widget();
            let mut labels = Vec::new();
            button_labels(
                with_id(root, "open-workspace-privacy").expect("the route"),
                &mut labels,
            );
            assert_eq!(labels, vec!["Open workspace access"]);
            // Two grants → the workspace's group carries a row for each,
            // its kind marked and how long it lasts as its value, built
            // from the real projection.
            let mut rows = 0;
            count_grant_rows(root, &mut rows);
            assert_eq!(rows, 2, "one row per grant");
            for index in 0..2 {
                let row = with_id(root, &format!("grants:so.tilo.camera:{index}"))
                    .expect("the grant's row");
                assert_eq!(count_named(row, "Button"), 0);
                assert_eq!(count_named(row, "Toggle"), 0);
            }
        }

        {
            let g = |scope, verb| {
                cap_view(&CapOverride::grant(Grantee::person("local"), scope, verb).through("a"))
                    .map(|v| v.kind)
            };
            for personal in [
                DeviceCap::Camera,
                DeviceCap::Microphone,
                DeviceCap::Screen,
                DeviceCap::Input,
            ] {
                assert_eq!(
                    g(Scope::Device(personal), Access::Read),
                    None,
                    "{personal:?}"
                );
            }
            assert_eq!(
                g(Scope::Device(DeviceCap::DesktopControl), Access::Write),
                Some("Desktop control")
            );
            assert_eq!(
                g(Scope::NetHost("*".into()), Access::Write),
                Some("Network")
            );
            let rw = cap_view(
                &CapOverride::grant(
                    Grantee::person("local"),
                    Scope::FsPath("/w".into()),
                    Access::Write,
                )
                .through("a"),
            )
            .unwrap();
            assert!(rw.detail.contains("read & write"));
            assert!(
                cap_view(
                    &CapOverride::new(
                        Grantee::person("local"),
                        Scope::Device(DeviceCap::Camera),
                        Effect::Restrict,
                        Access::Read,
                    )
                    .through("a"),
                )
                .is_none()
            );
        }
    }

    #[test]
    fn permission_state_is_reported_truthfully() {
        {
            // The projection is exactly what the page renders from, so
            // asserting on it asserts on the production path.
            let all = project(&sample_state()).expect("sample state parses");
            assert_eq!(all.len(), 1, "one workspace holds grants");
            assert_eq!(all[0].name, "Studio");
            assert_eq!(all[0].apps.len(), 1, "one app");
            assert_eq!(all[0].apps[0].app, "so.tilo.camera");
            assert_eq!(all[0].apps[0].caps.len(), 2, "desktop control + file grant");
            assert_eq!(grant_count(&all), 2);
            let kinds: Vec<&str> = all[0].apps[0].caps.iter().map(|c| c.kind).collect();
            assert!(kinds.contains(&"Desktop control") && kinds.contains(&"Files"));
            let file = all[0].apps[0]
                .caps
                .iter()
                .find(|c| c.kind == "Files")
                .unwrap();
            assert!(file.detail.contains("/var/workspace/Photos"));
            assert!(file.detail.contains("read-only"));
        }

        {
            let mut app = window_over(Arc::new(Fixed::state(None)));
            let mut rows = 0;
            count_grant_rows(app.harness().root_widget(), &mut rows);
            assert_eq!(rows, 0, "no grant rows when unread");
            let labels = said(&mut app);
            assert!(labels.contains(&"Workspace access can't be checked".to_string()));
            assert!(
                !labels
                    .iter()
                    .any(|label| label.contains("Nothing here means nothing is granted"))
            );
        }

        {
            let mut app = window_over(Arc::new(Fixed::state(Some(r#"{"workspaces":[]}"#))));
            let labels = said(&mut app);
            assert!(labels.contains(&"No workspace access".to_string()));
            assert!(!labels.contains(&"Workspace access can't be checked".to_string()));
        }

        {
            assert!(project("not json {").is_err());
            let mut app = window_over(Arc::new(Fixed::state(Some("not json {"))));
            let mut rows = 0;
            count_grant_rows(app.harness().root_widget(), &mut rows);
            assert_eq!(rows, 0);
            assert!(said(&mut app).contains(&"Workspace access can't be checked".to_string()));
        }
    }

    /// The switch of an app's secrets says the access it grants, which the
    /// row's own caption (the app's name) does not: a control that names
    /// itself apart from its row keeps its words, and says how it stands.
    #[test]
    fn a_secrets_switch_keeps_the_words_it_says_apart_from_its_row() {
        let apps = vec![SecretApp {
            id: "binary:/usr/bin/chromium".into(),
            name: "chromium".into(),
            access: "allowed".into(),
            uses: 2,
            items: 1,
        }];
        let mut app = window_over(Arc::new(Fixed {
            secrets: Ok(apps),
            ..Fixed::state(None)
        }));
        let switch = crate::testing::switch_named(&mut app, &t("perm-secrets-access"));
        assert_eq!(switch.toggled(), Some(ui::accesskit::Toggled::True));
    }
}
