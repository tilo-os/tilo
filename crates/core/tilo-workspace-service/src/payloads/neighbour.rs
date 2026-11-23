//! [`ShellPayloads`](super::ShellPayloads) for the desktop's peer-sync messages.
//!
//! Every classification here is the one the host applied before the boundary
//! existed, so retained data written by any earlier host is judged exactly
//! as it was: retention and channel come from the one host-retention
//! classifier (`neighbour_wire::retention`), and the host identity channel
//! keeps its persisted derivation.

use std::collections::BTreeSet;

use neighbour_wire::NeighbourMsg;
use neighbour_wire::{Retention, audience, retention, standalone};

use super::{Checkpoint, CheckpointKind, Route, ShellPayloads};

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
            document_id,
            envelope,
            readers,
            ..
        }) = NeighbourMsg::decode(body)
        else {
            return None;
        };
        Some((
            Route {
                app_id,
                cluster_id,
                document_id,
                readers,
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
            document_id: route.document_id.clone(),
            envelope: Box::new(op),
            readers: route.readers.clone(),
        })
        .map_err(|error| error.to_string())
    }

    fn checkpoint(&self, channel: &[u8; 32], body: &[u8]) -> Option<Checkpoint> {
        let msg = NeighbourMsg::decode(body).ok()?;
        if retention(&msg, false) != Some((*channel, Retention::Checkpoint)) {
            return None;
        }
        let kind = match &msg {
            msg if msg.governance_workspace().is_some() => CheckpointKind::Governance,
            NeighbourMsg::IdentityHello { .. } => CheckpointKind::Identity,
            _ => CheckpointKind::Other,
        };
        Some(Checkpoint {
            kind,
            standalone: standalone(&msg),
        })
    }

    fn audience(&self, body: &[u8]) -> Option<BTreeSet<[u8; 32]>> {
        // Only a channel's commit has an audience: nothing else is decoded.
        if !NeighbourMsg::is_governance(body) {
            return None;
        }
        let msg = NeighbourMsg::decode(body).ok()?;
        let devices = audience(&msg)?;
        Some(devices.iter().map(|device| *device.as_bytes()).collect())
    }

    fn asks_admission(&self, body: &[u8]) -> bool {
        NeighbourMsg::is_access_request(body)
    }

    fn identity(
        &self,
        body: &[u8],
    ) -> Option<(
        tilo_user_identity::DeviceCert,
        Vec<tilo_user_identity::RevocationEntry>,
    )> {
        match NeighbourMsg::decode(body).ok()? {
            NeighbourMsg::IdentityHello {
                cert, revocations, ..
            } => Some((*cert, revocations)),
            _ => None,
        }
    }

    fn compaction_covers(&self, body: &[u8]) -> Option<Vec<([u8; 32], u64)>> {
        if !NeighbourMsg::is_app_document(body) {
            return None;
        }
        let NeighbourMsg::AppDocument { envelope, .. } = NeighbourMsg::decode(body).ok()? else {
            return None;
        };
        let compaction = collab_types::SealedCompaction::parse(&envelope).ok()?;
        Some(
            compaction
                .covers
                .iter()
                .map(|(author, seq)| (*author.as_bytes(), *seq))
                .collect(),
        )
    }

    fn decoded_overhead(&self) -> usize {
        std::mem::size_of::<NeighbourMsg>()
    }
}
