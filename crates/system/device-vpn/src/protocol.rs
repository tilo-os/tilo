//! What the shell says to the device VPN helper, and its answers.

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::net::{self, Ipv4Net, MAX_MTU, MIN_MTU};

pub const SOCKET_PATH: &str = "/run/tilo-device-vpn/control.sock";

/// Bumped when a request or answer changes shape incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

pub const MAX_LINE_BYTES: u64 = 16 * 1024;

pub const MAX_NETWORKS: usize = 16;
pub const MAX_CARRIER_PORTS: usize = 8;
pub const MAX_RELAYS: usize = 16;
pub const MAX_PROFILE_SERVERS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Routes {
    All,
    Networks { networks: Vec<Ipv4Net> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carrier {
    pub udp_ports: Vec<u16>,
    pub relays: Vec<IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientLink {
    pub address: Ipv4Addr,
    pub mtu: u16,
    pub routes: Routes,
    pub dns: bool,
    pub kill_switch: bool,
    pub carrier: Carrier,
}

impl ClientLink {
    /// Why the helper must not act on this link, if it must not.
    ///
    /// # Errors
    ///
    /// Names the first value out of bounds.
    pub fn check(&self) -> Result<(), String> {
        if !net::is_client_address(self.address) {
            return Err("the tunnel address is not a client address".into());
        }
        check_mtu(self.mtu)?;
        if let Routes::Networks { networks } = &self.routes {
            if networks.is_empty() || networks.len() > MAX_NETWORKS {
                return Err("a client routes between one and sixteen networks".into());
            }
            if !networks.iter().copied().all(Ipv4Net::is_routable_home) {
                return Err("a network cannot be routed through an exit".into());
            }
        }
        let ports = &self.carrier.udp_ports;
        if ports.is_empty() || ports.len() > MAX_CARRIER_PORTS || ports.contains(&0) {
            return Err("the carrier needs between one and eight ports".into());
        }
        if self.carrier.relays.len() > MAX_RELAYS
            || !self
                .carrier
                .relays
                .iter()
                .copied()
                .all(net::is_relay_address)
        {
            return Err("a relay address cannot be exempted".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitLink {
    pub mtu: u16,
}

impl ExitLink {
    /// # Errors
    ///
    /// Names the value out of bounds.
    pub fn check(&self) -> Result<(), String> {
        check_mtu(self.mtu)
    }
}

/// The block that holds while one of the person's `NetworkManager` VPN profiles is set to
/// connect and is not connected: nothing leaves but through a tunnel interface, to the
/// profile's servers, address configuration (DHCP, neighbour discovery) and, when the
/// person allows it, the local network.
///
/// Servers are named by address only; the tunnel is let through by the kind of its
/// interface (TUN, `WireGuard`, PPP) or its `IPsec` policy, never by a caller-given name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileBlock {
    pub servers: Vec<IpAddr>,
    pub allow_local: bool,
}

impl ProfileBlock {
    /// Why the helper must not act on this block, if it must not.
    ///
    /// # Errors
    ///
    /// Names the first value out of bounds.
    pub fn check(&self) -> Result<(), String> {
        if self.servers.len() > MAX_PROFILE_SERVERS {
            return Err("a block lets through at most sixteen servers".into());
        }
        if !self.servers.iter().copied().all(net::is_server_address) {
            return Err("a server address cannot be let through".into());
        }
        Ok(())
    }
}

fn check_mtu(mtu: u16) -> Result<(), String> {
    if (MIN_MTU..=MAX_MTU).contains(&mtu) {
        Ok(())
    } else {
        Err(format!("the MTU must be between {MIN_MTU} and {MAX_MTU}"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    /// Open (or replace) this device's client tunnel. The answer carries the
    /// TUN descriptor.
    OpenClient { link: ClientLink },
    CloseClient,
    OpenExit { link: ExitLink },
    CloseExit,
    HoldProfileBlock { block: ProfileBlock },
    ReleaseProfileBlock,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol: u32,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientState {
    pub link: ClientLink,
    pub interface_up: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitState {
    pub link: ExitLink,
    pub interface_up: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Status {
        client: Option<ClientState>,
        exit: Option<ExitState>,
        #[serde(default)]
        profile_block: Option<ProfileBlock>,
    },
    Opened {
        interface: String,
        mtu: u16,
    },
    Done,
    Refused {
        reason: String,
    },
    Failed {
        message: String,
    },
}

/// Send one request to the helper at `socket` and wait for its answer, with
/// the descriptor an [`Response::Opened`] carries.
#[cfg(target_os = "linux")]
pub fn call(
    socket: &Path,
    request: &Request,
    timeout: Duration,
) -> io::Result<(Response, Option<std::os::fd::OwnedFd>)> {
    use std::io::Write as _;
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(&Envelope {
        protocol: PROTOCOL_VERSION,
        request: request.clone(),
    })
    .map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let (answer, fd) = crate::fdpass::read_line_with_fd(&stream, MAX_LINE_BYTES)?;
    let response: Response = serde_json::from_slice(&answer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match (&response, fd) {
        (Response::Opened { .. }, Some(fd)) => Ok((response, Some(fd))),
        (Response::Opened { .. }, None) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the helper opened a tunnel but sent no descriptor",
        )),
        (_, _) => Ok((response, None)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn link() -> ClientLink {
        ClientLink {
            address: Ipv4Addr::new(100, 100, 64, 2),
            mtu: 1280,
            routes: Routes::All,
            dns: true,
            kill_switch: false,
            carrier: Carrier {
                udp_ports: vec![40_000],
                relays: vec!["203.0.113.7".parse().unwrap()],
            },
        }
    }

    #[test]
    fn requests_name_values_never_interfaces_or_commands() {
        let line = serde_json::to_string(&Envelope {
            protocol: PROTOCOL_VERSION,
            request: Request::OpenClient { link: link() },
        })
        .unwrap();
        assert!(line.contains(r#""op":"open_client""#));
        assert!(line.contains(r#""kind":"all""#));
        assert!(!line.contains("tilovpn0"));
        let back: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(back.request, Request::OpenClient { link: link() });
        assert!(
            serde_json::from_str::<Envelope>(
                r#"{"protocol":1,"request":{"op":"run","command":"sh"}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn every_client_value_is_bounded() {
        assert_eq!(link().check(), Ok(()));
        let bad = [
            ClientLink {
                address: net::GATEWAY,
                ..link()
            },
            ClientLink {
                address: Ipv4Addr::new(192, 168, 1, 2),
                ..link()
            },
            ClientLink { mtu: 575, ..link() },
            ClientLink {
                mtu: 9000,
                ..link()
            },
            ClientLink {
                routes: Routes::Networks { networks: vec![] },
                ..link()
            },
            ClientLink {
                routes: Routes::Networks {
                    networks: vec!["0.0.0.0/0".parse().unwrap()],
                },
                ..link()
            },
            ClientLink {
                routes: Routes::Networks {
                    networks: vec!["192.168.0.0/24".parse().unwrap(); MAX_NETWORKS + 1],
                },
                ..link()
            },
            ClientLink {
                carrier: Carrier {
                    udp_ports: vec![],
                    relays: vec![],
                },
                ..link()
            },
            ClientLink {
                carrier: Carrier {
                    udp_ports: vec![0],
                    relays: vec![],
                },
                ..link()
            },
            ClientLink {
                carrier: Carrier {
                    udp_ports: vec![1],
                    relays: vec!["127.0.0.1".parse().unwrap()],
                },
                ..link()
            },
            ClientLink {
                carrier: Carrier {
                    udp_ports: vec![1],
                    relays: vec!["100.100.64.9".parse().unwrap()],
                },
                ..link()
            },
        ];
        for link in bad {
            assert!(link.check().is_err(), "{link:?}");
        }
        assert!(ExitLink { mtu: 1280 }.check().is_ok());
        assert!(ExitLink { mtu: 100 }.check().is_err());
    }

    pub(crate) fn block() -> ProfileBlock {
        ProfileBlock {
            servers: vec![
                "198.51.100.7".parse().unwrap(),
                "2001:db8::7".parse().unwrap(),
            ],
            allow_local: false,
        }
    }

    #[test]
    fn a_profile_block_names_server_addresses_and_a_switch_only() {
        let line = serde_json::to_string(&Envelope {
            protocol: PROTOCOL_VERSION,
            request: Request::HoldProfileBlock { block: block() },
        })
        .unwrap();
        assert!(line.contains(r#""op":"hold_profile_block""#));
        let back: Envelope = serde_json::from_str(&line).unwrap();
        assert_eq!(back.request, Request::HoldProfileBlock { block: block() });
        assert!(
            serde_json::from_str::<Envelope>(
                r#"{"protocol":1,"request":{"op":"hold_profile_block","block":{"servers":[],"allow_local":false,"interface":"eth0"}}}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Envelope>(
                r#"{"protocol":1,"request":{"op":"hold_profile_block","block":{"servers":["eth0"],"allow_local":false}}}"#
            )
            .is_err()
        );
    }

    #[test]
    fn every_profile_block_value_is_bounded() {
        assert_eq!(block().check(), Ok(()));
        let none = ProfileBlock {
            servers: vec![],
            allow_local: true,
        };
        assert_eq!(none.check(), Ok(()));
        let bad = [
            "127.0.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "ff02::1",
            "100.100.64.1",
        ];
        for server in bad {
            let block = ProfileBlock {
                servers: vec![server.parse().unwrap()],
                allow_local: false,
            };
            assert!(block.check().is_err(), "{server}");
        }
        let many = ProfileBlock {
            servers: vec!["198.51.100.7".parse().unwrap(); MAX_PROFILE_SERVERS + 1],
            allow_local: false,
        };
        assert!(many.check().is_err());
    }
}
