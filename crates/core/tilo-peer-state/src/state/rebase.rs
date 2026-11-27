//! A workspace's document starting over from a base ([`workspace::base`]).
use super::{StatePeer, err, workspace};
use automerge::ReadDoc as _;

impl StatePeer {
    /// Start the workspace's document over from a base holding its state as
    /// it stands, signed by `device` with `key` at `now` as the workspace's
    /// compaction ([`workspace::compaction`]): a member admitted from then on
    /// is given the workspace as it stands, not what was withdrawn before.
    /// Every receiver takes it only from a device of the workspace's owner or
    /// of an administrator, so only theirs make one that reaches anyone.
    /// Returns whether it did: a document holding nothing past its base stays
    /// as it is.
    ///
    /// # Errors
    /// `device` is not of the workspace's owner or an administrator, or the
    /// base could not be built or signed.
    pub fn compact_workspace(
        &mut self,
        device: &tilo_user_identity::DeviceCert,
        key: &dyn collab_types::signer::DeviceSigner,
        now: web_time::SystemTime,
    ) -> Result<bool, String> {
        let Some(held) = self.core.workspace_doc.clone() else {
            return Ok(false);
        };
        if !self.administers_history(&device.user_public_key) {
            return Err(
                "only a device of the workspace's owner or of an administrator compacts it".into(),
            );
        }
        let mut doc = (*held).clone();
        if doc.get_heads().is_empty() || workspace::compaction::nothing_past_base(&mut doc) {
            return Ok(false);
        }
        // The base starts the workspace's history again as its next generation, so it is signed
        // under this device's actor of that generation: the history it replaces keeps what it
        // gave this device's numbers, and no copy that still holds it can take the base's
        // writer's changes for the ones of the same numbers it holds.
        let actor = self.workspace_actor_in(
            &device.device_public_key,
            crate::workspace::base::generation(&held).saturating_add(1),
        );
        let at_ns = u64::try_from(
            now.duration_since(web_time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        )
        .unwrap_or(u64::MAX);
        let base = workspace::compaction::compact(
            &doc,
            actor,
            (device, key),
            &self.core.workspace,
            at_ns,
        )?;
        // The base holds the state as it stood, so every receipt the held state checked is
        // still checked: starting over must not make the next admission read them all.
        self.core.workspace_doc = Some(held.rebased_into(base));
        Ok(true)
    }

    /// Write again onto the newer base this copy adopted from `before` the
    /// changes `device` made there that the base does not hold
    /// ([`workspace::base::carried`]): a holder that wrote against the
    /// history its base maker replaced keeps what it wrote. Returns whether
    /// anything was carried; a copy that adopted no newer base carries nothing.
    ///
    /// # Errors
    /// The documents could not be read or written.
    pub fn carry_onto_base(&mut self, before: &Self, device: &[u8; 32]) -> Result<bool, String> {
        let (Some(was), Some(is)) = (
            before.core.workspace_doc.as_deref(),
            self.core.workspace_doc.as_deref(),
        ) else {
            return Ok(false);
        };
        if workspace::base::generation(is) <= workspace::base::generation(was) {
            return Ok(false);
        }
        let mut doc = is.clone();
        // The change is written on the base this copy adopted, as that generation numbers it;
        // the base names what it holds of the numbers the replaced history gave this device.
        let held_as = self.workspace_actor_in(device, workspace::base::generation(was));
        let actor = self.workspace_actor_in(device, workspace::base::generation(&doc));
        if !workspace::base::carried(was, &mut doc, (&held_as, &actor)).map_err(err)? {
            return Ok(false);
        }
        let apps = self
            .core
            .workspace_doc
            .as_ref()
            .and_then(|held| held.apps_read().cloned());
        let mut carried: crate::cached_document::CachedDocument = doc.into();
        carried.with_apps(apps.as_ref());
        self.core.workspace_doc = Some(carried);
        Ok(true)
    }
}

/// Changes past a base after which the workspace's history starts over from a new one, however
/// light they are ([`StatePeer::compaction_due`]).
const MAX_CHANGES_PAST_BASE: usize = 256;
/// The changes' weight past which even a small workspace's history starts over
/// ([`StatePeer::compaction_due`]).
const MIN_WEIGHT_PAST_BASE: usize = 64 * 1024;

impl StatePeer {
    /// Whether the workspace's history is due to start over from a new base
    /// ([`Self::compact_workspace`]), with someone `arriving` or not.
    ///
    /// A newcomer is sent the base and every change after it, and a member
    /// already in only the changes: so the history starts over once the
    /// changes past its base weigh half as much as the base itself (or there
    /// are [`MAX_CHANGES_PAST_BASE`] of them). A newcomer then fetches at
    /// most about one and a half times the workspace as it stands, however
    /// long it has run, and each change's share of the bases written is no
    /// more than twice its own size. A small workspace's base is cheap, so it
    /// starts over no more often than every [`MIN_WEIGHT_PAST_BASE`]. An
    /// arrival after something past the base was taken back (a value
    /// overwritten or deleted: a shared window closed, a member removed, a
    /// name changed) starts it over whatever its weight: replayed that
    /// history, a newcomer would read what was taken back before they came.
    #[must_use]
    pub fn compaction_due(&self, arriving: bool) -> bool {
        let Some(held) = self.core.workspace_doc.as_deref() else {
            return false;
        };
        let mut doc = automerge::AutoCommit::clone(held);
        let base = workspace::base::root(&mut doc);
        let base_weight = base
            .and_then(|base| doc.get_change_by_hash(&base))
            .map_or(0, |change| change.raw_bytes().len());
        let past = doc.get_changes(base.as_slice());
        let weight: usize = past.iter().map(|change| change.raw_bytes().len()).sum();
        if past.len() >= MAX_CHANGES_PAST_BASE
            || weight >= (base_weight / 2).max(MIN_WEIGHT_PAST_BASE)
        {
            return true;
        }
        arriving
            && past.iter().any(|change| {
                change
                    .decode()
                    .operations
                    .iter()
                    .any(|op| !op.pred.is_empty())
            })
    }
}
