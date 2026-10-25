//! The desktop as one person's device holds it: every workspace, which one
//! is showing, and the per-strip focus pointers.
//!
//! This is the durable, host-independent part of the tree. Replication
//! documents, persistence targets and admission records belong to the host
//! and wrap this value.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::collaborator::Collaborator;
use crate::ids::{GroupId, RoomId, StripId, WorkspaceId};
use crate::partition::OutputPartition;
use crate::serde_ids::deserialize_id_map;
use crate::workspace::Workspace;

/// The shape of the persisted desktop this build writes and understands.
/// Bump only when a change makes a file unreadable to an older build in a
/// way that loses meaning (field removed/repurposed, id space renumbered) —
/// a new `#[serde(default)]` field does not qualify. Guards the one
/// direction with no safe default: older reading newer.
pub const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopTree {
    /// Which version of the on-disk shape the file describes. Read back to
    /// decide whether this build may touch the file at all.
    pub schema_version: u32,
    /// Counts rounds in which a peer's changes landed. `#[serde(skip)]`
    /// — a restored value could match a live reader's cache and
    /// suppress the first resync.
    #[serde(skip)]
    pub room_epoch: u64,
    /// Mutations performed outside retained chrome (for example the local
    /// action broker or a lifecycle callback) increment this so chrome can
    /// invalidate retained panes. Process-local: restoring a persisted
    /// value could suppress the first refresh after launch.
    #[serde(skip)]
    pub external_revision: u64,
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub active: WorkspaceId,
    #[serde(deserialize_with = "deserialize_id_map")]
    pub active_strip: HashMap<WorkspaceId, StripId>,
    /// Per-strip "active group" pointer, one level deeper than
    /// [`Self::active_strip`]. Missing means "fall back to the first
    /// non-closing group" (see [`Self::resolve_active_group`]).
    #[serde(default, deserialize_with = "deserialize_id_map")]
    pub active_group: HashMap<StripId, GroupId>,
    /// Which display each window group is shown on, on this device. Local
    /// layout: persisted with this device's desktop, never replicated.
    #[serde(default, skip_serializing_if = "OutputPartition::is_empty")]
    pub outputs: OutputPartition,
    /// Legacy session-wide presence cache. Ordinary chrome must read the
    /// active workspace's roster instead: a person present elsewhere is not
    /// present here. Never deserialized, so no stale presence survives a
    /// reload.
    #[serde(skip_deserializing)]
    pub collaborators: Vec<Collaborator>,
    /// Mirror of each room's record index (record id → kind), pushed by
    /// the host so replication can reach it. Not persisted.
    #[serde(skip, default)]
    pub room_records_held: HashMap<RoomId, BTreeMap<String, String>>,
    /// References that arrived from a peer and have not been folded into
    /// the local index yet. Drained by the host.
    #[serde(skip, default)]
    pub room_records_inbound: Vec<(RoomId, String, String)>,
    /// The salt of the device this tree belongs to: the high 16 bits of
    /// every id the tree mints, and the namespace replication treats as its
    /// own. `0` until the device's session sets it. Kept beside the state
    /// file, not in it, so saved ids never depend on it.
    #[serde(skip, default)]
    pub salt: u16,
}

impl Default for DesktopTree {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            room_epoch: 0,
            external_revision: 0,
            workspaces: Vec::new(),
            active: WorkspaceId::default(),
            active_strip: HashMap::new(),
            active_group: HashMap::new(),
            outputs: OutputPartition::default(),
            collaborators: Vec::new(),
            room_records_held: HashMap::new(),
            room_records_inbound: Vec::new(),
            salt: 0,
        }
    }
}

impl DesktopTree {
    /// Read a tree from its JSON value.
    ///
    /// Readers in other crates go through this function instead of calling
    /// `serde_json` themselves: serde's derived code is generic over the
    /// input, and each crate and input kind instantiates its own copy of
    /// the whole tree's deserializer, about 200 KB each in the web bundle.
    pub fn from_json(value: serde_json::Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }

    /// Deserialize a tree field through [`Self::from_json`], for
    /// `#[serde(deserialize_with = "DesktopTree::deserialize_via_json")]`.
    pub fn deserialize_via_json<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        let value = <serde_json::Value as Deserialize>::deserialize(deserializer)?;
        Self::from_json(value).map_err(serde::de::Error::custom)
    }

    /// How many times a peer's changes have landed in this tree.
    /// Monotonic per session, not persisted — read by the frame loop,
    /// which bumps the chrome revision when it moves.
    pub fn room_epoch(&self) -> u64 {
        self.room_epoch
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == self.active)
    }

    /// The first instant after `now` (Unix seconds) at which a grant of any
    /// workspace stops being live ([`Workspace::next_grant_expiry_after`]): a
    /// policy decision read from this tree at `now` holds until then, or
    /// until the tree is written.
    pub fn next_grant_expiry_after(&self, now: u64) -> Option<u64> {
        self.workspaces
            .iter()
            .filter_map(|workspace| workspace.next_grant_expiry_after(now))
            .min()
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.id == self.active)
    }

    /// The `(workspace, strip)` indices of `strip_id`, searching every
    /// workspace.
    pub fn strip_position(&self, strip_id: StripId) -> Option<(usize, usize)> {
        self.workspaces.iter().enumerate().find_map(|(wi, ws)| {
            ws.strips
                .iter()
                .position(|strip| strip.id == strip_id)
                .map(|si| (wi, si))
        })
    }

    /// The currently-active [`GroupId`] for `strip_id`, or `None` if
    /// unseeded — callers compose this with their own fallback.
    pub fn active_group_for(&self, strip_id: StripId) -> Option<GroupId> {
        self.active_group.get(&strip_id).copied()
    }

    /// Mark `group_id` as the active group of `strip_id`. Not validated
    /// against the strip's actual membership — the host's reconciler is
    /// expected to be the sole writer.
    pub fn set_active_group(&mut self, strip_id: StripId, group_id: GroupId) {
        self.active_group.insert(strip_id, group_id);
    }

    /// Resolve the active group on `strip_id` under `is_closing`: if
    /// closing, returns the nearest surviving neighbour on its left, or on
    /// its right at the strip's start ([`Self::close_tab`] is the same rule
    /// for a group already gone). The strip then pans only
    /// as far as that neighbour needs, never back to its start. Pure read;
    /// `None` if no group survives, or the strip doesn't exist.
    pub fn resolve_active_group(
        &self,
        strip_id: StripId,
        is_closing: impl Fn(GroupId) -> bool,
    ) -> Option<GroupId> {
        let strip = self
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .find(|s| s.id == strip_id)?;
        let recorded = self.active_group.get(&strip_id).copied();
        // First non-closing group in the strip scanning right
        // (increasing index) from — and including — `from`.
        let scan_right = |from: usize| -> Option<GroupId> {
            strip
                .window_groups
                .get(from..)
                .unwrap_or(&[])
                .iter()
                .find(|g| !is_closing(g.id))
                .map(|g| g.id)
        };
        // Same, scanning left (decreasing index) from — and including —
        // `from`.
        let scan_left = |from: usize| -> Option<GroupId> {
            strip
                .window_groups
                .get(..=from)
                .unwrap_or(&[])
                .iter()
                .rev()
                .find(|g| !is_closing(g.id))
                .map(|g| g.id)
        };
        match recorded {
            Some(id) => {
                if !is_closing(id) {
                    return Some(id);
                }
                // Recorded group is closing — find its index, then
                // scan left for a survivor; if none, scan right.
                let Some(idx) = strip.window_groups.iter().position(|g| g.id == id) else {
                    // Recorded group no longer in the strip; fall
                    // through to "first non-closing" as a default.
                    return scan_right(0);
                };
                idx.checked_sub(1)
                    .and_then(scan_left)
                    .or_else(|| scan_right(idx.saturating_add(1)))
            }
            None => scan_right(0),
        }
    }

    /// Reconcile the active-group pointer against `closing_ids`: if the
    /// recorded group is closing, advance to the next non-closing
    /// sibling. `None` when every group is closing — the entry is
    /// removed, since an emptying strip has no active group rather than
    /// one clamped to a corpse. Idempotent.
    pub fn nudge_active_group_off_closing(
        &mut self,
        strip_id: StripId,
        closing_ids: &[GroupId],
    ) -> Option<GroupId> {
        let recorded = self.active_group.get(&strip_id).copied()?;
        if !closing_ids.contains(&recorded) {
            return Some(recorded);
        }
        let resolved = self.resolve_active_group(strip_id, |id| closing_ids.contains(&id));
        if let Some(next) = resolved {
            self.active_group.insert(strip_id, next);
            Some(next)
        } else {
            // Whole strip is fading; drop the entry rather than
            // leave a dangling pointer. The shell re-seeds when
            // fresh groups land.
            self.active_group.remove(&strip_id);
            None
        }
    }
}
