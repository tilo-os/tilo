//! Writing a governance change as this device: the operation recording what
//! it moved, then taking it in as every member will.
use super::{
    AutoCommit, CachedDocument, NeighbourMsg, StatePeer, SystemTime, TransportOrigin, backend, err,
    now_ns, workspace,
};
use collab_types::signer::DeviceSigner;

impl StatePeer {
    /// Write the governance change `doc` holds as `key`'s device, and take it in
    /// as every member will ([`Self::write_membership`] with no membership
    /// change).
    pub(super) fn write_governance(
        &mut self,
        doc: CachedDocument,
        before: &[automerge::ChangeHash],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        self.write_membership(doc, before, key, now, None)
    }

    /// Write the governance change `doc` holds, and `membership`, the change
    /// to the workspace's membership this device makes with it, as `key`'s
    /// device, and take it in as every member will.
    ///
    /// The membership change is this device's operation on the log every
    /// device replays ([`crate::members::replay`]), and a change its grants do
    /// not allow is refused here rather than written for other devices to
    /// discard. The other parts of the workspace's governance the change moves
    /// are recorded as an operation naming them, so every receiver judges each
    /// by its author's role ([`crate::workspace::membership`]).
    #[cfg_attr(
        test,
        expect(
            clippy::panic_in_result_fn,
            reason = "a test's every governance write checks the narrowed row diff against the whole one"
        )
    )]
    pub(super) fn write_membership(
        &mut self,
        mut doc: CachedDocument,
        before: &[automerge::ChangeHash],
        key: &dyn DeviceSigner,
        now: SystemTime,
        membership: Option<crate::members::MembershipOpCommand>,
    ) -> Result<bool, String> {
        // A change that writes no operation leaves the membership as it was.
        let written = doc.get_changes(before);
        crate::members::replay::carry(before, &doc, &written, &self.core.workspace);
        // The membership change goes first: the media policy is rebound to the
        // membership it leaves.
        let unrecorded = doc.get_heads();
        // An admission names the device row it writes itself
        // ([`workspace::membership::admit_operations`]).
        let admits = matches!(
            membership,
            Some(crate::members::MembershipOpCommand::Admit { .. })
        );
        if let Some(membership) = membership {
            self.record_membership(&mut doc, key, now, membership)?;
        }
        let mut rows = {
            let held = self
                .core
                .workspace_doc
                .as_deref()
                .ok_or("missing workspace")?;
            // A policy the workspace holds is rebound to the security basis its
            // change moves; one it does not hold yet is its author's, and
            // initialising it is never this refresh's.
            if workspace::media::parse(held).map_err(err)?.is_some() {
                workspace::media::refresh_authorization(held, &mut doc).map_err(err)?;
            }
            // Only what the change wrote can differ from what is held.
            let written = doc.get_changes(before);
            let rows = crate::workspace::changed_rows_by(held, &doc, &written, false);
            // Every governance write a test makes checks the narrowed
            // reading against the whole one.
            #[cfg(test)]
            assert_eq!(
                rows,
                crate::workspace::changed_rows(Some(held), Some(&doc), false),
                "the parts a change wrote are the parts it changed"
            );
            rows
        };
        if admits {
            rows.retain(|row| *row != crate::workspace::GovernanceRow::Identities);
        }
        self.record_governance(&mut doc, rows, key, now)?;
        // The operations are written on top of every head the state held.
        let recorded = doc.get_changes(&unrecorded);
        crate::members::replay::carry(&unrecorded, &doc, &recorded, &self.core.workspace);
        let changes = doc
            .get_changes(before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let workspace = self.core.workspace.clone();
        let outcome = self.receive_from(
            TransportOrigin::RelayDelta(key.device_public_key()),
            NeighbourMsg::SyncResp {
                changes,
                room: workspace,
            },
            now,
        )?;
        Ok(outcome.changed || outcome.metadata_changed)
    }

    /// Write `changes` to the workspace's membership as `key`'s device, and
    /// take them in as every member will. Whether the workspace changed: no
    /// change writes nothing.
    pub(super) fn write_changes(
        &mut self,
        mut doc: CachedDocument,
        changes: Vec<tilo_action_policy::Change>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        if changes.is_empty() {
            return Ok(false);
        }
        doc.set_actor(self.workspace_actor_of(key));
        let before = doc.get_heads();
        self.write_membership(
            doc,
            &before,
            key,
            now,
            Some(crate::members::MembershipOpCommand::Project { changes }),
        )
    }

    /// Record `command`, this device's membership change, as an operation on
    /// the log every device replays. A change its grants do not allow is
    /// refused here.
    fn record_membership(
        &self,
        doc: &mut CachedDocument,
        key: &dyn DeviceSigner,
        now: SystemTime,
        command: crate::members::MembershipOpCommand,
    ) -> Result<(), String> {
        use crate::members::{self, SignedMembershipOp};
        let workspace = self.core.workspace.clone();
        let cert = self
            .certificate(&key.device_public_key(), now)
            .map_err(|error| error.to_string())?;
        let op = SignedMembershipOp::signed(
            cert,
            key,
            members::heads_of(doc, &workspace),
            now_ns(now),
            command,
        )?;
        if let Some(why) = members::refused(doc, &members::replay(doc, &workspace), &op) {
            return Err(format!(
                "the membership change is not this device's to make, as device {} of {}: {why}",
                hex::encode(op.author.device_public_key),
                op.person(),
            ));
        }
        members::put_op(doc, &op)?;
        Ok(())
    }

    /// Record the governance rows `doc` moved as this device's operation, so
    /// every receiver judges each by its author's role
    /// ([`crate::workspace::membership`]).
    pub(super) fn record_governance(
        &self,
        doc: &mut CachedDocument,
        mut rows: Vec<crate::workspace::GovernanceRow>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        use crate::members::{self, MembershipOpCommand, SignedMembershipOp};
        let workspace = self.core.workspace.clone();
        if rows.is_empty() {
            return Ok(());
        }
        rows.sort_unstable();
        let cert = self
            .certificate(&key.device_public_key(), now)
            .map_err(|error| error.to_string())?;
        let op = SignedMembershipOp::signed(
            cert,
            key,
            members::heads_of(doc, &workspace),
            now_ns(now),
            MembershipOpCommand::Governance { rows },
        )?;
        members::put_op(doc, &op)?;
        Ok(())
    }

    /// [`Self::import_governed_certificates`] of a document whose rows were
    /// read once per state.
    pub(super) fn import_governed_rows(
        &mut self,
        doc: &CachedDocument,
        now: SystemTime,
    ) -> Result<bool, String> {
        let (certificates, revocations, changed) = backend::governed::cached(
            doc,
            &self.core.certificates,
            &self.core.revocations,
            &self.core.owner,
            now,
        )
        .map_err(err)?;
        self.core.certificates = certificates.into();
        self.core.revocations = revocations;
        Ok(changed)
    }
    pub(super) fn import_governed_certificates(
        &mut self,
        doc: &AutoCommit,
        now: SystemTime,
    ) -> Result<bool, String> {
        let (certificates, revocations, changed) = backend::governed::legacy(
            doc,
            &self.core.certificates,
            &self.core.revocations,
            &self.core.owner,
            now,
        )
        .map_err(err)?;
        self.core.certificates = certificates.into();
        self.core.revocations = revocations;
        Ok(changed)
    }
}
