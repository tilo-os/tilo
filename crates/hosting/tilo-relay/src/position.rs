//! Subscription positions: what a device has received of a workspace, as an
//! opaque value only this host can mint or read back.
//!
//! A subscriber that resumes ([`Envelope::SubscribeFrom`]) is followed by a
//! [`Envelope::Position`] after its durable deliveries and presents the last
//! one when it reconnects, so the host replays only what that device has not
//! received instead of the workspace's whole retained history. What it
//! received is the host's per-device resume log (see
//! [`crate::store::WorkspaceStore::append_resume`]); a position names a log
//! and how many entries of it the device holds. It binds the workspace, the
//! device and the store generation, so it is refused for another device and
//! after the host's state was replaced, and the host then replays everything.
//! Positions are authenticated with a key derived from the host's device key,
//! so they survive host restarts; they grant nothing, the subscription itself
//! is authorised as usual.
//!
//! This is delivery v3's cursor idea (`tilo-delivery-protocol`'s host-signed
//! `CursorToken`) reduced to what one relay host needs: a keyed BLAKE3 MAC in
//! place of a signature, since only the host that issued a position ever
//! checks it.

use serde::{Deserialize, Serialize};

use crate::transport::Envelope;

const KEY_CONTEXT: &str = "tilo-relay subscription position v2";
const VERSION: u8 = 2;
const MAC_BYTES: usize = 32;
/// Longest position accepted from a peer.
pub const MAX_POSITION_BYTES: usize = 256;

#[derive(Serialize, Deserialize)]
struct Claims {
    version: u8,
    workspace: u64,
    device: [u8; 32],
    generation: [u8; 16],
    log: [u8; 16],
    len: u64,
}

/// What a position stands for: `len` entries of resume log `log`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resume {
    /// The device's resume log.
    pub log: [u8; 16],
    /// How many of its entries the device holds.
    pub len: u64,
}

/// Mints and checks positions for one host.
#[derive(Clone, Copy)]
pub struct PositionIssuer {
    key: [u8; 32],
}

impl PositionIssuer {
    /// The issuer of the host whose device secret key is `host_secret`.
    pub fn new(host_secret: &[u8; 32]) -> Self {
        Self {
            key: blake3::derive_key(KEY_CONTEXT, host_secret),
        }
    }

    /// The position covering `resume` for `device` in `workspace`.
    pub fn issue(
        &self,
        workspace: u64,
        device: [u8; 32],
        generation: [u8; 16],
        resume: Resume,
    ) -> Vec<u8> {
        let mut bytes = postcard::to_allocvec(&Claims {
            version: VERSION,
            workspace,
            device,
            generation,
            log: resume.log,
            len: resume.len,
        })
        .unwrap_or_default();
        let mac = blake3::keyed_hash(&self.key, &bytes);
        bytes.extend_from_slice(mac.as_bytes());
        bytes
    }

    /// What `position` stands for, when this host issued it to `device` for
    /// `workspace` in store `generation`.
    pub fn verify(
        &self,
        position: &[u8],
        workspace: u64,
        device: &[u8; 32],
        generation: &[u8; 16],
    ) -> Option<Resume> {
        if position.len() > MAX_POSITION_BYTES {
            return None;
        }
        let split = position.len().checked_sub(MAC_BYTES)?;
        let (claims, mac) = position.split_at(split);
        let mac: [u8; MAC_BYTES] = mac.try_into().ok()?;
        if blake3::Hash::from(mac) != blake3::keyed_hash(&self.key, claims) {
            return None;
        }
        let claims: Claims = postcard::from_bytes(claims).ok()?;
        (claims.version == VERSION
            && claims.workspace == workspace
            && claims.device == *device
            && claims.generation == *generation)
            .then_some(Resume {
                log: claims.log,
                len: claims.len,
            })
    }

    /// The envelope telling `device` it holds `resume`.
    pub fn envelope(
        &self,
        workspace: u64,
        device: [u8; 32],
        generation: [u8; 16],
        resume: Resume,
    ) -> Envelope {
        Envelope::Position {
            workspace_id: workspace,
            position: self.issue(workspace, device, generation, resume),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_reads_back_only_where_it_was_issued() {
        let issuer = PositionIssuer::new(&[1; 32]);
        let resume = Resume {
            log: [4; 16],
            len: 42,
        };
        let position = issuer.issue(7, [2; 32], [3; 16], resume);
        assert_eq!(
            issuer.verify(&position, 7, &[2; 32], &[3; 16]),
            Some(resume)
        );
        assert_eq!(
            issuer.verify(&position, 8, &[2; 32], &[3; 16]),
            None,
            "workspace"
        );
        assert_eq!(
            issuer.verify(&position, 7, &[9; 32], &[3; 16]),
            None,
            "device"
        );
        assert_eq!(
            issuer.verify(&position, 7, &[2; 32], &[9; 16]),
            None,
            "generation"
        );
        assert_eq!(
            PositionIssuer::new(&[5; 32]).verify(&position, 7, &[2; 32], &[3; 16]),
            None,
            "another host"
        );
        let mut forged = position;
        forged[1] ^= 1;
        assert_eq!(
            issuer.verify(&forged, 7, &[2; 32], &[3; 16]),
            None,
            "tampered"
        );
        assert_eq!(issuer.verify(&[], 7, &[2; 32], &[3; 16]), None);
    }
}
