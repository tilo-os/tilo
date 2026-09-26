//! Normal governor refresh, retaining the actual original owner policy time.
use super::*;
use qos_action_broker::peer_actions::PolicyTime;

pub(crate) struct CheckpointPolicyFence {
    time: PolicyTime,
    root: PathBuf,
    room: String,
    certificate: DeviceCert,
    generation: u64,
    messages: Vec<NeighbourMsg>,
}
impl CheckpointPolicyFence {
    pub(crate) fn policy_time(&self) -> &PolicyTime {
        &self.time
    }
    pub(crate) fn revalidate(&self, owner: &SessionOwner) -> std::result::Result<(), String> {
        let now = policy_fence::sample(Some(&self.time))?;
        if owner.root() != self.root
            || owner.peer().room() != self.room
            || owner.committed_generation() != self.generation
            || !owner.permits_publication()
            || owner.identity.native_cert != self.certificate
            || owner.peer().authority() != &self.certificate
            || !owner.peer().certificates().contains(&self.certificate)
        {
            return Err("checkpoint original governor custody changed".into());
        }
        owner
            .peer()
            .permissions(self.certificate.device_public_key, now)?;
        owner.peer().validated_media_policy()?;
        for message in &self.messages {
            if let NeighbourMsg::AppDocument {
                app_id,
                cluster_id,
                record_id,
                envelope,
                ..
            } = message
            {
                if cluster_id != &self.room
                    || envelope.author.as_bytes() != &self.certificate.device_public_key
                    || !qos_peer_state::outbox::fresh(message, now)
                {
                    return Err("checkpoint signed publication is no longer current".into());
                }
                // This existing native lookup repeats current document write
                // policy, route and signature checks without applying history.
                // None means this newly signed envelope is not an old retry.
                owner
                    .peer()
                    .lookup_exact_local_document(app_id, record_id, envelope, now)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }
}

impl SessionOwner {
    pub(crate) fn refresh_checkpoint_with_policy_time(
        &mut self,
        time: &PolicyTime,
    ) -> Result<CommitReceipt> {
        self.require_owner_resource_scope()?;
        self.aggregate.require_writable()?;
        let now = policy_fence::sample(Some(time)).map_err(OwnerPolicyRefused)?;
        let mut candidate = self.peer.clone();
        let messages = self.prepare_certified_bootstrap_at(
            &mut candidate,
            self.aggregate
                .generation
                .checked_add(1)
                .context("session generation exhausted")?,
            now,
        )?;
        let mut next = self.aggregate.clone();
        self.replace_publication(&mut next, &messages)?;
        let fence = CheckpointPolicyFence {
            time: time.clone(),
            root: self.root().to_owned(),
            room: self.peer.room().to_owned(),
            certificate: self.identity.native_cert.clone(),
            generation: self.committed_generation(),
            messages,
        };
        fence.revalidate(self).map_err(OwnerPolicyRefused)?;
        self.commit_delivery(
            next,
            candidate,
            None,
            CommitMode::PolicyFenced(OwnerPolicyFence::Checkpoint(fence)),
        )
    }
}
