//! Structural owner completion. This component does not install an HTTP route.
//!
//! Growth and replay (fix plan B1):
//! - The Store keeps one minimal receipt per owner device (request ID, exact
//!   proof digest, completing generation), never the reply itself.
//! - A refresh while the governed state is exactly the one the last owner
//!   bootstrap was built from reuses that bootstrap's signed governance and
//!   makes no commit and no new governance messages.
//! - A lost-reply retry (same request ID and byte-identical proof, within
//!   `REPLAY_WINDOW_NS`) is answered from current state: byte-identical while
//!   nothing changed, otherwise an equivalent bootstrap at the current
//!   generation. Current owner membership is always rechecked; any other
//!   reuse of a request ID is refused.
use super::*;
use qos_peer_state::join::OwnerBootstrapV1;
mod receipt;
use receipt::{OwnerReceipt, ReceiptPurpose};

/// Drops legacy owner receipts that retained a whole bootstrap each. Runs on
/// every Store commit, so existing large Stores shrink on their next write.
pub(crate) fn prune_legacy_receipts(
    consumed: &mut std::collections::BTreeMap<String, (String, serde_json::Value)>,
) {
    consumed.retain(|key, _| !key.starts_with(receipt::LEGACY_PREFIX));
}

/// Process-local reuse of the signed governance an owner bootstrap was built
/// from. An entry is keyed by a digest of the exact governed Store content it
/// describes (room, host, roster, revocations and durable peer), so it can only
/// match a Store whose persisted governed state is that same content; it
/// grants nothing by itself. Commits that leave governance untouched (a
/// membership challenge, delivery bookkeeping) keep it reusable; the reply then
/// names the generation the governance was built at.
mod reuse {
    use std::{path::PathBuf, sync::Mutex, time::Instant};
    struct Entry {
        root: PathBuf,
        key: [u8; 32],
        generation: u64,
        built: Instant,
        messages: Vec<Vec<u8>>,
    }
    /// One entry per Store directory (a host serves one; tests open many).
    const ENTRIES: usize = 64;
    /// Reuse is bounded in time as well: an idle workspace re-signs its
    /// governance at most this often on refresh.
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(600);
    static LAST: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    pub(super) fn remember(
        root: &std::path::Path,
        key: [u8; 32],
        generation: u64,
        messages: Vec<Vec<u8>>,
    ) {
        if let Ok(mut entries) = LAST.lock() {
            entries.retain(|entry| entry.root != root);
            if entries.len() >= ENTRIES {
                entries.remove(0);
            }
            entries.push(Entry {
                root: root.to_owned(),
                key,
                generation,
                built: Instant::now(),
                messages,
            });
        }
    }
    /// The retained generation and governance for exactly this content.
    pub(super) fn lookup(
        root: &std::path::Path,
        key: [u8; 32],
        current_generation: u64,
    ) -> Option<(u64, Vec<Vec<u8>>)> {
        let entries = LAST.lock().ok()?;
        entries
            .iter()
            .find(|entry| {
                entry.root == root
                    && entry.key == key
                    && entry.generation <= current_generation
                    && entry.built.elapsed() < MAX_AGE
            })
            .map(|entry| (entry.generation, entry.messages.clone()))
    }
    #[cfg(test)]
    pub(super) fn forget(root: &std::path::Path) {
        if let Ok(mut entries) = LAST.lock() {
            entries.retain(|entry| entry.root != root);
        }
    }
}
fn reuse_key(state: &crate::State) -> Result<[u8; 32]> {
    let bound = state
        .bound
        .as_ref()
        .ok_or_else(|| anyhow!("workspace not enrolled"))?;
    let mut hasher = blake3::Hasher::new_derive_key("tilo cloud owner bootstrap reuse v1");
    let mut field = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    field(state.room.as_bytes());
    field(&postcard::to_stdvec(&bound.authority)?);
    field(&postcard::to_stdvec(&bound.roster)?);
    field(&postcard::to_stdvec(&bound.revocations)?);
    field(&bound.root);
    field(&bound.peer);
    Ok(*hasher.finalize().as_bytes())
}

/// Retains the signed governance a member bootstrap was built from, keyed by
/// the exact governed content `state` is about to commit (see `reuse`).
pub(super) fn remember_governance(
    root: &std::path::Path,
    state: &crate::State,
    generation: u64,
    messages: &[Vec<u8>],
) -> Result<()> {
    reuse::remember(root, reuse_key(state)?, generation, messages.to_vec());
    Ok(())
}
/// Test hook: forget the process-local reuse, as a restarted host would.
#[cfg(test)]
pub(crate) fn forget_reused_governance(root: &std::path::Path) {
    reuse::forget(root);
}
/// The retained governance for exactly the committed governed `state`.
pub(super) fn reused_governance(
    root: &std::path::Path,
    state: &crate::State,
) -> Result<Option<(u64, Vec<Vec<u8>>)>> {
    Ok(reuse::lookup(root, reuse_key(state)?, state.generation))
}

/// Result of a membership refresh, selected from governed Store state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MembershipRefresh {
    Member(JoinBootstrap),
    Owner(OwnerBootstrapV1),
}

impl MembershipRefresh {
    pub fn into_response(self) -> JoinTransportResponse {
        match self {
            Self::Member(bootstrap) => JoinTransportResponse::MembershipComplete(bootstrap),
            Self::Owner(bootstrap) => JoinTransportResponse::OwnerMembershipComplete(bootstrap),
        }
    }
}

impl Store {
    /// Routes a membership completion to the owner or collaborator path.
    /// The route comes only from host-held state: this device's typed owner
    /// receipt for this request, or the host-issued challenge's certificate
    /// root equal to the enrolled structural root. Each path then performs its
    /// own full verification, so routing never grants authority; the
    /// collaborator path still refuses owners and the owner path still
    /// refuses non-owners.
    pub fn membership_refresh(
        &mut self,
        remote_endpoint: [u8; 32],
        request: MembershipCompleteRequest,
        now: u64,
    ) -> Result<MembershipRefresh> {
        self.ensure_reconciled()?;
        let request_id = request.proof.challenge.request_id;
        let key = hex::encode(request_id);
        let owner = if self
            .owner_receipt(remote_endpoint)?
            .is_some_and(|receipt| receipt.request_id == request_id)
        {
            true
        } else if self.state.membership_responses.contains_key(&key) {
            false
        } else {
            let root = self.state.bound.as_ref().map(|bound| bound.root);
            super::issued::get(&self.root, &key)
                .is_some_and(|issued| Some(issued.device_cert.user_public_key) == root)
        };
        if owner {
            let bootstrap = self.owner_membership_complete(remote_endpoint, request, now)?;
            Ok(MembershipRefresh::Owner(bootstrap))
        } else {
            let bootstrap = self.membership_complete(remote_endpoint, request, now)?;
            self.verify_cached_membership(remote_endpoint, &bootstrap, now)?;
            Ok(MembershipRefresh::Member(bootstrap))
        }
    }

    /// Rechecks a refresh result before disclosure after an asynchronous step.
    pub fn verify_membership_refresh(
        &self,
        remote_endpoint: [u8; 32],
        refresh: &MembershipRefresh,
        now: u64,
    ) -> Result<()> {
        match refresh {
            MembershipRefresh::Member(bootstrap) => {
                self.verify_cached_membership(remote_endpoint, bootstrap, now)
            }
            MembershipRefresh::Owner(bootstrap) => {
                self.ensure_reconciled()?;
                if bootstrap.device_cert.device_public_key != remote_endpoint {
                    bail!("owner membership endpoint mismatch")
                }
                let now = self.policy_now_or(now)?;
                let peer = self.peer_at(now)?;
                self.verify_owner_scope(
                    &peer,
                    &bootstrap.device_cert,
                    &bootstrap.authority,
                    &bootstrap.room,
                    bootstrap.workspace_id,
                )?;
                peer.verify_current_owner(&bootstrap.device_cert, now_time(now))
                    .map_err(anyhow::Error::msg)
            }
        }
    }

    /// This device's retained owner receipt, if any. A malformed receipt is
    /// a refusal, never a missing one.
    fn owner_receipt(&self, device: [u8; 32]) -> Result<Option<OwnerReceipt>> {
        let Some((digest, value)) = self.state.consumed.get(&receipt::key(device)) else {
            return Ok(None);
        };
        let receipt = OwnerReceipt::decode(value)?;
        if receipt.device != device
            || receipt.purpose != ReceiptPurpose::OwnerMembership
            || digest != &hex::encode(receipt.proof_digest)
        {
            bail!("owner membership receipt binding mismatch")
        }
        Ok(Some(receipt))
    }

    fn owner_bootstrap(
        &self,
        device_cert: &DeviceCert,
        workspace_id: u64,
        peer: &qos_peer_state::StatePeer,
        governance_messages: Vec<Vec<u8>>,
        generation: u64,
    ) -> OwnerBootstrapV1 {
        OwnerBootstrapV1 {
            version: 1,
            device_cert: device_cert.clone(),
            room: self.state.room.clone(),
            workspace_id,
            authority: peer.authority().clone(),
            certified_roster: peer.certificates(),
            revocations: peer.revocations(),
            records: peer.rows(),
            governance_messages,
            generation,
        }
    }

    /// Completes only a currently verified structural owner's membership proof.
    /// The eventual HTTP dispatcher must choose this path from actual governed
    /// role and enrolled root, never from a client flag or material label.
    pub fn owner_membership_complete(
        &mut self,
        remote_endpoint: [u8; 32],
        request: MembershipCompleteRequest,
        now: u64,
    ) -> Result<OwnerBootstrapV1> {
        self.ensure_reconciled()?;
        let now = self.policy_now_or(now)?;
        let request_id = request.proof.challenge.request_id;
        if request_id == [0; 32] {
            bail!("owner membership request ID is absent")
        }
        let proof_digest = *blake3::hash(&postcard::to_stdvec(&request)?).as_bytes();
        let challenge_key = hex::encode(request_id);
        let (challenge, device_cert, verified_at, replay) =
            if let Some(issued) = super::issued::get(&self.root, &challenge_key) {
                if issued.device_cert.device_public_key != remote_endpoint {
                    bail!("owner membership proof endpoint mismatch")
                }
                super::issued::check_proof(&issued, proof_digest)?;
                (issued.challenge, issued.device_cert, now, false)
            } else if let Some(receipt) = self
                .owner_receipt(remote_endpoint)?
                .filter(|receipt| receipt.request_id == request_id)
            {
                // A lost-reply retry. The retained digest binds the exact proof
                // this Store verified at completion (against its own one-use
                // challenge); authority is re-established from current state below.
                if receipt.proof_digest != proof_digest {
                    bail!("owner membership proof request ID substitution")
                }
                if receipt.completed_at_ns > now
                    || now - receipt.completed_at_ns >= receipt::REPLAY_WINDOW_NS
                    || receipt.generation > self.state.generation
                {
                    bail!("owner membership receipt expired; request a new membership challenge")
                }
                let cert = self
                    .peer_at(now)?
                    .certificates()
                    .into_iter()
                    .find(|cert| cert.device_public_key == remote_endpoint)
                    .ok_or_else(|| {
                        anyhow!("owner membership certificate is absent from governed state")
                    })?;
                (
                    request.proof.challenge.clone(),
                    cert,
                    receipt.completed_at_ns,
                    true,
                )
            } else {
                bail!("membership challenge was not issued")
            };
        let (before, mut peer) = self.peer_transition_at(now)?;
        let membership = verify_membership_proof(
            &request.proof,
            &challenge,
            &device_cert,
            &revocations(&peer.revocations())?,
            verified_at,
        )
        .map_err(anyhow::Error::msg)?;
        if membership.workspace_id() != qos_relay::room_workspace_id(&self.state.room)? {
            bail!("owner membership workspace mismatch")
        }
        peer.verify_owner_refresh(&membership, now_time(self.policy_now_or(now)?))
            .map_err(anyhow::Error::msg)?;
        self.verify_owner_scope(
            &peer,
            &device_cert,
            peer.authority(),
            &self.state.room,
            membership.workspace_id(),
        )?;
        // Nothing changed since the last owner bootstrap was built from this
        // exact governed state: reuse its signed governance without a commit.
        if crate::live_room_publication(&before, &peer)?.is_none() {
            if let Some((generation, messages)) =
                reuse::lookup(&self.root, reuse_key(&self.state)?, self.state.generation)
            {
                let bootstrap = self.owner_bootstrap(
                    &device_cert,
                    membership.workspace_id(),
                    &peer,
                    messages,
                    generation,
                );
                peer.verify_current_owner(
                    &bootstrap.device_cert,
                    now_time(self.policy_now_or(now)?),
                )
                .map_err(anyhow::Error::msg)?;
                if !replay {
                    // No commit: bind the one-use challenge to this exact proof.
                    super::issued::complete(&self.root, &challenge_key, proof_digest);
                }
                audit(
                    "collaboration_owner_membership_refreshed",
                    serde_json::json!({
                        "room": self.state.room,
                        "request_id": hex::encode(request_id),
                        "caller_device": hex::encode(remote_endpoint),
                        "caller_root": hex::encode(bootstrap.device_cert.user_public_key),
                        "generation": generation.to_string(),
                        "result": if replay { "replayed" } else { "reused" },
                    }),
                );
                return Ok(bootstrap);
            }
        }
        let generation = self.next_generation()?;
        let messages = crate::durable_bootstrap_messages(
            &mut peer,
            &self.key(),
            now_time(self.policy_now_or(now)?),
            generation,
        )?;
        let live = live_projection_from_bootstrap(&before, &peer, &messages)?;
        let governance_messages = messages
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
        let bootstrap = self.owner_bootstrap(
            &device_cert,
            membership.workspace_id(),
            &peer,
            governance_messages,
            generation,
        );
        let stored = OwnerReceipt {
            version: 2,
            purpose: ReceiptPurpose::OwnerMembership,
            device: remote_endpoint,
            request_id,
            proof_digest,
            generation,
            completed_at_ns: now,
        };
        let value = stored.encode()?;
        let mut state = self.state.clone();
        state.outbox.extend(live);
        state
            .bound
            .as_mut()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?
            .peer = peer
            .durable()
            .map_err(|error| anyhow::Error::msg(error).context(crate::StoreUnavailable))?;
        prune_legacy_receipts(&mut state.consumed);
        state.consumed.insert(
            receipt::key(remote_endpoint),
            (hex::encode(proof_digest), value),
        );
        peer.verify_current_owner(&bootstrap.device_cert, now_time(self.policy_now_or(now)?))
            .map_err(anyhow::Error::msg)?;
        // Keyed by the exact prepared governed content: it can only ever match
        // a Store that actually holds this state (including a commit whose
        // outcome is reconciled after a reopen).
        reuse::remember(
            &self.root,
            reuse_key(&state)?,
            generation,
            bootstrap.governance_messages.clone(),
        );
        self.commit(state)?;
        // Consumed: a lost-reply retry is now answered from the owner receipt.
        super::issued::remove(&self.root, &challenge_key);
        audit(
            "collaboration_owner_membership_refreshed",
            serde_json::json!({
                "room": self.state.room,
                "request_id": hex::encode(request_id),
                "caller_device": hex::encode(remote_endpoint),
                "caller_root": hex::encode(bootstrap.device_cert.user_public_key),
                "generation": self.state.generation.to_string(),
                "result": if replay { "replay_committed" } else { "committed" },
            }),
        );
        peer.verify_current_owner(&bootstrap.device_cert, now_time(self.policy_now_or(now)?))
            .map_err(anyhow::Error::msg)?;
        Ok(bootstrap)
    }

    fn verify_owner_scope(
        &self,
        peer: &qos_peer_state::StatePeer,
        cert: &DeviceCert,
        authority: &DeviceCert,
        room: &str,
        workspace: u64,
    ) -> Result<()> {
        let bound = self
            .state
            .bound
            .as_ref()
            .ok_or_else(|| anyhow!("workspace not enrolled"))?;
        if room != self.state.room
            || workspace != qos_relay::room_workspace_id(&self.state.room)?
            || authority != &bound.authority
            || authority != peer.authority()
            || authority.device_public_key != self.key().verifying_key().to_bytes()
            || cert.user_public_key != bound.root
            || authority.user_public_key != bound.root
        {
            bail!("owner membership pinned scope mismatch")
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
