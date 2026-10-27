//! Sharing a window with a workspace where each person lays out their own
//! windows, on the production host: the window menu and the window bar's
//! menu offer it only there and only for this device's own windows,
//! choosing it asks the share and then its end, the bar marks the window
//! while the room's record holds the share, and a share whose window
//! closes is ended.

use std::cell::RefCell;
use std::rc::Rc;

use automerge::transaction::Transactable as _;
use tilo_desktop_model::projection;
use tilo_desktop_model::window_share::{window_node, window_share_id};

use super::overlay_event_absorb_tests::lay_out_host_with_theme;
use super::tab_menu::TabMenuTarget;
use super::window_share_controls::{SHARE_ITEM, STOP_ITEM};
use super::*;
use crate::shell::build_tilo_overlay_host_with_chrome;
use crate::shell::peer_sync_overlay::{PeerSyncRequest, PeerSyncResponse};
use crate::shell::state::{RoomId, Tab, TileContent, WorkspaceTree};
use crate::shell::window_menu::{WindowMenuAction, WindowMenuRow};

const SALT: u16 = 0x0A51;
const SURFACE: u64 = 0x5151;
const APP: &str = "org.alacritty.Alacritty";

struct Fixture {
    host: ShellOverlayHost,
    chrome: Chrome,
    doc: AutoCommit,
    room: RoomId,
    /// The room's document as its host keeps it.
    room_doc: AutoCommit,
    /// This device's terminal window.
    window: TabMenuTarget,
    /// A window of another device's, in the same strip.
    theirs: TabMenuTarget,
    /// Every request the network was asked, in order.
    asked: Rc<RefCell<Vec<PeerSyncRequest>>>,
}

/// The Lounge on this device: a terminal window of its own, and a window
/// another device's namespace holds.
fn fixture() -> Fixture {
    let mut tree = WorkspaceTree::bootstrap_default_with_salt(SALT);
    tree.desktop.workspaces[0].name = "Lounge".into();
    let window = crate::shell::tab_binding::adopt_toplevel(&mut tree, SURFACE, Some(APP.into()));
    let theirs_id = (0x0B52_u64 << 48) | 7;
    let (strip, group) = {
        let workspace = &mut tree.desktop.workspaces[0];
        let strip = &mut workspace.strips[0];
        let group = strip
            .window_groups
            .iter_mut()
            .find(|group| group.tabs.iter().any(|tab| tab.id == window))
            .expect("the terminal has a window");
        group.tabs.push(Tab {
            id: theirs_id,
            title: "Theirs".into(),
            tile: TileContent::Foreign {
                app_id: APP.into(),
                surface_id: 0x5252,
            },
            app_title: None,
        });
        (strip.id, group.id)
    };
    let mut ana = crate::shell::state::Collaborator::new("ana", "Ana Lima");
    ana.session = Some(601);
    tree.desktop.workspaces[0].collaborators.push(ana);
    let room = tree.desktop.workspaces[0].room();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&tree);
    let asked = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&asked);
    host.set_peer_sync_bridge(Rc::new(move |request| {
        log.borrow_mut().push(request);
        Ok(PeerSyncResponse::Message(String::new()))
    }));
    let _ = lay_out_host_with_theme(&mut host, 1280.0, 800.0, &Theme::dark());
    let _ = host.take_structure_dirty();
    let mut room_doc = AutoCommit::new();
    room_doc
        .put(automerge::ROOT, projection::ROOM_ID_KEY, room.to_string())
        .unwrap();
    room_doc.commit();
    Fixture {
        host,
        chrome,
        doc: AutoCommit::new(),
        room,
        room_doc,
        window: TabMenuTarget {
            strip,
            group,
            tab: window,
        },
        theirs: TabMenuTarget {
            strip,
            group,
            tab: theirs_id,
        },
        asked,
    }
}

impl Fixture {
    fn frame(&mut self) {
        let root = automerge::ROOT;
        self.host.update(&self.doc, &root);
        self.host.update(&self.doc, &root);
    }

    /// This device takes in the room's document as its host keeps it.
    fn take_in_room(&mut self) {
        self.room_doc.commit();
        let state = self.chrome.borrow();
        state
            .tree_mut()
            .project_room_document_for_test(self.room, &self.room_doc);
    }

    /// The room lays out each person's own windows, and this device holds
    /// the namespace of its windows.
    fn make_personal(&mut self) {
        self.room_doc
            .put(
                automerge::ROOT,
                projection::ROOM_LAYOUT_KEY,
                projection::RoomLayout::Personal.id(),
            )
            .unwrap();
        let owners = self
            .room_doc
            .put_object(
                automerge::ROOT,
                projection::ROOM_CONTENTS_OWNERS_KEY,
                automerge::ObjType::Map,
            )
            .unwrap();
        self.room_doc
            .put(&owners, format!("{SALT:04x}"), hex::encode([0x5a; 32]))
            .unwrap();
        self.take_in_room();
    }

    /// The room's host records the person's share of the window, as it
    /// writes a resource share.
    fn host_records_share(&mut self) {
        let room = self.room.to_string();
        let mut policy = projection::room_policy_of(&self.room_doc, &room);
        policy.grants.extend(
            tilo_action_policy::share::share_grants(
                &room,
                "me",
                &window_share_id(self.window.tab),
                &window_node(&room, APP, self.window.tab),
                tilo_action_policy::share::ShareAccess::View,
            )
            .expect("a window is one resource of the room"),
        );
        projection::project_room_policy(&mut self.room_doc, &policy).unwrap();
        self.take_in_room();
    }

    /// The room's host records the share's end.
    fn host_records_end(&mut self) {
        let room = self.room.to_string();
        let mut policy = projection::room_policy_of(&self.room_doc, &room);
        policy.grants.retain(|grant| {
            !tilo_action_policy::share::is_of_share(grant, &window_share_id(self.window.tab))
        });
        projection::project_room_policy(&mut self.room_doc, &policy).unwrap();
        self.take_in_room();
    }

    fn menu(&self, target: TabMenuTarget) -> Vec<WindowMenuRow> {
        self.host.window_menu_rows(target, 2, false)
    }

    fn share_row(&self, target: TabMenuTarget) -> Option<String> {
        self.menu(target).into_iter().find_map(|row| match row {
            WindowMenuRow::Entry(entry) if entry.action == WindowMenuAction::ShareWindow => {
                Some(entry.label)
            }
            _ => None,
        })
    }

    fn group(&mut self) -> &mut ui::widget::WindowGroup {
        let target = self.window;
        let index = self.chrome.borrow().tree().desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .position(|group| group.id == target.group)
            .expect("the window's group");
        self.host
            .group_at_for_tests(target.strip, index)
            .expect("the window's group is live")
    }

    /// The desktop's own items in the window's bar menu, by id.
    fn bar_items(&mut self) -> Vec<String> {
        let tab = self.window.tab;
        self.group()
            .tab_toolbar(tab)
            .map(|toolbar| {
                toolbar
                    .actions
                    .iter()
                    .filter(|action| action.id.starts_with(ui::window_slots::HOST_ACTION_PREFIX))
                    .map(|action| action.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn mark(&mut self) -> Option<ui::widget::SharedWith> {
        let tab = self.window.tab;
        self.group().tab_shared_with(tab).cloned()
    }

    fn take_asked(&self) -> Vec<(u64, bool)> {
        self.asked
            .borrow_mut()
            .drain(..)
            .filter_map(|request| match request {
                PeerSyncRequest::ShareWindow { tab, share, .. } => Some((tab, share)),
                _ => None,
            })
            .collect()
    }
}

/// A workspace whose layout shares every window offers no share; once its
/// layout is each person's own, this device's window offers "Share with
/// Lounge" in its window menu and in its bar's menu, and another device's
/// window offers neither.
#[test]
fn the_share_control_shows_only_in_a_personal_workspace_for_this_devices_windows() {
    let mut f = fixture();
    f.take_in_room();
    f.frame();
    assert_eq!(
        f.share_row(f.window),
        None,
        "a shared layout shares every window"
    );
    assert!(
        !f.bar_items().iter().any(|id| id == SHARE_ITEM),
        "nor does the bar's menu offer it"
    );

    f.make_personal();
    f.frame();
    assert_eq!(f.share_row(f.window).as_deref(), Some("Share with Lounge"));
    assert_eq!(f.bar_items(), [SHARE_ITEM]);
    assert_eq!(
        f.share_row(f.theirs),
        None,
        "another device's window is theirs to share"
    );
}

/// Choosing Share asks the room's host for the share; once the room says it
/// is shared the control reads "Stop sharing", and choosing it, in the
/// window menu or the bar's menu, asks the share's end.
#[test]
fn toggling_the_control_asks_the_share_and_then_its_end() {
    let mut f = fixture();
    f.make_personal();
    f.frame();
    let tab = f.window.tab;
    assert!(
        f.host
            .perform_tab_menu_action(f.window, WindowMenuAction::ShareWindow)
    );
    assert_eq!(f.take_asked(), [(tab, true)]);

    f.host_records_share();
    f.frame();
    assert_eq!(f.share_row(f.window).as_deref(), Some("Stop sharing"));
    assert_eq!(f.bar_items(), [STOP_ITEM]);
    assert!(
        f.host
            .perform_tab_menu_action(f.window, WindowMenuAction::ShareWindow)
    );
    assert_eq!(f.take_asked(), [(tab, false)]);

    f.host
        .share_from_bar_menus(vec![(tab, STOP_ITEM.to_owned())]);
    assert_eq!(
        f.take_asked(),
        [(tab, false)],
        "the bar's menu asks the same"
    );
}

/// The bar marks the window "Shared with Lounge", with the people there who
/// see it, exactly while the room's record holds the share.
#[test]
fn the_mark_follows_the_share() {
    let mut f = fixture();
    f.make_personal();
    f.frame();
    assert_eq!(f.mark(), None, "an unshared window is not marked");

    f.host_records_share();
    f.frame();
    assert_eq!(
        f.mark(),
        Some(ui::widget::SharedWith {
            place: "Lounge".into(),
            people: vec![(ui::permissions::UserId("ana".into()), "Ana Lima".into())],
        })
    );
    let theirs = f.theirs.tab;
    assert_eq!(f.group().tab_shared_with(theirs), None);

    f.host_records_end();
    f.frame();
    assert_eq!(f.mark(), None, "the mark goes with the share");
}

/// Closing a shared window ends its share: the same request Stop sharing
/// makes, asked once while the room's record still holds the share.
#[test]
fn closing_a_shared_window_ends_its_share() {
    let mut f = fixture();
    f.make_personal();
    f.host_records_share();
    f.frame();
    assert_eq!(f.take_asked(), [], "a live window's share stays");

    {
        let state = f.chrome.borrow();
        assert!(crate::shell::tab_binding::close_tab_for_destroyed_surface(
            &mut state.tree_mut(),
            SURFACE
        ));
    }
    f.frame();
    let tab = f.window.tab;
    assert_eq!(f.take_asked(), [(tab, false)]);
    f.frame();
    assert_eq!(f.take_asked(), [], "asked once");

    f.host_records_end();
    f.frame();
    assert_eq!(f.take_asked(), []);
}
