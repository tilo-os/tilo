//! Config import + system command planning.
//!
//! Two real, testable concerns live here:
//!
//! 1. Parsing a WireGuard `.conf` or OpenVPN `.ovpn` file into a
//!    [`Connection`] (used by the import flow).
//! 2. Building the exact argv we would hand to `wg-quick` / `nmcli` /
//!    `openvpn` to bring a tunnel up or down. The argv is shown to the
//!    user in a confirm dialog *before* anything is executed — the
//!    network is never touched without consent, and never in `--shots`.

use crate::model::{ConnKind, Connection};

/// Parse the textual contents of a config file into a [`Connection`].
///
/// `kind` disambiguates the grammar; `name` seeds the display title
/// (callers usually derive it from the file stem).
pub fn parse_config(name: &str, kind: ConnKind, text: &str) -> Connection {
    match kind {
        ConnKind::WireGuard => parse_wireguard(name, text),
        ConnKind::OpenVpn => parse_openvpn(name, text),
    }
}

/// Detect the protocol from a file name extension.
pub fn kind_from_filename(name: &str) -> Option<ConnKind> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".conf") {
        Some(ConnKind::WireGuard)
    } else if lower.ends_with(".ovpn") {
        Some(ConnKind::OpenVpn)
    } else {
        None
    }
}

/// Parse a WireGuard `[Interface]` / `[Peer]` ini-style config.
fn parse_wireguard(name: &str, text: &str) -> Connection {
    let mut conn = Connection::new(name, ConnKind::WireGuard);
    let mut section = "";
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(sec) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = match sec.trim() {
                "Interface" => "interface",
                "Peer" => "peer",
                _ => "",
            };
            continue;
        }
        let Some((key, value)) = split_kv(line) else { continue };
        match (section, key.as_str()) {
            ("interface", "Address") => conn.address = value,
            ("interface", "DNS") => conn.dns = value,
            ("peer", "PublicKey") => conn.public_key = value,
            ("peer", "Endpoint") => conn.endpoint = value,
            ("peer", "AllowedIPs") => conn.allowed_ips = value,
            _ => {}
        }
    }
    conn
}

/// Parse an OpenVPN `.ovpn` directive file.
fn parse_openvpn(name: &str, text: &str) -> Connection {
    let mut conn = Connection::new(name, ConnKind::OpenVpn);
    let mut routes: Vec<String> = Vec::new();
    let mut proto = String::new();
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(directive) = parts.next() else { continue };
        let rest: Vec<&str> = parts.collect();
        match directive {
            "remote" => {
                // remote <host> [port]
                if let Some(host) = rest.first() {
                    let port = rest.get(1).copied().unwrap_or("1194");
                    conn.endpoint = format!("{host}:{port}");
                }
            }
            "proto" => proto = rest.first().copied().unwrap_or("").to_string(),
            "route" => {
                if let Some(net) = rest.first() {
                    let mask = rest.get(1).copied().unwrap_or("");
                    if mask.is_empty() {
                        routes.push(net.to_string());
                    } else {
                        routes.push(format!("{net}/{mask}"));
                    }
                }
            }
            "dhcp-option" => {
                // dhcp-option DNS <ip>
                if rest.first().map(|s| s.eq_ignore_ascii_case("DNS")).unwrap_or(false) {
                    if let Some(ip) = rest.get(1) {
                        if conn.dns.is_empty() {
                            conn.dns = ip.to_string();
                        } else {
                            conn.dns = format!("{}, {}", conn.dns, ip);
                        }
                    }
                }
            }
            "ifconfig" => {
                if let Some(addr) = rest.first() {
                    conn.address = addr.to_string();
                }
            }
            _ => {}
        }
    }
    if !proto.is_empty() {
        // Annotate the endpoint label with the transport when present.
        conn.endpoint = format!("{} ({})", conn.endpoint, proto.to_uppercase());
    }
    if !routes.is_empty() {
        conn.allowed_ips = routes.join(", ");
    }
    conn.public_key = format!("CN={name}");
    conn
}

fn strip_comment(line: &str) -> &str {
    // WireGuard uses '#', OpenVPN uses '#' and ';'.
    let cut = line
        .find(|c| c == '#' || c == ';')
        .unwrap_or(line.len());
    &line[..cut]
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

/// Which backend tool a connection drives. The live app prefers
/// NetworkManager (`nmcli`) when present for a unified, polkit-gated
/// path, and falls back to the protocol-native tools otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// `wg-quick up/down <name>` (WireGuard native).
    WgQuick,
    /// `openvpn --config <path>` (OpenVPN native).
    OpenVpnCli,
    /// `nmcli connection up/down <name>` (NetworkManager).
    NetworkManager,
}

/// Plan the command to bring `conn` up using `backend`.
pub fn connect_plan(conn: &Connection, backend: Backend) -> CommandPlan {
    match backend {
        Backend::WgQuick => CommandPlan {
            program: "wg-quick".into(),
            args: vec!["up".into(), conn.name.clone()],
            summary: format!("Bring up WireGuard tunnel '{}'", conn.name),
        },
        Backend::OpenVpnCli => CommandPlan {
            program: "openvpn".into(),
            args: vec![
                "--config".into(),
                format!("/etc/openvpn/{}.ovpn", conn.name),
                "--daemon".into(),
            ],
            summary: format!("Start OpenVPN tunnel '{}'", conn.name),
        },
        Backend::NetworkManager => CommandPlan {
            program: "nmcli".into(),
            args: vec!["connection".into(), "up".into(), conn.name.clone()],
            summary: format!("Activate '{}' via NetworkManager", conn.name),
        },
    }
}

/// Plan the command to tear `conn` down using `backend`.
pub fn disconnect_plan(conn: &Connection, backend: Backend) -> CommandPlan {
    match backend {
        Backend::WgQuick => CommandPlan {
            program: "wg-quick".into(),
            args: vec!["down".into(), conn.name.clone()],
            summary: format!("Tear down WireGuard tunnel '{}'", conn.name),
        },
        Backend::OpenVpnCli => CommandPlan {
            program: "pkill".into(),
            args: vec!["-f".into(), format!("openvpn.*{}.ovpn", conn.name)],
            summary: format!("Stop OpenVPN tunnel '{}'", conn.name),
        },
        Backend::NetworkManager => CommandPlan {
            program: "nmcli".into(),
            args: vec!["connection".into(), "down".into(), conn.name.clone()],
            summary: format!("Deactivate '{}' via NetworkManager", conn.name),
        },
    }
}

/// Default backend choice for a protocol.
pub fn default_backend(kind: ConnKind) -> Backend {
    match kind {
        ConnKind::WireGuard => Backend::WgQuick,
        ConnKind::OpenVpn => Backend::OpenVpnCli,
    }
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
/// This is intentionally guarded by an env var so that an accidental
/// invocation in a sandbox/test never mutates host networking — the live
/// app sets `QOS_VPN_ALLOW_EXEC=1` once the user has acknowledged the
/// confirm dialog.
pub fn execute(plan: &CommandPlan) -> Result<String, String> {
    if std::env::var("QOS_VPN_ALLOW_EXEC").as_deref() != Ok("1") {
        return Err(format!(
            "execution disabled (set QOS_VPN_ALLOW_EXEC=1): {}",
            plan.display()
        ));
    }
    let out = std::process::Command::new(&plan.program)
        .args(&plan.args)
        .output()
        .map_err(|e| format!("spawn {}: {e}", plan.program))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
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
    fn wireguard_parses_all_fields() {
        let c = parse_config("Home", ConnKind::WireGuard, WG);
        assert_eq!(c.address, "10.7.0.2/32");
        assert_eq!(c.dns, "1.1.1.1");
        assert_eq!(c.endpoint, "home.aurora.net:51820");
        assert_eq!(c.allowed_ips, "0.0.0.0/0, ::/0");
        assert_eq!(c.public_key, "kP8x2yQ1mZ7vR3nL9fH0sJtW6cB4dE5gA1iU2oXyz0=");
        assert_eq!(c.status, ConnStatus::Disconnected);
    }

    #[test]
    fn openvpn_parses_remote_and_routes() {
        let c = parse_config("Work", ConnKind::OpenVpn, OVPN);
        assert_eq!(c.endpoint, "vpn.workcorp.example:1194 (UDP)");
        assert_eq!(c.allowed_ips, "172.20.0.0/255.255.0.0");
        assert_eq!(c.dns, "172.20.4.1");
        assert_eq!(c.public_key, "CN=Work");
    }

    #[test]
    fn kind_detected_from_extension() {
        assert_eq!(kind_from_filename("home.conf"), Some(ConnKind::WireGuard));
        assert_eq!(kind_from_filename("WORK.OVPN"), Some(ConnKind::OpenVpn));
        assert_eq!(kind_from_filename("notes.txt"), None);
    }

    #[test]
    fn command_plans_are_well_formed() {
        let mut c = Connection::new("Home", ConnKind::WireGuard);
        c.name = "Home".into();
        let up = connect_plan(&c, Backend::WgQuick);
        assert_eq!(up.display(), "wg-quick up Home");
        let down = disconnect_plan(&c, Backend::WgQuick);
        assert_eq!(down.display(), "wg-quick down Home");
        let nm = connect_plan(&c, Backend::NetworkManager);
        assert_eq!(nm.display(), "nmcli connection up Home");
    }

    #[test]
    fn execute_is_guarded_off_by_default() {
        // Without the opt-in env var, execution must refuse — proving the
        // confirm gate can never be bypassed in tests/sandboxes.
        let plan = tor_plan(true);
        let r = execute(&plan);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("execution disabled"));
    }
}
