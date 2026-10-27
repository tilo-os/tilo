//! This device's layout of a room another device governs.
//!
//! The room's layout is a record every device of it writes
//! ([`tilo_desktop_model::shared_record::device`]), here as changes on top of
//! what the room's governor admitted and what this device wrote before that
//! the governor has not admitted yet. The tree shows them at once, offline
//! too; the network sends them to the governor until the room holds them,
//! and every member then takes them in. Nothing is asked of the device a
//! window runs on: moving, resizing or closing it is a change like any other.

use automerge::{AutoCommit, ChangeHash};
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::{projection, shared_record};

use super::{RoomId, StripId, WorkspaceTree, counter_of, peer_salt_of};

#[cfg(test)]
thread_local! {
    /// Room documents [`WorkspaceTree::capture_governed_layout`] copied, on
    /// this thread.
    static COPIES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// What one look at a room's layout found nothing to write for. Writing is a
/// function of exactly these, so while they hold, the next look would write
/// nothing again, and the room's document is not copied to find that out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captured {
    /// The admitted room document's heads.
    heads: Vec<ChangeHash>,
    /// The layout as other devices are to see it, serialised.
    layout: blake3::Hash,
    /// How many changes were waiting for the governor.
    pending: usize,
}

impl WorkspaceTree {
    /// Write this device's layout of `room`, whose document the native owner
    /// admits, as changes on top of what the room admitted and what this
    /// device wrote that it has not admitted yet
    /// ([`Self::layout_pending`]). Nothing in a room whose layout is
    /// personal. Returns whether it wrote anything; cheap when nothing
    /// changed since the last look.
    pub(crate) fn capture_governed_layout(&mut self, room: RoomId) -> bool {
        let local_salt = self.local_salt();
        if local_salt == 0 {
            return false;
        }
        let Some(admitted) = self.room_docs.get(&room) else {
            return false;
        };
        if projection::room_layout(admitted).is_personal() {
            self.layout_pending.remove(&room);
            self.layout_captured.remove(&room);
            return false;
        }
        let Some(workspace) = self.workspace_in_room(room) else {
            return false;
        };
        let mut published = workspace.clone();
        if let Some(hosting) = &self.window_hosting {
            published.strips = hosting.published(&workspace.strips, &self.package_launches);
        }
        let heads = automerge::transaction::Transactable::base_heads(admitted);
        let looked_at = Captured {
            heads,
            layout: serde_json::to_vec(&published.strips)
                .map_or_else(|_| blake3::hash(&[]), |bytes| blake3::hash(&bytes)),
            pending: self.layout_pending.get(&room).map_or(0, Vec::len),
        };
        if self.layout_captured.get(&room) == Some(&looked_at) {
            return false;
        }
        #[cfg(test)]
        COPIES.with(|copies| copies.set(copies.get().saturating_add(1)));
        let mut doc: AutoCommit = admitted.clone();
        // What the governor admitted since is done; the rest still stands,
        // in the order it was written, and what is written now builds on it:
        // two different changes with one actor and sequence number poison
        // the room for everyone who receives both.
        let pending = self.layout_pending.entry(room).or_default();
        pending.retain(|bytes| {
            automerge::Change::from_bytes(bytes.clone())
                .is_ok_and(|change| doc.get_change_by_hash(&change.hash()).is_none())
        });
        let unadmitted = pending
            .iter()
            .filter_map(|bytes| automerge::Change::from_bytes(bytes.clone()).ok());
        if doc.apply_incoming(unadmitted).is_err() {
            pending.clear();
            return false;
        }
        let before = doc.get_heads();
        doc.set_actor(self.layout_actor());
        let share_floor = self.room_share_floor.get(&room).copied();
        let mine = |id: u64| {
            peer_salt_of(id) == local_salt
                && share_floor.is_none_or(|floor| counter_of(id) >= floor)
        };
        let retained = |id: u64| self.strips_to_retain.contains(&StripId(id));
        let person = self.local_person.map(hex::encode);
        let written = shared_record::device::write(
            &mut doc,
            room,
            &published,
            &shared_record::device::Writer {
                person: person.as_deref(),
                mine: &mine,
                retained: &retained,
            },
        );
        doc.commit();
        let changes: Vec<Vec<u8>> = doc
            .get_changes(&before)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect();
        let wrote = written.is_ok() && !changes.is_empty();
        let pending = self.layout_pending.entry(room).or_default();
        if wrote {
            pending.extend(changes);
        }
        let looked_at = Captured {
            pending: pending.len(),
            ..looked_at
        };
        self.layout_captured.insert(room, looked_at);
        wrote
    }

    /// The changes to `room`'s layout this device wrote that the room's
    /// governor has not admitted yet, oldest first.
    pub(crate) fn pending_layout(&self, room: RoomId) -> Vec<Vec<u8>> {
        self.layout_pending.get(&room).cloned().unwrap_or_default()
    }

    /// The actor this device writes room layouts as: its device key, so
    /// every device numbers its own changes.
    fn layout_actor(&self) -> automerge::ActorId {
        self.window_hosting
            .as_ref()
            .and_then(|hosting| hex::decode(&hosting.device).ok())
            .map_or_else(
                || automerge::ActorId::from(self.local_salt().to_be_bytes().to_vec()),
                automerge::ActorId::from,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Strip, Tab, TileContent, WindowGroup, WindowWidth, Workspace};
    use tilo_desktop_model::WorkspaceId;

    const SALT: u16 = 0x0042;

    fn own(counter: u64) -> u64 {
        (u64::from(SALT) << 48) | counter
    }

    fn group(id: u64, tab: u64) -> WindowGroup {
        WindowGroup {
            id: own(id),
            tabs: vec![Tab {
                id: own(tab),
                title: format!("tab {tab}"),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.chat".into(),
                    surface_id: tab,
                },
                app_title: None,
            }],
            active_tab: own(tab),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }
    }

    fn tree(room: RoomId) -> WorkspaceTree {
        let mut tree = WorkspaceTree::default();
        tree.set_salt_override(SALT);
        let mut workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Room");
        let mut strip = Strip::new(StripId(own(1)), "Main");
        strip.window_groups.push(group(2, 3));
        workspace.strips.push(strip);
        tree.desktop.active = workspace.id;
        tree.desktop.workspaces.push(workspace);
        tree.room_docs.insert(room, AutoCommit::new());
        tree
    }

    /// A window opened while the last change is still waiting for the
    /// governor builds on it: the resend is the same change, the new window
    /// is the next one by the same device, and the governor (and every
    /// member after it) takes both.
    #[test]
    fn a_layout_written_before_the_last_is_admitted_builds_on_it() {
        let room = RoomId::new();
        let mut tree = tree(room);
        assert!(tree.capture_governed_layout(room));
        let first = tree.pending_layout(room);
        assert!(!first.is_empty());
        assert!(
            !tree.capture_governed_layout(room),
            "an unchanged layout writes nothing"
        );
        assert_eq!(tree.pending_layout(room), first, "the resend is the same");

        tree.desktop.workspaces[0].strips[0]
            .window_groups
            .push(group(4, 5));
        assert!(tree.capture_governed_layout(room));
        let both = tree.pending_layout(room);
        assert_eq!(
            both[..first.len()],
            first[..],
            "the first change still leads"
        );
        let seqs: Vec<u64> = both
            .iter()
            .map(|bytes| automerge::Change::from_bytes(bytes.clone()).unwrap().seq())
            .collect();
        let mut expected = seqs.clone();
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(
            seqs, expected,
            "each change has a sequence number of its own"
        );

        // The governor takes them all, and nothing is waiting any more.
        let mut admitted = tree.room_docs[&room].clone();
        for change in &both {
            tilo_automerge::ApplyIncoming::load_incoming(&mut admitted, change).unwrap();
        }
        tree.room_docs.insert(room, admitted);
        assert!(!tree.capture_governed_layout(room));
        assert!(tree.pending_layout(room).is_empty());
    }

    /// A room in use carries thousands of edits of history, and copying it on
    /// every look is what a settled room must not cost: counted, not timed.
    #[test]
    fn a_settled_room_is_looked_at_without_copying_it() {
        const LOOKS: u64 = 50;
        let room = RoomId::new();
        let mut tree = tree(room);
        assert!(tree.capture_governed_layout(room));
        let mut admitted = tree.room_docs[&room].clone();
        for change in tree.pending_layout(room) {
            tilo_automerge::ApplyIncoming::load_incoming(&mut admitted, &change).unwrap();
        }
        tree.room_docs.insert(room, admitted);
        assert!(
            !tree.capture_governed_layout(room),
            "admitted: nothing to write"
        );
        let before = COPIES.with(std::cell::Cell::get);
        for _ in 0..LOOKS {
            assert!(!tree.capture_governed_layout(room));
        }
        assert_eq!(
            COPIES.with(std::cell::Cell::get),
            before,
            "no look copies the room"
        );
    }

    /// A window moved here while the room's governor is away stays where it
    /// was put through every fold of what the room admitted meanwhile, and
    /// reaches the room once it takes the change.
    #[test]
    fn a_move_waiting_for_the_governor_is_shown_through_every_fold() {
        let room = RoomId::new();
        let mut tree = tree(room);
        tree.capture_governed_layout(room);
        let mut admitted = tree.room_docs[&room].clone();
        for change in tree.pending_layout(room) {
            tilo_automerge::ApplyIncoming::load_incoming(&mut admitted, &change).unwrap();
        }
        tree.room_docs.insert(room, admitted.clone());

        // A second strip, and the window moved into it, here only.
        let later = Strip::new(StripId(own(10)), "Later");
        let ws = &mut tree.desktop.workspaces[0];
        ws.strips.push(later);
        let moved = ws.strips[0].window_groups.remove(0);
        ws.strips[1].window_groups.push(moved);
        assert!(tree.capture_governed_layout(room));

        // The governor's room moves on without it; folding it keeps the move.
        tree.room_docs.insert(room, admitted);
        tree.merge_room_contents(room);
        let places: Vec<u64> = tree.desktop.workspaces[0]
            .strips
            .iter()
            .filter(|strip| !strip.window_groups.is_empty())
            .map(|strip| strip.id.raw())
            .collect();
        assert_eq!(places, [own(10)]);
    }
}
