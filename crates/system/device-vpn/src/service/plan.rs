//! The commands that set up and take down a tunnel, built from checked
//! values only.

use std::fmt::Write as _;
use std::net::IpAddr;

use crate::net::{CLIENT_INTERFACE, EXIT_INTERFACE, GATEWAY, TUNNEL_NETWORK};
use crate::protocol::{ClientLink, ExitLink, ProfileBlock, Routes};

pub const TABLE: &str = "5270";
const PRIORITY_CARRIER_PORTS: &str = "5260";
const PRIORITY_CARRIER_RELAYS: &str = "5261";
const PRIORITY_LOCAL: &str = "5262";
const PRIORITY_TUNNEL: &str = "5263";

pub const CLIENT_TABLE: &str = "tilo_device_vpn_client";
pub const EXIT_TABLE: &str = "tilo_device_vpn_exit";
pub const PROFILE_TABLE: &str = "tilo_device_vpn_profile";

/// The private and link-local networks the kill switch leaves reachable, as
/// the all-traffic routes leave the local network.
const LOCAL_V4: &str = "10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16";
const LOCAL_V6: &str = "fe80::/10, fc00::/7";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    Ip,
    Nft,
    Busctl,
}

/// One command: a program, its arguments, what it reads on stdin, and
/// whether its failure fails the request (undoing something that is not
/// there is not a failure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub program: Program,
    pub args: Vec<String>,
    pub input: Option<String>,
    pub required: bool,
}

impl Command {
    fn ip(args: &[&str]) -> Self {
        Self {
            program: Program::Ip,
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            input: None,
            required: true,
        }
    }

    fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    fn nft_script(script: String) -> Self {
        Self {
            program: Program::Nft,
            args: vec!["-f".into(), "-".into()],
            input: Some(script),
            required: true,
        }
    }
}

fn family(address: IpAddr) -> &'static str {
    match address {
        IpAddr::V4(_) => "-4",
        IpAddr::V6(_) => "-6",
    }
}

/// The policy rules of `link` for the person `owner`, each as the
/// arguments after `ip <family> rule add|del`.
fn rules(owner: u32, link: &ClientLink) -> Vec<(&'static str, Vec<String>)> {
    let uidrange = format!("{owner}-{owner}");
    let all = matches!(link.routes, Routes::All);
    let mut rules = Vec::new();
    let rule = |family: &'static str, priority: &str, selector: &[&str], action: &[&str]| {
        let mut args = vec![
            "priority".to_owned(),
            priority.to_owned(),
            "uidrange".to_owned(),
            uidrange.clone(),
        ];
        args.extend(selector.iter().map(|part| (*part).to_owned()));
        args.extend(action.iter().map(|part| (*part).to_owned()));
        (family, args)
    };
    for port in &link.carrier.udp_ports {
        let port = port.to_string();
        for family in ["-4", "-6"] {
            rules.push(rule(
                family,
                PRIORITY_CARRIER_PORTS,
                &["ipproto", "udp", "sport", &port],
                &["lookup", "main"],
            ));
        }
    }
    for relay in &link.carrier.relays {
        let to = relay.to_string();
        rules.push(rule(
            family(*relay),
            PRIORITY_CARRIER_RELAYS,
            &["to", &to],
            &["lookup", "main"],
        ));
    }
    let families: &[&'static str] = if all { &["-4", "-6"] } else { &["-4"] };
    for family in families {
        if all {
            rules.push(rule(
                family,
                PRIORITY_LOCAL,
                &[],
                &["lookup", "main", "suppress_prefixlength", "0"],
            ));
        }
        rules.push(rule(family, PRIORITY_TUNNEL, &[], &["lookup", TABLE]));
    }
    rules
}

fn rule_commands(owner: u32, link: &ClientLink, verb: &str) -> Vec<Command> {
    rules(owner, link)
        .into_iter()
        .map(|(family, args)| {
            let mut command = vec![family.to_owned(), "rule".to_owned(), verb.to_owned()];
            command.extend(args);
            Command {
                program: Program::Ip,
                args: command,
                input: None,
                required: verb == "add",
            }
        })
        .collect()
}

fn route_commands(routes: &Routes) -> Vec<Command> {
    match routes {
        Routes::All => vec![
            Command::ip(&[
                "-4",
                "route",
                "replace",
                "default",
                "dev",
                CLIENT_INTERFACE,
                "table",
                TABLE,
            ]),
            // The tunnel carries IPv4 only: the owner's IPv6 to the internet
            // fails at once instead of going around the tunnel.
            Command::ip(&[
                "-6",
                "route",
                "replace",
                "unreachable",
                "default",
                "table",
                TABLE,
            ]),
        ],
        Routes::Networks { networks } => networks
            .iter()
            .map(|network| {
                let network = network.to_string();
                Command::ip(&[
                    "-4",
                    "route",
                    "replace",
                    &network,
                    "dev",
                    CLIENT_INTERFACE,
                    "table",
                    TABLE,
                ])
            })
            .collect(),
    }
}

fn joined<T: ToString>(items: impl IntoIterator<Item = T>) -> String {
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The kill switch: the owner's traffic leaves only through the tunnel,
/// loopback, the local network and the carrier; anything else is refused.
fn kill_switch_script(owner: u32, link: &ClientLink) -> String {
    let mut owner_chain = String::new();
    let _ = writeln!(owner_chain, "    oifname \"lo\" return");
    let _ = writeln!(owner_chain, "    oifname \"{CLIENT_INTERFACE}\" return");
    if let Routes::Networks { networks } = &link.routes {
        // The home networks never go to the local network instead.
        let _ = writeln!(
            owner_chain,
            "    ip daddr {{ {} }} reject with icmpx admin-prohibited",
            joined(networks)
        );
    }
    let _ = writeln!(owner_chain, "    ip daddr {{ {LOCAL_V4} }} return");
    let _ = writeln!(owner_chain, "    ip6 daddr {{ {LOCAL_V6} }} return");
    let _ = writeln!(
        owner_chain,
        "    udp sport {{ {} }} return",
        joined(&link.carrier.udp_ports)
    );
    let (v4, v6): (Vec<IpAddr>, Vec<IpAddr>) = link
        .carrier
        .relays
        .iter()
        .partition(|relay| relay.is_ipv4());
    if !v4.is_empty() {
        let _ = writeln!(owner_chain, "    ip daddr {{ {} }} return", joined(v4));
    }
    if !v6.is_empty() {
        let _ = writeln!(owner_chain, "    ip6 daddr {{ {} }} return", joined(v6));
    }
    let _ = writeln!(owner_chain, "    reject with icmpx admin-prohibited");
    format!(
        "table inet {CLIENT_TABLE}\n\
         delete table inet {CLIENT_TABLE}\n\
         table inet {CLIENT_TABLE} {{\n\
         \x20 chain output {{\n\
         \x20   type filter hook output priority filter; policy accept;\n\
         \x20   meta skuid {owner} jump owner\n\
         \x20 }}\n\
         \x20 chain owner {{\n\
         {owner_chain}\
         \x20 }}\n\
         }}\n"
    )
}

fn delete_table(table: &str) -> Command {
    Command {
        program: Program::Nft,
        args: vec!["delete".into(), "table".into(), "inet".into(), table.into()],
        input: None,
        required: false,
    }
}

const NM: [&str; 8] = [
    "--system",
    "call",
    "org.freedesktop.NetworkManager",
    "/org/freedesktop/NetworkManager",
    "org.freedesktop.DBus.Properties",
    "Set",
    "ssv",
    "org.freedesktop.NetworkManager",
];

/// `NetworkManager`'s global DNS: every name asked of the gateway, or (with
/// `None`) back to each connection's own servers.
#[must_use]
pub fn global_dns(through_proxyway: bool) -> Command {
    let mut args: Vec<String> = NM.iter().map(|arg| (*arg).to_owned()).collect();
    args.extend(["GlobalDnsConfiguration".to_owned(), "a{sv}".to_owned()]);
    if through_proxyway {
        let gateway = GATEWAY.to_string();
        args.extend(
            [
                "2", "searches", "as", "0", "domains", "a{sv}", "1", "*", "a{sv}", "1", "servers",
                "as", "1", &gateway,
            ]
            .map(str::to_owned),
        );
    } else {
        args.push("0".to_owned());
    }
    Command {
        program: Program::Busctl,
        args,
        input: None,
        required: through_proxyway,
    }
}

/// Configure the freshly created client TUN for `link` and route `owner`'s
/// traffic through it. `previous` is the link this one replaces, whose
/// rules are taken down first; its kill switch stays until the new one
/// replaces it in one nftables transaction.
#[must_use]
pub fn open_client(
    owner: u32,
    link: &ClientLink,
    previous: Option<(u32, &ClientLink)>,
) -> Vec<Command> {
    let mtu = link.mtu.to_string();
    let address = format!("{}/{}", link.address, TUNNEL_NETWORK.prefix());
    let mut commands = Vec::new();
    if let Some((previous_owner, previous)) = previous {
        commands.extend(rule_commands(previous_owner, previous, "del"));
    }
    commands.extend([
        Command::ip(&["-4", "route", "flush", "table", TABLE]).optional(),
        Command::ip(&["-6", "route", "flush", "table", TABLE]).optional(),
        Command::ip(&["link", "set", "dev", CLIENT_INTERFACE, "mtu", &mtu]),
        Command::ip(&["address", "add", &address, "dev", CLIENT_INTERFACE]),
        Command::ip(&["link", "set", "dev", CLIENT_INTERFACE, "up"]),
    ]);
    commands.extend(route_commands(&link.routes));
    commands.extend(rule_commands(owner, link, "add"));
    if link.kill_switch {
        commands.push(Command::nft_script(kill_switch_script(owner, link)));
    } else {
        commands.push(delete_table(CLIENT_TABLE));
    }
    commands
}

/// Undo a client tunnel: its rules, routes, kill switch and interface.
/// `NetworkManager`'s DNS is the caller's to put back ([`global_dns`]).
#[must_use]
pub fn close_client(owner: u32, link: &ClientLink) -> Vec<Command> {
    let mut commands = rule_commands(owner, link, "del");
    commands.extend([
        Command::ip(&["-4", "route", "flush", "table", TABLE]).optional(),
        Command::ip(&["-6", "route", "flush", "table", TABLE]).optional(),
        delete_table(CLIENT_TABLE),
        Command::ip(&["link", "delete", CLIENT_INTERFACE]).optional(),
    ]);
    commands
}

/// The exit's forwarding rules: only the tunnel network leaves through
/// the uplink, masqueraded; nothing new enters the tunnel from outside; TCP
/// fits the tunnel. `guard` drops every other forwarded packet, for while
/// forwarding is on only because of the exit.
fn exit_script(guard: bool) -> String {
    let network = TUNNEL_NETWORK;
    let guard = if guard {
        format!("    iifname != \"{EXIT_INTERFACE}\" oifname != \"{EXIT_INTERFACE}\" drop\n")
    } else {
        String::new()
    };
    format!(
        "table inet {EXIT_TABLE}\n\
         delete table inet {EXIT_TABLE}\n\
         table inet {EXIT_TABLE} {{\n\
         \x20 chain forward {{\n\
         \x20   type filter hook forward priority filter - 5; policy accept;\n\
         \x20   iifname \"{EXIT_INTERFACE}\" meta nfproto ipv6 drop\n\
         \x20   iifname \"{EXIT_INTERFACE}\" ip saddr != {network} drop\n\
         \x20   oifname \"{EXIT_INTERFACE}\" ct state != {{ established, related }} drop\n\
         \x20   oifname \"{EXIT_INTERFACE}\" tcp flags & (syn | rst) == syn tcp option maxseg size set rt mtu\n\
         {guard}\
         \x20 }}\n\
         \x20 chain postrouting {{\n\
         \x20   type nat hook postrouting priority srcnat; policy accept;\n\
         \x20   ip saddr {network} oifname != \"{EXIT_INTERFACE}\" masquerade\n\
         \x20 }}\n\
         }}\n"
    )
}

/// Configure the freshly created exit TUN. `guard` as in the exit's
/// forwarding rules: the helper turned forwarding on for it.
#[must_use]
pub fn open_exit(link: &ExitLink, guard: bool) -> Vec<Command> {
    let mtu = link.mtu.to_string();
    let address = format!("{GATEWAY}/{}", TUNNEL_NETWORK.prefix());
    vec![
        Command::ip(&["link", "set", "dev", EXIT_INTERFACE, "mtu", &mtu]),
        Command::ip(&["address", "add", &address, "dev", EXIT_INTERFACE]),
        Command::ip(&["link", "set", "dev", EXIT_INTERFACE, "up"]),
        Command::nft_script(exit_script(guard)),
    ]
}

/// The kinds of interface a `NetworkManager` VPN's tunnel is: `WireGuard`,
/// TUN (`OpenVPN`, `OpenConnect`, vpnc and most plugins) and PPP (PPTP,
/// L2TP, SSTP). Only a privileged process can make one.
const TUNNEL_KINDS: &str = "\"tun\", \"wireguard\", \"ppp\"";

/// Multicast and broadcast on the local network: discovery of printers and
/// shares, which "the local network" includes.
const LOCAL_MULTICAST_V4: &str = "224.0.0.0/4, 255.255.255.255";
const LOCAL_MULTICAST_V6: &str = "ff02::/16";

/// The block of the person's VPN profiles: whatever this computer sends or
/// forwards is refused, but through a tunnel interface or an `IPsec` policy,
/// to the profiles' servers, loopback, DHCP and neighbour discovery, and
/// (with `allow_local`) the local network.
fn profile_block_script(block: &ProfileBlock) -> String {
    let mut hold = String::new();
    let _ = writeln!(hold, "    oifname \"lo\" return");
    let _ = writeln!(hold, "    meta oifkind {{ {TUNNEL_KINDS} }} return");
    let _ = writeln!(hold, "    rt ipsec exists return");
    // Address configuration: DHCP both ways (this computer as a client, or
    // serving a hotspot), DHCPv6, and neighbour and router discovery with
    // the multicast listener reports they depend on.
    let _ = writeln!(hold, "    udp sport 68 udp dport 67 return");
    let _ = writeln!(hold, "    udp sport 67 udp dport 68 return");
    let _ = writeln!(hold, "    udp sport 546 udp dport 547 return");
    let _ = writeln!(hold, "    udp sport 547 udp dport 546 return");
    let _ = writeln!(
        hold,
        "    icmpv6 type {{ nd-router-solicit, nd-router-advert, nd-neighbor-solicit, nd-neighbor-advert, nd-redirect, mld-listener-report, mld2-listener-report }} return"
    );
    let (v4, v6): (Vec<IpAddr>, Vec<IpAddr>) =
        block.servers.iter().partition(|server| server.is_ipv4());
    if !v4.is_empty() {
        let _ = writeln!(hold, "    ip daddr {{ {} }} return", joined(v4));
    }
    if !v6.is_empty() {
        let _ = writeln!(hold, "    ip6 daddr {{ {} }} return", joined(v6));
    }
    if block.allow_local {
        let _ = writeln!(
            hold,
            "    ip daddr {{ {LOCAL_V4}, {LOCAL_MULTICAST_V4} }} return"
        );
        let _ = writeln!(
            hold,
            "    ip6 daddr {{ {LOCAL_V6}, {LOCAL_MULTICAST_V6} }} return"
        );
    }
    let _ = writeln!(hold, "    reject with icmpx admin-prohibited");
    format!(
        "table inet {PROFILE_TABLE}\n\
         delete table inet {PROFILE_TABLE}\n\
         table inet {PROFILE_TABLE} {{\n\
         \x20 chain output {{\n\
         \x20   type filter hook output priority filter; policy accept;\n\
         \x20   jump hold\n\
         \x20 }}\n\
         \x20 chain forward {{\n\
         \x20   type filter hook forward priority filter; policy accept;\n\
         \x20   jump hold\n\
         \x20 }}\n\
         \x20 chain hold {{\n\
         {hold}\
         \x20 }}\n\
         }}\n"
    )
}

/// Put up (or replace, in one nftables transaction) the block of the
/// person's VPN profiles.
#[must_use]
pub fn hold_profile_block(block: &ProfileBlock) -> Vec<Command> {
    vec![Command::nft_script(profile_block_script(block))]
}

#[must_use]
pub fn release_profile_block() -> Vec<Command> {
    vec![delete_table(PROFILE_TABLE)]
}

/// Undo an exit's rules and interface. Forwarding is the caller's to put
/// back.
#[must_use]
pub fn close_exit() -> Vec<Command> {
    vec![
        delete_table(EXIT_TABLE),
        Command::ip(&["link", "delete", EXIT_INTERFACE]).optional(),
    ]
}
