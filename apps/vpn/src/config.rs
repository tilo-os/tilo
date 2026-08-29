//! Config import + system command planning.
//!
//! Two real, testable concerns live here:
//!
//! 1. Parsing a `WireGuard` `.conf` or `OpenVPN` `.ovpn` file into a
//!    [`Connection`] (used by the import flow).
//! 2. Building the exact argv we would hand to `wg-quick` / `nmcli` /
//!    `openvpn` to bring a tunnel up or down. The argv is shown to the
//!    user in a confirm dialog *before* anything is executed — the
//!    network is never touched without consent, and never in `--shots`.

use std::path::PathBuf;

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

/// Parse a `WireGuard` `[Interface]` / `[Peer]` ini-style config.
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
        let Some((key, value)) = split_kv(line) else {
            continue;
        };
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

/// Parse an `OpenVPN` `.ovpn` directive file.
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
        let Some(directive) = parts.next() else {
            continue;
        };
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
                if rest.first().is_some_and(|s| s.eq_ignore_ascii_case("DNS"))
                    && let Some(ip) = rest.get(1)
                {
                    if conn.dns.is_empty() {
                        conn.dns = ip.to_string();
                    } else {
                        conn.dns = format!("{}, {}", conn.dns, ip);
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
    let cut = line.find(['#', ';']).unwrap_or(line.len());
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
/// `NetworkManager` (`nmcli`) when present for a unified, polkit-gated
/// path, and falls back to the protocol-native tools otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// `wg-quick up/down <name>` (`WireGuard` native).
    WgQuick,
    /// `openvpn --config <path>` (`OpenVPN` native).
    OpenVpnCli,
    /// `nmcli connection up/down <name>` (`NetworkManager`).
    NetworkManager,
}

/// Plan the command to bring `conn` up using `backend`.
///
/// The bring-up references the tunnel's real, imported config file whenever
/// one is present: `wg-quick up <config-path>` derives the interface from the
/// file's basename, and `openvpn --config <config-path>` starts the daemon on
/// the exact `.ovpn` the user imported. Without an imported config (a
/// hand-added stub) it falls back to the conventional `/etc/...` location and
/// the interface name — that command will honestly fail at run time if no such
/// file exists, rather than pretend to connect.
pub fn connect_plan(conn: &Connection, backend: Backend) -> CommandPlan {
    match backend {
        Backend::WgQuick => {
            let target = conn
                .config_path()
                .map_or_else(|| iface_name(conn), std::string::ToString::to_string);
            CommandPlan {
                program: "wg-quick".into(),
                args: vec!["up".into(), target],
                summary: format!("Bring up WireGuard tunnel '{}'", conn.name),
            }
        }
        Backend::OpenVpnCli => {
            let cfg = conn.config_path().map_or_else(
                || format!("/etc/openvpn/{}.ovpn", conn.name),
                std::string::ToString::to_string,
            );
            CommandPlan {
                program: "openvpn".into(),
                args: vec!["--config".into(), cfg, "--daemon".into()],
                summary: format!("Start OpenVPN tunnel '{}'", conn.name),
            }
        }
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
        Backend::WgQuick => {
            let target = conn
                .config_path()
                .map_or_else(|| iface_name(conn), std::string::ToString::to_string);
            CommandPlan {
                program: "wg-quick".into(),
                args: vec!["down".into(), target],
                summary: format!("Tear down WireGuard tunnel '{}'", conn.name),
            }
        }
        Backend::OpenVpnCli => {
            // Match the specific config path (or the tunnel name) so we only
            // kill *this* tunnel's daemon, never every openvpn on the box.
            let needle = conn.config_path().map_or_else(
                || format!("{}.ovpn", conn.name),
                std::string::ToString::to_string,
            );
            CommandPlan {
                program: "pkill".into(),
                args: vec!["-f".into(), needle],
                summary: format!("Stop OpenVPN tunnel '{}'", conn.name),
            }
        }
        Backend::NetworkManager => CommandPlan {
            program: "nmcli".into(),
            args: vec!["connection".into(), "down".into(), conn.name.clone()],
            summary: format!("Deactivate '{}' via NetworkManager", conn.name),
        },
    }
}

/// Derive the Linux interface name for a tunnel. When a config file was
/// imported its basename *is* the interface `wg-quick` creates, so the two are
/// kept in lockstep; otherwise the tunnel name is sanitised into a valid,
/// `IFNAMSIZ`-bounded name.
pub fn iface_name(conn: &Connection) -> String {
    if let Some(p) = conn.config_path()
        && let Some(stem) = std::path::Path::new(p).file_stem().and_then(|s| s.to_str())
    {
        let s = sanitize_iface(stem);
        if !s.is_empty() {
            return s;
        }
    }
    let s = sanitize_iface(&conn.name);
    if s.is_empty() {
        format!("qvpn{}", &conn.id.simple().to_string()[..4])
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

/// The per-user data directory where imported tunnel configs are persisted so
/// the real bring-up command can reference them (`$XDG_DATA_HOME/qos-vpn`).
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("qos-vpn")
}

/// Persist an imported raw config so the real bring-up can reference it. The
/// filename basename becomes the `wg-quick` interface name; the file is written
/// `0600` because `WireGuard` refuses a world-readable config that holds a
/// private key. Returns the absolute path written.
pub fn persist_config(iface: &str, kind: ConnKind, raw: &str) -> std::io::Result<PathBuf> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{iface}.{}", kind.ext()));
    std::fs::write(&path, raw)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// Whether the user has opted in to real host command execution. Mutating
/// commands (bring-up / teardown / Tor) never run without this.
pub fn exec_allowed() -> bool {
    std::env::var("QOS_VPN_ALLOW_EXEC").as_deref() == Ok("1")
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
    if !exec_allowed() {
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
    fn command_plans_use_iface_without_imported_config() {
        // No imported config → the interface name is the sanitised tunnel
        // name, and wg-quick resolves it against /etc/wireguard.
        let c = Connection::new("Home", ConnKind::WireGuard);
        assert_eq!(iface_name(&c), "home");
        let up = connect_plan(&c, Backend::WgQuick);
        assert_eq!(up.display(), "wg-quick up home");
        let down = disconnect_plan(&c, Backend::WgQuick);
        assert_eq!(down.display(), "wg-quick down home");
        let nm = connect_plan(&c, Backend::NetworkManager);
        assert_eq!(nm.display(), "nmcli connection up Home");
    }

    #[test]
    fn wireguard_plan_uses_imported_config_path() {
        // With an imported config, the real bring-up references the exact
        // file; wg-quick derives the interface from its basename.
        let mut c = Connection::new("Home", ConnKind::WireGuard);
        c.config_path = Some("/home/u/.local/share/qos-vpn/home.conf".into());
        assert_eq!(iface_name(&c), "home");
        let up = connect_plan(&c, Backend::WgQuick);
        assert_eq!(up.program, "wg-quick");
        assert_eq!(
            up.args,
            vec!["up", "/home/u/.local/share/qos-vpn/home.conf"]
        );
        let down = disconnect_plan(&c, Backend::WgQuick);
        assert_eq!(
            down.args,
            vec!["down", "/home/u/.local/share/qos-vpn/home.conf"]
        );
    }

    #[test]
    fn openvpn_plan_uses_imported_config_path() {
        let mut c = Connection::new("Work", ConnKind::OpenVpn);
        c.config_path = Some("/home/u/.local/share/qos-vpn/work.ovpn".into());
        let up = connect_plan(&c, Backend::OpenVpnCli);
        assert_eq!(up.program, "openvpn");
        assert_eq!(
            up.args,
            vec![
                "--config",
                "/home/u/.local/share/qos-vpn/work.ovpn",
                "--daemon"
            ]
        );
        // Teardown targets *this* tunnel's config, not every openvpn process.
        let down = disconnect_plan(&c, Backend::OpenVpnCli);
        assert_eq!(down.program, "pkill");
        assert_eq!(
            down.args,
            vec!["-f", "/home/u/.local/share/qos-vpn/work.ovpn"]
        );
    }

    #[test]
    fn iface_name_is_sanitised_and_bounded() {
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

    #[test]
    #[serial_test::serial(vpn_env)]
    fn persist_config_writes_the_raw_bytes() {
        // Point the data dir at a scratch location and confirm the exact raw
        // config is written where the bring-up command will find it.
        let tmp = std::env::temp_dir().join(format!("qos-vpn-persist-{}", uuid::Uuid::new_v4()));
        unsafe { std::env::set_var("XDG_DATA_HOME", &tmp) };
        let path = persist_config("home", ConnKind::WireGuard, WG).expect("persist");
        assert!(path.ends_with("qos-vpn/home.conf"));
        let back = std::fs::read_to_string(&path).unwrap();
        assert_eq!(back, WG);
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    #[serial_test::serial(vpn_env)]
    fn execute_is_guarded_off_by_default() {
        // Without the opt-in env var, execution must refuse — proving the
        // confirm gate can never be bypassed in tests/sandboxes.
        let plan = tor_plan(true);
        let r = execute(&plan);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("execution disabled"));
    }
}
