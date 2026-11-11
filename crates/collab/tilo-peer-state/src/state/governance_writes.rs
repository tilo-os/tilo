//! Writing a governance change as this device: the operation recording what
//! it moved, then taking it in as every member will.
use super::{
    AutoCommit, CachedDocument, NeighbourMsg, SigningKey, StatePeer, SystemTime, TransportOrigin,
    backend, err, now_ns, workspace,
};

impl StatePeer {
    /// Write the governance change `doc` holds as `key`'s device, and take it in
    /// as every member will.
    ///
    /// The parts of the workspace's governance the change moves are recorded as
    /// this device's operation, so every receiver judges each by its author's
    /// role ([`crate::workspace::membership`]): a part this device's role does
    /// not permit is refused here, by the same admission that receives it.
    #[cfg_attr(
        test,
        expect(
            clippy::panic_in_result_fn,
            reason = "a test's every governance write checks the narrowed row diff against the whole one"
        )
    )]
    pub(super) fn write_governance(
        &mut self,
        mut doc: CachedDocument,
        before: &[automerge::ChangeHash],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let rows = {
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
            // A change that leaves what the membership is read from as it
            // was leaves the membership as it was.
            crate::members::replay::carry(before, &doc, &written, &self.core.workspace);
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
        self.record_membership(&mut doc, before, key, now)?;
        let membership_recorded = doc.get_heads();
        self.record_governance(&mut doc, rows, key, now)?;
        // The operation naming the rows the change moved moves no membership.
        let governance_recorded = doc.get_changes(&membership_recorded);
        crate::members::replay::carry(
            &membership_recorded,
            &doc,
            &governance_recorded,
            &self.core.workspace,
        );
        let changes = doc
            .get_changes(before)
            .into_iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let workspace = self.core.workspace.clone();
        let outcome = self.receive_from(
            TransportOrigin::RelayDelta(key.verifying_key().to_bytes()),
            NeighbourMsg::SyncResp {
                changes,
                room: workspace,
            },
            now,
        )?;
        Ok(outcome.changed || outcome.metadata_changed)
    }
    /// Record this device's membership change as an operation on the log every
    /// device replays, and project the log back into the document's rows.
    ///
    /// The owner's device starts the log: a workspace that has none yet — new,
    /// or one whose membership was last-writer-wins — begins at an operation
    /// naming the membership it holds now. After that every change is an
    /// operation carrying the steps this device made, and a change its grants
    /// do not allow is refused here rather than written for other devices to
    /// discard.
    pub(super) fn record_membership(
        &self,
        doc: &mut CachedDocument,
        before: &[automerge::ChangeHash],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        use crate::members::{self, MembershipOpCommand, SignedMembershipOp};
        let workspace = self.core.workspace.clone();
        let target = members::rows(doc, &workspace);
        let old = self.core.workspace_doc.as_deref();
        let live = old.is_some_and(|old| members::holds_genesis(old, &workspace));
        let cert = self
            .certificate(&key.verifying_key().to_bytes(), now)
            .map_err(|error| error.to_string())?;
        let owner = cert.user_public_key == self.core.owner;
        if !live {
            if owner && old.is_none_or(crate::members::legacy::needs_genesis) {
                let op =
                    crate::members::legacy::genesis_op(cert, key, before, &target, now_ns(now))?;
                crate::members::legacy::put_op(doc, &op)?;
            }
            return Ok(());
        }
        // What the log already gives this document, its rows and receipts
        // included, is what the change is written on top of: an invitation's
        // receipt admits its joiner and a removal takes that admission back
        // without an operation, so where the log already says what the document
        // says, nothing is written.
        let held = members::replay(doc, &workspace);
        if held == target {
            return Ok(());
        }
        let steps = members::steps(&held, &target)
            .map_err(|error| format!("the membership change is not a step to write: {error}"))?;
        if steps.is_empty() {
            return Err(members::unwritten(&held, &target));
        }
        let op = SignedMembershipOp::signed(
            cert,
            key,
            members::heads_of(doc, &workspace),
            now_ns(now),
            MembershipOpCommand::Project { changes: steps },
        )?;
        crate::members::legacy::put_op(doc, &op)?;
        if members::replay(doc, &workspace) != target {
            return Err(members::refusal(&held, &target, &op));
        }
        members::materialise(doc, &target)?;
        Ok(())
    }

    /// Record the governance rows `doc` moved as this device's operation, so
    /// every receiver judges each by its author's role
    /// ([`crate::workspace::membership`]). A workspace whose log does not start
    /// yet has no operations to name anything with, and its rows govern as they
    /// did before the log.
    pub(super) fn record_governance(
        &self,
        doc: &mut CachedDocument,
        mut rows: Vec<crate::workspace::GovernanceRow>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        use crate::members::{self, MembershipOpCommand, SignedMembershipOp};
        let workspace = self.core.workspace.clone();
        if rows.is_empty() || !members::starts(doc, &workspace) {
            return Ok(());
        }
        rows.sort_unstable();
        let cert = self
            .certificate(&key.verifying_key().to_bytes(), now)
            .map_err(|error| error.to_string())?;
        let op = SignedMembershipOp::signed(
            cert,
            key,
            members::heads_of(doc, &workspace),
            now_ns(now),
            MembershipOpCommand::Governance { rows },
        )?;
        crate::members::legacy::put_op(doc, &op)?;
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
