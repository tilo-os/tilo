//! Config import + system command planning.
//!
//! Two real, testable concerns live here:
//!
//! 1. Parsing a `WireGuard` `.conf` or `OpenVPN` `.ovpn` file into a
//!    [`Connection`] (used by the import flow).
//! 2. Building the exact `NetworkManager` action shown to the user before a
//!    connection changes. Execution accepts only that narrow vocabulary;
//!    the network is never touched without consent or in screenshot tests.

use crate::model::{ConnKind, Connection, LocalProfileDetails};

/// Maximum provider configuration accepted through the import portal.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// Validate that an imported profile contains the minimum information needed
/// to create a real `NetworkManager` connection. Credentials are checked for
/// presence but are never returned or copied into the collaborative model.
///
/// # Errors
///
/// Returns an actionable list of missing readiness or server-verification
/// fields when the provider profile is incomplete.
pub fn validate_config(kind: ConnKind, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("The selected configuration is empty.".into());
    }
    match kind {
        ConnKind::WireGuard => {
            let has = |section: &str, key: &str| {
                let mut current = "";
                text.lines().any(|raw| {
                    let line = strip_comment(raw).trim();
                    if let Some(next) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                        current = next.trim();
                        return false;
                    }
                    current.eq_ignore_ascii_case(section)
                        && split_kv(line).is_some_and(|(candidate, value)| {
                            candidate.eq_ignore_ascii_case(key) && !value.trim().is_empty()
                        })
                })
            };
            let mut missing = Vec::new();
            for (section, key) in [
                ("Interface", "PrivateKey"),
                ("Interface", "Address"),
                ("Peer", "PublicKey"),
                ("Peer", "Endpoint"),
                ("Peer", "AllowedIPs"),
            ] {
                if !has(section, key) {
                    missing.push(format!("{section}.{key}"));
                }
            }
            if missing.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "WireGuard profile is incomplete; missing {}.",
                    missing.join(", ")
                ))
            }
        }
        ConnKind::OpenVpn => {
            let lower = text.to_ascii_lowercase();
            let has_remote = lower
                .lines()
                .any(|line| line.trim_start().starts_with("remote "));
            let has_server_auth = lower.contains("<ca>")
                || lower.lines().any(|line| {
                    let line = line.trim_start();
                    line.starts_with("ca ") || line.starts_with("peer-fingerprint ")
                });
            if !has_remote {
                return Err("OpenVPN profile is incomplete; no remote server is declared.".into());
            }
            if !has_server_auth {
                return Err("OpenVPN profile does not declare a CA or peer fingerprint, so the server identity cannot be verified.".into());
            }
            Ok(())
        }
    }
}

/// Query `NetworkManager`'s authoritative active state for a saved profile.
///
/// # Errors
///
/// Returns an error when `NetworkManager` is unavailable or the named profile
/// no longer exists.
pub fn network_manager_active(uuid: &str) -> Result<bool, String> {
    let profiles = os_actions::list_vpn_connections()?;
    profiles
        .into_iter()
        .find(|profile| profile.uuid == uuid)
        .map(|profile| profile.active)
        .ok_or_else(|| "NetworkManager did not expose the locally provisioned VPN profile".into())
}

/// Build the only shareable result of a successful import: a stable profile
/// identity. Raw configuration is deliberately not parsed into this value;
/// it can contain addresses, endpoints and secrets and must remain with the
/// local `NetworkManager` secret agent.
pub fn parse_config(name: &str, kind: ConnKind, text: &str) -> Connection {
    let _ = text;
    Connection::new(name, kind)
}

/// Parse useful, non-secret display facts into a device-local value. Callers
/// must keep it out of collaboration/action/resource surfaces. `PrivateKey`,
/// auth blocks and raw text are intentionally never copied here.
pub fn parse_local_details(kind: ConnKind, text: &str) -> LocalProfileDetails {
    let mut details = LocalProfileDetails::default();
    match kind {
        ConnKind::WireGuard => {
            let mut section = "";
            for raw in text.lines() {
                let line = strip_comment(raw).trim();
                if let Some(sec) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                    section = sec.trim();
                    continue;
                }
                let Some((key, value)) = split_kv(line) else {
                    continue;
                };
                match (section, key.as_str()) {
                    ("Interface", "Address") => details.address = value,
                    ("Interface", "DNS") => details.dns = value,
                    ("Peer", "PublicKey") => details.public_key = value,
                    ("Peer", "Endpoint") => details.endpoint = value,
                    ("Peer", "AllowedIPs") => details.allowed_ips = value,
                    _ => {}
                }
            }
        }
        ConnKind::OpenVpn => {
            let mut routes = Vec::new();
            let mut proto = "";
            for raw in text.lines() {
                let line = strip_comment(raw).trim();
                let mut words = line.split_whitespace();
                let Some(directive) = words.next() else {
                    continue;
                };
                let rest: Vec<_> = words.collect();
                match directive {
                    "remote" => {
                        if let Some(host) = rest.first() {
                            details.endpoint =
                                format!("{}:{}", host, rest.get(1).unwrap_or(&"1194"));
                        }
                    }
                    "proto" => proto = rest.first().copied().unwrap_or(""),
                    "route" => {
                        if let Some(network) = rest.first() {
                            routes.push(format!("{}/{}", network, rest.get(1).unwrap_or(&"")));
                        }
                    }
                    "dhcp-option"
                        if rest
                            .first()
                            .is_some_and(|value| value.eq_ignore_ascii_case("DNS")) =>
                    {
                        if let Some(dns) = rest.get(1) {
                            if !details.dns.is_empty() {
                                details.dns.push_str(", ");
                            }
                            details.dns.push_str(dns);
                        }
                    }
                    "ifconfig" => details.address = rest.first().copied().unwrap_or("").to_string(),
                    _ => {}
                }
            }
            if !proto.is_empty() && !details.endpoint.is_empty() {
                details.endpoint.push_str(" (");
                details.endpoint.push_str(&proto.to_ascii_uppercase());
                details.endpoint.push(')');
            }
            details.allowed_ips = routes.join(", ");
        }
    }
    details
}

/// Detect the protocol from a file name extension.
pub fn kind_from_filename(name: &str) -> Option<ConnKind> {
    let ext = std::path::Path::new(name).extension()?.to_str()?;
    if ext.eq_ignore_ascii_case("conf") {
        Some(ConnKind::WireGuard)
    } else if ext.eq_ignore_ascii_case("ovpn") {
        Some(ConnKind::OpenVpn)
    } else {
        None
    }
}

fn strip_comment(line: &str) -> &str {
    // WireGuard uses '#', OpenVPN uses '#' and ';'.
    let cut = line.find(['#', ';']).unwrap_or(line.len());
    // `find` on a `char` needle always returns a byte offset on a UTF-8
    // boundary, so this is unreachable in practice — `.get()` documents
    // that instead of slicing directly.
    line.get(..cut).unwrap_or(line)
}

fn split_kv(line: &str) -> Option<(String, String)> {
    let (k, v) = line.split_once('=')?;
    Some((k.trim().to_string(), v.trim().to_string()))
}

// ----------------------------------------------------------------------
// System command planning (executed only after explicit confirmation).
// ----------------------------------------------------------------------

/// A fully-resolved command we are about to run on the host, ready to
/// show the user and (on confirm) execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPlan {
    pub program: String,
    pub args: Vec<String>,
    /// One-line human description shown in the confirm dialog.
    pub summary: String,
}

impl CommandPlan {
    /// The shell-ish single line preview, e.g. `wg-quick up Home`.
    pub fn display(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }
}

/// Plan a production `NetworkManager` operation against the local profile UUID,
/// never against the mutable collaborative label.
pub fn connect_local_plan(conn: &Connection, uuid: &str) -> CommandPlan {
    CommandPlan {
        program: "nmcli".into(),
        args: vec!["connection".into(), "up".into(), "uuid".into(), uuid.into()],
        summary: format!("Activate '{}' via NetworkManager", conn.name),
    }
}

/// See [`connect_local_plan`].
pub fn disconnect_local_plan(conn: &Connection, uuid: &str) -> CommandPlan {
    CommandPlan {
        program: "nmcli".into(),
        args: vec![
            "connection".into(),
            "down".into(),
            "uuid".into(),
            uuid.into(),
        ],
        summary: format!("Deactivate '{}' via NetworkManager", conn.name),
    }
}

/// Derive a local interface hint from the user-visible profile name. It is
/// never persisted or shared and is used only for best-effort local telemetry.
pub fn iface_name(conn: &Connection) -> String {
    let s = sanitize_iface(&conn.name);
    if s.is_empty() {
        // A UUID's `simple()` form is 32 lowercase hex digits — always at
        // least 4 ASCII bytes — so this is unreachable in practice;
        // `.get()` documents that instead of slicing directly.
        let hex = conn.id.simple().to_string();
        format!("qvpn{}", hex.get(..4).unwrap_or(&hex))
    } else {
        s
    }
}

/// Reduce an arbitrary label to a valid Linux interface name: ASCII
/// alphanumerics / `-` / `_`, runs of anything else collapsed to a single
/// `-`, lowercased, and clamped to 15 bytes (`IFNAMSIZ - 1`).
fn sanitize_iface(s: &str) -> String {
    let cleaned: String = s
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut joined = cleaned.split_whitespace().collect::<Vec<_>>().join("-");
    joined.truncate(15);
    joined.trim_matches('-').to_string()
}

/// Production uses the narrow live backend directly. Unit tests stay
/// hermetic unless a specifically marked integration run opts in.
pub fn exec_allowed() -> bool {
    !cfg!(test) || std::env::var("TILO_VPN_ALLOW_EXEC").as_deref() == Ok("1")
}

/// The command that would toggle the system Tor service.
pub fn tor_plan(enable: bool) -> CommandPlan {
    let action = if enable { "start" } else { "stop" };
    CommandPlan {
        program: "systemctl".into(),
        args: vec![action.into(), "tor.service".into()],
        summary: if enable {
            "Start the Tor daemon and route traffic over it".into()
        } else {
            "Stop the Tor daemon".into()
        },
    }
}

/// Execute a planned command. ONLY called after the user confirms; never
/// reached on the `--shots` path. Returns the captured status text.
///
/// Only a deliberately small command vocabulary is accepted. VPN changes
/// are delegated to the shared `NetworkManager` boundary; this function is
/// not an arbitrary process launcher.
///
/// # Errors
/// Returns `Err` when execution is disabled in the current process (e.g.
/// tests, unless `TILO_VPN_ALLOW_EXEC` opts in), when `plan` isn't the
/// narrow `nmcli connection up|down <name>` vocabulary this function
/// accepts, or when the underlying `NetworkManager` call fails.
pub fn execute(plan: &CommandPlan) -> Result<String, String> {
    if !exec_allowed() {
        return Err(format!(
            "execution disabled in test process: {}",
            plan.display()
        ));
    }
    if plan.program == "nmcli"
        && let [action, up_down, target, value] = plan.args.as_slice()
        && action == "connection"
        && matches!(up_down.as_str(), "up" | "down")
        && target == "uuid"
    {
        os_actions::set_vpn_connection_uuid(value, up_down == "up")?;
        return Ok(String::new());
    }
    Err(format!("unsupported privileged action: {}", plan.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ConnStatus;

    const WG: &str = "\
[Interface]
PrivateKey = aGVsbG8=
Address = 10.7.0.2/32
DNS = 1.1.1.1   # cloudflare

[Peer]
PublicKey = kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0=
Endpoint = home.aurora.net:51820
AllowedIPs = 0.0.0.0/0, ::/0
";

    const OVPN: &str = "\
client
dev tun
proto udp
remote vpn.workcorp.example 1194
; a comment
route 172.20.0.0 255.255.0.0
dhcp-option DNS 172.20.4.1
";

    #[test]
    fn config_parsing_yields_secret_free_validated_profiles() {
        // Case `parse_result_is_secret_free_profile_identity`.
        {
            let c = parse_config("Home", ConnKind::WireGuard, WG);
            assert_eq!(c.name, "Home");
            assert_eq!(c.kind, ConnKind::WireGuard);
            assert_eq!(c.status, ConnStatus::Disconnected);
        }
        // Case `readiness_validation_rejects_partial_or_unverifiable_profiles`.
        {
            assert!(validate_config(ConnKind::WireGuard, "[Peer]\nEndpoint=x:1").is_err());
            assert!(validate_config(ConnKind::WireGuard, WG).is_ok());
            assert!(validate_config(ConnKind::OpenVpn, "client\nremote vpn.example 1194").is_err());
            assert!(
                validate_config(
                    ConnKind::OpenVpn,
                    "client\nremote vpn.example 1194\n<ca>\ncertificate\n</ca>"
                )
                .is_ok()
            );
        }
        // Case `openvpn_parse_result_does_not_contain_server_details`.
        {
            let c = parse_config("Work", ConnKind::OpenVpn, OVPN);
            assert_eq!(c.name, "Work");
            assert_eq!(c.kind, ConnKind::OpenVpn);
        }
        // Case `kind_detected_from_extension`.
        {
            assert_eq!(kind_from_filename("home.conf"), Some(ConnKind::WireGuard));
            assert_eq!(kind_from_filename("WORK.OVPN"), Some(ConnKind::OpenVpn));
            assert_eq!(kind_from_filename("notes.txt"), None);
        }
    }

    #[test]
    #[serial_test::serial(vpn_env)]
    fn command_plans_are_sanitised_and_guarded() {
        // Case `iface_name_without_imported_config`.
        {
            // No imported config → the interface name is the sanitised tunnel name.
            let c = Connection::new("Home", ConnKind::WireGuard);
            assert_eq!(iface_name(&c), "home");
        }
        // Case `iface_name_is_sanitised_and_bounded`.
        {
            let mut c = Connection::new("My Work VPN (São Paulo)!!", ConnKind::WireGuard);
            let n = iface_name(&c);
            assert!(
                crate::net::is_valid_iface(&n),
                "derived name {n:?} must be a valid iface"
            );
            assert!(n.len() <= 15);
            // An empty name still yields a valid, unique-ish interface.
            c.name = "   ".into();
            let n2 = iface_name(&c);
            assert!(
                crate::net::is_valid_iface(&n2),
                "fallback {n2:?} must be valid"
            );
        }
        // Case `execute_is_guarded_off_by_default`.
        {
            // The execution boundary rejects anything outside its fixed VPN
            // vocabulary, even after a UI confirmation.
            let plan = tor_plan(true);
            let r = execute(&plan);
            assert!(r.is_err());
            assert!(r.unwrap_err().contains("execution disabled"));
        }
    }
}
