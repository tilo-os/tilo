//! Writing a governance change as this device: the operation recording what
//! it moved, then taking it in as every member will.
use super::{
    AutoCommit, CachedDocument, NeighbourMsg, StatePeer, SystemTime, TransportOrigin, backend, err,
    now_ns,
};
use automerge::transaction::Transactable as _;
use collab_types::signer::DeviceSigner;
use std::collections::BTreeSet;

/// The action a governance change is made by, while a copy writes one
/// ([`StatePeer::stamp_action`]): the verb every change of it names, and
/// who decided it.
///
/// A governance change says who wrote it and when by its actor and its time.
/// This says which of the workspace's actions it carries out, so an app
/// watching the workspace reads a ban, a removal and an unban apart, as a
/// reader of an ordinary store change does ([`collab_types::Stamp`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionStamp {
    /// The governance action's verb, as its surface declares it (`ban`,
    /// `remove-member`).
    pub verb: &'static str,
    /// Who decided it: the person whose device asked, or the workspace's
    /// moderation bot.
    pub by: tilo_action_policy::Actor,
}

impl ActionStamp {
    /// The stamp this governance change commits as: its verb and invoker, no
    /// arguments (one may be a secret) and no invocation of its own.
    fn as_stamp(&self) -> collab_types::Stamp {
        collab_types::Stamp {
            verb: self.verb.to_owned(),
            invoker: collab_types::Stamp::by(&self.by),
            ..collab_types::Stamp::default()
        }
    }
}

/// Commit what `doc` holds as one governance change, stamped with `unix_secs`.
///
/// The change says who wrote it and when by itself: its actor names the writing device — or, in a
/// workspace a server keeps, that server's actor — whose certificate among the workspace's own
/// rows names the person it belongs to, and its time is `unix_secs`. `stamp`, when the copy writes
/// as a governance action, names that action and who decided it as the change's message.
pub(crate) fn commit_governance(doc: &mut AutoCommit, unix_secs: i64, stamp: Option<&ActionStamp>) {
    if doc.pending_ops() == 0 {
        // Commit would make a change of nobody's: roll the empty transaction back instead.
        doc.rollback();
        return;
    }
    let options = automerge::transaction::CommitOptions::default().with_time(unix_secs);
    let options = match stamp {
        Some(stamp) => options.with_message(stamp.as_stamp().message()),
        None => options,
    };
    doc.commit_with(options);
}

/// The wall clock in the unit automerge stamps a change's time with, for a governance change whose
/// writer has no `now` handed to it.
pub(crate) fn wall_secs() -> i64 {
    i64::try_from(tilo_clock::try_unix_now().unwrap_or_default()).unwrap_or(i64::MAX)
}

impl StatePeer {
    /// Write this copy's governance changes from now on as the action `verb`
    /// by `by`, until [`Self::clear_action_stamp`]: each change's message
    /// names that action and who decided it, so an app watching the workspace
    /// tells a ban from a removal. Order, admission and what is written stay
    /// exactly as they are.
    pub fn stamp_action(&mut self, verb: &'static str, by: tilo_action_policy::Actor) {
        self.action_stamp = Some(ActionStamp { verb, by });
    }

    /// Stop naming an action in this copy's governance changes
    /// ([`Self::stamp_action`]).
    pub fn clear_action_stamp(&mut self) {
        self.action_stamp = None;
    }

    /// Make `run`, this copy's governance write, as the action `verb` by the
    /// person `who`: each change it makes names that action, and none made
    /// after it does.
    pub(super) fn as_action<T>(
        &mut self,
        verb: &'static str,
        who: [u8; 32],
        run: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.stamp_action(
            verb,
            tilo_action_policy::Actor::Member(tilo_action_policy::MemberId::person(hex::encode(
                who,
            ))),
        );
        let outcome = run(self);
        self.clear_action_stamp();
        outcome
    }

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
        // time and, when this copy writes as a governance action, that action: the operation
        // recording it is the next one.
        commit_governance(&mut doc, super::now_secs(now), self.action_stamp.as_ref());
        // A change that writes no operation leaves the membership as it was.
        let written = doc.get_changes(before);
        crate::members::replay::carry(before, &doc, &written, &self.core.workspace);
        let unrecorded = doc.get_heads();
        // An admission names the device row it writes itself
        // ([`workspace::membership::admit_operations`]).
        let admitted = match &membership {
            Some(crate::members::MembershipOpCommand::Admit { person, .. }) => Some(*person),
            _ => None,
        };
        let admits = admitted.is_some();
        // The agents whose delegation the change withdraws, read before the membership operation
        // is written: it writes no identity row, and the document read with it still open is read
        // whole.
        let withdrawn = crate::standing::newly_withdrawn(
            self.core
                .workspace_doc
                .as_deref()
                .ok_or("missing workspace")?,
            &doc,
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
        if self.cut_standing_taken(&mut doc, before, key, (admitted, &withdrawn))?
            && !rows.contains(&crate::workspace::GovernanceRow::Standing)
        {
            rows.push(crate::workspace::GovernanceRow::Standing);
        }
        self.record_governance(&mut doc, rows, key, now)?;
        // The operations, and the policy bound to what they leave, are one change of this
        // device's, stamped like the one that moved the rows: a member change's rows and its
        // operation are applied here, where a governance write's own operations are in the
        // change before.
        commit_governance(&mut doc, super::now_secs(now), self.action_stamp.as_ref());
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

    /// Write into `doc`, the governance a change of `key`'s device leaves, the
    /// cut of every standing the change takes away ([`crate::standing`]): of
    /// each person it takes ownership, a role, an assigned role or a grant
    /// from, or gives a refusal, with what this copy had taken of each device
    /// of theirs; of each server it no longer has keep the workspace; and of
    /// each agent whose delegation it withdraws (`withdrawn`). `admitted` is
    /// the person an admission lets in. Whether it wrote any.
    fn cut_standing_taken(
        &self,
        doc: &mut CachedDocument,
        before: &[automerge::ChangeHash],
        key: &dyn DeviceSigner,
        (admitted, withdrawn): (Option<[u8; 32]>, &BTreeSet<[u8; 32]>),
    ) -> Result<bool, String> {
        use crate::standing::{self, Standing};
        let held = self
            .core
            .workspace_doc
            .as_ref()
            .ok_or("missing workspace")?;
        let workspace = &self.core.workspace;
        let was = Standing::all(held.workspace_policy(workspace));
        // An admission gives standing to the person it admits and takes none from anyone: of a
        // newcomer it cuts nothing, and the membership it leaves is not replayed to find that
        // out, which would read every member's operation for each one let in.
        let newcomer = admitted.is_some_and(|person| !was.contains_key(&hex::encode(person)));
        let lost: Vec<(String, Standing)> = if newcomer {
            Vec::new()
        } else {
            let is = Standing::all(&crate::members::replay(doc, workspace));
            was.into_iter()
                .filter(|(person, prior)| {
                    !prior.kept_by(&is.get(person).cloned().unwrap_or_default())
                })
                .collect()
        };
        let id = |subject: &[u8; 32]| {
            let mut hasher = blake3::Hasher::new_derive_key("tilo standing cut v1");
            for head in before {
                hasher.update(&head.0);
            }
            hasher.update(subject);
            hasher.update(&key.device_public_key());
            hex::encode(&hasher.finalize().as_bytes()[..16])
        };
        let mut wrote = false;
        for (person, prior) in lost {
            let Ok(bytes) = hex::decode(&person) else {
                tracing::warn!(
                    person = %person,
                    "a standing a change takes away is keyed by no key; its devices are not reached"
                );
                continue;
            };
            let Ok(root) = <[u8; 32]>::try_from(bytes) else {
                tracing::warn!(
                    person = %person,
                    "a standing a change takes away is keyed by something other than a key; its devices are not reached"
                );
                continue;
            };
            let taken = held
                .identities()
                .values()
                .filter_map(|row| row.as_ref().ok())
                .filter(|row| row.device_cert.user_public_key == root)
                .map(|row| {
                    let device = row.device_cert.device_public_key;
                    (device, self.taken_from(&device))
                })
                .collect();
            standing::write(doc, &root, &id(&root), &prior, &taken)?;
            wrote = true;
        }
        let kept: BTreeSet<[u8; 32]> = super::keeping_devices_of(doc)
            .iter()
            .map(|hint| hint.host.device_public_key)
            .collect();
        let taken_out: BTreeSet<[u8; 32]> = super::keeping_devices_of(held)
            .iter()
            .map(|hint| hint.host.device_public_key)
            .filter(|server| !kept.contains(server))
            .collect();
        for server in taken_out {
            let taken = std::collections::BTreeMap::from([(server, self.taken_from(&server))]);
            standing::write(doc, &server, &id(&server), &Standing::default(), &taken)?;
            wrote = true;
        }
        for agent in withdrawn {
            let taken = std::collections::BTreeMap::from([(*agent, self.taken_from(agent))]);
            standing::write(doc, agent, &id(agent), &Standing::default(), &taken)?;
            wrote = true;
        }
        Ok(wrote)
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
        let cert = self.signing_certificate(doc, &key.device_public_key(), now)?;
        let op = SignedMembershipOp::signed(
            &cert,
            key,
            members::heads_of(doc, &workspace),
            now_ns(now),
            command,
        )?;
        if let Some(why) = members::refused(doc, &members::replay(doc, &workspace), &op) {
            return Err(why);
        }
        members::put_op(doc, &op)?;
        Ok(())
    }

    /// The certificate `device` signs a membership operation of `doc` as: the
    /// one the workspace holds of it, or, for a device enrolling itself, the
    /// one its own row in `doc` carries, which the change being signed wrote.
    /// Every receiver judges what the operation names by its author all the
    /// same.
    fn signing_certificate(
        &self,
        doc: &CachedDocument,
        device: &[u8; 32],
        now: SystemTime,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        match self.certificate(device, now) {
            Ok(cert) => Ok(cert.clone()),
            Err(error) => doc
                .identities()
                .get(&hex::encode(device))
                .and_then(|identity| identity.as_ref().ok())
                .map(|identity| identity.device_cert.clone())
                .filter(|cert| cert.device_public_key == *device)
                .ok_or_else(|| error.to_string()),
        }
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
        let cert = self.signing_certificate(doc, &key.device_public_key(), now)?;
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

    /// Take in the certificates and revocations `doc`'s governed identity
    /// rows prove.
    pub(super) fn import_governed_rows(
        &mut self,
        doc: &CachedDocument,
        now: SystemTime,
    ) -> Result<bool, String> {
        let (certificates, revocations, changed) = backend::governed::import(
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
