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

use ui::tokens::Rhythm;

use crate::i18n::{t, t_args};
use crate::widgets::{
    El, Item, SettingsSection, StatusTone, card, cards, error_row, note, row, rows,
    semantic_status_pill, untitled, value_row,
};
use collab_types::caps::{Access, CapOverride, Effect, Expiry, Principal, Scope, Subject};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_actions::policy::{Grant, Level, Lifetime};
use ui::tilo_resource;
use ui::widget::{Button, Column, Row, Spacer, Text, Widget};

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
    /// Let an application use the saved passwords and keys again, or stop it.
    #[effect(
        job,
        idempotent,
        device_only,
        label = "Set an app's access to saved secrets"
    )]
    SetSecretsAccess {
        #[param("The application identity the secret store recorded")]
        app: String,
        #[param("Whether the application may use saved secrets")]
        allowed: bool,
    },
    /// Remove an application's saved secrets and its record.
    #[effect(job, device_only, label = "Forget an app's saved secrets")]
    ForgetSecrets {
        #[param("The application identity the secret store recorded")]
        app: String,
    },
}

/// Answer `effect` by opening the tool it names.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: PermissionsEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    match effect {
        PermissionsEffect::SetSecretsAccess { app, allowed } => {
            let verb = if allowed { "allow" } else { "revoke" };
            Ok(secret_command(cx, vec!["apps".into(), verb.into(), app]))
        }
        PermissionsEffect::ForgetSecrets { app } => Ok(secret_command(
            cx,
            vec!["apps".into(), "forget".into(), app],
        )),
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

/// Tilo's Secret Service, which keeps the saved passwords and keys of every
/// application and records which applications used it.
const SECRET_SERVICE: &str = "/usr/bin/tilo-secret-service";

fn secret_command(cx: &mut EffectCx<Settings, SettingsApp>, args: Vec<String>) -> Effected {
    cx.thread(
        move |_| {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            command_output(SECRET_SERVICE, &args).map(|_| ())
        },
        |_: &mut SettingsApp, done: Result<(), String>, _| done.map(|()| Effected::done()),
    )
}

/// One application the secret store recorded, as `apps list --json` says it.
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

/// The applications that use saved passwords and keys, each with the one
/// control that matters: stop it, or let it back, or forget it entirely.
/// Applications are named by what the system observed of them, never by a
/// name they chose.
fn secrets_card(cx: &PaneCx, apps: &Result<Vec<SecretApp>, String>) -> SettingsSection {
    let title = t("perm-secrets-title");
    let mut body: Vec<Item> = vec![note(&t("perm-secrets-intro"))];
    match apps {
        Err(_) => body.push(note(&t("perm-secrets-unavailable"))),
        Ok(apps) if apps.is_empty() => body.push(note(&t("perm-secrets-none"))),
        Ok(apps) => {
            for app in apps {
                let allowed = app.access == "allowed";
                body.push(row(Box::new(Text::new(&app.name))));
                body.push(note(&t_args(
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
                )));
                let toggle = if allowed {
                    t("perm-secrets-revoke")
                } else {
                    t("perm-secrets-allow")
                };
                body.push(row(Box::new(cx.bind_invocation(
                    Button::new(&format!("secrets-access:{}", app.id), &toggle),
                    PermissionsEffect::SetSecretsAccess {
                        app: app.id.clone(),
                        allowed: !allowed,
                    },
                ))));
                body.push(row(Box::new(cx.bind_invocation(
                    Button::new(
                        &format!("secrets-forget:{}", app.id),
                        &t("perm-secrets-forget"),
                    ),
                    PermissionsEffect::ForgetSecrets {
                        app: app.id.clone(),
                    },
                ))));
            }
        }
    }
    card(&title, rows(body))
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
    disk_encryption: String,
    secure_boot: String,
    /// The firewall in force, or why that could not be read.
    firewall: Result<String, String>,
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
fn disk_encryption_status() -> String {
    let Some(source) = command_stdout("findmnt", &["-no", "SOURCE", "/"]) else {
        return t("perm-unavailable");
    };
    let Some(chain) = command_stdout("lsblk", &["-s", "-n", "-o", "TYPE", &source]) else {
        return t("perm-unavailable");
    };
    if chain.lines().any(|line| line.trim() == "crypt") {
        t("perm-encrypted")
    } else {
        t("perm-not-encrypted")
    }
}

fn secure_boot_status() -> String {
    let Ok(entries) = std::fs::read_dir("/sys/firmware/efi/efivars") else {
        return t("perm-unavailable");
    };
    let Some(path) = entries.flatten().map(|e| e.path()).find(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("SecureBoot-"))
    }) else {
        return t("perm-disabled");
    };
    // EFI variable files start with four attribute bytes, followed by the
    // one-byte SecureBoot value.
    match std::fs::read(path).ok().and_then(|v| v.get(4).copied()) {
        Some(1) => t("perm-enabled"),
        Some(_) => t("perm-disabled"),
        None => t("perm-unavailable"),
    }
}

/// firewalld when it runs, otherwise the nftables ruleset. "Not active" is
/// only said when the ruleset was read and is empty; a ruleset that could
/// not be read (it needs privileges) is reported as unreadable.
fn firewall_status() -> Result<String, String> {
    firewall_from(command_output("firewall-cmd", &["--state"]), || {
        command_output("nft", &["list", "ruleset"])
    })
}

fn firewall_from(
    firewalld: Result<String, String>,
    nft: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    if firewalld.as_deref() == Ok("running") {
        return Ok(t("perm-firewall-firewalld"));
    }
    match nft() {
        Ok(ruleset) if !ruleset.trim().is_empty() => Ok(t("perm-firewall-nftables")),
        Ok(_) => Ok(t("perm-firewall-off")),
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
/// ignores every other workspace field; each `sandbox` entry is the real
/// [`CapOverride`] and each policy grant the real [`Grant`] the compositor
/// serialised, so the parse tracks the enforced shape rather than a copy of
/// it.
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
    #[serde(default)]
    policy: PolicyView,
}

#[derive(Deserialize, Default)]
struct PolicyView {
    #[serde(default)]
    grants: Vec<Grant>,
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

/// [`expiry_label`] for a grant's lifetime.
fn lifetime_label(lifetime: &Lifetime) -> &'static str {
    match lifetime {
        Lifetime::UntilRevoked => "Always",
        Lifetime::Until(_) | Lifetime::Task(_) => "Temporary",
        Lifetime::Session(_) => "Until log out",
        Lifetime::Once => "Once",
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
            let verb = if cap.access == Access::Write {
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
        _ => return None,
    };
    Some(CapView {
        kind,
        detail,
        expiry,
        tint,
    })
}

/// Project one grant naming an app into a display capability: another app's
/// action this app may use, which the consent sheet grants. Denials and
/// grants that ask are enforcement, not something the app holds.
fn grant_view(grant: &Grant) -> Option<CapView> {
    if grant.answer != ui::tilo_actions::policy::Answer::Allow {
        return None;
    }
    let action = grant.node.id_at(Level::Action)?;
    let resource = grant
        .node
        .id_at(Level::Resource)
        .or_else(|| grant.node.id_at(Level::Kind))
        .unwrap_or_default();
    let app = grant.node.id_at(Level::App).unwrap_or_default();
    Some(CapView {
        kind: "App action",
        detail: t_args(
            "perm-action-detail",
            &[
                ("action", action.into()),
                ("resource", resource.into()),
                ("app", app.into()),
            ],
        ),
        expiry: lifetime_label(&grant.lifetime),
        tint: Tint::AppAction,
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
        for cap in &ws.sandbox {
            let Subject::Principal(Principal::App(app)) = &cap.subject else {
                continue;
            };
            if let Some(view) = cap_view(cap) {
                by_app.entry(app.clone()).or_default().push(view);
            }
        }
        for grant in &ws.policy.grants {
            let ui::tilo_actions::policy::Subject::App(app) = &grant.subject else {
                continue;
            };
            if let Some(view) = grant_view(grant) {
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
fn cap_row(view: &CapView) -> Item {
    let kind = kind_shown(view.kind);
    let expiry = expiry_shown(view.expiry);
    // why: kind pill + detail + lifetime chip is not a label/control pair.
    row(Box::new(
        Row::new()
            .push(semantic_status_pill(&kind, view.tint.tone()))
            .push(Box::new(
                Text::new(&view.detail)
                    .with_role(ui::TextRole::Caption)
                    .dimmed(),
            ))
            .push(Box::new(Spacer::new()))
            .push(semantic_status_pill(&expiry, StatusTone::Neutral)),
    ))
}

/// One app's block within a workspace card: the app id, then its grants.
fn app_block(grants: &AppGrants) -> Vec<Item> {
    let mut items = vec![row(Box::new(
        Text::new(&grants.app).with_role(ui::TextRole::Body),
    ))];
    items.extend(grants.caps.iter().map(cap_row));
    items
}

pub fn build(cx: &PaneCx) -> Box<dyn Widget> {
    build_view(cx, load_text().as_deref())
}

/// Include the hosted disk-unlock model beside the device security summary.
pub(crate) fn build_with_disk_unlock(cx: &PaneCx, disk_unlock: El) -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_rhythm(Rhythm::Group)
            .push(build(cx))
            .push(disk_unlock),
    )
}

fn privacy_hardware_card(privacy: &os_actions::PrivacyStatus) -> SettingsSection {
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
    card(&t("perm-active-use"), rows(usage_rows))
}

fn device_security_card() -> SettingsSection {
    let security = device_security();
    card(
        &t("perm-device-security"),
        rows(vec![
            value_row(&t("perm-disk"), &security.disk_encryption),
            value_row(&t("perm-secure-boot"), &security.secure_boot),
            match &security.firewall {
                Ok(firewall) => value_row(&t("perm-firewall"), firewall),
                Err(error) => error_row(
                    &t("perm-firewall"),
                    &t_args("perm-firewall-error", &[("error", error.as_str().into())]),
                ),
            },
            note(&t("perm-security-caption")),
        ]),
    )
}

/// Establish the boundary before mixing the two related, but independently
/// owned, kinds of access below. System Settings reports this device's state;
/// collaborators, roles, and app access belong to the workspace that owns
/// them. Keeping the distinction in the first visible card prevents this
/// cross-workspace overview from looking like an editor for every workspace.
fn scope_card() -> SettingsSection {
    card(
        &t("perm-this-device"),
        rows(vec![note(&t("perm-this-device-body"))]),
    )
}

/// Intro card: what these grants are and where they come from.
fn workspace_grants_intro_card(cx: &PaneCx) -> SettingsSection {
    let open = t("perm-open-workspace");
    card(
        &t("perm-workspace-intro-title"),
        rows(vec![
            note(&t("perm-workspace-intro-body")),
            row(Box::new(cx.bind_invocation(
                Button::new("open-workspace-privacy", &open),
                PermissionsEffect::OpenWorkspacePrivacy,
            ))),
        ]),
    )
}

/// Empty state — a successfully loaded workspace record contains no
/// app grants.
fn no_workspace_grants_card() -> SettingsSection {
    card(
        &t("perm-no-grants-title"),
        rows(vec![
            note(&t("perm-no-grants-body")),
            note(&t("perm-no-grants-caption")),
        ]),
    )
}

/// Loading workspace state is distinct from successfully reading a workspace
/// that has no app grants. A privacy surface must never turn an I/O or parse
/// failure into a claim that access is absent.
fn workspace_grants_unavailable_card() -> SettingsSection {
    card(
        &t("perm-unavailable-title"),
        rows(vec![
            note(&t("perm-unavailable-body")),
            note(&t("perm-unavailable-caption")),
        ]),
    )
}

/// A quiet summary line above the per-workspace cards, followed by one card
/// per workspace with a grant.
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
        panel.push(card(&title, rows(body)));
    }
}

/// Where the compositor enforces the grants.
fn enforcement_card() -> SettingsSection {
    card(
        &t("perm-enforcement-title"),
        rows(vec![note(&t("perm-enforcement-caption"))]),
    )
}

/// Core builder, split from [`build`] so it can be exercised against an
/// explicit state document in tests without touching the real config dir.
fn build_view(cx: &PaneCx, state_json: Option<&str>) -> Box<dyn Widget> {
    let workspace_access = state_json.map(project).transpose();

    let mut panel: Vec<SettingsSection> = Vec::new();

    let privacy = os_actions::get_privacy_status();
    panel.push(scope_card());
    panel.push(privacy_hardware_card(&privacy));
    panel.push(active_privacy_use_card(&privacy));
    panel.push(device_security_card());
    panel.push(secrets_card(cx, &secret_apps()));
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
mod tests {
    use super::*;
    use crate::host::testing::{count_named, labels_of};

    fn cx() -> PaneCx {
        PaneCx::in_file(std::env::temp_dir().join("tilo-settings-permissions-unused.json"))
    }
    use collab_types::caps::{Access, CapOverride, Scope, Subject};
    use std::collections::HashMap;

    #[test]
    fn the_secret_store_listing_is_read_as_the_service_prints_it() {
        let json = r#"[{"id":"binary:/usr/bin/chromium","name":"chromium","access":"allowed","first_seen":1,"last_used":2,"uses":9,"items":3},
                       {"id":"local-process:/home/a/t","name":"t (not part of the system)","access":"revoked","uses":1,"items":0}]"#;
        let apps = parse_secret_apps(json).unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].items, 3);
        assert_eq!(apps[1].access, "revoked");
        assert!(parse_secret_apps("not json").is_err());
        assert_eq!(parse_secret_apps("[]").unwrap(), Vec::new());
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
        let mut card = cards(vec![secrets_card(&cx(), &Ok(apps))]);
        build_layout_of(&mut card);
        let mut labels = Vec::new();
        button_labels(card.as_ref(), &mut labels);
        assert_eq!(
            labels,
            vec![
                t("perm-secrets-revoke"),
                t("perm-secrets-forget"),
                t("perm-secrets-allow"),
                t("perm-secrets-forget")
            ]
        );
        // A store that cannot be reached is said so, never shown as empty.
        let mut unreachable = cards(vec![secrets_card(&cx(), &Err("no bus".into()))]);
        build_layout_of(&mut unreachable);
        let mut said = Vec::new();
        text_labels(unreachable.as_ref(), &mut said);
        assert!(said.contains(&t("perm-secrets-unavailable")));
    }

    /// "Not active" is only said about a ruleset that was read and is empty.
    /// A ruleset this session may not read (nft needs privileges) and no
    /// running firewalld is not a firewall that is off.
    #[test]
    fn an_unreadable_firewall_is_not_reported_as_off() {
        let denied = || Err("nft: Operation not permitted (you must be root)".to_string());
        assert_eq!(
            firewall_from(Ok("running".into()), denied),
            Ok(t("perm-firewall-firewalld"))
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
            Ok(t("perm-firewall-off"))
        );
        assert_eq!(
            firewall_from(Err("firewall-cmd: not running".into()), || {
                Ok("table inet filter {}".into())
            }),
            Ok(t("perm-firewall-nftables"))
        );
    }
    use taffy::{NodeId, TaffyTree};
    use ui::Theme;

    /// A real state.json fragment: one workspace, one app holding a camera
    /// grant and a read-only file grant. Built by serialising the actual
    /// `CapOverride` type so the fixture matches the compositor's on-disk
    /// shape exactly.
    fn sample_state() -> String {
        let cam = CapOverride::grant(
            Subject::app("so.tilo.camera"),
            Scope::Device("camera".into()),
            Access::Read,
        );
        let file = CapOverride::grant(
            Subject::app("so.tilo.camera"),
            Scope::FsPath("/var/workspace/Photos".into()),
            Access::Read,
        );
        let caps = serde_json::to_string(&vec![cam, file]).unwrap();
        format!(r#"{{"workspaces":[{{"name":"Studio","sandbox":{caps}}}]}}"#)
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
            labels_of(pane.as_ref(), "Button", &mut labels);
            assert_eq!(labels, vec!["Open workspace access"]);
        }

        // Case `cap_view_maps_each_scope_to_its_kind`.
        {
            let g = |scope, verb| {
                cap_view(&CapOverride::grant(Subject::app("a"), scope, verb)).map(|v| v.kind)
            };
            assert_eq!(
                g(Scope::Device("camera".into()), Access::Read),
                Some("Camera")
            );
            assert_eq!(
                g(Scope::Device("microphone".into()), Access::Read),
                Some("Microphone")
            );
            assert_eq!(
                g(Scope::Device("screencast".into()), Access::Read),
                Some("Screen sharing")
            );
            assert_eq!(
                g(Scope::Device("remote-desktop".into()), Access::Write),
                Some("Remote control")
            );
            assert_eq!(
                g(Scope::NetHost("*".into()), Access::Write),
                Some("Network")
            );
            let action = grant_view(&Grant::new(
                "shortcuts-freeform",
                ui::tilo_actions::policy::Subject::App("so.tilo.shortcuts".into()),
                ui::tilo_actions::policy::TreePath::room("studio")
                    .app("so.tilo.agent")
                    .any(Level::Kind)
                    .resource("agent.transform:freeform")
                    .action("freeform"),
                ui::tilo_actions::policy::KindSet::ALL,
                ui::tilo_actions::policy::Answer::Allow,
                ui::tilo_actions::policy::Subject::person("owner"),
            ))
            .unwrap();
            assert_eq!(action.kind, "App action");
            assert!(
                action.detail.contains("freeform")
                    && action.detail.contains("agent.transform:freeform")
                    && action.detail.contains("so.tilo.agent"),
                "{}",
                action.detail
            );
            // Read vs write is reflected in the file detail.
            let rw = cap_view(&CapOverride::grant(
                Subject::app("a"),
                Scope::FsPath("/w".into()),
                Access::Write,
            ))
            .unwrap();
            assert!(rw.detail.contains("read & write"));
            // A restriction (not a grant) renders nothing.
            assert!(
                cap_view(&CapOverride::new(
                    Subject::app("a"),
                    Scope::Device("camera".into()),
                    Effect::Restrict,
                    Access::Read,
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
            assert!(count_named(pane.as_ref(), "FormSection") >= 3);
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
            labels_of(pane.as_ref(), "Text", &mut labels);
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
            labels_of(pane.as_ref(), "Text", &mut labels);
            assert!(labels.contains(&"No workspace access".to_string()));
            assert!(!labels.contains(&"Workspace access can't be checked".to_string()));
        }

        // Case `workspace_without_app_grants_is_omitted`.
        {
            // A workspace whose only grant names a person holds no *app*
            // grant and must not render a card.
            let person = Grant::new(
                "alice",
                ui::tilo_actions::policy::Subject::person("alice"),
                ui::tilo_actions::policy::TreePath::room("room-1"),
                ui::tilo_actions::policy::KindSet::ALL,
                ui::tilo_actions::policy::Answer::Allow,
                ui::tilo_actions::policy::Subject::person("owner"),
            );
            let grants = serde_json::to_string(&vec![person]).unwrap();
            let json =
                format!(r#"{{"workspaces":[{{"name":"Empty","policy":{{"grants":{grants}}}}}]}}"#);
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
            labels_of(pane.as_ref(), "Text", &mut labels);
            assert!(labels.contains(&"Workspace access can't be checked".to_string()));
        }
    }
}
