//! The Lounge: the strip every workspace starts with, where its people meet.
//!
//! It holds the workspace's chat, tasks and notes, and it belongs to nobody:
//! its strip, its first windows and their tabs carry ids in the
//! [`SHARED_SALT`](crate::ids::SHARED_SALT) namespace, derived from the room
//! ([`crate::ids::shared_id`]), so every member's desktop has the same
//! Lounge without anyone publishing it. What it shows is the room's own
//! records: the room's chat, the notebook of the Lounge's Notes tab, the
//! room's tasks in its activity window. Windows a person opens into the
//! Lounge are theirs as anywhere else and reach the others the usual way.
//!
//! [`ensure`] keeps the invariant: one Lounge, first in the workspace. A
//! Lounge created here starts with its three windows; one already there
//! keeps whatever its person closed or added.

use crate::app::AppKind;
use crate::ids::{GroupId, StripId, TabId, is_shared, shared_id};
use crate::layout::{Strip, Tab, TileContent, WindowGroup};
use crate::width::WindowWidth;
use crate::workspace::Workspace;

/// The name the model stores; hosts show their own word for it.
pub const LOUNGE_NAME: &str = "Lounge";

/// The windows a new Lounge opens with: the part name its ids derive from,
/// the tab title, and what the tab shows.
fn first_windows() -> [(&'static str, &'static str, TileContent); 3] {
    [
        (
            "chat",
            "Chat",
            TileContent::AppPlaceholder {
                kind: AppKind::Other("Chat".into()),
                label: "Chat".into(),
            },
        ),
        // The room's tasks live in its activity window.
        ("tasks", "Activity", TileContent::WorkspaceActivity),
        (
            "notes",
            "Notes",
            TileContent::AppPlaceholder {
                kind: AppKind::Other("Notes".into()),
                label: "Notes".into(),
            },
        ),
    ]
}

/// What the Lounge's ids derive from: the room every member shares, or the
/// workspace itself while it is in no room.
fn scope(workspace: &Workspace) -> String {
    if workspace.room.is_nil() {
        format!("workspace:{}", workspace.id.0)
    } else {
        workspace.room.to_string()
    }
}

/// The Lounge strip's id in `workspace`.
pub fn strip_id(workspace: &Workspace) -> StripId {
    StripId(shared_id(&scope(workspace), "lounge"))
}

/// A new Lounge for `workspace`, with its first windows.
pub fn new_lounge(workspace: &Workspace) -> Strip {
    let scope = scope(workspace);
    let window_groups = first_windows()
        .into_iter()
        .map(|(part, title, tile)| {
            let group: GroupId = shared_id(&scope, &format!("lounge/{part}"));
            let tab: TabId = shared_id(&scope, &format!("lounge/{part}/tab"));
            WindowGroup {
                id: group,
                tabs: vec![Tab {
                    id: tab,
                    title: title.into(),
                    tile,
                }],
                active_tab: tab,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            }
        })
        .collect();
    Strip {
        id: strip_id(workspace),
        name: LOUNGE_NAME.into(),
        window_groups,
    }
}

/// Make `workspace` have exactly one Lounge, first. A Lounge under another
/// id (the workspace joined a room since, so its scope changed) takes the
/// current id and keeps its windows; a second one folds its windows into the
/// first. Returns whether anything changed.
pub fn ensure(workspace: &mut Workspace) -> bool {
    let id = strip_id(workspace);
    let first_is_lounge = workspace
        .strips
        .first()
        .is_some_and(|strip| is_shared(strip.id.raw()));
    let mut changed = !first_is_lounge;
    let mut lounges: Vec<Strip> = Vec::new();
    let mut others: Vec<Strip> = Vec::with_capacity(workspace.strips.len());
    for strip in std::mem::take(&mut workspace.strips) {
        if is_shared(strip.id.raw()) {
            lounges.push(strip);
        } else {
            others.push(strip);
        }
    }
    let mut lounge = if lounges.is_empty() {
        new_lounge(workspace)
    } else {
        let mut first = lounges.remove(0);
        for extra in lounges {
            changed = true;
            // A window both hold is one window: ids key it everywhere.
            for group in extra.window_groups {
                if !first.window_groups.iter().any(|kept| kept.id == group.id) {
                    first.window_groups.push(group);
                }
            }
        }
        first
    };
    if lounge.id != id {
        lounge.id = id;
        changed = true;
    }
    if lounge.name != LOUNGE_NAME {
        lounge.name = LOUNGE_NAME.into();
        changed = true;
    }
    // First: where Super+1 and the strip switcher's top row find it.
    workspace.strips = std::iter::once(lounge).chain(others).collect();
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{RoomId, WorkspaceId, peer_salt_of};
    use crate::layout::StripKind;

    fn workspace(room: Option<&str>) -> Workspace {
        let mut w = Workspace::new(WorkspaceId(7), "Home");
        w.strips.push(Strip::new(StripId(42), "Main"));
        if let Some(room) = room {
            w.room = RoomId::parse(room).expect("room");
        }
        w
    }

    const ROOM: &str = "6f1c2c1e-6d3b-4c55-9d1e-2f6e5a1b9c01";

    #[test]
    fn every_member_derives_the_same_lounge_first_and_owned_by_nobody() {
        let mut mine = workspace(Some(ROOM));
        let mut theirs = workspace(Some(ROOM));
        theirs.id = WorkspaceId(99);
        assert!(ensure(&mut mine));
        assert!(ensure(&mut theirs));
        let (a, b) = (&mine.strips[0], &theirs.strips[0]);
        assert_eq!(a.kind(), StripKind::Lounge);
        assert_eq!(a.id, b.id, "the room decides the id, not the device");
        assert_eq!(peer_salt_of(a.id.raw()), crate::ids::SHARED_SALT);
        let ids = |s: &Strip| -> Vec<(u64, Vec<u64>)> {
            s.window_groups
                .iter()
                .map(|g| (g.id, g.tabs.iter().map(|t| t.id).collect()))
                .collect()
        };
        assert_eq!(ids(a), ids(b), "and the same first windows");
        assert_eq!(a.window_groups.len(), 3);
        assert_eq!(mine.strips[1].name, "Main", "regular strips follow it");
        assert!(!ensure(&mut mine), "a second pass changes nothing");
    }

    #[test]
    fn a_lounge_keeps_its_windows_when_the_workspace_joins_a_room() {
        let mut w = workspace(None);
        ensure(&mut w);
        w.strips[0].window_groups.truncate(1);
        let local = w.strips[0].id;
        w.room = RoomId::parse(ROOM).expect("room");
        assert!(ensure(&mut w));
        assert_ne!(w.strips[0].id, local);
        assert_eq!(w.strips[0].id, strip_id(&w));
        assert_eq!(
            w.strips[0].window_groups.len(),
            1,
            "what was closed stays closed"
        );
        assert_eq!(w.strips.len(), 2);
    }

    #[test]
    fn a_lounge_that_drifted_or_doubled_is_put_back_first_and_single() {
        let mut w = workspace(Some(ROOM));
        let mut lounge = new_lounge(&w);
        lounge.name = "Renamed".into();
        // A Lounge from before the workspace's scope changed: other ids.
        let mut elsewhere = w.clone();
        elsewhere.room = RoomId::new();
        let mut extra = new_lounge(&elsewhere);
        extra.window_groups.truncate(1);
        let mut copy = new_lounge(&w);
        copy.window_groups.truncate(1);
        w.strips.push(lounge);
        w.strips.push(extra);
        w.strips.push(copy);
        assert!(ensure(&mut w));
        assert_eq!(w.strips.len(), 2);
        assert_eq!(w.strips[0].name, LOUNGE_NAME);
        assert_eq!(w.strips[0].id, strip_id(&w));
        assert_eq!(
            w.strips[0].window_groups.len(),
            4,
            "its own three windows and the other Lounge's one, each once"
        );
        assert!(!ensure(&mut w), "a second pass changes nothing");
    }

    /// Through a room document: nobody publishes the Lounge, so no member's
    /// merge drops it; a window one member opens into it reaches the others
    /// inside their Lounge; and a strip a person made reaches them while
    /// still empty.
    #[test]
    fn the_lounge_survives_merges_and_carries_members_windows() {
        use crate::ids::{WorkspaceId, compose_id};
        use crate::projection::{
            EmptyStrips, Reader, contents_of, merge_room_contents, project_owned_contents,
            retain_owned_strip,
        };
        use crate::tree::DesktopTree;
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let room = RoomId::parse(ROOM).expect("room");
        let desktop = |id: u64| {
            let mut w = Workspace::new(WorkspaceId(id), "Team");
            w.room = room;
            let mut tree = DesktopTree {
                active: w.id,
                workspaces: vec![w],
                ..DesktopTree::default()
            };
            tree.ensure_lounges();
            tree
        };
        let mut a = desktop(1);
        let mut b = desktop(2);
        // A opens a window into the Lounge, and makes an empty strip.
        let group = compose_id(A, 10);
        let tab = compose_id(A, 11);
        a.workspaces[0].strips[0].window_groups.push(WindowGroup {
            id: group,
            tabs: vec![Tab {
                id: tab,
                title: "Sketch".into(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("Canvas".into()),
                    label: "Sketch".into(),
                },
            }],
            active_tab: tab,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        });
        let made = Strip::new(StripId(compose_id(A, 12)), "Research");
        a.workspaces[0].strips.push(made.clone());
        let mut doc = automerge::AutoCommit::new();
        retain_owned_strip(&mut doc, &made, 1, A).expect("retained");
        project_owned_contents(
            &mut doc,
            &a.workspaces[0].strips,
            A,
            None,
            EmptyStrips::Skip,
        )
        .expect("projected");

        assert!(merge_room_contents(
            &mut b,
            room,
            contents_of(&doc),
            Reader::Owner(B)
        ));
        let strips = &b.workspaces[0].strips;
        assert_eq!(
            strips[0].kind(),
            StripKind::Lounge,
            "B's Lounge stays first"
        );
        assert_eq!(
            strips[0].window_groups.len(),
            4,
            "its three windows and A's"
        );
        assert!(strips[0].window_groups.iter().any(|g| g.id == group));
        assert!(
            strips
                .iter()
                .any(|s| s.id == made.id && s.window_groups.is_empty()),
            "a made strip arrives empty"
        );

        // A merging the same document keeps its own Lounge untouched.
        let before = a.workspaces[0].strips[0].window_groups.len();
        merge_room_contents(&mut a, room, contents_of(&doc), Reader::Owner(A));
        assert_eq!(a.workspaces[0].strips[0].window_groups.len(), before);
        assert_eq!(a.workspaces[0].strips[0].kind(), StripKind::Lounge);
    }

    /// Every member orders the strips the same from the same document, and
    /// merging it again changes nothing: a sync of a tree that is already
    /// right leaves it alone instead of re-sorting it for the others to
    /// re-sort back.
    #[test]
    fn the_strip_order_is_canonical_and_settles() {
        use crate::ids::{WorkspaceId, compose_id};
        use crate::projection::{EmptyStrips, Reader, contents_of, merge_room_contents};
        use crate::tree::DesktopTree;
        const A: u16 = 0x1111;
        const B: u16 = 0x2222;
        let room = RoomId::parse(ROOM).expect("room");
        let strip = |salt: u16, n: u64, name: &str| {
            let mut strip = Strip::new(StripId(compose_id(salt, n)), name);
            let id = compose_id(salt, n + 1);
            strip.window_groups.push(WindowGroup {
                id,
                tabs: vec![Tab {
                    id,
                    title: name.into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Canvas".into()),
                        label: name.into(),
                    },
                }],
                active_tab: id,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            });
            strip
        };
        let desktop = |id: u64, strips: Vec<Strip>| {
            let mut w = Workspace::new(WorkspaceId(id), "Team");
            w.room = room;
            w.strips = strips;
            let mut tree = DesktopTree {
                active: w.id,
                workspaces: vec![w],
                ..DesktopTree::default()
            };
            tree.ensure_lounges();
            tree
        };
        let mut a = desktop(1, vec![strip(A, 10, "a1"), strip(A, 20, "a2")]);
        let mut b = desktop(2, vec![strip(B, 10, "b1")]);
        let mut doc = automerge::AutoCommit::new();
        for (tree, salt) in [(&a, A), (&b, B)] {
            crate::projection::project_owned_contents(
                &mut doc,
                &tree.workspaces[0].strips,
                salt,
                None,
                EmptyStrips::Skip,
            )
            .expect("projected");
        }
        let names = |tree: &DesktopTree| -> Vec<String> {
            tree.workspaces[0]
                .strips
                .iter()
                .map(|s| s.name.clone())
                .collect()
        };
        assert!(merge_room_contents(
            &mut a,
            room,
            contents_of(&doc),
            Reader::Owner(A)
        ));
        assert!(merge_room_contents(
            &mut b,
            room,
            contents_of(&doc),
            Reader::Owner(B)
        ));
        assert_eq!(names(&a), names(&b), "one order for every member");
        assert_eq!(names(&a)[0], LOUNGE_NAME);
        for (tree, salt) in [(&mut a, A), (&mut b, B)] {
            assert!(
                !merge_room_contents(tree, room, contents_of(&doc), Reader::Owner(salt)),
                "merging the same document again changes nothing"
            );
        }
    }

    /// Nobody renames or removes the Lounge; anyone may put a window in it.
    #[test]
    fn commands_keep_the_lounge_and_admit_windows_into_it() {
        use crate::commands::{DesktopCommand, TabPlacement, apply_command, check_owned};
        let mut w = workspace(Some(ROOM));
        ensure(&mut w);
        let lounge = w.strips[0].id.raw();
        let mut mint = || Ok(1);
        let mut create = |_: &str, _: u64| Err::<(String, TileContent), String>("none".into());
        for command in [
            DesktopCommand::RenameStrip {
                strip: lounge,
                name: "Mine".into(),
            },
            DesktopCommand::RemoveStrip { strip: lounge },
        ] {
            assert!(apply_command(&mut w, &command, &mut mint, &mut create).is_err());
            assert!(check_owned(&command, |_| false).is_err());
        }
        let open = DesktopCommand::OpenTab {
            app: "so.tilo.canvas".into(),
            placement: TabPlacement::NewGroup {
                strip: lounge,
                after: Some(w.strips[0].window_groups[0].id),
            },
        };
        assert!(
            check_owned(&open, |_| false).is_ok(),
            "a window may join it"
        );
        assert_eq!(w.strips[0].kind(), StripKind::Lounge);
    }
}
