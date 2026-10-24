//! What the shell says to the device VPN helper, and its answers.
//!
//! One JSON request per connection, one JSON answer back, each a single
//! line. Only the compositor of the person at the seat is answered (see the
//! service's `peer` module); anyone else is refused before the request is
//! read. The vocabulary is closed: a request names addresses, prefixes, ports
//! and switches, never an interface, a path, a command or a rule, and
//! [`ClientLink::check`] and [`ExitLink::check`] bound every value before the
//! helper acts on it. An opened tunnel's TUN descriptor comes back with the
//! [`Response::Opened`] line as `SCM_RIGHTS` ancillary data.

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::net::{self, Ipv4Net, MAX_MTU, MIN_MTU};

/// Where the helper listens.
pub const SOCKET_PATH: &str = "/run/tilo-device-vpn/control.sock";

/// Bumped when a request or answer changes shape incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest line either side accepts.
pub const MAX_LINE_BYTES: u64 = 16 * 1024;

/// The most home networks a client routes through an exit.
pub const MAX_NETWORKS: usize = 16;
/// The most local UDP ports the carrier is sent from.
pub const MAX_CARRIER_PORTS: usize = 8;
/// The most relay addresses the carrier is exempted towards.
pub const MAX_RELAYS: usize = 16;

/// What a client sends through the tunnel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Routes {
    /// Everything but the local network.
    All,
    /// Only these networks: the exit's home networks.
    Networks { networks: Vec<Ipv4Net> },
}

/// How the tunnel's own packets find the exit without entering the tunnel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Carrier {
    /// The local UDP ports of the shell's peer endpoint.
    pub udp_ports: Vec<u16>,
    /// The relays the connection may fall back to.
    pub relays: Vec<IpAddr>,
}

/// A client tunnel, as the exit offered it and the person chose it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientLink {
    /// This device's address in the tunnel network, given by the exit.
    pub address: Ipv4Addr,
    pub mtu: u16,
    pub routes: Routes,
    /// Resolve names through the exit.
    pub dns: bool,
    /// Block this person's traffic while the tunnel is down.
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

/// An exit tunnel.
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
    /// What is open now.
    Status,
    /// Open (or replace) this device's client tunnel. The answer carries the
    /// TUN descriptor.
    OpenClient { link: ClientLink },
    /// Undo the client tunnel: routes, DNS and the kill switch.
    CloseClient,
    /// Open this device's exit. The answer carries the TUN descriptor.
    OpenExit { link: ExitLink },
    /// Undo the exit: forwarding and masquerade.
    CloseExit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol: u32,
    pub request: Request,
}

/// A client tunnel the helper keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientState {
    pub link: ClientLink,
    /// Whether the TUN still exists. A kill switch outlives it.
    pub interface_up: bool,
}

/// An exit the helper keeps.
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
    },
    /// The tunnel is up; its TUN descriptor rides with this line.
    Opened {
        interface: String,
        mtu: u16,
    },
    Done,
    /// The caller may not ask this.
    Refused {
        reason: String,
    },
    /// The request was allowed and could not be carried out.
    Failed {
        message: String,
    },
}

/// Send one request to the helper at `socket` and wait for its answer, with
/// the descriptor an [`Response::Opened`] carries.
///
/// # Errors
///
/// The socket cannot be reached, or the answer is not one the helper sends.
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
        // A descriptor with any other answer is dropped, closing it.
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
}
