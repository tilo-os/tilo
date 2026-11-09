//! The bar on its own: a fixed desktop for a capture, a test and the app's
//! own window, staged per scenario.

use tilo_desktop::chrome::{
    ClockSnapshot, DesktopChromeSnapshot, Invite, LinkKind, LinkStatus, OwnChip, SystemStatus,
    VolumeState,
};
use tilo_desktop::profile_card::ProfileModel;
use tilo_desktop::workspace_menu::{WorkspaceAction, WorkspaceMenuModel};
use tilo_desktop_model::{Collaborator, DesktopTree, Strip, StripId, Workspace, WorkspaceSlot};
use ui::vello::kurbo::Rect;

use crate::link::{BarFeed, BarPlace, Placement};

/// The window a scenario is drawn in.
#[must_use]
pub fn viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some("displays") => (2560, 800),
        Some("narrow") => (420, 800),
        _ => (1280, 800),
    }
}

/// The person at the fixed desktop.
const LOCAL: &str = "me-quinn";

fn tree() -> DesktopTree {
    let mut studio = Workspace::new(WorkspaceSlot(1), "Studio");
    studio.strips = vec![
        Strip::new(StripId(11), "Design"),
        Strip::new(StripId(12), "Review"),
    ];
    studio.collaborators = vec![
        Collaborator::new("ada", "Ada Lovelace"),
        Collaborator::new("grace", "Grace Hopper"),
        Collaborator::new(LOCAL, "Quinn"),
    ];
    let mut home = Workspace::new(WorkspaceSlot(2), "Home");
    home.strips = vec![Strip::new(StripId(21), "Main")];
    let mut tree = DesktopTree {
        active: studio.slot,
        workspaces: vec![studio, home],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(11));
    tree.active_strip.insert(WorkspaceSlot(2), StripId(21));
    tree
}

/// The time every capture shows.
fn clock() -> ClockSnapshot {
    ClockSnapshot {
        unix_seconds: 1_700_000_000,
        clock: "09:41".into(),
        weekday: "Tuesday".into(),
        date: "14 November".into(),
    }
}

fn status() -> SystemStatus {
    SystemStatus {
        battery_percent: Some(76),
        link: Some(LinkStatus {
            kind: LinkKind::Wifi,
            name: Some("Studio".into()),
            signal: 3,
        }),
        volume: Some(VolumeState {
            level: 0.6,
            muted: false,
        }),
        ..SystemStatus::default()
    }
}

fn workspace_menu() -> WorkspaceMenuModel {
    WorkspaceMenuModel {
        place: tilo_desktop::workspace_menu::MenuPlace::Bar,
        desk: Vec::new(),
        host: None,
        actions: vec![
            WorkspaceAction {
                id: "new-workspace".into(),
                title: "New workspace".into(),
                icon: Some(ui::icons::Icon::Plus),
                destructive: false,
            },
            WorkspaceAction {
                id: "join-workspace".into(),
                title: "Join a workspace".into(),
                icon: Some(ui::icons::Icon::Link),
                destructive: false,
            },
        ],
        can_rename: true,
        can_add_strip: true,
        can_delete: false,
    }
}

fn profile() -> ProfileModel {
    ProfileModel {
        name: "Quinn".into(),
        identity: LOCAL.into(),
        // The person wears blue, as identity colours travel: the dark
        // material's presence palette (see the profile card's swatches).
        colour: Some(
            ui::Theme::dark()
                .tokens
                .colors
                .presence
                .hue(ui::tokens::PresenceHue::Blue),
        ),
        colour_id: Some(ui::tokens::PresenceHue::Blue.id().to_owned()),
        offers_presence: true,
        ..ProfileModel::default()
    }
}

/// What the fixed desktop lends the bar in `scenario`.
#[must_use]
pub fn feed(scenario: Option<&str>) -> BarFeed {
    let tree = tree();
    let snapshot = DesktopChromeSnapshot {
        status: status(),
        notification_badge: if scenario == Some("bar") || scenario.is_none() {
            0
        } else {
            3
        },
        own: Some(OwnChip {
            name: "Quinn".into(),
            rgb: {
                let colour = ui::presence::identity_colour(LOCAL).to_rgba8();
                (colour.r, colour.g, colour.b)
            },
            identity: String::new(),
        }),
        invite: Invite::when(true),
        ..DesktopChromeSnapshot::default()
    };
    BarFeed {
        workspace_menu: workspace_menu(),
        profile: profile(),
        local_user: LOCAL.into(),
        tree,
        snapshot,
        clock: Some(clock()),
        ..BarFeed::default()
    }
}

/// Where the fixed desktop puts its bars in `scenario`, over an area of
/// `size`: one full bar, or a full and a compact one side by side.
#[must_use]
pub fn placement(scenario: Option<&str>, size: (f64, f64)) -> Placement {
    let (width, height) = size;
    if scenario == Some("displays") {
        let half = width / 2.0;
        return Placement {
            bars: vec![
                BarPlace {
                    display: 1,
                    rect: Rect::new(0.0, 0.0, half, height),
                    full: true,
                },
                BarPlace {
                    display: 2,
                    rect: Rect::new(half, 0.0, width, height),
                    full: false,
                },
            ],
            fullscreen: Vec::new(),
        };
    }
    Placement::default()
}
