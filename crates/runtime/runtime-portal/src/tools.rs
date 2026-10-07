//! Typed requests from sandboxed apps to shell-owned tools, over the
//! `tools` service.
//!
//! The wire deliberately cannot carry a program, package id, or argv. A caller
//! chooses from this enum; the shell maps it to an installed Tilo package and
//! validates the contextual data before launching anything. Intents that run
//! a command in Terminal carry only typed operands; the argv is built here, by
//! [`ToolIntent::terminal_argv`], from fixed programs.

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::fd::BorrowedFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};

pub const TOOL_PROTOCOL_VERSION: u32 = 1;
pub const TOOL_SERVICE_FD_ENV: &str = "TILO_SVC_TOOLS_FD";
pub const TOOL_CONTEXT_ENV: &str = "TILO_TOOL_CONTEXT";
const MAX_WIRE_BYTES: usize = 4 * 1024;
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
            Self::Firewall { .. }
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
            Self::Vpn { .. } => Some("so.tilo.vpn"),
            Self::Firewall { .. } => Some("so.tilo.firewall"),
            Self::Disks { .. } => Some("so.tilo.disks"),
            Self::Backups { .. } => Some("so.tilo.backups"),
            Self::Terminal { .. } | Self::ContainerShell { .. } => Some(TERMINAL_PACKAGE_ID),
            Self::WorkspaceSettings { .. } | Self::IdentityBackup => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRequest {
    pub protocol: u32,
    pub workspace_id: String,
    pub intent: ToolIntent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolReply {
    pub accepted: bool,
    pub message: String,
}

/// Ask the current shell session to open a known tool at a typed context.
///
/// # Errors
///
/// Returns an error message if `intent` fails [`ToolIntent::validate`], the process is not
/// attached to a Tilo workspace or its tool service, the request is too large to send, or the
/// shell rejects or fails to answer the request.
pub fn open_tool(intent: ToolIntent) -> Result<(), String> {
    intent.validate()?;
    send_tool_request(intent)
}

#[cfg(not(unix))]
fn send_tool_request(_intent: ToolIntent) -> Result<(), String> {
    Err("The shell tool service is unavailable".to_string())
}

#[cfg(unix)]
fn send_tool_request(intent: ToolIntent) -> Result<(), String> {
    let workspace_id = std::env::var("TILO_WORKSPACE_ID")
        .map_err(|_| "This app is not attached to a Tilo workspace".to_string())?;
    let raw_fd: i32 = std::env::var(TOOL_SERVICE_FD_ENV)
        .map_err(|_| "The shell tool service is unavailable".to_string())?
        .parse()
        .map_err(|_| "The shell supplied an invalid tool service descriptor".to_string())?;

    // Duplicate the inherited descriptor: dropping the per-request stream must
    // not close the process-wide service connection retained by the sandbox.
    // SAFETY: the launcher keeps the service descriptor open for the
    // process's lifetime; it is only borrowed long enough to duplicate it.
    let owned = unsafe { BorrowedFd::borrow_raw(raw_fd) }
        .try_clone_to_owned()
        .map_err(|e| format!("opening the shell tool service: {e}"))?;
    let mut stream = UnixStream::from(owned);
    let request = ToolRequest {
        protocol: TOOL_PROTOCOL_VERSION,
        workspace_id,
        intent,
    };
    let encoded =
        serde_json::to_vec(&request).map_err(|e| format!("encoding tool request: {e}"))?;
    if encoded.len() > MAX_WIRE_BYTES {
        return Err("tool request is too large".into());
    }
    stream
        .write_all(&encoded)
        .and_then(|()| stream.write_all(b"\n"))
        .map_err(|e| format!("sending tool request: {e}"))?;

    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| format!("reading tool reply: {e}"))?;
    if reply.len() > MAX_WIRE_BYTES {
        return Err("tool reply is too large".into());
    }
    let reply: ToolReply =
        serde_json::from_str(&reply).map_err(|e| format!("decoding tool reply: {e}"))?;
    if reply.accepted {
        Ok(())
    } else {
        Err(reply.message)
    }
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
