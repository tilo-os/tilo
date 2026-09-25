//! Read-only recognition of a scoped worker publication that adds nothing.
//!
//! A mounted host worker republishes the changes it merged from remote peers
//! (browser uploads, relay deliveries) as `sent_heads..heads`. The SDK attaches
//! required document custody to that body, but the body carries no invocation
//! contributor and no change the durable owner has not already committed. Such
//! a body is not an invocation effect: it is an echo of committed history and
//! may use the ordinary owner signing door. Anything else keeps its refusal.
use super::*;
use collab_types::DocumentMutation;
use std::collections::BTreeSet;
use ui::{automerge, collaboration::RouteSpec, invocation_scope::DocumentAuthorization};

/// Bound on the change chunks one echoed worker publication may contain.
const MAX_ECHO_CHANGES: usize = 65_536;

impl SessionOwner {
    /// Succeeds only when `mutation` is bound to `authorization`, carries no
    /// original invocation contributor, parses completely, and every change it
    /// contains is already present in this owner's committed document. This
    /// performs no store write and grants no invocation authority.
    pub(crate) fn validate_committed_echo(
        &self,
        route: &RouteSpec,
        mutation: &DocumentMutation,
        authorization: &DocumentAuthorization,
    ) -> Result<(), String> {
        authorization.check_binding(route, mutation)?;
        authorization.validate_owner_contributors(|_, _, _| {
            Err("committed echo cannot acknowledge invocation contributors".into())
        })?;
        if route.cluster_id.to_string() != self.peer.room() {
            return Err("committed echo route belongs to another room".into());
        }
        let record = route.record.as_deref().unwrap_or("");
        let committed = self
            .peer
            .document(&route.app_id, record)
            .ok_or("committed echo document unavailable")?;
        let mut committed = automerge::AutoCommit::load(&committed)
            .map_err(|_| "committed document unavailable")?;
        let actual = mutation_change_hashes(mutation)?;
        if actual.is_empty() {
            return Err("committed echo carries no change".into());
        }
        let actual = actual.into_iter().collect::<Vec<_>>();
        authorization
            .validate_owner_base(&actual, |hash| committed.get_change_by_hash(hash).is_some())
    }
}

/// Complete change set parsed from the exact mutation body, never from the
/// captured metadata. Snapshot bodies contain every change of the document;
/// incremental bodies are concatenated raw change chunks (`save_after`).
pub(crate) fn mutation_change_hashes(
    mutation: &DocumentMutation,
) -> Result<BTreeSet<automerge::ChangeHash>, String> {
    match mutation {
        DocumentMutation::Snapshot(bytes) => {
            let mut document = automerge::AutoCommit::load(bytes)
                .map_err(|_| "echo snapshot body does not parse")?;
            let changes = document.get_changes(&[]);
            if changes.len() > MAX_ECHO_CHANGES {
                return Err("echo snapshot body has too many changes".into());
            }
            Ok(changes.into_iter().map(|change| change.hash()).collect())
        }
        DocumentMutation::Incremental(bytes) => {
            const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
            let mut hashes = BTreeSet::new();
            let mut rest = bytes.as_slice();
            while !rest.is_empty() {
                if hashes.len() >= MAX_ECHO_CHANGES {
                    return Err("echo incremental body has too many changes".into());
                }
                // magic(4) checksum(4) type(1) uleb128(len) data(len)
                if rest.len() < 9 || rest[..4] != MAGIC {
                    return Err("echo incremental body has an invalid chunk header".into());
                }
                let (mut length, mut shift, mut at) = (0_u64, 0_u32, 9_usize);
                loop {
                    let byte = *rest.get(at).ok_or("echo incremental chunk truncated")?;
                    at += 1;
                    if shift >= 63 {
                        return Err("echo incremental chunk length overflow".into());
                    }
                    length |= u64::from(byte & 0x7f) << shift;
                    shift += 7;
                    if byte & 0x80 == 0 {
                        break;
                    }
                }
                let end = usize::try_from(length)
                    .ok()
                    .and_then(|length| at.checked_add(length))
                    .filter(|end| *end <= rest.len())
                    .ok_or("echo incremental chunk truncated")?;
                let change = automerge::Change::try_from(&rest[..end])
                    .map_err(|_| "echo incremental body contains a non-change chunk")?;
                if !hashes.insert(change.hash()) {
                    return Err("echo incremental body repeats a change".into());
                }
                rest = &rest[end..];
            }
            Ok(hashes)
        }
    }
}
