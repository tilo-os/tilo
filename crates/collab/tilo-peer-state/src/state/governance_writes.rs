//! Writing a governance change as this device: the operation recording what
//! it moved, then taking it in as every member will.
use super::{
    AutoCommit, CachedDocument, NeighbourMsg, StatePeer, SystemTime, TransportOrigin, backend, err,
    now_ns, workspace,
};
use automerge::transaction::Transactable as _;
use collab_types::signer::DeviceSigner;

/// Commit what `doc` holds as one governance change, stamped with `unix_secs`.
///
/// The change says who wrote it and when by itself: its actor names the writing device — or, in a
/// workspace a server keeps, that server's actor — whose certificate among the workspace's own
/// rows names the person it belongs to, and its time is `unix_secs`. Nothing writes either again
/// as a message.
pub(crate) fn commit_governance(doc: &mut AutoCommit, unix_secs: i64) {
    if doc.pending_ops() == 0 {
        // Commit would make a change of nobody's: roll the empty transaction back instead.
        doc.rollback();
        return;
    }
    doc.commit_with(automerge::transaction::CommitOptions::default().with_time(unix_secs));
}

/// The wall clock in the unit automerge stamps a change's time with, for a governance change whose
/// writer has no `now` handed to it.
pub(crate) fn wall_secs() -> i64 {
    i64::try_from(tilo_clock::try_unix_now().unwrap_or_default()).unwrap_or(i64::MAX)
}

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
        // What the caller wrote lands as one governance change by this device, stamped with its
        // time: the operation recording it is the next one.
        commit_governance(&mut doc, super::now_secs(now));
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
            // Only what the change wrote can differ from what is held: the rows of the change
            // committed above, not the operations still open here, which reading the document's
            // changes would close without a stamp.
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
        // The operations, and the policy bound to what they leave, are one change of this
        // device's, stamped like the one that moved the rows.
        commit_governance(&mut doc, super::now_secs(now));
        // The operations are written on top of every head the state held.
        let recorded = doc.get_changes(&unrecorded);
        crate::members::replay::carry(&unrecorded, &doc, &recorded, &self.core.workspace);
        let changes = doc
            .get_changes(before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let workspace = self.core.workspace.clone();
        let outcome = self
            .receive_from(
                TransportOrigin::ServerDelta(key.device_public_key()),
                NeighbourMsg::SyncResp { changes, workspace },
                now,
            )
            .map_err(|error| error.to_string())?;
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
        // A holder that is no member admits through invitations under the
        // certificate its signed address carries.
        let device = key.device_public_key();
        let cert = match self.certificate(&device, now) {
            Ok(cert) => cert.clone(),
            Err(error) => match (&command, self.holder_certificate(&device)) {
                (members::MembershipOpCommand::Admit { .. }, Some(cert)) => cert,
                _ => return Err(error.to_string()),
            },
        };
        let op = SignedMembershipOp::signed(
            &cert,
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
        // A member's device signs as the certificate the workspace holds of
        // it; a holder that is no member, as the device its signed entry
        // names. Every receiver judges the row the operation names by the
        // operation's author all the same.
        let cert = match self.certificate(&key.device_public_key(), now) {
            Ok(cert) => cert.clone(),
            Err(error) => self
                .holder_certificate(&key.device_public_key())
                .ok_or_else(|| error.to_string())?,
        };
        let op = SignedMembershipOp::signed(
            &cert,
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
