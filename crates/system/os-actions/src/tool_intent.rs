//! Typed requests from sandboxed system surfaces to shell-owned tools.
//!
//! The wire deliberately cannot carry a program, package id, or argv. A caller
//! chooses from this enum; the shell maps it to an installed Tilo package and
//! validates the contextual data before launching anything.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};

pub const TOOL_PROTOCOL_VERSION: u32 = 1;
pub const TOOL_SERVICE_FD_ENV: &str = "TILO_SVC_TOOLS_FD";
pub const TOOL_CONTEXT_ENV: &str = "TILO_TOOL_CONTEXT";
const MAX_WIRE_BYTES: usize = 4 * 1024;

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
}

impl ToolIntent {
    /// Check that this intent's contextual data is safe to hand to the shell.
    ///
    /// # Errors
    ///
    /// Returns an error message if a route that requires a VPN profile name has none, or the
    /// supplied profile name is empty, too long, or contains control characters.
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
            Self::Firewall { .. }
            | Self::Disks { .. }
            | Self::Backups { .. }
            | Self::WorkspaceSettings { .. }
            | Self::IdentityBackup => Ok(()),
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
    let workspace_id = std::env::var("TILO_WORKSPACE_ID")
        .map_err(|_| "Settings is not attached to a Tilo workspace".to_string())?;
    let raw_fd: i32 = std::env::var(TOOL_SERVICE_FD_ENV)
        .map_err(|_| "The shell tool service is unavailable".to_string())?
        .parse()
        .map_err(|_| "The shell supplied an invalid tool service descriptor".to_string())?;

    // Duplicate the inherited descriptor: dropping the per-request stream must
    // not close the process-wide service connection retained by the sandbox.
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
}
