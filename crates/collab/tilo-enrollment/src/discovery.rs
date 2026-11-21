//! The unprivileged listener for machines announcing that they are ready to enroll.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::bail;
use socket2::{Domain, Protocol, Socket, Type};
use tilo_identity::certification_ceremony::pairing_code;
use tilo_identity::enrollment::{Method, Methods};

use crate::approval::received_packet;
use crate::protocol::{EnrollmentKind, MAX_PACKET, NEARBY_PORT, PairSolicitation};
use crate::{deadline_after, socket_timeout, valid_solicitation};

/// A nearby device awaiting explicit approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairCandidate {
    pub device_public_key: [u8; 32],
    pub server_node_id: [u8; 32],
    pub label: String,
    /// The code both devices show, derived from these two keys.
    pub verification_code: String,
    pub kind: EnrollmentKind,
}

/// Let discovery and approval listeners share the nearby port. Broadcasts
/// reach each listener, and enrollment authenticates the subsequent exchange.
pub fn bind_pair_listener(port: u16) -> anyhow::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
    Ok(socket.into())
}

/// Find one device of `kind` in the bounded nearby discovery window, when
/// the image offers local-network enrollment.
pub fn discover(
    kind: EnrollmentKind,
    timeout: u64,
    methods: Methods,
) -> anyhow::Result<PairCandidate> {
    methods.ensure(Method::LocalNetwork)?;
    listen(&bind_pair_listener(NEARBY_PORT)?, kind, timeout)
}

fn listen(socket: &UdpSocket, kind: EnrollmentKind, timeout: u64) -> anyhow::Result<PairCandidate> {
    socket.set_read_timeout(Some(Duration::from_secs(1)))?;
    let deadline = deadline_after(socket_timeout(timeout))?;
    let mut packet = vec![0_u8; MAX_PACKET];
    while Instant::now() < deadline {
        match socket.recv_from(&mut packet) {
            Ok((len, _)) => {
                let Ok(solicitation) =
                    postcard::from_bytes::<PairSolicitation>(received_packet(&packet, len)?)
                else {
                    continue;
                };
                if !valid_solicitation(&solicitation) || solicitation.kind != kind {
                    continue;
                }
                return Ok(PairCandidate {
                    device_public_key: solicitation.device_public_key,
                    server_node_id: solicitation.server_node_id,
                    label: solicitation.label,
                    verification_code: pairing_code(
                        solicitation.device_public_key,
                        solicitation.server_node_id,
                    ),
                    kind: solicitation.kind,
                });
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    bail!("no matching unprovisioned Tilo device was found nearby")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PAIR_MAGIC;

    #[test]
    fn nearby_discovery_filters_datagrams_and_binds_the_code_to_both_keys() {
        let receiver = bind_pair_listener(0).expect("nearby listener");
        let address = SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            receiver.local_addr().expect("address").port(),
        );
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("nearby sender");
        let device = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let mut solicitation = PairSolicitation {
            magic: PAIR_MAGIC,
            device_public_key: device.verifying_key().to_bytes(),
            server_node_id: [8; 32],
            label: "Phone".into(),
            hardware: String::new(),
            kind: EnrollmentKind::HomePeer,
        };
        let send = |message: &PairSolicitation| {
            let bytes = postcard::to_allocvec(message).expect("encode announcement");
            sender.send_to(&bytes, address).expect("announce");
        };
        sender
            .send_to(b"invalid", address)
            .expect("send invalid datagram");
        send(&solicitation);
        solicitation.kind = EnrollmentKind::PersonalDevice;
        solicitation.magic = [0; 8];
        solicitation.server_node_id = [9; 32];
        send(&solicitation);
        solicitation.magic = PAIR_MAGIC;
        solicitation.server_node_id = [8; 32];
        send(&solicitation);

        let candidate = listen(&receiver, EnrollmentKind::PersonalDevice, 1).expect("candidate");
        assert_eq!(candidate.kind, EnrollmentKind::PersonalDevice);
        assert_eq!(candidate.device_public_key, solicitation.device_public_key);
        assert_eq!(candidate.server_node_id, solicitation.server_node_id);
        assert_eq!(candidate.label, "Phone");
        assert_eq!(
            candidate.verification_code,
            pairing_code(solicitation.device_public_key, solicitation.server_node_id)
        );
    }

    #[test]
    fn nearby_discovery_refuses_an_image_without_local_network_enrollment() {
        let error = discover(EnrollmentKind::PersonalDevice, 1, Methods::NONE)
            .expect_err("image disallows discovery");
        let refusal = error
            .downcast_ref::<tilo_identity::enrollment::Refused>()
            .expect("method refusal");
        assert_eq!(refusal.method, Method::LocalNetwork);
    }
}
