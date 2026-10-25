//! Another device of the same person, held in memory beside the shown one,
//! so a scenario can render what one of a person's devices receives from
//! another: the person's workspace list through the real signed directory,
//! the workspace's content as the room's host serves it.
#![allow(
    clippy::expect_used,
    reason = "a scene whose devices cannot be set up has nothing true to render"
)]

use shell::shell::personal_workspaces::{HeldRooms, PersonalWorkspaces};
use shell::shell::state::{
    AppKind, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId, WorkspaceTree,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

/// Topology namespace of the in-memory laptop.
pub const LAPTOP_SALT: u16 = 0x1A97;
/// Topology namespace the shown device writes under in these scenes.
pub const SHOWN_SALT: u16 = 0xB0A2;

pub struct OwnDevice {
    pub tree: WorkspaceTree,
    list: PersonalWorkspaces,
    key: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    pub room: RoomId,
}

fn device(
    root: &ed25519_dalek::SigningKey,
    seed: u8,
    label: &str,
) -> (ed25519_dalek::SigningKey, DeviceCert) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).expect("a scene's device is certified");
    (key, cert)
}

fn root() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x51; 32])
}

fn salted(salt: u16, counter: u64) -> u64 {
    (u64::from(salt) << shell::shell::state::PEER_ID_SALT_SHIFT) | counter
}

fn hosted(id: u64, app: &str, title: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::AppPlaceholder {
            kind: AppKind::Other(app.into()),
            label: title.into(),
        },
        app_title: None,
    }
}

/// Add the "Research" workspace as the device under `salt` creates it: one
/// strip, a group of a note and an outline, and a reading list beside it.
/// Returns its room.
pub fn seed_research(tree: &mut WorkspaceTree, salt: u16, person: &str) -> RoomId {
    let id = WorkspaceId(salted(salt, 1));
    let room = RoomId::new();
    let mut workspace = Workspace::in_room(id, room, "Research");
    workspace.set_owner(person.to_string());
    let mut strip = Strip::new(StripId(salted(salt, 2)), "Thesis");
    let draft = salted(salt, 4);
    strip.window_groups.push(WindowGroup {
        id: salted(salt, 3),
        tabs: vec![
            hosted(draft, "Notes", "Draft"),
            hosted(salted(salt, 5), "Editor", "Outline"),
        ],
        active_tab: draft,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    let reading = salted(salt, 7);
    strip.window_groups.push(WindowGroup {
        id: salted(salt, 6),
        tabs: vec![hosted(reading, "Notes", "Reading list")],
        active_tab: reading,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    workspace.strips.push(strip);
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    room
}

/// Move the "Outline" tab out of its group into a window of its own at the
/// end of the strip, as the device under `salt` does when a person drags it
/// out there.
pub fn split_outline(tree: &mut WorkspaceTree, room: RoomId, salt: u16) {
    let Some(workspace) = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|w| w.room() == room)
    else {
        return;
    };
    let Some(strip) = workspace.strips.first_mut() else {
        return;
    };
    let outline = salted(salt, 5);
    let mut moved = None;
    for group in &mut strip.window_groups {
        if let Some(at) = group.tabs.iter().position(|tab| tab.id == outline) {
            moved = Some(group.tabs.remove(at));
        }
    }
    let Some(tab) = moved else {
        return;
    };
    strip.window_groups.push(WindowGroup {
        id: salted(salt, 8),
        active_tab: tab.id,
        tabs: vec![tab],
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
}

impl OwnDevice {
    /// The person's laptop, holding the "Research" workspace.
    pub fn laptop(person: &str) -> Self {
        let (key, cert) = device(&root(), 0x1A, "Laptop");
        let mut tree = WorkspaceTree::default();
        tree.set_peer_salt(LAPTOP_SALT);
        let room = seed_research(&mut tree, LAPTOP_SALT, person);
        let list = PersonalWorkspaces::new(cert.user_public_key, cert.device_public_key);
        Self {
            tree,
            list,
            key,
            cert,
            room,
        }
    }

    /// Bring what this device has to `shown`, which keeps `shown_list` as
    /// the person's list on that device: the signed workspace list, then the
    /// room's content. Returns the ids of tabs that arrived.
    pub fn deliver(
        &mut self,
        shown: &mut WorkspaceTree,
        shown_list: &mut PersonalWorkspaces,
    ) -> Vec<u64> {
        // The laptop's own host serves the room.
        let held = HeldRooms::from([(self.room, self.cert.device_public_key)]);
        self.list.publish_local(&self.tree, &held);
        let update = self
            .list
            .snapshot(&self.key, 1)
            .expect("the laptop signs its list");
        shown_list.publish_local(shown, &HeldRooms::new());
        shown_list
            .merge(&update, &self.cert, 2)
            .expect("the same person's device");
        // The list names the laptop as the room's host; the shown device
        // enters it there, which a scene without a network stands in for.
        let (_, board) = device(&root(), 0xB0, "Orange Pi");
        for entry in shown_list.adopt_into(shown, &board.device_public_key).enter {
            let _ = shown.adopt_personal_room(entry.room, &entry.name);
        }
        let before = tab_ids(shown);
        // The room's content, as its host serves it to every member.
        let content = self
            .tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == self.room)
            .map(|workspace| workspace.strips.clone())
            .expect("the laptop is in its room");
        if let Some(workspace) = shown
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room() == self.room)
        {
            workspace.strips = content;
        }
        tab_ids(shown)
            .into_iter()
            .filter(|tab| !before.contains(tab))
            .collect()
    }
}

/// The list the shown device keeps, as the board.
pub fn board_list() -> PersonalWorkspaces {
    let (_, cert) = device(&root(), 0xB0, "Orange Pi");
    PersonalWorkspaces::new(cert.user_public_key, cert.device_public_key)
}

fn tab_ids(tree: &WorkspaceTree) -> Vec<u64> {
    tree.desktop
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.id)
        .collect()
}
