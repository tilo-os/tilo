use tilo_automerge::ApplyIncoming as _;

use super::*;
use crate::ids::StripId;
use crate::width::WindowWidth;

const LAPTOP: &str = "aa";
const DESK: &str = "bb";
const PERSON: &str = "ada";

/// One of the person's devices: its own tree of the workspace and its own
/// copy of the record.
struct Device {
    key: &'static str,
    workspace: Workspace,
    doc: AutoCommit,
}

impl Device {
    fn new(key: &'static str, room: RoomId, actor: u8) -> Self {
        Self {
            key,
            workspace: Workspace::in_room(WorkspaceId::of_room(room), room, "Home"),
            doc: AutoCommit::new().with_actor(automerge::ActorId::from(vec![actor; 16])),
        }
    }

    /// A person's change here: applied to the tree, then written to the
    /// record. Legacy windows this device runs are written as hosted on it.
    fn change(&mut self, edit: impl FnOnce(&mut Workspace)) {
        edit(&mut self.workspace);
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
        write(
            &mut self.doc,
            self.workspace.room(),
            &published,
            Some(PERSON),
        )
        .unwrap();
        self.doc.commit();
    }

    /// Take in everything `other` wrote, and show it.
    fn hear(&mut self, other: &mut Self) {
        self.doc.merge_incoming(&mut other.doc).unwrap();
        fold(&self.doc, &mut self.workspace, self.key);
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

/// The laptop opens a notes window and a legacy editor on strip 1, and
/// both devices hold that.
fn two_devices() -> (Device, Device) {
    let room = RoomId::new();
    let mut laptop = Device::new(LAPTOP, room, 1);
    let mut desk = Device::new(DESK, room, 2);
    laptop.change(|w| {
        let mut main = Strip::new(StripId(1), "Main");
        main.window_groups.push(group(10, vec![notes(100)]));
        main.window_groups.push(group(11, vec![editor(101)]));
        w.strips.push(main);
        w.strips.push(Strip::new(StripId(2), "Later"));
        w.strips[1].window_groups.push(group(12, vec![notes(102)]));
    });
    desk.hear(&mut laptop);
    (laptop, desk)
}

#[test]
fn a_move_made_while_the_window_s_device_is_offline_sticks_and_applies_on_reconnect() {
    let (mut laptop, mut desk) = two_devices();
    assert_eq!(desk.places(), laptop.places());

    // The laptop is away. At the desk the person moves the laptop's editor
    // to the other strip and closes a laptop window.
    desk.change(|w| {
        move_tab(w, 101, 2, 11);
        for s in &mut w.strips {
            for g in &mut s.window_groups {
                g.tabs.retain(|t| t.id != 102);
            }
            s.window_groups.retain(|g| !g.tabs.is_empty());
        }
    });
    let at_desk = vec![(1, 10, 100), (2, 11, 101)];
    assert_eq!(desk.places(), at_desk);
    // Nothing the desk folds back while the laptop stays away undoes it.
    fold(&desk.doc.clone(), &mut desk.workspace, DESK);
    assert_eq!(desk.places(), at_desk);

    // Meanwhile the laptop, offline too, opens a window of its own.
    laptop.change(|w| w.strips[0].window_groups[0].tabs.push(notes(103)));

    // Back online: each takes in what the other did.
    desk.hear(&mut laptop);
    laptop.hear(&mut desk);
    let merged = vec![(1, 10, 100), (1, 10, 103), (2, 11, 101)];
    assert_eq!(desk.places(), merged);
    assert_eq!(laptop.places(), merged);

    // The laptop shows its own editor live where the desk put it.
    assert!(matches!(
        laptop.tab(101).tile,
        TileContent::Foreign { surface_id: 77, .. }
    ));
    // Writing again after the merge changes nothing on either device.
    for device in [&mut laptop, &mut desk] {
        let room = device.workspace.room();
        let mut published = device.workspace.clone();
        published.strips[1].window_groups[0].tabs[0].tile = TileContent::HostedWindow {
            app_id: "org.gnome.TextEditor".into(),
            label: "Text Editor".into(),
            host: LAPTOP.into(),
            host_label: String::new(),
        };
        assert!(!write(&mut device.doc, room, &published, Some(PERSON)).unwrap());
    }
}

#[test]
fn concurrent_moves_of_one_window_settle_in_one_place_on_every_device() {
    for order in [false, true] {
        let (mut laptop, mut desk) = two_devices();
        let mut phone = Device::new("cc", laptop.workspace.room(), 3);
        phone.hear(&mut laptop);

        // All offline: each moves the notes window somewhere else.
        laptop.change(|w| move_tab(w, 100, 1, 20));
        desk.change(|w| move_tab(w, 100, 2, 12));
        phone.change(|w| move_tab(w, 100, 2, 30));

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
            places.iter().filter(|(_, _, tab)| *tab == 100).count(),
            1,
            "the window is in exactly one place: {places:?}"
        );
        // The others stay where they were.
        assert!(places.contains(&(1, 11, 101)));
        assert!(places.contains(&(2, 12, 102)));
    }
}

#[test]
fn a_close_wins_over_a_concurrent_move() {
    let (mut laptop, mut desk) = two_devices();
    laptop.change(|w| move_tab(w, 102, 1, 10));
    desk.change(|w| {
        w.strips[1].window_groups.clear();
    });
    laptop.hear(&mut desk);
    desk.hear(&mut laptop);
    assert_eq!(laptop.places(), desk.places());
    assert!(!laptop.places().iter().any(|(_, _, tab)| *tab == 102));
}

#[test]
fn a_window_runs_on_its_device_and_is_shown_there_wherever_it_is_placed() {
    let (mut laptop, mut desk) = two_devices();
    // Everywhere else the laptop's editor is that window running on the
    // laptop, never a copy started there.
    assert_eq!(
        desk.tab(101).tile,
        TileContent::HostedWindow {
            app_id: "org.gnome.TextEditor".into(),
            label: "Text Editor".into(),
            host: LAPTOP.into(),
            host_label: String::new(),
        }
    );
    // A Tilo app opens its own instance everywhere.
    assert!(matches!(desk.tab(100).tile, TileContent::PeerWindow { .. }));

    // The desk puts the editor in the notes window's group on the other
    // strip; the laptop shows its live surface there.
    desk.change(|w| move_tab(w, 101, 2, 12));
    laptop.hear(&mut desk);
    assert_eq!(
        laptop.places(),
        vec![(1, 10, 100), (2, 12, 102), (2, 12, 101)]
    );
    assert_eq!(laptop.tab(101).tile, editor(101).tile);
    assert_eq!(laptop.tab(100).tile, notes(100).tile);
}

#[test]
fn what_each_device_decides_alone_survives_a_fold() {
    let (mut laptop, mut desk) = two_devices();
    desk.workspace.strips[0].window_groups[0].width = WindowWidth::Wide;
    desk.workspace.strips[0].window_groups[0].maximized = true;
    laptop.change(|w| w.strips[0].window_groups[0].tabs.push(notes(103)));
    desk.hear(&mut laptop);
    let group = &desk.workspace.strips[0].window_groups[0];
    assert_eq!(group.width, WindowWidth::Wide);
    assert!(group.maximized);
    assert_eq!(group.active_tab, 100);
    // And none of it reached the record.
    let mut doc = desk.doc.clone();
    let mut fresh = Workspace::in_room(desk.workspace.id, desk.workspace.room(), "Home");
    fold(&doc, &mut fresh, "dd");
    assert_eq!(fresh.strips[0].window_groups[0].width, WindowWidth::Normal);
    assert!(!fresh.strips[0].window_groups[0].maximized);
    assert!(
        !write(
            &mut doc,
            desk.workspace.room(),
            &desk.workspace,
            Some(PERSON)
        )
        .unwrap()
    );
}

#[test]
fn a_window_renamed_on_another_device_takes_the_name_everywhere() {
    let (mut laptop, mut desk) = two_devices();
    desk.change(|w| w.strips[0].window_groups[0].tabs[0].title = "Plans".into());
    laptop.hear(&mut desk);
    assert_eq!(laptop.tab(100).title, "Plans");
    // The laptop keeps its live instance of the renamed window.
    assert_eq!(laptop.tab(100).tile, notes(100).tile);
}
