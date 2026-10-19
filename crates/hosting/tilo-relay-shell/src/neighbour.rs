//! [`ShellPayloads`] for the desktop's peer-sync messages.
//!
//! Every classification here is the one the host applied before the seam
//! existed, so retained data written by any earlier host is judged exactly
//! as it was: retention and channel come from the one host-retention
//! classifier (`neighbour_wire::retention`), and the host identity channel
//! keeps its persisted derivation.

use std::collections::BTreeSet;

use neighbour_wire::NeighbourMsg;
use neighbour_wire::{Retention, audience, retention, standalone, steward_only};

use crate::{Checkpoint, CheckpointKind, Route, ShellPayloads};

/// Classifies co-edit bodies as the desktop's `NeighbourMsg` vocabulary.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeighbourPayloads;

impl ShellPayloads for NeighbourPayloads {
    fn is_document(&self, body: &[u8]) -> bool {
        NeighbourMsg::is_app_document(body)
    }

    fn open_document(&self, body: &[u8]) -> Option<(Route, Box<collab_types::SignedOp>)> {
        let Ok(NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            record_id,
            envelope,
            ..
        }) = postcard::from_bytes::<NeighbourMsg>(body)
        else {
            return None;
        };
        Some((
            Route {
                app_id,
                cluster_id,
                record_id,
            },
            envelope,
        ))
    }

    fn document_body(
        &self,
        author: &[u8; 32],
        route: &Route,
        op: collab_types::SignedOp,
    ) -> Result<Vec<u8>, String> {
        postcard::to_stdvec(&NeighbourMsg::AppDocument {
            peer: tilo_peer_state::peer_handle(author),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.clone(),
            record_id: route.record_id.clone(),
            envelope: Box::new(op),
        })
        .map_err(|error| error.to_string())
    }

    fn host_identity(
        &self,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<([u8; 32], Vec<u8>), String> {
        let hello = NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert.clone()),
            revocations: Vec::new(),
        };
        // Persisted: retained identity frames are keyed by this channel.
        let channel =
            *blake3::hash(&[b"identity-hello\0".as_slice(), &cert.device_public_key].concat())
                .as_bytes();
        let body = postcard::to_stdvec(&hello).map_err(|error| error.to_string())?;
        Ok((channel, body))
    }

    fn checkpoint(&self, channel: &[u8; 32], body: &[u8]) -> Option<Checkpoint> {
        let (msg, trailing) = postcard::take_from_bytes::<NeighbourMsg>(body).ok()?;
        if !trailing.is_empty() || retention(&msg, false) != Some((*channel, Retention::Checkpoint))
        {
            return None;
        }
        let kind = match &msg {
            NeighbourMsg::MlsGovernance { .. }
            | NeighbourMsg::MlsAccessRequest { .. }
            | NeighbourMsg::MlsRoutedAccessRequest { .. }
            | NeighbourMsg::MlsGovernanceAck { .. } => CheckpointKind::Governance,
            NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key, ..
            } => CheckpointKind::Continuity {
                user: *user_public_key,
            },
            _ => CheckpointKind::Other,
        };
        Some(Checkpoint {
            kind,
            standalone: standalone(&msg),
        })
    }

    fn audience(&self, body: &[u8]) -> Option<BTreeSet<[u8; 32]>> {
        let msg = postcard::from_bytes::<NeighbourMsg>(body).ok()?;
        let devices = audience(&msg)?;
        Some(devices.iter().map(|device| *device.as_bytes()).collect())
    }

    fn steward_only(&self, body: &[u8]) -> bool {
        postcard::from_bytes::<NeighbourMsg>(body).is_ok_and(|msg| steward_only(&msg))
    }

    fn decoded_overhead(&self) -> usize {
        std::mem::size_of::<NeighbourMsg>()
    }

    fn replay(&self) -> Option<&dyn tilo_relay_store::shell::ShellReplay> {
        Some(&crate::NeighbourReplay)
    }
}
