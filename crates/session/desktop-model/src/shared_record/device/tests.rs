use tilo_automerge::ApplyIncoming as _;

use super::*;
use crate::ids::StripId;
use crate::width::WindowWidth;

const LAPTOP: &str = "aa";
const DESK: &str = "bb";
const PERSON: &str = "ada";

/// What the device numbered `actor` mints: ids `actor * 1000 + n`.
const fn minted(actor: u64, n: u64) -> u64 {
    actor.saturating_mul(1000).saturating_add(n)
}

/// One of the person's devices: its own tree of the workspace and its own
/// copy of the record.
struct Device {
    key: &'static str,
    actor: u64,
    workspace: Workspace,
    doc: AutoCommit,
}

impl Device {
    fn new(key: &'static str, room: RoomId, actor: u8) -> Self {
        Self {
            key,
            actor: u64::from(actor),
            workspace: Workspace::in_room(WorkspaceId::of_room(room), room, "Home"),
            doc: AutoCommit::new().with_actor(automerge::ActorId::from(vec![actor; 16])),
        }
    }

    /// The workspace as other devices are to see it: legacy windows this
    /// device runs are hosted on it.
    fn published(&self) -> Workspace {
        let mut published = self.workspace.clone();
        for tab in published
            .strips
            .iter_mut()
            .flat_map(|s| &mut s.window_groups)
            .flat_map(|g| &mut g.tabs)
        {
            if let TileContent::Foreign { app_id, .. } = &tab.tile
                && app_id.starts_with("org.")
            {
                tab.tile = TileContent::HostedWindow {
                    app_id: app_id.clone(),
                    label: tab.title.clone(),
                    host: self.key.to_owned(),
                    host_label: String::new(),
                };
            }
        }
        published
    }

    /// Write the workspace as it stands here; whether the record changed.
    fn write(&mut self) -> bool {
        let actor = self.actor;
        let mine = move |id: u64| id / 1000 == actor;
        let writer = Writer {
            person: Some(PERSON),
            mine: &mine,
            retained: &|_| false,
        };
        let published = self.published();
        let changed = write(&mut self.doc, self.workspace.room(), &published, &writer).unwrap();
        self.doc.commit();
        changed
    }

    /// A person's change here: applied to the tree, then written to the
    /// record.
    fn change(&mut self, edit: impl FnOnce(&mut Workspace)) {
        edit(&mut self.workspace);
        self.write();
    }

    /// Take in everything `other` wrote, and show it.
    fn hear(&mut self, other: &mut Self) {
        self.doc.merge_incoming(&mut other.doc).unwrap();
        let actor = self.actor;
        fold(
            &self.doc,
            &mut self.workspace,
            self.key,
            &Kept {
                mine: &|id| id / 1000 == actor,
                private: &|_| false,
            },
        );
    }

    /// Where each window is: `(strip, group, tab)`, in order.
    fn places(&self) -> Vec<(u64, u64, u64)> {
        self.workspace
            .strips
            .iter()
            .flat_map(|s| {
                s.window_groups
                    .iter()
                    .flat_map(move |g| g.tabs.iter().map(move |t| (s.id.0, g.id, t.id)))
            })
            .collect()
    }

    fn tab(&self, id: u64) -> &Tab {
        tabs_of(&self.workspace)[&id]
    }
}

fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
    WindowGroup {
        id,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

fn notes(id: u64) -> Tab {
    Tab {
        id,
        title: format!("Notes {id}"),
        tile: TileContent::Foreign {
            app_id: "so.tilo.notes".into(),
            surface_id: id,
        },
        app_title: None,
    }
}

/// A legacy window, running on the device that opened it.
fn editor(id: u64) -> Tab {
    Tab {
        id,
        title: "Text Editor".into(),
        tile: TileContent::Foreign {
            app_id: "org.gnome.TextEditor".into(),
            surface_id: 77,
        },
        app_title: None,
    }
}

/// Move window `tab` into a group `to` of strip `strip` (made when absent).
fn move_tab(workspace: &mut Workspace, tab: u64, strip: u64, to: u64) {
    let mut moved = None;
    for g in workspace
        .strips
        .iter_mut()
        .flat_map(|s| &mut s.window_groups)
    {
        if let Some(i) = g.tabs.iter().position(|t| t.id == tab) {
            moved = Some(g.tabs.remove(i));
        }
    }
    for s in &mut workspace.strips {
        s.window_groups.retain(|g| !g.tabs.is_empty());
    }
    let moved = moved.unwrap();
    let strip = workspace
        .strips
        .iter_mut()
        .find(|s| s.id.0 == strip)
        .unwrap();
    match strip.window_groups.iter_mut().find(|g| g.id == to) {
        Some(g) => g.tabs.push(moved),
        None => strip.window_groups.push(group(to, vec![moved])),
    }
}

fn close(workspace: &mut Workspace, tab: u64) {
    for s in &mut workspace.strips {
        for g in &mut s.window_groups {
            g.tabs.retain(|t| t.id != tab);
        }
        s.window_groups.retain(|g| !g.tabs.is_empty());
    }
}

const MAIN: u64 = minted(1, 1);
const LATER: u64 = minted(1, 2);
const NOTES_GROUP: u64 = minted(1, 10);
const EDITOR_GROUP: u64 = minted(1, 11);
const LATER_GROUP: u64 = minted(1, 12);
const NOTES: u64 = minted(1, 100);
const EDITOR: u64 = minted(1, 101);
const LATER_NOTES: u64 = minted(1, 102);

/// The laptop opens a notes window and a legacy editor on its main strip,
/// and notes on a second strip; both devices hold that.
fn two_devices() -> (Device, Device) {
    let room = RoomId::new();
    let mut laptop = Device::new(LAPTOP, room, 1);
    let mut desk = Device::new(DESK, room, 2);
    laptop.change(|w| {
        let mut main = Strip::new(StripId(MAIN), "Main");
        main.window_groups
            .push(group(NOTES_GROUP, vec![notes(NOTES)]));
        main.window_groups
            .push(group(EDITOR_GROUP, vec![editor(EDITOR)]));
        w.strips.push(main);
        let mut later = Strip::new(StripId(LATER), "Later");
        later
            .window_groups
            .push(group(LATER_GROUP, vec![notes(LATER_NOTES)]));
        w.strips.push(later);
    });
    desk.hear(&mut laptop);
    (laptop, desk)
}

#[test]
fn a_move_made_while_the_window_s_device_is_offline_sticks_and_applies_on_reconnect() {
    let (mut laptop, mut desk) = two_devices();
    assert_eq!(desk.places(), laptop.places());

    // The laptop is away. At the desk the person moves the laptop's editor
    // into a group of its own on the other strip, and closes a laptop window.
    let moved_to = minted(2, 1);
    desk.change(|w| {
        move_tab(w, EDITOR, LATER, moved_to);
        close(w, LATER_NOTES);
    });
    let at_desk = vec![(MAIN, NOTES_GROUP, NOTES), (LATER, moved_to, EDITOR)];
    assert_eq!(desk.places(), at_desk);
    // Nothing the desk folds back while the laptop stays away undoes it.
    fold(
        &desk.doc.clone(),
        &mut desk.workspace,
        DESK,
        &Kept {
            mine: &|id| id / 1000 == 2,
            private: &|_| false,
        },
    );
    assert_eq!(desk.places(), at_desk);

    // Meanwhile the laptop, offline too, opens a window of its own.
    let opened = minted(1, 103);
    laptop.change(|w| w.strips[0].window_groups[0].tabs.push(notes(opened)));

    // Back online: each takes in what the other did.
    desk.hear(&mut laptop);
    laptop.hear(&mut desk);
    let merged = vec![
        (MAIN, NOTES_GROUP, NOTES),
        (MAIN, NOTES_GROUP, opened),
        (LATER, moved_to, EDITOR),
    ];
    assert_eq!(desk.places(), merged);
    assert_eq!(laptop.places(), merged);

    // The laptop shows its own editor live where the desk put it.
    assert!(matches!(
        laptop.tab(EDITOR).tile,
        TileContent::Foreign { surface_id: 77, .. }
    ));
    // Writing again after the merge changes nothing on either device.
    assert!(!laptop.write());
    assert!(!desk.write());
}

#[test]
fn concurrent_moves_of_one_window_settle_in_one_place_on_every_device() {
    for order in [false, true] {
        let (mut laptop, mut desk) = two_devices();
        let mut phone = Device::new("cc", laptop.workspace.room(), 3);
        phone.hear(&mut laptop);

        // All offline: each moves the notes window somewhere else.
        laptop.change(|w| move_tab(w, NOTES, MAIN, minted(1, 20)));
        desk.change(|w| move_tab(w, NOTES, LATER, LATER_GROUP));
        phone.change(|w| move_tab(w, NOTES, LATER, minted(3, 30)));

        // They meet in different orders.
        if order {
            laptop.hear(&mut desk);
            laptop.hear(&mut phone);
            desk.hear(&mut phone);
            desk.hear(&mut laptop);
            phone.hear(&mut laptop);
        } else {
            phone.hear(&mut desk);
            phone.hear(&mut laptop);
            laptop.hear(&mut phone);
            desk.hear(&mut laptop);
        }
        let places = laptop.places();
        assert_eq!(desk.places(), places);
        assert_eq!(phone.places(), places);
        assert_eq!(
            places.iter().filter(|(_, _, tab)| *tab == NOTES).count(),
            1,
            "the window is in exactly one place: {places:?}"
        );
        // The others stay where they were.
        assert!(places.contains(&(MAIN, EDITOR_GROUP, EDITOR)));
        assert!(places.contains(&(LATER, LATER_GROUP, LATER_NOTES)));
    }
}

#[test]
fn a_close_wins_over_a_concurrent_move() {
    let (mut laptop, mut desk) = two_devices();
    laptop.change(|w| move_tab(w, LATER_NOTES, MAIN, NOTES_GROUP));
    desk.change(|w| close(w, LATER_NOTES));
    laptop.hear(&mut desk);
    desk.hear(&mut laptop);
    assert_eq!(laptop.places(), desk.places());
    assert!(
        !laptop
            .places()
            .iter()
            .any(|(_, _, tab)| *tab == LATER_NOTES)
    );
}

#[test]
fn a_window_runs_on_its_device_and_is_shown_there_wherever_it_is_placed() {
    let (mut laptop, mut desk) = two_devices();
    // Everywhere else the laptop's editor is that window running on the
    // laptop, never a copy started there.
    assert_eq!(
        desk.tab(EDITOR).tile,
        TileContent::HostedWindow {
            app_id: "org.gnome.TextEditor".into(),
            label: "Text Editor".into(),
            host: LAPTOP.into(),
            host_label: String::new(),
        }
    );
    // A Tilo app opens its own instance everywhere.
    assert!(matches!(
        desk.tab(NOTES).tile,
        TileContent::PeerWindow { .. }
    ));

    // The desk puts the editor beside the notes on the other strip; the
    // laptop shows its live surface there.
    desk.change(|w| move_tab(w, EDITOR, LATER, LATER_GROUP));
    laptop.hear(&mut desk);
    assert_eq!(
        laptop.places(),
        vec![
            (MAIN, NOTES_GROUP, NOTES),
            (LATER, LATER_GROUP, LATER_NOTES),
            (LATER, LATER_GROUP, EDITOR)
        ]
    );
    assert_eq!(laptop.tab(EDITOR).tile, editor(EDITOR).tile);
    assert_eq!(laptop.tab(NOTES).tile, notes(NOTES).tile);
}

#[test]
fn what_each_device_decides_alone_survives_a_fold() {
    let (mut laptop, mut desk) = two_devices();
    desk.workspace.strips[0].window_groups[0].width = WindowWidth::Wide;
    desk.workspace.strips[0].window_groups[0].maximized = true;
    laptop.change(|w| {
        w.strips[0].window_groups[0]
            .tabs
            .push(notes(minted(1, 103)));
    });
    desk.hear(&mut laptop);
    let group = &desk.workspace.strips[0].window_groups[0];
    assert_eq!(group.width, WindowWidth::Wide);
    assert!(group.maximized);
    assert_eq!(group.active_tab, NOTES);
    // And none of it reached the record.
    assert!(!desk.write());
    let mut fresh = Workspace::in_room(desk.workspace.id, desk.workspace.room(), "Home");
    fold(&desk.doc, &mut fresh, "dd", &Kept::NOTHING);
    assert_eq!(fresh.strips[0].window_groups[0].width, WindowWidth::Normal);
    assert!(!fresh.strips[0].window_groups[0].maximized);
}

#[test]
fn a_window_renamed_on_another_device_takes_the_name_everywhere() {
    let (mut laptop, mut desk) = two_devices();
    desk.change(|w| w.strips[0].window_groups[0].tabs[0].title = "Plans".into());
    laptop.hear(&mut desk);
    assert_eq!(laptop.tab(NOTES).title, "Plans");
    // The laptop keeps its live instance of the renamed window.
    assert_eq!(laptop.tab(NOTES).tile, notes(NOTES).tile);
}

#[test]
fn a_device_adds_only_what_it_made_and_what_it_made_is_the_person_s() {
    let (mut laptop, mut desk) = two_devices();
    // The laptop closes a window while the desk is away; the desk, not yet
    // knowing, writes again: the window stays closed, and the desk's own
    // new window is added as the person's.
    laptop.change(|w| close(w, LATER_NOTES));
    let own = minted(2, 5);
    desk.change(|w| w.strips[0].window_groups[0].tabs.push(notes(own)));
    laptop.hear(&mut desk);
    desk.hear(&mut laptop);
    assert!(!desk.places().iter().any(|(_, _, tab)| *tab == LATER_NOTES));
    assert_eq!(laptop.places(), desk.places());
    let shared = crate::shared_record::read(&desk.doc, None).unwrap();
    assert_eq!(shared.authors.get(&own).map(String::as_str), Some(PERSON));

    // A window some other device made that the record lacks is not added.
    let stranger = minted(9, 1);
    desk.change(|w| w.strips[0].window_groups[0].tabs.push(notes(stranger)));
    let shared = crate::shared_record::read(&desk.doc, None).unwrap();
    assert!(
        !shared.tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .iter()
            .any(|tab| tab.id == stranger)
    );
}

#[test]
fn withdrawing_takes_out_only_what_the_device_made() {
    let (mut laptop, mut desk) = two_devices();
    let own = minted(2, 5);
    desk.change(|w| {
        w.strips[0].window_groups[0].tabs.push(notes(own));
        w.strips.push(Strip::new(StripId(minted(2, 6)), "Desk"));
        w.strips[2]
            .window_groups
            .push(group(minted(2, 7), vec![notes(minted(2, 8))]));
    });
    assert!(withdraw(&mut desk.doc, &|id| id / 1000 == 2).unwrap());
    desk.doc.commit();
    laptop.hear(&mut desk);
    assert_eq!(
        laptop.places(),
        vec![
            (MAIN, NOTES_GROUP, NOTES),
            (MAIN, EDITOR_GROUP, EDITOR),
            (LATER, LATER_GROUP, LATER_NOTES)
        ]
    );
    assert_eq!(laptop.workspace.strips.len(), 2);
}

#[test]
fn a_room_laid_out_before_the_record_brings_its_windows_over() {
    use automerge::transaction::Transactable as _;
    let room = RoomId::new();
    let mut desk = Device::new(DESK, room, 2);
    // The laptop's windows, as the old layout kept them.
    let contents = desk.doc.put_object(ROOT, "contents", ObjType::Map).unwrap();
    let strip = desk
        .doc
        .put_object(&contents, format!("{MAIN:016x}"), ObjType::Map)
        .unwrap();
    let groups = desk.doc.put_object(&strip, "groups", ObjType::Map).unwrap();
    let group_entry = desk
        .doc
        .put_object(&groups, format!("{NOTES_GROUP:016x}"), ObjType::Map)
        .unwrap();
    let tabs = desk
        .doc
        .put_object(&group_entry, "tabs", ObjType::Map)
        .unwrap();
    desk.doc
        .put_object(&tabs, format!("{NOTES:016x}"), ObjType::Map)
        .unwrap();
    desk.doc.commit();
    // The desk's tree holds them, folded in before the record existed.
    let mut main = Strip::new(StripId(MAIN), "Main");
    main.window_groups
        .push(group(NOTES_GROUP, vec![notes(NOTES)]));
    desk.workspace.strips.push(main);
    assert!(desk.write());
    let shared = crate::shared_record::read(&desk.doc, None).unwrap();
    assert_eq!(
        shared.tree.workspaces[0].strips[0].window_groups[0].tabs[0].id,
        NOTES
    );
    // Not as the desk's: nobody's until the room says otherwise.
    assert!(!shared.authors.contains_key(&NOTES));
}
