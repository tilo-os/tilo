//! Reaching a device by name and waking it.
//!
//! A device's address is only a hint: DHCP hands out another one, a laptop
//! roams between access points. tilo-vm records a device's hostname, its MAC
//! on the network it was reached on and what can wake it ([`NetFacts`],
//! read by [`NET_PROBE`]) whenever it reaches the device. When the recorded
//! address stops answering it looks the device up as `<hostname>.local`
//! (mDNS, through `getent hosts` or `avahi-resolve`) and keeps the new
//! address. A device that is not found either is asleep or gone, and
//! `tilo-vm vm wake` sends its Wake-on-LAN magic packet from this machine.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::hypervisor::{CommandLine, Runner, SshEndpoint};

/// The UDP port magic packets go to (discard).
pub const WAKE_PORT: u16 = 9;

/// Read, as root, what reaching and waking the device needs: its hostname
/// and, for the interface that holds `%ADDR%` (the address tilo-vm reached
/// it at; else, as behind a forwarded port, the default route's), its MAC,
/// subnet broadcast and magic-packet wake setting; and its route over the
/// peer network (`tilo-device-admin id`), on a dev image that serves one.
pub const NET_PROBE: &str = r#"addr=%ADDR%
iface=$(ip -o addr show 2>/dev/null | awk -v a="$addr" '{split($4, p, "/"); if (p[1] == a) {print $2; exit}}')
[ "$iface" != lo ] || iface=
[ -n "$iface" ] || iface=$(ip -o -4 route show default 2>/dev/null | sed -n 's/.* dev \([^ ]*\).*/\1/p' | head -n 1)
echo "hostname=$(cat /proc/sys/kernel/hostname)"
if [ -n "$iface" ]; then
echo "interface=$iface"
echo "mac=$(cat "/sys/class/net/$iface/address" 2>/dev/null)"
echo "broadcast=$(ip -o -4 addr show dev "$iface" 2>/dev/null | sed -n 's/.* brd \([0-9.]*\).*/\1/p' | head -n 1)"
if [ -e "/sys/class/net/$iface/phy80211" ]; then
  phy=$(cat "/sys/class/net/$iface/phy80211/name")
  echo "kind=wifi"
  echo "phy=$phy"
  if command -v iw >/dev/null 2>&1; then
    echo @@wowlan; iw phy "$phy" wowlan show 2>&1
    echo @@support; iw phy "$phy" info 2>&1 | sed -n '/WoWLAN support/,/^[[:space:]][^[:space:]*]/p'
  fi
else
  echo "kind=ethernet"
  if command -v ethtool >/dev/null 2>&1; then
    echo @@ethtool; ethtool "$iface" 2>&1 | grep -i 'wake-on'
  fi
fi
fi
if command -v tilo-device-admin >/dev/null 2>&1; then
  echo @@admin; tilo-device-admin id 2>/dev/null
fi
true"#;

/// [`NET_PROBE`] for a device reached at `host`, on one line: a remote
/// command is one line of the login shell's.
pub fn net_probe(host: &str) -> String {
    NET_PROBE
        .replace("%ADDR%", &crate::vmbuild::sh_quote(host))
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("; ")
        .replace("then;", "then")
        .replace("else;", "else")
}

/// What reaching and waking a device needs, as it reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetFacts {
    pub hostname: Option<String>,
    pub mac: Option<String>,
    pub wake: Option<WakeInfo>,
    pub peer: Option<crate::config::PeerRoute>,
}

/// How the device can be woken, as it reported it when last reached.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeInfo {
    /// The interface tilo-vm reached it on.
    pub interface: String,
    /// `wol` (Ethernet Wake-on-LAN) or `wowlan` (Wi-Fi).
    pub via: String,
    /// `enabled`, `disabled`, `unsupported`, or `unknown` when the device
    /// has no tool that says (`ethtool`, `iw`).
    pub state: String,
    /// Its subnet's broadcast address, where the magic packet goes besides
    /// 255.255.255.255.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broadcast: Option<String>,
    /// The Wi-Fi interface's phy, for `iw phy <phy> wowlan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phy: Option<String>,
}

impl WakeInfo {
    /// Can a magic packet be counted on to wake it? Only Ethernet
    /// Wake-on-LAN that is enabled. A Wi-Fi chip may list `WoWLAN` and keep
    /// its association in suspend while the machine around it still does
    /// not wake on the packet (a Snapdragon laptop's WCN7850, say), so
    /// `WoWLAN` is never counted on.
    pub fn reliable(&self) -> bool {
        self.via == "wol" && self.state == "enabled"
    }

    /// One line: what wakes it, and how to turn it on when it is off.
    pub fn describe(&self) -> String {
        let on = self.enable_command();
        let interface = &self.interface;
        match (self.via.as_str(), self.state.as_str()) {
            ("wol", "enabled") => format!("Wake-on-LAN magic packet on {interface} (enabled)"),
            ("wol", "disabled") => format!(
                "Wake-on-LAN on {interface} is supported but disabled; until the image enables it: {on}"
            ),
            ("wowlan", "enabled") => format!(
                "Wi-Fi WoWLAN magic packet on {interface}: listed by the chip and enabled, but not known to wake this machine"
            ),
            ("wowlan", "disabled") => format!(
                "Wi-Fi WoWLAN on {interface}: listed by the chip but disabled, and not known to wake this machine (to try: {on})"
            ),
            (via, "unsupported") => format!(
                "{} on {interface}: cannot wake on a magic packet",
                if via == "wowlan" {
                    "Wi-Fi WoWLAN"
                } else {
                    "Wake-on-LAN"
                }
            ),
            (via, _) => format!(
                "wake on {interface} unknown: the device has no {} to ask",
                if via == "wowlan" { "iw" } else { "ethtool" }
            ),
        }
    }

    /// The command that turns magic-packet wake on, as root on the device.
    pub fn enable_command(&self) -> String {
        match (self.via.as_str(), &self.phy) {
            ("wowlan", Some(phy)) => format!("iw phy {phy} wowlan enable magic-packet"),
            ("wowlan", None) => "iw phy <phy> wowlan enable magic-packet".into(),
            _ => format!("ethtool -s {} wol g", self.interface),
        }
    }
}

/// What `tilo-device-admin id` prints.
#[derive(Deserialize)]
struct AdminId {
    device: String,
    endpoint: String,
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    relay: Option<String>,
    #[serde(default)]
    home_relay: Option<String>,
}

/// Read [`NET_PROBE`]'s answer.
pub fn parse_net_probe(output: &str) -> NetFacts {
    let mut sections = output.split("@@");
    let head = sections.next().unwrap_or_default();
    let field = |key: &str| {
        head.lines()
            .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let mut tool = std::collections::BTreeMap::new();
    for section in sections {
        let (name, body) = section.split_once('\n').unwrap_or((section, ""));
        tool.insert(name.trim().to_owned(), body.to_owned());
    }
    let wake = field("interface").map(|interface| {
        let wifi = field("kind").as_deref() == Some("wifi");
        let state = if wifi {
            wowlan_state(tool.get("wowlan"), tool.get("support"))
        } else {
            wol_state(tool.get("ethtool"))
        };
        WakeInfo {
            interface,
            via: if wifi { "wowlan" } else { "wol" }.into(),
            state: state.into(),
            broadcast: field("broadcast"),
            phy: field("phy").filter(|_| wifi),
        }
    });
    let peer = tool
        .get("admin")
        .and_then(|body| serde_json::from_str::<AdminId>(body.trim()).ok())
        .map(|id| crate::config::PeerRoute {
            device: id.device,
            endpoint: id.endpoint,
            identity: id.identity,
            relay: id.relay,
            home_relay: id.home_relay,
        });
    NetFacts {
        peer,
        hostname: field("hostname"),
        mac: field("mac")
            .filter(|mac| parse_mac(mac).is_some_and(|bytes| bytes != [0; 6]))
            .map(|mac| mac.to_ascii_lowercase()),
        wake,
    }
}

/// `ethtool`'s `Supports Wake-on: pumbg` and `Wake-on: d` lines: `g` is the
/// magic packet.
fn wol_state(ethtool: Option<&String>) -> &'static str {
    let Some(text) = ethtool else {
        return "unknown";
    };
    let value = |key: &str| {
        text.lines().find_map(|line| {
            let (name, value) = line.trim().split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(key)
                .then(|| value.trim().to_owned())
        })
    };
    match (value("Supports Wake-on"), value("Wake-on")) {
        (Some(supports), _) if !supports.contains('g') => "unsupported",
        (_, Some(on)) if on.contains('g') => "enabled",
        (Some(_), _) => "disabled",
        _ => "unknown",
    }
}

/// `iw phy <phy> wowlan show` (`WoWLAN is disabled.`, or enabled with a
/// list of triggers) and the `WoWLAN support:` part of `iw phy <phy> info`.
fn wowlan_state(show: Option<&String>, support: Option<&String>) -> &'static str {
    let Some(show) = show else {
        return "unknown";
    };
    let show = show.to_ascii_lowercase();
    let magic_supported = support
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.to_ascii_lowercase().contains("magic packet"));
    if show.contains("not supported") || magic_supported == Some(false) {
        "unsupported"
    } else if show.contains("is enabled") && show.contains("magic packet") {
        "enabled"
    } else if show.contains("wowlan is") {
        "disabled"
    } else {
        "unknown"
    }
}

/// `aa:bb:cc:dd:ee:ff` (or with `-`).
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut bytes = [0u8; 6];
    let mut parts = text.trim().split([':', '-']);
    for byte in &mut bytes {
        let part = parts.next()?;
        if part.len() != 2 {
            return None;
        }
        *byte = u8::from_str_radix(part, 16).ok()?;
    }
    parts.next().is_none().then_some(bytes)
}

/// Six `0xff` bytes, then the MAC sixteen times.
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut packet = [0xff; 102];
    for chunk in packet[6..].chunks_exact_mut(6) {
        chunk.copy_from_slice(&mac);
    }
    packet
}

/// Where the packet goes: the limited broadcast, and the device's subnet
/// broadcast when it reported one.
pub fn wake_targets(broadcast: Option<&str>) -> Vec<Ipv4Addr> {
    let mut targets = vec![Ipv4Addr::BROADCAST];
    if let Some(subnet) = broadcast.and_then(|b| b.parse::<Ipv4Addr>().ok())
        && !targets.contains(&subnet)
    {
        targets.push(subnet);
    }
    targets
}

/// Send the magic packet for `mac` to each target's [`WAKE_PORT`]; the
/// targets it went to.
///
/// # Errors
/// No target took it.
pub fn send_magic_packet(mac: [u8; 6], targets: &[Ipv4Addr]) -> std::io::Result<Vec<String>> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    let packet = magic_packet(mac);
    let mut sent = Vec::new();
    let mut last_error = None;
    for target in targets {
        match socket.send_to(&packet, (*target, WAKE_PORT)) {
            Ok(_) => sent.push(format!("{target}:{WAKE_PORT}")),
            Err(error) => last_error = Some(error),
        }
    }
    match (sent.is_empty(), last_error) {
        (true, Some(error)) => Err(error),
        _ => Ok(sent),
    }
}

/// Is this ssh failure a lost route rather than a refusal: the device
/// asleep, gone, or on another address?
pub fn lost(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    [
        "no route to host",
        "network is unreachable",
        "host is unreachable",
        "timed out",
        "connection reset",
        "broken pipe",
    ]
    .iter()
    .any(|sign| detail.contains(sign))
}

/// What an authenticated SSH attempt says about device reachability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The machine answers; normal connection handling reports any refusal.
    There,
    /// No route, or no answer in time.
    Lost(String),
    /// The SSH transport could not be run; normal connection handling explains it.
    Unknown,
}

/// Check reachability through the ordinary key-only SSH transport.
///
/// A raw TCP connect followed by close counts as an unauthenticated connection
/// to sshd and can penalize later legitimate requests. Authenticate instead,
/// using the same key and host-key policy as the eventual daemon connection.
/// `true` needs neither root nor the daemon to be installed.
pub fn probe(
    runner: &mut dyn Runner,
    endpoint: &SshEndpoint,
    key: Option<&str>,
    timeout: Duration,
) -> Probe {
    let mut command = crate::remote::ssh_transport(endpoint, key, false);
    let timeout = format!("ConnectTimeout={}", timeout.as_secs().max(1));
    for argument in &mut command.args {
        if argument.starts_with("ConnectTimeout=") {
            argument.clone_from(&timeout);
        }
    }
    command.args.push("true".into());
    match runner.output(&command) {
        Ok(output) if !output.success && lost(&output.stderr) => {
            Probe::Lost(output.stderr.trim().to_owned())
        }
        Ok(_) => Probe::There,
        Err(_) => Probe::Unknown,
    }
}

/// `name` (`<hostname>.local`) through the system resolver (`getent hosts`,
/// which asks mDNS where nss-mdns is set up) or Avahi; IPv4 first, never a
/// link-local IPv6 address ssh could not use without its interface.
pub fn resolve(runner: &mut dyn Runner, name: &str) -> Option<IpAddr> {
    let lookups = [
        CommandLine::new("getent", &["hosts", name]),
        CommandLine::new("avahi-resolve", &["--name", name]),
    ];
    for lookup in &lookups {
        let Ok(output) = runner.output(lookup) else {
            continue;
        };
        if !output.success {
            continue;
        }
        let mut found: Vec<IpAddr> = output
            .stdout
            .split_whitespace()
            .filter_map(|word| word.parse::<IpAddr>().ok())
            .filter(|ip| match ip {
                IpAddr::V4(v4) => !v4.is_unspecified() && !v4.is_loopback(),
                IpAddr::V6(v6) => !v6.is_loopback() && (v6.segments()[0] & 0xffc0) != 0xfe80,
            })
            .collect();
        found.sort_by_key(|ip| !ip.is_ipv4());
        if let Some(ip) = found.first() {
            return Some(*ip);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::CliError;
    use crate::hypervisor::Output;

    /// A magic packet is six 0xff bytes and the MAC sixteen times, to the
    /// limited and the subnet broadcast.
    #[test]
    fn the_magic_packet_is_the_mac_sixteen_times_after_six_ff() {
        let mac = parse_mac("AA:bb:cc:01:02:03").unwrap();
        assert_eq!(mac, [0xaa, 0xbb, 0xcc, 1, 2, 3]);
        assert_eq!(parse_mac("aa-bb-cc-01-02-03"), Some(mac));
        assert_eq!(parse_mac("aa:bb:cc:01:02"), None);
        assert_eq!(parse_mac("aa:bb:cc:01:02:03:04"), None);
        assert_eq!(parse_mac("aa:bb:cc:01:02:zz"), None);
        let packet = magic_packet(mac);
        assert_eq!(packet.len(), 102);
        assert_eq!(packet[..6], [0xff; 6]);
        for copy in packet[6..].chunks(6) {
            assert_eq!(copy, mac);
        }
        assert_eq!(
            wake_targets(Some("10.0.0.255")),
            [Ipv4Addr::BROADCAST, Ipv4Addr::new(10, 0, 0, 255)]
        );
        assert_eq!(wake_targets(None), [Ipv4Addr::BROADCAST]);
        assert_eq!(wake_targets(Some("garbage")), [Ipv4Addr::BROADCAST]);
    }

    /// The probe's answer gives the hostname, MAC and an honest wake state:
    /// enabled, disabled (with the command that enables it), unsupported or
    /// unknown.
    #[test]
    fn the_net_probe_reports_enabled_disabled_and_unsupported_wake() {
        let wired = |ethtool: &str| {
            format!(
                "hostname=opi\ninterface=end0\nmac=AA:BB:CC:00:11:22\nbroadcast=10.0.0.255\nkind=ethernet\n@@ethtool\n{ethtool}"
            )
        };
        let facts = parse_net_probe(&wired("\tSupports Wake-on: pumbg\n\tWake-on: g\n"));
        assert_eq!(facts.hostname.as_deref(), Some("opi"));
        assert_eq!(facts.mac.as_deref(), Some("aa:bb:cc:00:11:22"));
        let wake = facts.wake.unwrap();
        assert_eq!(
            (
                wake.via.as_str(),
                wake.state.as_str(),
                wake.broadcast.as_deref()
            ),
            ("wol", "enabled", Some("10.0.0.255"))
        );
        assert!(wake.reliable());
        let disabled = parse_net_probe(&wired("\tSupports Wake-on: pumbg\n\tWake-on: d\n"))
            .wake
            .unwrap();
        assert_eq!(disabled.state, "disabled");
        assert!(!disabled.reliable());
        assert!(disabled.describe().contains("ethtool -s end0 wol g"));
        let none = parse_net_probe(&wired("\tSupports Wake-on: d\n\tWake-on: d\n"));
        assert_eq!(none.wake.unwrap().state, "unsupported");
        let no_tool =
            parse_net_probe("hostname=opi\ninterface=end0\nmac=aa:bb:cc:00:11:22\nkind=ethernet\n");
        assert_eq!(no_tool.wake.unwrap().state, "unknown");

        let wifi = |show: &str, support: &str| {
            parse_net_probe(&format!(
                "hostname=yoga\ninterface=wlan0\nmac=02:11:22:33:44:55\nbroadcast=10.0.0.255\nkind=wifi\nphy=phy0\n@@wowlan\n{show}@@support\n{support}"
            ))
            .wake
            .unwrap()
        };
        let on = wifi(
            "WoWLAN is enabled:\n * wake up on magic packet\n",
            "\tWoWLAN support:\n\t\t * wake up on magic packet\n",
        );
        assert_eq!((on.via.as_str(), on.state.as_str()), ("wowlan", "enabled"));
        // Listed and enabled is still not a machine known to wake on it.
        assert!(!on.reliable());
        assert!(
            on.describe().contains("not known to wake"),
            "{}",
            on.describe()
        );
        let off = wifi(
            "WoWLAN is disabled.\n",
            "\tWoWLAN support:\n\t\t * wake up on disconnect\n\t\t * wake up on magic packet\n",
        );
        assert_eq!(off.state, "disabled");
        assert!(
            off.describe()
                .contains("iw phy phy0 wowlan enable magic-packet"),
            "{}",
            off.describe()
        );
        assert_eq!(wifi("WoWLAN is disabled.\n", "").state, "disabled");
        assert_eq!(
            wifi(
                "WoWLAN is disabled.\n",
                "\tWoWLAN support:\n\t\t * wake up on disconnect\n"
            )
            .state,
            "unsupported"
        );
        assert_eq!(
            wifi("command failed: Operation not supported (-95)\n", "").state,
            "unsupported"
        );
        // A loopback-only machine (a VM behind a forwarded port) has no MAC worth recording.
        let lo = parse_net_probe("hostname=vm\ninterface=lo\nmac=00:00:00:00:00:00\n");
        assert_eq!(lo.mac, None);
        let line = net_probe("10.0.0.5");
        assert!(line.starts_with("addr=10.0.0.5; iface="), "{line}");
        assert!(!line.contains('\n') && !line.contains("then;"), "{line}");
        assert!(line.contains("tilo-device-admin id"), "{line}");
        // The route over the peer network, whatever the interface.
        let admin = parse_net_probe(concat!(
            "hostname=yoga\n@@admin\n",
            r#"{"device":"aa","endpoint":"bb","label":"Yoga","identity":"cc","relay":null,"home_relay":"https://relay.example/","serving":true}"#,
            "\n"
        ));
        assert_eq!(admin.hostname.as_deref(), Some("yoga"));
        assert_eq!(
            admin.peer,
            Some(crate::config::PeerRoute {
                device: "aa".into(),
                endpoint: "bb".into(),
                identity: Some("cc".into()),
                relay: None,
                home_relay: Some("https://relay.example/".into()),
            })
        );
        assert_eq!(
            parse_net_probe("hostname=vm\n@@admin\nnot json\n").peer,
            None
        );
    }

    /// Answers `getent` and `avahi-resolve` from a script; a missing tool is
    /// what the system runner reports for one.
    struct Lookups {
        getent: Option<&'static str>,
        avahi: Option<&'static str>,
        asked: Vec<String>,
    }

    impl Runner for Lookups {
        fn output(&mut self, command: &CommandLine) -> Result<Output, CliError> {
            self.asked.push(command.display());
            let answer = match command.program.as_str() {
                "getent" => self.getent,
                "avahi-resolve" => self.avahi,
                _ => None,
            };
            answer
                .map(|stdout| Output {
                    success: !stdout.is_empty(),
                    status: String::new(),
                    stdout: stdout.into(),
                    stderr: String::new(),
                })
                .ok_or_else(|| CliError::ToolMissing {
                    tool: command.program.clone(),
                    purpose: "resolve".into(),
                })
        }

        fn interactive(&mut self, _command: &CommandLine) -> Result<bool, CliError> {
            Ok(false)
        }
    }

    /// The name is looked up through the system resolver first, then Avahi;
    /// IPv4 wins, and link-local IPv6 is never used.
    #[test]
    fn a_name_resolves_through_getent_then_avahi() {
        let mut runner = Lookups {
            getent: Some("fe80::1        yoga.local\n10.0.0.42       yoga.local\n"),
            avahi: None,
            asked: Vec::new(),
        };
        assert_eq!(
            resolve(&mut runner, "yoga.local"),
            Some("10.0.0.42".parse().unwrap())
        );
        assert_eq!(runner.asked, ["getent hosts yoga.local"]);

        // getent has no mDNS here (exit 2, nothing printed): Avahi answers.
        let mut runner = Lookups {
            getent: Some(""),
            avahi: Some("yoga.local\t10.0.0.77\n"),
            asked: Vec::new(),
        };
        assert_eq!(
            resolve(&mut runner, "yoga.local"),
            Some("10.0.0.77".parse().unwrap())
        );
        assert_eq!(runner.asked.len(), 2);

        // Neither tool, or only link-local: not found.
        let mut runner = Lookups {
            getent: None,
            avahi: None,
            asked: Vec::new(),
        };
        assert_eq!(resolve(&mut runner, "yoga.local"), None);
        let mut runner = Lookups {
            getent: Some("fe80::1 yoga.local\n"),
            avahi: Some(""),
            asked: Vec::new(),
        };
        assert_eq!(resolve(&mut runner, "yoga.local"), None);
    }

    struct SshProbe {
        stderr: &'static str,
        commands: Vec<CommandLine>,
        interactive_called: bool,
    }

    impl Runner for SshProbe {
        fn output(&mut self, command: &CommandLine) -> Result<Output, CliError> {
            self.commands.push(command.clone());
            Ok(Output {
                success: self.stderr.is_empty(),
                status: String::new(),
                stdout: String::new(),
                stderr: self.stderr.into(),
            })
        }

        fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError> {
            self.interactive_called = true;
            Err(CliError::ToolMissing {
                tool: command.program.clone(),
                purpose: "reachability must never request interactive authentication".into(),
            })
        }
    }

    #[test]
    fn reachability_authenticates_without_opening_an_unauthenticated_tcp_probe() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = SshEndpoint {
            host: "127.0.0.1".into(),
            port: listener.local_addr().unwrap().port(),
            user: "owner".into(),
        };
        let mut runner = SshProbe {
            stderr: "",
            commands: Vec::new(),
            interactive_called: false,
        };
        for _ in 0..10 {
            assert_eq!(
                probe(
                    &mut runner,
                    &endpoint,
                    Some("/private/key"),
                    Duration::from_secs(3)
                ),
                Probe::There
            );
        }
        assert_eq!(runner.commands.len(), 10);
        assert!(!runner.interactive_called);
        for command in &runner.commands {
            let mut expected = crate::remote::ssh_transport(&endpoint, Some("/private/key"), false);
            for argument in &mut expected.args {
                if argument == "ConnectTimeout=10" {
                    *argument = "ConnectTimeout=3".into();
                }
            }
            expected.args.push("true".into());
            assert_eq!(command, &expected);
            assert!(command.args.iter().any(|arg| arg == "BatchMode=yes"));
            assert!(
                command
                    .args
                    .iter()
                    .any(|arg| arg == "StrictHostKeyChecking=accept-new")
            );
            assert!(!command.args.iter().any(|arg| arg == "sudo" || arg == "-t"));
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn authenticated_reachability_keeps_refusals_distinct_from_network_loss() {
        let endpoint = SshEndpoint {
            host: "device-alias".into(),
            port: 22,
            user: "owner".into(),
        };
        let mut runner = SshProbe {
            stderr: "",
            commands: Vec::new(),
            interactive_called: false,
        };
        for detail in [
            "Permission denied (publickey).",
            "Host key verification failed.",
            "ssh: connect to host x port 22: Connection refused",
        ] {
            runner.stderr = detail;
            assert_eq!(
                probe(&mut runner, &endpoint, None, Duration::from_secs(3)),
                Probe::There
            );
        }
        for detail in [
            "ssh: connect to host x port 22: No route to host",
            "ssh: connect to host x port 22: Connection timed out",
        ] {
            runner.stderr = detail;
            assert_eq!(
                probe(&mut runner, &endpoint, None, Duration::from_secs(3)),
                Probe::Lost(detail.into())
            );
        }
    }

    /// ssh's words for a lost route, as opposed to a refusal.
    #[test]
    fn a_lost_route_is_told_apart_from_a_refusal() {
        assert!(lost(
            "ssh: connect to host 10.0.0.223 port 22: No route to host"
        ));
        assert!(lost("ssh: connect to host x port 22: Connection timed out"));
        assert!(lost("connect to host x port 22: Network is unreachable"));
        assert!(!lost("ssh: connect to host x port 22: Connection refused"));
        assert!(!lost("Permission denied (publickey)."));
    }
}
