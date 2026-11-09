use tilo_automerge::ApplyIncoming as _;

use std::collections::BTreeSet;

use super::*;
use crate::arrangement::{FloatCorner, FloatPlace};
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
    /// The record each window this device runs shows, as its session notes
    /// them.
    notes: BTreeMap<u64, String>,
}

impl Device {
    fn new(key: &'static str, identity: WorkspaceId, actor: u8) -> Self {
        Self {
            key,
            actor: u64::from(actor),
            workspace: Workspace::in_workspace(
                WorkspaceSlot::of_workspace(identity),
                identity,
                "Home",
            ),
            doc: AutoCommit::new().with_actor(automerge::ActorId::from(vec![actor; 16])),
            notes: BTreeMap::new(),
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
        let former = former_ids(&self.doc);
        let notes = self.notes.clone();
        let records = move |id: u64| notes.get(&id).cloned();
        let writer = Writer {
            person: Some(PERSON),
            mine: &mine,
            retained: &|_| false,
            carried: &|id| former.contains(&id),
            records: &records,
        };
        let published = self.published();
        let changed = write(&mut self.doc, self.workspace.id(), &published, &writer).unwrap();
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
                closed: &|_| false,
                writing: false,
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
        view: String::new(),
        pinned: false,
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
        view: String::new(),
        pinned: false,
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
    let identity = WorkspaceId::new();
    let mut laptop = Device::new(LAPTOP, identity, 1);
    let mut desk = Device::new(DESK, identity, 2);
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
            closed: &|_| false,
            writing: false,
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
fn a_window_a_device_closed_is_not_folded_back_from_the_record() {
    let (mut laptop, mut desk) = two_devices();
    // The record as the laptop last wrote it, before the close: it holds the notes window.
    let before_the_close = laptop.doc.clone();
    // The laptop closes its notes window.
    laptop.change(|w| close(w, NOTES));
    desk.hear(&mut laptop);
    assert!(
        !desk.places().iter().any(|(_, _, tab)| *tab == NOTES),
        "the window went on the desk that heard the close"
    );
    // The record still held the window when this device's last write was made, so folding it into
    // the desk lays the window back: a slot nothing fills.
    let mut laid_back = laptop.workspace.clone();
    fold(&before_the_close, &mut laid_back, LAPTOP, &Kept::NOTHING);
    assert!(
        laid_back.strips.iter().any(|strip| strip
            .window_groups
            .iter()
            .any(|group| group.tabs.iter().any(|tab| tab.id == NOTES))),
        "fixture: the record holds the window this device closed"
    );
    // Told what this device closed, the fold leaves it out, so the write that follows takes the
    // window's entry out of the record instead of writing it again.
    let closed: BTreeSet<u64> = [NOTES].into();
    let mut laid_out = laptop.workspace.clone();
    fold(
        &before_the_close,
        &mut laid_out,
        LAPTOP,
        &Kept {
            mine: &|id| id / 1000 == 1,
            private: &|_| false,
            closed: &|id| closed.contains(&id),
            writing: true,
        },
    );
    assert!(
        !laid_out.strips.iter().any(|strip| strip
            .window_groups
            .iter()
            .any(|group| group.tabs.iter().any(|tab| tab.id == NOTES))),
        "a window this device closed is not laid back into its desk"
    );
    assert_eq!(
        laid_out
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .count(),
        laptop
            .workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .count(),
        "the windows the desk does hold are still there"
    );
}

#[test]
fn concurrent_moves_of_one_window_settle_in_one_place_on_every_device() {
    for order in [false, true] {
        let (mut laptop, mut desk) = two_devices();
        let mut phone = Device::new("cc", laptop.workspace.id(), 3);
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
    let mut fresh = Workspace::in_workspace(desk.workspace.slot, desk.workspace.id(), "Home");
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
fn a_workspace_laid_out_before_the_record_brings_its_windows_over() {
    use automerge::transaction::Transactable as _;
    let identity = WorkspaceId::new();
    let mut desk = Device::new(DESK, identity, 2);
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
    // Not as the desk's: nobody's until the workspace says otherwise.
    assert!(!shared.authors.contains_key(&NOTES));
}

/// What the record holds of which record each window shows.
fn noted(doc: &AutoCommit) -> Vec<(u64, String)> {
    crate::shared_record::read(doc, None)
        .expect("the record")
        .records
}

/// A notes window's record: the workspace's one notebook.
const NOTEBOOK: &str = "derived:default@tab:0000000000000000";

#[test]
fn a_window_notes_the_record_it_shows_on_every_device() {
    let (mut laptop, mut desk) = two_devices();
    assert!(noted(&laptop.doc).is_empty(), "nothing noted yet");

    // The notes window on the laptop shows the workspace's notebook.
    laptop.notes.insert(NOTES, NOTEBOOK.into());
    assert!(laptop.write());
    assert_eq!(noted(&laptop.doc), vec![(NOTES, NOTEBOOK.to_owned())]);
    desk.hear(&mut laptop);
    assert_eq!(
        noted(&desk.doc),
        vec![(NOTES, NOTEBOOK.to_owned())],
        "the desk knows which record the laptop's window is on"
    );

    // Writing the same note again changes nothing.
    assert!(!laptop.write());
    assert_eq!(noted(&laptop.doc), vec![(NOTES, NOTEBOOK.to_owned())]);

    // The desk lays the window out elsewhere, noting nothing of its own:
    // what the laptop noted stays.
    desk.change(|w| move_tab(w, NOTES, LATER, LATER_GROUP));
    assert_eq!(noted(&desk.doc), vec![(NOTES, NOTEBOOK.to_owned())]);
    laptop.hear(&mut desk);
    assert_eq!(noted(&laptop.doc), vec![(NOTES, NOTEBOOK.to_owned())]);
}

#[test]
fn a_window_that_closes_takes_its_note_out_of_the_record() {
    let (mut laptop, mut desk) = two_devices();
    laptop.notes.insert(NOTES, NOTEBOOK.into());
    laptop.write();
    desk.hear(&mut laptop);

    desk.change(|w| close(w, NOTES));
    assert!(noted(&desk.doc).is_empty(), "no window, no note");
    laptop.hear(&mut desk);
    assert!(noted(&laptop.doc).is_empty());
}

/// What the record holds of where a window floats.
fn float_of(doc: &AutoCommit, group: u64) -> Option<FloatPlace> {
    crate::shared_record::read(doc, None)
        .expect("the record")
        .tree
        .workspaces
        .iter()
        .find_map(|workspace| workspace.arrangement.float_of(group))
}

/// The desk as `kept` folds `doc` into it.
fn folded(device: &Device, doc: &AutoCommit, kept: &Kept<'_>) -> Workspace {
    let mut desk = device.workspace.clone();
    fold(doc, &mut desk, device.key, kept);
    desk
}

#[test]
fn a_float_is_the_record_s_so_every_device_sees_it_and_snapping_back_clears_it() {
    let (mut laptop, mut desk) = two_devices();
    let float = FloatPlace::new(FloatCorner::TopRight, 40, 24, 640, 480);

    // A window the laptop floats is written floating, and the desk hears it
    // wherever it shows that window.
    laptop.change(|w| {
        assert!(w.arrangement.set_float(NOTES_GROUP, Some(float)));
    });
    assert_eq!(float_of(&laptop.doc, NOTES_GROUP), Some(float));
    desk.hear(&mut laptop);
    assert_eq!(float_of(&desk.doc, NOTES_GROUP), Some(float));
    assert_eq!(
        desk.workspace.arrangement.float_of(NOTES_GROUP),
        Some(float)
    );

    // Snapped back at the desk: the write says so, and the laptop puts it
    // back in its strip, as a float the record says nothing about is the
    // record's to drop.
    desk.change(|w| {
        assert!(w.arrangement.set_float(NOTES_GROUP, None));
    });
    assert_eq!(float_of(&desk.doc, NOTES_GROUP), None);
    laptop.hear(&mut desk);
    assert_eq!(laptop.workspace.arrangement.float_of(NOTES_GROUP), None);
    assert_eq!(float_of(&laptop.doc, NOTES_GROUP), None);
}

#[test]
fn the_fold_a_write_makes_lays_down_the_floats_of_the_device_writing() {
    let (mut laptop, mut desk) = two_devices();
    let float = FloatPlace::new(FloatCorner::BottomLeft, 8, 16, 320, 200);
    laptop.change(|w| {
        w.arrangement.set_float(NOTES_GROUP, Some(float));
    });
    desk.hear(&mut laptop);

    // The laptop floats a window it had in its strip: the fold its write
    // makes lays the float down, so the write records it, and the desk hears.
    laptop.workspace.arrangement.set_float(NOTES_GROUP, None);
    let floated = FloatPlace::new(FloatCorner::TopLeft, 12, 6, 500, 400);
    laptop.change(|w| {
        w.arrangement.set_float(EDITOR_GROUP, Some(floated));
    });
    let writing = folded(
        &laptop,
        &laptop.doc,
        &Kept {
            mine: &|_| true,
            private: &|_| true,
            closed: &|_| false,
            writing: true,
        },
    );
    assert_eq!(writing.arrangement.float_of(EDITOR_GROUP), Some(floated));
    assert_eq!(float_of(&laptop.doc, EDITOR_GROUP), Some(floated));
    desk.hear(&mut laptop);
    assert_eq!(
        desk.workspace.arrangement.float_of(EDITOR_GROUP),
        Some(floated)
    );

    // A viewer folds what the record says: a float it can not account for is
    // the record's to drop.
    desk.workspace.arrangement.set_float(EDITOR_GROUP, None);
    let reading = folded(&desk, &laptop.doc, &Kept::NOTHING);
    assert_eq!(reading.arrangement.float_of(EDITOR_GROUP), Some(floated));
    laptop.doc.rollback();
    laptop.change(|w| {
        w.arrangement.set_float(EDITOR_GROUP, None);
    });
    let reading = folded(&desk, &laptop.doc, &Kept::NOTHING);
    assert_eq!(reading.arrangement.float_of(EDITOR_GROUP), None);
}

#[test]
fn a_window_the_record_does_not_hold_yet_floats_where_the_device_showing_it_put_it() {
    let (mut laptop, _) = two_devices();
    let opened = minted(1, 103);
    let opened_group = minted(1, 13);
    let float = FloatPlace::new(FloatCorner::BottomLeft, 8, 16, 320, 200);
    laptop
        .workspace
        .strips
        .get_mut(0)
        .expect("the main strip")
        .window_groups
        .push(group(opened_group, vec![notes(opened)]));
    assert!(
        laptop
            .workspace
            .arrangement
            .set_float(opened_group, Some(float))
    );

    // A device that keeps the window apart from the record — one it had open
    // before it joined — keeps the float that came with it.
    let mine = Kept {
        mine: &|_| false,
        private: &|id| id == opened,
        closed: &|_| false,
        writing: false,
    };
    let shown = folded(&laptop, &laptop.doc.clone(), &mine);
    let kept = shown
        .strips
        .iter()
        .flat_map(|strip| &strip.window_groups)
        .find(|group| group.id == opened_group)
        .expect("the window the laptop opened stays");
    assert!(kept.tabs.iter().any(|tab| tab.id == opened));
    assert_eq!(shown.arrangement.float_of(opened_group), Some(float));
}
