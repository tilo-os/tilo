//! The desktop as one person's device holds it: every workspace, which one
//! is showing, and the per-strip focus pointers.
//!
//! This is the durable, host-independent part of the tree. Replication
//! documents, persistence targets and admission records belong to the host
//! and wrap this value.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::ids::{GroupId, StripId, TabId, WorkspaceId};
use crate::layout::Tab;
use crate::partition::OutputPartition;
use crate::serde_ids::deserialize_id_map;
use collab_types::Member;
use tilo_action_policy::{Grantee, Preset, Role, WorkspacePolicy, roles};
use tilo_permissions::caps::CapOverride;

use crate::layout::{Strip, WorkspaceLayout};
use crate::present::Present;
use crate::shared_surfaces::SharedSurfaces;
use tilo_actions::receipt::WorkspaceEffectReceipt;

/// The shape of the persisted desktop this build writes and understands.
/// Bump only when a change makes a file unreadable to an older build in a
/// way that loses meaning (field removed/repurposed, id space renumbered) —
/// a new `#[serde(default)]` field does not qualify. Guards the one
/// direction with no safe default: older reading newer.
pub const STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layout {
    /// Which version of the on-disk shape the file describes. Read back to
    /// decide whether this build may touch the file at all.
    pub schema_version: u32,
    /// Counts rounds in which a peer's changes landed. `#[serde(skip)]`
    /// — a restored value could match a live reader's cache and
    /// suppress the first resync.
    #[serde(skip)]
    pub workspace_epoch: u64,
    /// Mutations performed outside retained chrome (for example the local
    /// action broker or a lifecycle callback) increment this so chrome can
    /// invalidate retained panes. Process-local: restoring a persisted
    /// value could suppress the first refresh after launch.
    #[serde(skip)]
    pub external_revision: u64,
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub active: WorkspaceId,
    #[serde(default)]
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
    /// The window group covering its display on this device, with the bar
    /// stepped aside, while the strip it stands in is the one shown. What
    /// the person is looking at rather than part of the desktop: persisted
    /// with this device's layout, so a session starts where the person left
    /// it, and never sent to the workspace's peers, whose windows lie
    /// elsewhere.
    /// Written only by [`Layout::set_fullscreen`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<GroupId>,
    /// Mirror of each workspace's record index (record id → kind), pushed by
    /// the host so replication can reach it. Not persisted.
    #[serde(skip, default)]
    pub workspace_records_held: HashMap<WorkspaceId, BTreeMap<String, String>>,
    /// The salt of the device this tree belongs to: the high 16 bits of
    /// every id the tree mints, and the namespace replication treats as its
    /// own. `0` until the device's session sets it. Kept beside the state
    /// file, not in it, so saved ids never depend on it.
    #[serde(skip, default)]
    pub salt: u16,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            workspace_epoch: 0,
            external_revision: 0,
            workspaces: Vec::new(),
            active: WorkspaceId::nil(),
            active_strip: HashMap::new(),
            active_group: HashMap::new(),
            outputs: OutputPartition::default(),
            fullscreen: None,
            workspace_records_held: HashMap::new(),
            salt: 0,
        }
    }
}

impl Layout {
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
    /// `#[serde(deserialize_with = "Layout::deserialize_via_json")]`.
    pub fn deserialize_via_json<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        let value = <serde_json::Value as Deserialize>::deserialize(deserializer)?;
        Self::from_json(value).map_err(serde::de::Error::custom)
    }

    /// How many times a peer's changes have landed in this tree.
    /// Monotonic per session, not persisted — read by the frame loop,
    /// which bumps the chrome revision when it moves.
    pub fn workspace_epoch(&self) -> u64 {
        self.workspace_epoch
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id() == self.active)
    }

    /// The window this device has in front: the tab the group in front of the active workspace's
    /// shown strip keeps active, and, where the layout records no such pointer — a layout whose
    /// shell draws the windows itself keeps none of the keyboard's — the first window that strip
    /// shows. `None` for a layout with no window at all.
    #[must_use]
    pub fn tab_in_front(&self) -> Option<TabId> {
        let workspace = self
            .active_workspace()
            .or_else(|| self.workspaces.first())?;
        let strip_id = self.active_strip_for(workspace)?;
        let strip = workspace.strips.iter().find(|strip| strip.id == strip_id)?;
        let group = self
            .active_group_for(strip_id)
            .and_then(|group| strip.window_groups.iter().find(|held| held.id == group))
            .or_else(|| strip.window_groups.first())?;
        group
            .tabs
            .iter()
            .find(|tab| tab.id == group.active_tab)
            .or_else(|| group.tabs.first())
            .map(|tab| tab.id)
    }

    /// Every window of the layout, in the tree's order: workspace by workspace, strip by strip,
    /// group by group.
    pub fn tabs(&self) -> impl Iterator<Item = &Tab> {
        self.workspaces.iter().flat_map(Workspace::tabs)
    }

    /// The window `id` shows, wherever on the layout it is.
    #[must_use]
    pub fn tab(&self, id: crate::TabId) -> Option<&Tab> {
        self.tabs().find(|tab| tab.id == id)
    }

    /// The workspace holding a window of `app`, the first in the tree's
    /// order: the one whose temperature and workspace that window lives in. A
    /// window the tree does not hold yet is in none.
    #[must_use]
    pub fn workspace_holding_app(&self, app: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|workspace| {
            workspace.strips.iter().any(|strip| {
                strip.window_groups.iter().any(|group| {
                    group
                        .tabs
                        .iter()
                        .any(|tab| crate::apps::canonical_app_id(&tab.tile).as_deref() == Some(app))
                })
            })
        })
    }

    /// The first instant after `now` (Unix seconds) at which a grant or
    /// sandbox capability of any workspace stops being live: a policy
    /// decision read from this tree at `now` holds until then, or until the
    /// tree is written.
    pub fn next_grant_expiry_after(&self, now: u64) -> Option<u64> {
        self.workspaces
            .iter()
            .flat_map(|workspace| {
                [
                    workspace.policy.next_grant_expiry_after(now),
                    tilo_permissions::caps::sandbox::next_expiry_after(&workspace.sandbox, now),
                ]
            })
            .flatten()
            .min()
    }

    /// Record one terminal effect in `workspace_id`'s history, exactly once
    /// per invocation: the receipt as stored, or `None` when the workspace is
    /// gone or already holds a receipt for the invocation. An application's
    /// effect is recorded [`WorkspaceEffectReceipt::redacted`]; a record the
    /// host writes itself, such as a grant a person gave,
    /// [`WorkspaceEffectReceipt::recorded`].
    pub fn append_workspace_effect_receipt(
        &mut self,
        workspace_id: WorkspaceId,
        receipt: WorkspaceEffectReceipt,
    ) -> Option<WorkspaceEffectReceipt> {
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id() == workspace_id)?;
        if workspace
            .effect_receipts
            .iter()
            .any(|held| held.invocation == receipt.invocation)
        {
            return None;
        }
        workspace.effect_receipts.push(receipt.clone());
        tilo_actions::receipt::bounded_receipts(&mut workspace.effect_receipts);
        Some(receipt)
    }

    /// The strip `workspace` shows: its recorded one while that strip exists,
    /// else its first.
    #[must_use]
    pub fn active_strip_for(&self, workspace: &Workspace) -> Option<StripId> {
        self.active_strip
            .get(&workspace.id())
            .copied()
            .filter(|id| workspace.strips.iter().any(|strip| strip.id == *id))
            .or_else(|| workspace.strips.first().map(|strip| strip.id))
    }

    /// The active workspace's selected strip: its recorded one while that
    /// strip exists, else its first.
    pub fn active_strip_id(&self) -> Option<StripId> {
        self.active_strip_for(self.active_workspace()?)
    }

    /// Whether this device lays the desktop out across several displays,
    /// each showing a strip of its own.
    pub fn is_partitioned(&self) -> bool {
        self.outputs.displays.len() >= 2
    }

    /// Bring this device's display partition in line with the displays
    /// now connected, primary first ([`OutputPartition::reconcile`]), and
    /// give every group without a display the focused one. A partition is
    /// kept once a second display has been seen; a device that only ever
    /// had one display carries none. Returns whether the partition changed.
    pub fn reconcile_displays(&mut self, displays: &[crate::partition::Display]) -> bool {
        if displays.len() < 2 && self.outputs.is_empty() {
            return false;
        }
        let before = self.outputs.clone();
        self.outputs.reconcile(&self.workspaces, displays);
        self.outputs.place_unhomed(&self.workspaces);
        self.outputs != before
    }

    /// The strip `display` shows in the active workspace. Every display
    /// shows a strip of its own: the focused display the active strip, any
    /// other the one it showed when focus left it, else the active strip.
    pub fn strip_shown_on(&self, display: u32) -> Option<StripId> {
        let workspace = self.active_workspace()?;
        let active = self.active_strip_id();
        if !self.is_partitioned() || self.outputs.focused_display() == Some(display) {
            return active;
        }
        self.outputs
            .shown_on(display, workspace.id())
            .filter(|id| workspace.strips.iter().any(|strip| strip.id == *id))
            .or(active)
    }

    /// Show `strip` of the active workspace on `display`, leaving every
    /// other display where it is. On the focused display that makes it the
    /// active strip. Returns whether anything changed.
    pub fn show_strip_on(&mut self, display: u32, strip: StripId) -> bool {
        let Some(workspace) = self.active_workspace() else {
            return false;
        };
        let ws = workspace.id();
        if !workspace.strips.iter().any(|s| s.id == strip) {
            return false;
        }
        if !self.is_partitioned() || self.outputs.focused_display() == Some(display) {
            return self.active_strip.insert(ws, strip) != Some(strip);
        }
        self.outputs.set_shown(display, ws, strip)
    }

    /// Keep what the displays show in step with focus, after anything
    /// moved it: the display holding the focused group shows the active
    /// strip, and with no group focused the focused display does. Every
    /// other display keeps the strip it shows, and one that has never been
    /// told shows the active strip from now on, so going to another strip
    /// elsewhere later leaves it be. Returns whether anything changed.
    pub fn note_shown_strip(&mut self) -> bool {
        let Some((ws, strip, focus)) = self.shown_strip_focus() else {
            return false;
        };
        let mut changed = false;
        let displays: Vec<u32> = self.outputs.displays.iter().map(|d| d.id).collect();
        for display in displays {
            if self.outputs.shown_on(display, ws).is_none() {
                changed |= self.outputs.set_shown(display, ws, strip);
            }
        }
        changed | self.outputs.set_shown(focus, ws, strip)
    }

    /// Whether [`Self::note_shown_strip`] would change anything.
    pub fn shown_strips_behind(&self) -> bool {
        self.shown_strip_focus().is_some_and(|(ws, strip, focus)| {
            self.outputs.shown_on(focus, ws) != Some(strip)
                || self
                    .outputs
                    .displays
                    .iter()
                    .any(|d| self.outputs.shown_on(d.id, ws).is_none())
        })
    }

    /// The active workspace, its active strip, and the display focus is on
    /// in it: the one holding the focused group, else the focused display.
    fn shown_strip_focus(&self) -> Option<(WorkspaceId, StripId, u32)> {
        if !self.is_partitioned() {
            return None;
        }
        let ws = self.active_workspace()?.id();
        let strip = self.active_strip_id()?;
        let display = self
            .active_group_for(strip)
            .and_then(|group| self.outputs.display_of(group))
            .or_else(|| self.outputs.focused_display())?;
        Some((ws, strip, display))
    }

    /// Focus moves to `display`: the display it leaves keeps showing the
    /// active strip, and the strip `display` shows becomes the active one.
    /// Returns whether the active strip changed.
    pub fn focus_display(&mut self, display: u32) -> bool {
        let leaving = self.outputs.focused_display();
        let ws = self.active_workspace().map(crate::Workspace::id);
        if let (Some(leaving), Some(ws), Some(strip)) = (leaving, ws, self.active_strip_id())
            && leaving != display
        {
            self.outputs.set_shown(leaving, ws, strip);
        }
        let arriving = self
            .strip_shown_on(display)
            .filter(|_| leaving != Some(display));
        self.outputs.set_focused(display);
        match (ws, arriving) {
            (Some(ws), Some(strip)) => self.active_strip.insert(ws, strip) != Some(strip),
            _ => false,
        }
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.id() == self.active)
    }

    /// Take the workspace `id` off this layout, the person going to the next one along when it was
    /// the one in front (the one before it when it was the last), with what pointed into it. `None`
    /// when the layout holds no such workspace.
    pub fn take_off(&mut self, id: WorkspaceId) -> Option<Workspace> {
        let at = self.workspaces.iter().position(|w| w.id() == id)?;
        let taken = self.workspaces.remove(at);
        if taken.id() == self.active {
            let next_at = at.min(self.workspaces.len().saturating_sub(1));
            if let Some(next) = self.workspaces.get(next_at) {
                self.active = next.id();
            }
        }
        self.active_strip.remove(&id);
        let strips: Vec<StripId> = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .map(|strip| strip.id)
            .collect();
        self.active_group.retain(|strip, _| strips.contains(strip));
        Some(taken)
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
    /// unseeded — callers compose this with their own fallback. A group
    /// carried to another strip leaves this strip's pointer behind, so a
    /// pointer naming a group the strip no longer holds reads as unseeded
    /// here: a reader that falls back then focuses a window the strip
    /// actually shows, never one that is elsewhere. Pure read; nothing is
    /// rewritten.
    pub fn active_group_for(&self, strip_id: StripId) -> Option<GroupId> {
        let recorded = self.active_group.get(&strip_id).copied()?;
        self.strip_holds(strip_id, recorded).then_some(recorded)
    }

    /// Whether `strip_id` holds the group `group_id` — the membership a
    /// strip's focus pointer is judged by.
    fn strip_holds(&self, strip_id: StripId, group_id: GroupId) -> bool {
        self.workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .find(|strip| strip.id == strip_id)
            .is_some_and(|strip| strip.window_groups.iter().any(|group| group.id == group_id))
    }

    /// Mark `group_id` as the active group of `strip_id`. Not validated
    /// against the strip's actual membership — the host's reconciler is
    /// expected to be the sole writer.
    pub fn set_active_group(&mut self, strip_id: StripId, group_id: GroupId) {
        self.active_group.insert(strip_id, group_id);
    }

    /// The window group covering the display the person works on, if one
    /// is: the window taken full screen there. A window covers the display
    /// it stands on and is out of sight on any other, so it covers nothing
    /// while the person is on another strip. `None` when no window covers
    /// its display, or when the one that did has since gone.
    pub fn fullscreen_group(&self) -> Option<GroupId> {
        let group = self.fullscreen?;
        let strip = self.active_strip_id()?;
        self.active_workspace()?
            .strips
            .iter()
            .find(|candidate| candidate.id == strip)?
            .window_groups
            .iter()
            .any(|candidate| candidate.id == group)
            .then_some(group)
    }

    /// Cover the display with the window group `group` of `strip`, or give
    /// it back. One window covers a display, so covering gives the display
    /// back from any other one first. Whether anything changed; false for a
    /// group the tree does not hold or one that was not covering the
    /// display, which leave the desktop as it was.
    pub fn set_fullscreen(&mut self, strip: StripId, group: GroupId, fullscreen: bool) -> bool {
        let held = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .find(|candidate| candidate.id == strip)
            .is_some_and(|held| {
                held.window_groups
                    .iter()
                    .any(|candidate| candidate.id == group)
            });
        let next = if fullscreen && held {
            Some(group)
        } else if !fullscreen && self.fullscreen == Some(group) {
            None
        } else {
            return false;
        };
        let changed = self.fullscreen != next;
        self.fullscreen = next;
        changed
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
                // The pointer names a group the strip holds, and it is not
                // closing: focus stays where it was. A group carried to
                // another strip is no longer this strip's, whatever the
                // pointer still says: the strip focuses a window it holds.
                let idx = strip.window_groups.iter().position(|g| g.id == id);
                if idx.is_some() && !is_closing(id) {
                    return Some(id);
                }
                // Recorded group is closing, or has left — find its index,
                // then scan left for a survivor; if none, scan right.
                let Some(idx) = idx else {
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

/// A workspace on the desktop: its strips and canvas, with the policy,
/// sandbox and grant sets its members' devices decide by.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(from = "StoredWorkspace")]
pub struct Workspace {
    id: WorkspaceId,
    /// The workspace's name, as its governance records it: projected onto the shell by the
    /// device showing it ([`Self::named_as`]), never stored or sent with the layout, so the
    /// workspace has one name, the one every device and list of it reads.
    #[serde(skip)]
    pub name: String,
    pub strips: Vec<Strip>,
    /// Whether the workspace shows its strips or its canvas, and where each
    /// window group stands on the canvas: the same windows, arranged on a
    /// 2D surface ([`crate::layout`]).
    #[serde(default, skip_serializing_if = "WorkspaceLayout::is_default")]
    pub layout: WorkspaceLayout,
    /// The workspace's members: its people with their roles and the agents
    /// admitted in it, each with the colour it wears everywhere.
    /// Placed from [`Workspace::policy`] and presence before a surface reads
    /// them; never read back from a stored workspace.
    /// `skip_deserializing` rather than `skip`: reading is the direction
    /// that can fabricate a stale member, but `Serialize` still feeds the
    /// control socket's state dump.
    #[serde(skip_deserializing, default)]
    pub members: Vec<Member>,
    /// The name each person goes by, as the workspace's governance keeps it, by their root: the
    /// name they gave themselves, which every surface showing someone who is not here reads
    /// ([`Self::person_name`]). Projected by the device showing the workspace from the governance
    /// it holds ([`Self::named_people`]); never stored or sent with the layout.
    #[serde(skip)]
    pub people_names: BTreeMap<String, String>,
    /// How each of them is here now: where they are, what they say they are
    /// doing, how available they are. Said again on every frame and never
    /// written: presence, not the workspace's document.
    #[serde(skip, default)]
    pub present: Vec<Present>,
    /// Who may do what in this workspace: its owner (structural, `None` until the
    /// local user adopts it on load), each member's role preset, and every
    /// grant on the workspace's tree. Changed only through governance actions.
    pub policy: WorkspacePolicy,
    /// What apps' code may reach outside their documents in this workspace:
    /// filesystem paths, file records, devices and network hosts granted
    /// through portals. Never what a person grants an agent.
    #[serde(default)]
    pub sandbox: Vec<CapOverride>,
    /// Named grant sets people assign instead of writing grants one by one.
    #[serde(default)]
    pub presets: Vec<Preset>,
    /// Default-application policy belongs to the workspace, while each
    /// person's ordinary defaults remain in their personal settings. All
    /// mutation goes through the authority-checked methods below.
    #[serde(default)]
    pub default_apps: desktop_policy::WorkspaceDefaults,
    /// Consequential effects are activity, but never pretend to be edits to
    /// an app's collaborative document.
    #[serde(default, skip_serializing_if = "crate::history::omitted")]
    pub effect_receipts: Vec<WorkspaceEffectReceipt>,
    /// Shortcuts saved in this workspace: a person's own in their personal
    /// workspace, shared ones in a collaborative one. Projected as a CRDT
    /// map keyed by shortcut id, so they reach every desktop the workspace
    /// reaches.
    #[serde(default)]
    pub shortcuts: Vec<tilo_automation::SavedShortcut>,
    /// Surfaces from this workspace being streamed to peers. Keys are
    /// wayland `wl_surface` ids; values are the target Iroh peers
    /// (hashed to `u64`) — a `HashSet` for multi-recipient streaming.
    #[serde(default)]
    pub shared_surfaces: SharedSurfaces,
    /// When each grant was given, in Unix milliseconds, by the grant's id, as the workspace's
    /// membership log records it: what a view listing its grants says of when they arrived. The
    /// device holding the workspace projects it from that log, as it projects the policy, and the
    /// next projection replaces whatever a stored layout kept.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub grant_times: BTreeMap<String, u64>,
    /// The windows closed here and not reopened, newest last
    /// ([`crate::closed_tabs`]). This device's own history, so it is written
    /// to no document and projected into no workspace: a close is undone where
    /// the person closed it, with the app this device opens.
    #[serde(skip)]
    pub(crate) closed_tabs: crate::closed_tabs::ClosedTabs,
}

/// A workspace as stored: its policy names the workspace it was stored for, or is
/// absent for a workspace that names no authority yet.
#[derive(Deserialize)]
struct StoredWorkspace {
    id: WorkspaceId,
    strips: Vec<Strip>,
    #[serde(default)]
    layout: WorkspaceLayout,
    #[serde(skip_deserializing, default)]
    members: Vec<Member>,
    #[serde(default)]
    policy: Option<WorkspacePolicy>,
    #[serde(default)]
    sandbox: Vec<CapOverride>,
    #[serde(default)]
    presets: Vec<Preset>,
    #[serde(default)]
    default_apps: desktop_policy::WorkspaceDefaults,
    #[serde(default)]
    effect_receipts: Vec<WorkspaceEffectReceipt>,
    #[serde(default)]
    shortcuts: Vec<tilo_automation::SavedShortcut>,
    #[serde(default)]
    shared_surfaces: SharedSurfaces,
    #[serde(default)]
    grant_times: BTreeMap<String, u64>,
}

impl From<StoredWorkspace> for Workspace {
    fn from(stored: StoredWorkspace) -> Self {
        let identity = stored.id;
        let policy = match stored.policy {
            Some(mut policy) => {
                policy.rehome(identity.to_string());
                policy
            }
            None => WorkspacePolicy::unowned(identity.to_string(), &Grantee::role(Role::Owner)),
        };
        Self {
            id: identity,
            name: String::new(),
            strips: stored.strips,
            layout: stored.layout,
            members: stored.members,
            present: Vec::new(),
            policy,
            sandbox: stored.sandbox,
            presets: stored.presets,
            default_apps: stored.default_apps,
            effect_receipts: stored.effect_receipts,
            shortcuts: stored.shortcuts,
            shared_surfaces: stored.shared_surfaces,
            grant_times: stored.grant_times,
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
            people_names: BTreeMap::new(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WorkspaceDefaultError {
    #[error("only a workspace administrator may change workspace policy")]
    NotAdministrator,
    #[error("a person may only change their own workspace default")]
    WrongIdentity,
    #[error("invalid default application: {0}")]
    InvalidSelection(desktop_policy::DefaultAppError),
}

/// Every question taking `now` reads the caller's clock, Unix seconds: grants
/// expire, and the model does not read a clock of its own.
impl Workspace {
    /// Set the administrator layer of default-app policy. The shell supplies
    /// the authenticated local identity; a caller cannot widen policy by
    /// merely painting an enabled control.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceDefaultError::NotAdministrator`] if `actor` may not
    /// change the workspace's settings, or
    /// [`WorkspaceDefaultError::InvalidSelection`] if `policy` names an app
    /// package the resolver rejects for `resource`.
    pub fn set_default_app_policy_as_at(
        &mut self,
        actor: &str,
        resource: desktop_policy::ResourceKind,
        policy: desktop_policy::WorkspaceDefaultPolicy,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
        now: u64,
    ) -> Result<(), WorkspaceDefaultError> {
        if !self
            .policy
            .may_change_at(actor, roles::governance::SETTINGS, now)
        {
            return Err(WorkspaceDefaultError::NotAdministrator);
        }
        if let desktop_policy::WorkspaceDefaultPolicy::App(package) = &policy {
            resolver
                .validate_app(&resource, package)
                .map_err(WorkspaceDefaultError::InvalidSelection)?;
        }
        self.default_apps.policy.insert(resource, policy);
        Ok(())
    }

    /// Set a person's own workspace-level choice. Even a workspace owner may
    /// not silently rewrite another person's preference; administrative
    /// policy is the separate method above and remains visibly attributable.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceDefaultError::WrongIdentity`] if `actor` differs
    /// from `subject` — a person may only set their own choice — or
    /// [`WorkspaceDefaultError::InvalidSelection`] if `choice` names an app
    /// package the resolver rejects for `resource`.
    pub fn set_default_app_choice_as(
        &mut self,
        actor: &str,
        subject: &str,
        resource: desktop_policy::ResourceKind,
        choice: desktop_policy::UserWorkspaceChoice,
        resolver: &desktop_policy::DefaultAppResolver<'_>,
    ) -> Result<(), WorkspaceDefaultError> {
        if actor != subject {
            return Err(WorkspaceDefaultError::WrongIdentity);
        }
        if let desktop_policy::UserWorkspaceChoice::App(package) = &choice {
            resolver
                .validate_app(&resource, package)
                .map_err(WorkspaceDefaultError::InvalidSelection)?;
        }
        self.default_apps
            .user_choices
            .entry(subject.to_owned())
            .or_default()
            .insert(resource, choice);
        Ok(())
    }

    /// The workspace this workspace is, given when it was made and never changed.
    #[must_use]
    pub fn id(&self) -> WorkspaceId {
        self.id
    }

    /// Project the name the workspace's governance records, `name`, onto this shell: the
    /// workspace's one name, which the layout keeps none of, so the device showing the workspace
    /// projects it here from the governance it holds. Naming it what it is already called changes
    /// nothing. Whether the shell changed.
    pub fn named_as(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.name == name {
            return false;
        }
        name.clone_into(&mut self.name);
        true
    }

    /// Project the names the workspace's governance gives its people, `names` (person root, hex,
    /// to the name they go by), onto this shell. A blank name names nobody. Whether the shell
    /// changed.
    pub fn named_people<'a>(
        &mut self,
        names: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> bool {
        let names: BTreeMap<String, String> = names
            .into_iter()
            .map(|(person, name)| (person.to_owned(), name.trim().to_owned()))
            .filter(|(_, name)| !name.is_empty())
            .collect();
        if self.people_names == names {
            return false;
        }
        self.people_names = names;
        true
    }

    /// The name the person `person` (root, hex) goes by here: the one they show by while here,
    /// else the one the workspace's governance keeps for them. `None` for someone nobody named,
    /// never their key.
    #[must_use]
    pub fn person_name(&self, person: &str) -> Option<&str> {
        fn shown(name: &str) -> Option<&str> {
            let name = name.trim();
            let key = name.len() >= 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit());
            (!name.is_empty() && !key).then_some(name)
        }
        self.members
            .iter()
            .find(|member| member.id.person_id() == Some(person))
            .and_then(|member| shown(&member.name))
            .or_else(|| self.people_names.get(person).and_then(|name| shown(name)))
    }

    /// `member` joins this workspace's member list: what a host places from its governance and
    /// presence before a surface reads who is in the workspace. A member already listed keeps
    /// their place.
    pub fn list_member(&mut self, member: Member) {
        match self
            .members
            .iter_mut()
            .find(|listed| listed.id == member.id)
        {
            Some(listed) => *listed = member,
            None => self.members.push(member),
        }
    }

    /// `here` is `member`'s presence now, replacing what was said of them before.
    pub fn place_presence(&mut self, here: Present) {
        match self
            .present
            .iter_mut()
            .find(|placed| placed.member == here.member)
        {
            Some(placed) => *placed = here,
            None => self.present.push(here),
        }
    }

    /// `members` join this workspace's member list, each of them here now saying nothing of
    /// themselves.
    pub fn members_here(&mut self, members: impl IntoIterator<Item = Member>) {
        for member in members {
            self.member_here(member);
        }
    }

    /// `member` is one of this workspace's members and is here now, saying nothing of themselves
    /// yet: what a host places for someone presence named before the roster listed them.
    pub fn member_here(&mut self, member: Member) {
        self.place_presence(Present::of(member.id.clone()));
        self.list_member(member);
    }
}

impl Workspace {
    /// The workspace `id`, named `name`, with nothing in it yet.
    pub fn new(id: WorkspaceId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            strips: Vec::new(),
            layout: WorkspaceLayout::default(),
            members: Vec::new(),
            present: Vec::new(),
            policy: WorkspacePolicy::unowned(id.to_string(), &Grantee::role(Role::Owner)),
            sandbox: Vec::new(),
            presets: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            effect_receipts: Vec::new(),
            shortcuts: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
            grant_times: BTreeMap::new(),
            closed_tabs: crate::closed_tabs::ClosedTabs::default(),
            people_names: BTreeMap::new(),
        }
    }

    /// Every window of this workspace, in its order: strip by strip, group by group.
    pub fn tabs(&self) -> impl Iterator<Item = &crate::Tab> {
        self.strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
    }

    /// Mark `surface_id` as being streamed to `peer_id`. Additive — two
    /// calls record both peers in the per-surface set.
    pub fn share_surface(&mut self, surface_id: u64, peer_id: u64) {
        self.shared_surfaces.share(surface_id, peer_id);
    }

    /// Drop the share entry for `surface_id` (no-op if unshared).
    /// Returns the peers it was shared with, so callers can fire
    /// `StopShare` for each without a second lookup.
    pub fn unshare_surface(&mut self, surface_id: u64) -> Option<std::collections::HashSet<u64>> {
        self.shared_surfaces.unshare(surface_id)
    }

    /// Stop streaming `surface_id` to one peer, leaving others intact.
    /// Drops the surface key entirely once the last peer is removed, so
    /// [`Self::is_surface_shared`] reports `false` again.
    pub fn unshare_surface_from_peer(&mut self, surface_id: u64, peer_id: u64) -> bool {
        self.shared_surfaces.unshare_peer(surface_id, peer_id)
    }

    /// Whether `surface_id` is currently streamed somewhere. Used by
    /// the compositor's commit hook to short-circuit the readback for
    /// unshared surfaces (~all of them in steady state).
    pub fn is_surface_shared(&self, surface_id: u64) -> bool {
        self.shared_surfaces.contains(surface_id)
    }
}

impl Default for Workspace {
    /// A workspace with a fresh id: two made this way are never the same workspace.
    fn default() -> Self {
        Self::new(WorkspaceId::new(), String::new())
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    /// Someone is called what they show by while here, else what the workspace's governance
    /// names them: never their key, and nobody named stays unnamed.
    #[test]
    fn a_person_goes_by_their_given_name_here_or_not() {
        let ada = "ad".repeat(32);
        let bo = "b0".repeat(32);
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Lounge");
        assert!(workspace.named_people([(ada.as_str(), " Ada "), (bo.as_str(), bo.as_str())]));
        assert!(
            !workspace.named_people([(ada.as_str(), "Ada"), (bo.as_str(), bo.as_str())]),
            "the same names change nothing"
        );
        assert_eq!(
            workspace.person_name(&ada),
            Some("Ada"),
            "governance names someone not here"
        );
        assert_eq!(workspace.person_name(&bo), None, "a key is never a name");
        assert_eq!(workspace.person_name("someone"), None);
        workspace.member_here(Member::named(
            collab_types::MemberId::person(ada.clone()),
            "Ada Lovelace",
        ));
        assert_eq!(
            workspace.person_name(&ada),
            Some("Ada Lovelace"),
            "who is here says their own"
        );
        assert!(
            workspace.named_people([]),
            "names leave with the people the workspace names"
        );
        assert_eq!(workspace.person_name(&bo), None);
    }

    /// A shell takes the name its workspace's governance records, whether it
    /// landed before anything named the workspace or its copy is older.
    #[test]
    fn a_shell_takes_the_name_its_governance_records() {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "");
        assert!(workspace.named_as("Tilo"), "an unnamed shell takes it");
        assert_eq!(workspace.name, "Tilo");
        assert!(!workspace.named_as("Tilo"), "its own name changes nothing");
        assert!(workspace.named_as("Tilo Coffee"), "a rename reaches it");
        assert_eq!(workspace.name, "Tilo Coffee");
        assert!(
            !workspace.named_as("  "),
            "a governance that names nothing leaves the name it has"
        );
        assert_eq!(workspace.name, "Tilo Coffee");
        assert!(
            workspace.named_as(" Tilo "),
            "the name as written is trimmed"
        );
        assert_eq!(workspace.name, "Tilo");
    }

    #[test]
    fn a_receipt_is_recorded_once_and_bounded() {
        use tilo_actions::{ActionContext, EffectReceiptStatus, InvocationId, MemberId};
        let id = WorkspaceId::from_u128(1);
        let mut tree = Layout {
            workspaces: vec![Workspace::new(id, "Home")],
            ..Layout::default()
        };
        let invocation = InvocationId::new();
        let receipt = tree
            .append_workspace_effect_receipt(
                id,
                WorkspaceEffectReceipt::recorded(
                    invocation,
                    &MemberId::person("owner").into(),
                    ActionContext::default(),
                    EffectReceiptStatus::Succeeded,
                    42,
                ),
            )
            .unwrap();
        assert!(
            receipt
                .context
                .facets
                .iter()
                .any(|facet| facet.key == "tilo.invocation")
        );
        assert!(
            tree.append_workspace_effect_receipt(
                id,
                WorkspaceEffectReceipt::redacted(
                    invocation,
                    &MemberId::person("owner").into(),
                    ActionContext::default(),
                    EffectReceiptStatus::Succeeded,
                    43,
                ),
            )
            .is_none()
        );
        assert_eq!(tree.workspaces[0].effect_receipts.len(), 1);
    }
}
