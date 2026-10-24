//! Typed requests from sandboxed apps to shell-owned tools, over the
//! `tools` service.
//!
//! The wire deliberately cannot carry a program, package id, or argv. A caller
//! chooses from this enum; the shell maps it to an installed Tilo package and
//! validates the contextual data before launching anything. Intents that run
//! a command in Terminal carry only typed operands; the argv is built here, by
//! [`ToolIntent::terminal_argv`], from fixed programs.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};

/// The `tools` service is part of the runtime interface and speaks its version.
pub const TOOL_PROTOCOL_VERSION: u32 = crate::RUNTIME_INTERFACE_VERSION;
pub const TOOL_SERVICE_FD_ENV: &str = "TILO_SVC_TOOLS_FD";
pub const TOOL_CONTEXT_ENV: &str = "TILO_TOOL_CONTEXT";
const MAX_WIRE_BYTES: usize = 4 * 1024;
/// A reply may carry the drive list or the app list, so it gets more room
/// than a request: the most apps one reply lists, each at its largest.
pub const MAX_REPLY_BYTES: usize = MAX_LISTED_APPS * MAX_APP_ENTRY_BYTES + 16 * 1024;
/// The most drives one reply lists.
pub const MAX_LISTED_DRIVES: usize = 32;
/// The most apps one reply lists ([`ToolIntent::Applications`]).
pub const MAX_LISTED_APPS: usize = 1024;
/// The most one listed app takes on the wire: its id, name, icon and
/// program, each bounded by the shell before it is listed.
pub const MAX_APP_ENTRY_BYTES: usize = 2 * 1024;
/// The longest app id an app may name ([`ToolIntent::UninstallApplication`]).
pub const MAX_APP_ID_BYTES: usize = 512;
const MAX_DRIVE_ID_BYTES: usize = 64;
const MAX_DIRECTORY_BYTES: usize = 2 * 1024;
const MAX_CONTAINER_NAME_BYTES: usize = 255;
const TERMINAL_PACKAGE_ID: &str = "so.tilo.terminal";

/// The engine that owns a container a Terminal shell attaches to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerEngine {
    /// `podman exec` an interactive `/bin/sh` in a running container.
    Podman,
    /// `distrobox enter` a distrobox.
    Distrobox,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpnRoute {
    #[default]
    Overview,
    Profile,
    DnsRouting,
    Import,
}

/// Safe destinations inside the firewall tool. Settings may show status and
/// the emergency on/off posture, while editing zones and rules stays in the
/// dedicated expert workspace.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirewallRoute {
    #[default]
    Overview,
    Zones,
    Rules,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisksRoute {
    #[default]
    Overview,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupsRoute {
    #[default]
    Overview,
    Restore,
}

/// Safe destinations inside the shell-owned workspace lens. Unlike a Studio,
/// this does not launch a package: the shell binds the request to the
/// requester's concrete workspace before revealing its governance controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSettingsRoute {
    #[default]
    General,
    Members,
    PrivacySecurity,
    AppsFiles,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum ToolIntent {
    Vpn {
        #[serde(default)]
        route: VpnRoute,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
    },
    Firewall {
        #[serde(default)]
        route: FirewallRoute,
    },
    Disks {
        #[serde(default)]
        route: DisksRoute,
    },
    Backups {
        #[serde(default)]
        route: BackupsRoute,
    },
    WorkspaceSettings {
        #[serde(default)]
        route: WorkspaceSettingsRoute,
    },
    /// Open the trusted shell flow that creates and verifies an encrypted
    /// portable copy of the active personal identity.
    IdentityBackup,
    /// Open Terminal, with its shell started in `directory` when given and
    /// visible to Terminal.
    Terminal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        directory: Option<String>,
    },
    /// Open Terminal attached to an interactive shell in a container.
    ContainerShell {
        container: String,
        engine: ContainerEngine,
    },
    /// List the NTFS drives this computer can open ([`ToolReply::drives`]).
    Drives,
    /// Open an NTFS drive from [`ToolIntent::Drives`] in Files. The shell
    /// asks the person first unless they chose to remember the drive; the
    /// app learns nothing more than that the request was taken.
    OpenDrive { drive: String },
    /// List the person's apps, every kind in one list ([`ToolReply::applications`]).
    /// The shell answers only Files: it is the person's own view of them.
    Applications,
    /// Open an app from [`ToolIntent::Applications`].
    OpenApplication { app: String },
    /// Uninstall an app from [`ToolIntent::Applications`], or take a part of
    /// the system off the person's apps. The shell asks the person first, in
    /// its own sheet; the app learns nothing more than that the request was
    /// taken.
    UninstallApplication { app: String },
}

/// What uninstalling a listed app does, as the person is asked about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppRemoval {
    /// A Flatpak: uninstalled, and its data moved to the Trash unless the
    /// person keeps it.
    UninstallWithData,
    /// Uninstalled; nothing of the person's is touched (a Tilo app), or the
    /// program file goes to the Trash (a downloaded program).
    Uninstall,
    /// Part of the system: it can only be taken off the person's apps.
    RemoveFromApps,
    /// Neither: an app the system image ships.
    None,
}

/// Where a listed app came from, as the person knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    /// A Tilo package.
    Tilo,
    Flatpak,
    /// A downloaded `AppImage`.
    AppImage,
    /// Another downloaded program, or a menu entry the person made.
    Program,
    /// A menu entry of the operating system.
    #[default]
    System,
}

/// One app as an app lists it ([`ToolIntent::Applications`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    /// The id [`ToolIntent::OpenApplication`] and
    /// [`ToolIntent::UninstallApplication`] take.
    pub id: String,
    pub name: String,
    /// A Tilo app's package id, for its own glyph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// The icon file, an absolute path, when the app has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// The program file of a downloaded program, an absolute path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    pub removal: AppRemoval,
    #[serde(default)]
    pub kind: AppKind,
    /// What it takes on disk, in bytes, where that is known: a Flatpak's
    /// installed files, a downloaded program's file, a Tilo app's bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl AppEntry {
    /// Whether every field fits the bound [`MAX_APP_ENTRY_BYTES`] sets.
    #[must_use]
    pub fn fits(&self) -> bool {
        let optional = |field: &Option<String>| field.as_ref().map_or(0, String::len);
        let total = [
            self.id.len(),
            self.name.len(),
            optional(&self.package),
            optional(&self.icon),
            optional(&self.program),
            128,
        ]
        .into_iter()
        .fold(0_usize, usize::saturating_add);
        self.id.len() <= MAX_APP_ID_BYTES && total <= MAX_APP_ENTRY_BYTES
    }
}

/// An NTFS drive as an app lists it: what to call it and whether it is open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveEntry {
    /// The id [`ToolIntent::OpenDrive`] takes.
    pub id: String,
    /// The volume label; empty when the drive has none.
    pub label: String,
    pub size_bytes: u64,
    /// Built into this computer, as opposed to external or removable.
    pub internal: bool,
    /// Where it is open, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_at: Option<String>,
}

impl ToolIntent {
    /// Check that this intent's contextual data is safe to hand to the shell.
    ///
    /// # Errors
    ///
    /// Returns an error message if a route that requires a VPN profile name has none, or the
    /// supplied profile name is empty, too long, or contains control characters; if a Terminal
    /// directory is not a bounded absolute path free of control characters; or if a container
    /// name is empty, too long, starts with `-`, or has characters outside `[A-Za-z0-9_.-]`.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Vpn { route, profile } => {
                if matches!(route, VpnRoute::Profile | VpnRoute::DnsRouting)
                    && profile.as_deref().is_none_or(str::is_empty)
                {
                    return Err("this VPN route requires a profile name".into());
                }
                if let Some(name) = profile {
                    let trimmed = name.trim();
                    if trimmed.is_empty()
                        || trimmed.len() > 255
                        || trimmed.chars().any(char::is_control)
                    {
                        return Err("invalid VPN profile name".into());
                    }
                }
                Ok(())
            }
            Self::Terminal {
                directory: Some(directory),
            } => {
                if !directory.starts_with('/')
                    || directory.len() > MAX_DIRECTORY_BYTES
                    || directory.chars().any(char::is_control)
                {
                    return Err("invalid Terminal directory".into());
                }
                Ok(())
            }
            Self::ContainerShell { container, .. } => {
                if container.is_empty()
                    || container.len() > MAX_CONTAINER_NAME_BYTES
                    || container.starts_with('-')
                    || !container
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
                {
                    return Err("invalid container name".into());
                }
                Ok(())
            }
            Self::OpenDrive { drive } => {
                if drive.is_empty()
                    || drive.len() > MAX_DRIVE_ID_BYTES
                    || !drive
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'-'))
                {
                    return Err("invalid drive".into());
                }
                Ok(())
            }
            Self::OpenApplication { app } | Self::UninstallApplication { app } => {
                if app.is_empty()
                    || app.len() > MAX_APP_ID_BYTES
                    || app.chars().any(char::is_control)
                {
                    return Err("invalid app".into());
                }
                Ok(())
            }
            Self::Drives
            | Self::Applications
            | Self::Firewall { .. }
            | Self::Disks { .. }
            | Self::Backups { .. }
            | Self::WorkspaceSettings { .. }
            | Self::IdentityBackup
            | Self::Terminal { directory: None } => Ok(()),
        }
    }

    /// The command Terminal runs for this intent, or `None` when the intent
    /// opens its package plainly. Only fixed programs appear here; the
    /// intent's validated operands are passed as separate arguments, never
    /// spliced into a shell script.
    pub fn terminal_argv(&self) -> Option<Vec<String>> {
        match self {
            Self::Terminal {
                directory: Some(directory),
            } => Some(vec![
                "sh".into(),
                "-c".into(),
                r#"cd -- "$1" 2>/dev/null; exec "${SHELL:-/bin/sh}" -l"#.into(),
                "sh".into(),
                directory.clone(),
            ]),
            Self::ContainerShell {
                container,
                engine: ContainerEngine::Podman,
            } => Some(vec![
                "podman".into(),
                "exec".into(),
                "-it".into(),
                container.clone(),
                "/bin/sh".into(),
            ]),
            Self::ContainerShell {
                container,
                engine: ContainerEngine::Distrobox,
            } => Some(vec!["distrobox".into(), "enter".into(), container.clone()]),
            _ => None,
        }
    }

    /// Installed package for a specialist tool, or `None` for a shell-owned
    /// contextual surface.
    pub fn package_id(&self) -> Option<&'static str> {
        match self {
            // VPN lives in Settings → Network.
            Self::Vpn { .. } => Some("so.tilo.settings"),
            Self::Firewall { .. } => Some("so.tilo.firewall"),
            Self::Disks { .. } => Some("so.tilo.disks"),
            Self::Backups { .. } => Some("so.tilo.backups"),
            Self::Terminal { .. } | Self::ContainerShell { .. } => Some(TERMINAL_PACKAGE_ID),
            Self::WorkspaceSettings { .. }
            | Self::IdentityBackup
            | Self::Drives
            | Self::OpenDrive { .. }
            | Self::Applications
            | Self::OpenApplication { .. }
            | Self::UninstallApplication { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRequest {
    pub protocol: u32,
    pub workspace_id: String,
    pub intent: ToolIntent,
    /// The broker invocation the app makes this request for, when one the
    /// broker delivered is running (an agent asked the app to open another
    /// one): its id as 32 lowercase hex digits. The shell admits the request
    /// as that invocation's caller; without it, only the person's own press
    /// in the app is behind the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
}

/// The wire form of [`ToolRequest::on_behalf_of`].
#[must_use]
pub fn invocation_hex(invocation: [u8; 16]) -> String {
    use std::fmt::Write as _;
    invocation
        .iter()
        .fold(String::with_capacity(32), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReply {
    pub accepted: bool,
    pub message: String,
    /// The answer to [`ToolIntent::Drives`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drives: Vec<DriveEntry>,
    /// The answer to [`ToolIntent::Applications`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applications: Vec<AppEntry>,
}

/// Ask the current shell session to open a known tool at a typed context.
///
/// # Errors
///
/// Returns an error message if `intent` fails [`ToolIntent::validate`], the process is not
/// attached to a Tilo workspace or its tool service, the request is too large to send, or the
/// shell rejects or fails to answer the request.
///
/// `on_behalf_of` is the broker invocation the request is made for
/// ([`ToolRequest::on_behalf_of`]): `ui::invocation_scope::delivered_invocation`
/// read where the app decided to ask, `None` for the person's own press.
pub fn open_tool(intent: ToolIntent, on_behalf_of: Option<[u8; 16]>) -> Result<(), String> {
    intent.validate()?;
    send_tool_request(intent, on_behalf_of).map(drop)
}

/// The NTFS drives the shell can open, for an app's list of places.
///
/// # Errors
///
/// As [`open_tool`].
pub fn list_drives() -> Result<Vec<DriveEntry>, String> {
    send_tool_request(ToolIntent::Drives, None).map(|reply| reply.drives)
}

/// The person's apps, for Files' Applications place.
///
/// # Errors
///
/// As [`open_tool`].
pub fn list_applications() -> Result<Vec<AppEntry>, String> {
    send_tool_request(ToolIntent::Applications, None).map(|reply| reply.applications)
}

#[cfg(not(unix))]
fn send_tool_request(
    _intent: ToolIntent,
    _on_behalf_of: Option<[u8; 16]>,
) -> Result<ToolReply, String> {
    Err("The shell tool service is unavailable".to_string())
}

#[cfg(unix)]
fn send_tool_request(
    intent: ToolIntent,
    on_behalf_of: Option<[u8; 16]>,
) -> Result<ToolReply, String> {
    let workspace_id = std::env::var("TILO_WORKSPACE_ID")
        .map_err(|_| "This app is not attached to a Tilo workspace".to_string())?;
    let raw_fd: i32 = std::env::var(TOOL_SERVICE_FD_ENV)
        .map_err(|_| "The shell tool service is unavailable".to_string())?
        .parse()
        .map_err(|_| "The shell supplied an invalid tool service descriptor".to_string())?;

    let request = ToolRequest {
        protocol: TOOL_PROTOCOL_VERSION,
        workspace_id,
        intent,
        on_behalf_of: on_behalf_of.map(invocation_hex),
    };
    // SAFETY: the launcher keeps the service descriptor open for the
    // process's lifetime; it is only borrowed for this exchange.
    let reply = exchange(unsafe { BorrowedFd::borrow_raw(raw_fd) }, &request)?;
    if reply.accepted {
        Ok(reply)
    } else {
        Err(reply.message)
    }
}

/// One request and its reply over the process-wide service stream.
///
/// Every caller shares that one stream and replies carry no request id, so
/// the whole exchange is one critical section: otherwise concurrent callers
/// interleave frames or read each other's replies.
#[cfg(unix)]
fn exchange(service: BorrowedFd<'_>, request: &ToolRequest) -> Result<ToolReply, String> {
    use tilo_lock::MutexRecover as _;
    static EXCHANGE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    let mut encoded =
        serde_json::to_vec(request).map_err(|e| format!("encoding tool request: {e}"))?;
    if encoded.len() > MAX_WIRE_BYTES {
        return Err("tool request is too large".into());
    }
    encoded.push(b'\n');
    let _exchange = EXCHANGE.lock_recover();
    // Duplicate the inherited descriptor: dropping the per-request stream must
    // not close the process-wide service connection retained by the sandbox.
    let owned = service
        .try_clone_to_owned()
        .map_err(|e| format!("opening the shell tool service: {e}"))?;
    let mut stream = UnixStream::from(owned);
    stream
        .write_all(&encoded)
        .map_err(|e| format!("sending tool request: {e}"))?;

    let mut reply = String::new();
    BufReader::new(stream)
        .take((MAX_REPLY_BYTES as u64).saturating_add(1))
        .read_line(&mut reply)
        .map_err(|e| format!("reading tool reply: {e}"))?;
    if reply.len() > MAX_REPLY_BYTES {
        return Err("tool reply is too large".into());
    }
    serde_json::from_str(&reply).map_err(|e| format!("decoding tool reply: {e}"))
}

/// Context passed by the trusted launcher to a newly opened tool.
pub fn tool_intent_from_env() -> Option<ToolIntent> {
    let raw = std::env::var(TOOL_CONTEXT_ENV).ok()?;
    if raw.len() > MAX_WIRE_BYTES {
        return None;
    }
    let intent: ToolIntent = serde_json::from_str(&raw).ok()?;
    intent.validate().ok()?;
    Some(intent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn concurrent_callers_each_get_their_own_reply() {
        use std::os::fd::AsFd as _;
        let (client, server) = UnixStream::pair().unwrap();
        let shell = std::thread::spawn(move || {
            let mut answer = server.try_clone().unwrap();
            for line in BufReader::new(server).lines() {
                let request: ToolRequest = serde_json::from_str(&line.unwrap()).unwrap();
                let reply = ToolReply {
                    accepted: true,
                    message: request.workspace_id,
                    ..ToolReply::default()
                };
                let mut encoded = serde_json::to_vec(&reply).unwrap();
                encoded.push(b'\n');
                answer.write_all(&encoded).unwrap();
            }
        });
        std::thread::scope(|scope| {
            for caller in 0..8 {
                let client = &client;
                scope.spawn(move || {
                    for turn in 0..200 {
                        let request = ToolRequest {
                            protocol: TOOL_PROTOCOL_VERSION,
                            workspace_id: format!("{caller}-{turn}"),
                            intent: ToolIntent::Drives,
                            on_behalf_of: None,
                        };
                        let reply = exchange(client.as_fd(), &request).unwrap();
                        assert_eq!(reply.message, request.workspace_id);
                    }
                });
            }
        });
        drop(client);
        shell.join().unwrap();
    }

    #[test]
    fn tool_intents_name_only_typed_routes_never_executables() {
        // Phase `protocol_cannot_name_an_executable_or_package`.
        {
            let json = serde_json::to_value(ToolIntent::Vpn {
                route: VpnRoute::Profile,
                profile: Some("Work VPN".into()),
            })
            .unwrap();
            assert_eq!(json["tool"], "vpn");
            assert!(json.get("program").is_none());
            assert!(json.get("package_id").is_none());
            assert!(json.get("argv").is_none());
        }
        // Phase `profile_route_requires_a_safe_name`.
        {
            assert!(
                ToolIntent::Vpn {
                    route: VpnRoute::Profile,
                    profile: None
                }
                .validate()
                .is_err()
            );
            assert!(
                ToolIntent::Vpn {
                    route: VpnRoute::Profile,
                    profile: Some("bad\nname".into())
                }
                .validate()
                .is_err()
            );
        }
        // Phase `firewall_intent_selects_only_a_typed_route`.
        {
            let intent = ToolIntent::Firewall {
                route: FirewallRoute::Rules,
            };
            assert_eq!(intent.package_id(), Some("so.tilo.firewall"));
            let json = serde_json::to_value(intent).unwrap();
            assert_eq!(json["tool"], "firewall");
            assert_eq!(json["route"], "rules");
            assert!(json.get("program").is_none());
        }
        // Phase `workspace_settings_is_a_shell_route_not_an_executable`.
        {
            let intent = ToolIntent::WorkspaceSettings {
                route: WorkspaceSettingsRoute::PrivacySecurity,
            };
            assert_eq!(intent.package_id(), None);
            let json = serde_json::to_value(intent).unwrap();
            assert_eq!(json["tool"], "workspace_settings");
            assert_eq!(json["route"], "privacy_security");
            assert!(json.get("program").is_none());
            assert!(json.get("package_id").is_none());
        }
        // Phase `identity_backup_is_a_shell_owned_typed_intent`.
        {
            let intent = ToolIntent::IdentityBackup;
            assert_eq!(intent.package_id(), None);
            assert!(intent.validate().is_ok());
            let json = serde_json::to_value(intent).unwrap();
            assert_eq!(json["tool"], "identity_backup");
            assert!(json.get("program").is_none());
        }
    }

    #[test]
    fn drive_intents_are_shell_routes_with_a_bounded_id() {
        let open = ToolIntent::OpenDrive {
            drive: "nvme0n1p3:1".into(),
        };
        assert!(open.validate().is_ok());
        assert_eq!(
            open.package_id(),
            None,
            "the shell asks, no package opens it"
        );
        assert_eq!(
            serde_json::to_value(&open).unwrap(),
            serde_json::json!({ "tool": "open_drive", "drive": "nvme0n1p3:1" })
        );
        for bad in ["", "../sda", "sda 1", &"a".repeat(MAX_DRIVE_ID_BYTES + 1)] {
            assert!(
                ToolIntent::OpenDrive { drive: bad.into() }
                    .validate()
                    .is_err(),
                "{bad:?}"
            );
        }
        assert!(ToolIntent::Drives.validate().is_ok());
        let reply: ToolReply =
            serde_json::from_str(r#"{"accepted":true,"message":"opened"}"#).unwrap();
        assert!(
            reply.drives.is_empty(),
            "replies without a list still parse"
        );
    }

    #[test]
    fn terminal_intents_carry_typed_operands_and_build_fixed_commands() {
        // Phase `plain_terminal_opens_the_package_without_a_command`.
        {
            let intent = ToolIntent::Terminal { directory: None };
            assert!(intent.validate().is_ok());
            assert_eq!(intent.package_id(), Some("so.tilo.terminal"));
            assert_eq!(intent.terminal_argv(), None);
            let json = serde_json::to_value(&intent).unwrap();
            assert_eq!(json, serde_json::json!({ "tool": "terminal" }));
        }
        // Phase `directory_is_an_operand_not_part_of_the_script`.
        {
            let directory = "/home/u/it's \"odd\"; rm -rf ~";
            let intent = ToolIntent::Terminal {
                directory: Some(directory.into()),
            };
            assert!(intent.validate().is_ok());
            let argv = intent.terminal_argv().unwrap();
            assert_eq!(argv[..2], ["sh", "-c"]);
            assert!(!argv[2].contains(directory));
            assert_eq!(argv[3], "sh");
            assert_eq!(argv[4], directory);
            assert_eq!(argv.len(), 5);
        }
        // Phase `terminal_directory_must_be_a_bounded_absolute_path`.
        {
            for bad in [
                "relative/dir".to_string(),
                String::new(),
                "/tmp/a\nb".to_string(),
                "/tmp/a\0b".to_string(),
                format!("/{}", "a".repeat(MAX_DIRECTORY_BYTES)),
            ] {
                assert!(
                    ToolIntent::Terminal {
                        directory: Some(bad.clone())
                    }
                    .validate()
                    .is_err(),
                    "{bad:?}"
                );
            }
        }
        // Phase `container_shells_use_the_engine_cli_with_the_name_as_one_argument`.
        {
            let podman = ToolIntent::ContainerShell {
                container: "web-1.prod_a".into(),
                engine: ContainerEngine::Podman,
            };
            assert!(podman.validate().is_ok());
            assert_eq!(podman.package_id(), Some("so.tilo.terminal"));
            assert_eq!(
                podman.terminal_argv().unwrap(),
                ["podman", "exec", "-it", "web-1.prod_a", "/bin/sh"]
            );
            let distrobox = ToolIntent::ContainerShell {
                container: "fedora".into(),
                engine: ContainerEngine::Distrobox,
            };
            assert_eq!(
                distrobox.terminal_argv().unwrap(),
                ["distrobox", "enter", "fedora"]
            );
            let json = serde_json::to_value(&distrobox).unwrap();
            assert_eq!(json["tool"], "container_shell");
            assert_eq!(json["engine"], "distrobox");
            assert!(json.get("argv").is_none());
        }
        // Phase `container_names_cannot_become_options_or_shell_syntax`.
        {
            for bad in [
                String::new(),
                "--privileged".to_string(),
                "-x".to_string(),
                "a b".to_string(),
                "a;b".to_string(),
                "a/b".to_string(),
                "a\nb".to_string(),
                "é".to_string(),
                "a".repeat(MAX_CONTAINER_NAME_BYTES + 1),
            ] {
                assert!(
                    ToolIntent::ContainerShell {
                        container: bad.clone(),
                        engine: ContainerEngine::Podman,
                    }
                    .validate()
                    .is_err(),
                    "{bad:?}"
                );
            }
        }
    }
}
