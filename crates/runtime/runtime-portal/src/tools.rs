//! Typed requests from sandboxed apps to shell-owned tools, over the `tools`
//! service. The wire cannot carry a program, a package id or an argv: a caller
//! chooses an intent, and the shell maps it to an installed package and builds
//! any command itself.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};

pub const TOOL_PROTOCOL_VERSION: u32 = crate::RUNTIME_INTERFACE_VERSION;
pub const TOOL_SERVICE_FD_ENV: &str = "TILO_SVC_TOOLS_FD";
pub const TOOL_CONTEXT_ENV: &str = "TILO_TOOL_CONTEXT";
pub const MAX_WIRE_BYTES: usize = 4 * 1024;
/// A reply may carry the drive list or the app list, so it gets more room than
/// a request.
pub const MAX_REPLY_BYTES: usize = MAX_LISTED_APPS * MAX_APP_ENTRY_BYTES + 16 * 1024;
pub const MAX_LISTED_DRIVES: usize = 32;
pub const MAX_LISTED_APPS: usize = 1024;
pub const MAX_APP_ENTRY_BYTES: usize = 2 * 1024;
pub const MAX_APP_ID_BYTES: usize = 512;
const MAX_DRIVE_ID_BYTES: usize = 64;
const MAX_PATH_BYTES: usize = 2 * 1024;
const MAX_CONTAINER_NAME_BYTES: usize = 255;
const TERMINAL_PACKAGE_ID: &str = "so.tilo.terminal";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerEngine {
    Podman,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceSettingsRoute {
    #[default]
    General,
    Members,
    PrivacySecurity,
    AppsFiles,
    Emoji,
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
    IdentityBackup,
    Terminal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        directory: Option<String>,
    },
    ContainerShell {
        container: String,
        engine: ContainerEngine,
    },
    Drives,
    /// Open an NTFS drive from [`ToolIntent::Drives`] in Files. The shell
    /// asks the person first unless they chose to remember the drive; the
    /// app learns nothing more than that the request was taken.
    OpenDrive {
        drive: String,
    },
    DocumentApps {
        path: String,
    },
    OpenDocument {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app: Option<String>,
    },
    /// List the person's apps, every kind in one list ([`ToolReply::applications`]).
    /// The shell answers only Files: it is the person's own view of them.
    Applications,
    OpenApplication {
        app: String,
    },
    UninstallApplication {
        app: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppRemoval {
    /// A Flatpak: uninstalled, and its data moved to the Trash unless the
    /// person keeps it.
    UninstallWithData,
    Uninstall,
    /// Part of the system: it can only be taken off the person's apps.
    RemoveFromApps,
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Tilo,
    Flatpak,
    AppImage,
    Program,
    #[default]
    System,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentApp {
    pub id: String,
    pub name: String,
    /// The glyph the app's own rows wear, by its Lucide name: the symbol the
    /// app has in search, the switcher and the tab strip, never a generic grid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Where this device got the app, said only where another app that opens
    /// the same document goes by the same name, so two rows never read alike.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<AppKind>,
    pub is_default: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveEntry {
    pub id: String,
    pub label: String,
    pub size_bytes: u64,
    pub internal: bool,
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
                if !is_path(directory) {
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
                if !is_app(app) {
                    return Err("invalid app".into());
                }
                Ok(())
            }
            Self::DocumentApps { path } => {
                if !is_path(path) {
                    return Err("invalid document path".into());
                }
                Ok(())
            }
            Self::OpenDocument { path, app } => {
                if !is_path(path) {
                    return Err("invalid document path".into());
                }
                if app.as_deref().is_some_and(|app| !is_app(app)) {
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

    pub fn package_id(&self) -> Option<&'static str> {
        match self {
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
            | Self::UninstallApplication { .. }
            | Self::DocumentApps { .. }
            | Self::OpenDocument { .. } => None,
        }
    }
}

fn is_app(app: &str) -> bool {
    !app.is_empty() && app.len() <= MAX_APP_ID_BYTES && !app.chars().any(char::is_control)
}

fn is_path(path: &str) -> bool {
    path.starts_with('/') && path.len() <= MAX_PATH_BYTES && !path.chars().any(char::is_control)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRequest {
    pub protocol: u32,
    pub workspace_id: String,
    pub intent: ToolIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
}

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drives: Vec<DriveEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applications: Vec<AppEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<DocumentApp>,
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

pub fn list_drives() -> Result<Vec<DriveEntry>, String> {
    send_tool_request(ToolIntent::Drives, None).map(|reply| reply.drives)
}

pub fn list_applications() -> Result<Vec<AppEntry>, String> {
    send_tool_request(ToolIntent::Applications, None).map(|reply| reply.applications)
}

pub fn list_document_apps(path: &str) -> Result<Vec<DocumentApp>, String> {
    send_tool_request(
        ToolIntent::DocumentApps {
            path: path.to_owned(),
        },
        None,
    )
    .map(|reply| reply.documents)
}

pub fn open_document(path: &str, app: Option<&str>) -> Result<(), String> {
    open_tool(
        ToolIntent::OpenDocument {
            path: path.to_owned(),
            app: app.map(str::to_owned),
        },
        None,
    )
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
    // Duplicate the inherited descriptor: dropping the per-request stream must not
    // close the process-wide service connection.
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

pub fn tool_intent_from_env() -> Option<ToolIntent> {
    let raw = std::env::var(TOOL_CONTEXT_ENV).ok()?;
    if raw.len() > MAX_WIRE_BYTES {
        return None;
    }
    // The launcher wrote this context; one that does not read is its error,
    // reported here, and the tool opens without it.
    let intent: ToolIntent = match serde_json::from_str(&raw) {
        Ok(intent) => intent,
        Err(error) => {
            eprintln!(
                "tilo-portal: the launcher's tool context does not decode, the tool opens without it: {error}"
            );
            return None;
        }
    };
    if let Err(error) = intent.validate() {
        eprintln!(
            "tilo-portal: the launcher's tool context is not valid, the tool opens without it: {error}"
        );
        return None;
    }
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
    fn document_intents_name_an_app_and_a_bounded_path() {
        let path = "/home/you/notes.pdf";
        let open = ToolIntent::OpenDocument {
            path: path.into(),
            app: None,
        };
        assert!(open.validate().is_ok());
        assert_eq!(
            open.package_id(),
            None,
            "the shell routes a document, no package opens it"
        );
        assert_eq!(
            serde_json::to_value(&open).unwrap(),
            serde_json::json!({ "tool": "open_document", "path": path })
        );
        let chosen = ToolIntent::OpenDocument {
            path: path.into(),
            app: Some("so.tilo.pdf-viewer".into()),
        };
        assert!(chosen.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&chosen).unwrap()["app"],
            "so.tilo.pdf-viewer"
        );
        let listed = ToolIntent::DocumentApps { path: path.into() };
        assert!(listed.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&listed).unwrap(),
            serde_json::json!({ "tool": "document_apps", "path": path })
        );
        for bad in [
            String::new(),
            "relative.pdf".to_string(),
            "/a\nb".to_string(),
            format!("/{}", "a".repeat(MAX_PATH_BYTES)),
        ] {
            assert!(
                ToolIntent::OpenDocument {
                    path: bad.clone(),
                    app: None
                }
                .validate()
                .is_err(),
                "{bad:?}"
            );
            assert!(
                ToolIntent::DocumentApps { path: bad }.validate().is_err(),
                "a listing is bounded like an open"
            );
        }
        assert!(
            ToolIntent::OpenDocument {
                path: path.into(),
                app: Some("a\nb".into())
            }
            .validate()
            .is_err()
        );
        let reply: ToolReply = serde_json::from_str(
            r#"{"accepted":true,"message":"listed","documents":[
                {"id":"so.tilo.pdf-viewer","name":"PDF Viewer","is_default":true},
                {"id":"org.gnome.Evince.desktop","name":"Evince","is_default":false}]}"#,
        )
        .unwrap();
        assert_eq!(reply.documents.len(), 2);
        assert!(reply.documents[0].is_default);
        assert!(reply.applications.is_empty(), "a list is not another list");
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
        {
            let intent = ToolIntent::Terminal { directory: None };
            assert!(intent.validate().is_ok());
            assert_eq!(intent.package_id(), Some("so.tilo.terminal"));
            assert_eq!(intent.terminal_argv(), None);
            let json = serde_json::to_value(&intent).unwrap();
            assert_eq!(json, serde_json::json!({ "tool": "terminal" }));
        }
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
        {
            for bad in [
                "relative/dir".to_string(),
                String::new(),
                "/tmp/a\nb".to_string(),
                "/tmp/a\0b".to_string(),
                format!("/{}", "a".repeat(MAX_PATH_BYTES)),
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
