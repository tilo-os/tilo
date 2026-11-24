//! Keeping this device's sealed record histories within their bound.
//!
//! A sealed record's history has a bound, and every device holding its keys
//! compacts its own copy: a device alone or offline goes on writing however
//! long a record's history grows. A compaction is the key holder's own signed
//! checkpoint of the merged history, admitted like any write.

use super::history_keys::{SealedHistoryKeys, seal_for_document};
use super::{NativeWorkspaceOwner, NeighbourMsg, SystemTime, WorkspaceId, write};
use collab_types::SignedOp;
use collab_types::signer::DeviceSigner;
use std::sync::Arc;

impl NativeWorkspaceOwner {
    /// Keep every sealed record history this device holds within its bound
    /// with `keys`, the channel keys this device holds: before each of this
    /// device's writes, and whenever its holder asks
    /// ([`Self::compact_outgoing`]).
    pub fn keep_sealed_histories(&self, keys: Arc<dyn SealedHistoryKeys>) {
        *self.0.history_keys.lock() = Some(keys);
    }

    /// Compact every sealed record history of `workspace` that has used most of
    /// its bound into one sealed checkpoint, as this device's own signed write.
    ///
    /// Answers how many histories were compacted.
    ///
    /// # Errors
    /// The workspace is not held here, this device may not write it, or signing
    /// failed.
    pub async fn compact_outgoing(
        &self,
        workspace: WorkspaceId,
        signer: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(keys) = self.0.history_keys.lock().clone() else {
            return Ok(0);
        };
        let due = self.with_current(workspace, |view| {
            Ok(view.peer.publication().sealed_to_compact())
        })?;
        let mut compacted = 0usize;
        for (app, record) in due {
            compacted = compacted.saturating_add(
                self.compact_route(workspace, (&app, &record), keys.as_ref(), signer, now)
                    .await?,
            );
        }
        Ok(compacted)
    }

    /// [`Self::compact_outgoing`] for `record` of `app` alone, whatever share of
    /// its bound its history used.
    ///
    /// # Errors
    /// As [`Self::compact_outgoing`].
    pub async fn compact_document(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        signer: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let Some(keys) = self.0.history_keys.lock().clone() else {
            return Ok(0);
        };
        self.compact_route(workspace, (app, record), keys.as_ref(), signer, now)
            .await
    }

    /// Draft, sign and admit the compaction of one record's history as this
    /// device's own write, and make it durable.
    #[allow(
        clippy::too_many_lines,
        reason = "one compaction, drafted, signed, admitted and kept in the order it happens"
    )]
    async fn compact_route(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        keys: &dyn SealedHistoryKeys,
        signer: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<usize, String> {
        let mut drafted: Option<Vec<SignedOp>> = None;
        let compacted = write::signed_turn(signer, |signed| -> Result<Vec<NeighbourMsg>, String> {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            let device = candidate.device().device_public_key;
            if signed.device_public_key() != device {
                return Err("the compaction signer differs from this device".into());
            }
            candidate
                .peer
                .permissions(device, now)
                .map_err(|error| error.to_string())?;
            let named = keys.named_readers(workspace, record);
            if neighbour_wire::KeyScope::has_named_readers(record) && named.is_none() {
                return Ok(Vec::new());
            }
            let ops = if let Some(ops) = drafted.take() {
                ops
            } else {
                Self::draft_compaction(
                    &candidate,
                    keys,
                    workspace,
                    (app, record),
                    named.as_deref(),
                    now,
                )?
            };
            let mut signed_ops = Vec::with_capacity(ops.len());
            for op in &ops {
                let mut op = op.clone();
                collab_types::sign_op(&mut op, signed).map_err(|error| error.to_string())?;
                signed_ops.push(op);
            }
            drafted = Some(ops);
            signed.settled()?;
            let record_name = record;
            let (frames, record) =
                candidate.change(|live, record| -> Result<Vec<NeighbourMsg>, String> {
                    let frames = live.publication_mut().admit_sealed_compaction(
                        app,
                        record_name,
                        named.as_deref(),
                        signed_ops,
                        now,
                    )?;
                    // The device that drafted the checkpoint checks it as every device with the
                    // record's keys does, and keeps that it did: the writes it stands in for are
                    // dropped once every one of them has.
                    let group = collab_types::reader_group(
                        workspace,
                        named.as_deref().unwrap_or(collab_types::EVERY_MEMBER),
                    )
                    .ok_or("the record's channel names no group")?;
                    if let Some((checkpoint, true)) = live
                        .publication()
                        .check_leading_checkpoint((app, record_name), &mut |write| {
                            keys.open(group, write)
                        })
                    {
                        live.workspace_mut().record_checkpoint_check(
                            candidate.device(),
                            (app, record_name),
                            &checkpoint,
                            signed,
                            now,
                        )?;
                        signed.settled()?;
                    }
                    record.generation = record
                        .generation
                        .checked_add(1)
                        .ok_or("native owner generation exhausted")?;
                    Ok(frames)
                })?;
            self.publish_document(workspace, &candidate, record, None)?;
            Ok(frames)
        })
        .await?;
        if compacted.is_empty() {
            return Ok(0);
        }
        // The compaction is this device's own write: it is kept as the frame a link
        // carries, as every write of this device's is.
        for message in compacted {
            self.keep_frame(workspace, &message)?;
        }
        Ok(1)
    }

    /// Draft the sealed compaction of `record` of `app` as it stands in `candidate`: the writes
    /// this device signs and admits.
    fn draft_compaction(
        candidate: &super::NativeCommittedView,
        keys: &dyn SealedHistoryKeys,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
        named: Option<&str>,
        now: SystemTime,
    ) -> Result<Vec<SignedOp>, String> {
        let group =
            collab_types::reader_group(workspace, named.unwrap_or(collab_types::EVERY_MEMBER))
                .ok_or("the record's channel names no group")?;
        let mut open = |write: &SignedOp| keys.open(group, write);
        let mut seal = |snapshot: &[u8]| {
            seal_for_document(
                keys,
                &candidate.peer,
                workspace,
                (app, record),
                named,
                snapshot,
                now,
            )
        };
        candidate
            .peer
            .clone()
            .publication()
            .draft_sealed_compaction(
                (app, record),
                named,
                None,
                &mut open,
                &mut seal,
                candidate.device().device_public_key,
                now,
            )
    }
}
