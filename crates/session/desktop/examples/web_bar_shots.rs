//! The browser desktop's top bar as a room shows it: the Tilo mark, the
//! workspace's name as its one menu, strips, the time, the people here, your
//! own avatar and quick settings. Captured wide and narrow, with the
//! workspace menu open, your profile card open (in a room bridged to a
//! Discord server: your account there not linked, its link code shown, and
//! linked), and the People list with two people who share a name.
//!
//! Run:  `cargo run -p tilo-desktop --example web_bar_shots -- [OUT_DIR]`
//! Out:  `OUT_DIR` (default `target/look/web-bar`):
//!       `bar-<dark|light|narrow>.png`, `workspace-menu.png`, `profile.png`,
//!       `profile-linking.png`, `profile-linked.png`, `people.png`,
//!       `people-pills.png`, `bar-unread.png`, `bar-unread-2x.png`

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "a capture run reports where it wrote and treats a failed setup as fatal"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use taffy::{Display, FlexDirection, NodeId, Size, Style, TaffyTree};
use tilo_desktop::about::{AboutLinks, AboutVariant, TiloMenu};
use tilo_desktop::chrome::{
    ChromeIntent, ClockSnapshot, DesktopChromeSnapshot, DesktopChromeSource, OwnChip,
};
use tilo_desktop::collaborators::{CollaboratorsModel, CollaboratorsOverlay};
use tilo_desktop::profile_card::{
    LinkState, LinkedAccount, ProfileAction, ProfileCard, ProfileGroup, ProfileModel,
};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop::workspace_menu::{
    WorkspaceAction, WorkspaceChoice, WorkspaceMenu, WorkspaceMenuModel,
};
use tilo_desktop_model::{Collaborator, DesktopTree, Strip, StripId, Workspace, WorkspaceId};
use ui::State;
use ui::headless::FrameHarness;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Duration;
use ui::widget::{TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

const ME: &str = "me";
const MY_NAME: &str = "Quinn Davis";

/// A page's desktop on a room: two strips, three people here (two of them
/// called Ada), and this page's own person.
struct Room {
    tree: RefCell<DesktopTree>,
    /// Unread notifications, counted on the bell.
    unread: u32,
}

impl DesktopChromeSource for Room {
    fn snapshot(&self) -> DesktopChromeSnapshot {
        DesktopChromeSnapshot {
            revision: 1,
            clock: ClockSnapshot {
                unix_seconds: 1_760_000_000,
                clock: "14:32".into(),
                weekday: "Thu".into(),
                date: "16 October".into(),
            },
            own: Some(OwnChip {
                name: MY_NAME.into(),
                rgb: (214, 140, 96),
            }),
            invite: tilo_desktop::chrome::Invite::Offered,
            notification_badge: self.unread,
            notifications_only: true,
            ..DesktopChromeSnapshot::default()
        }
    }

    fn tree(&self) -> DesktopTree {
        self.tree.borrow().clone()
    }

    fn intent(&self, _intent: ChromeIntent) -> bool {
        true
    }
}

fn tree() -> DesktopTree {
    let mut lounge = Workspace::new(WorkspaceId(1), "Lounge");
    lounge.strips = vec![
        Strip::new(StripId(10), "Main"),
        Strip::new(StripId(11), "Games"),
    ];
    lounge.collaborators = vec![
        Collaborator::new("key-a", "Ada").with_session(7),
        Collaborator::new("key-b", "ada").with_session(9),
        Collaborator::new("key-m", "Milo").with_session(11),
    ];
    let mut tree = DesktopTree {
        active: lounge.id,
        workspaces: vec![lounge],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(10));
    tree
}

/// The bar over an empty desktop, with what it opens over both, as the
/// browser shell stacks them.
struct Stage {
    bar: TopBarChrome,
    tilo: TiloMenu,
    workspace: WorkspaceMenu,
    profile: ProfileCard,
    people: CollaboratorsOverlay,
    people_open: bool,
    tree: DesktopTree,
    rect: Rect,
}

impl Widget for Stage {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let bar = self.bar.build_layout(taffy, theme, map);
        let tilo = self.tilo.build_layout(taffy, theme, map);
        let workspace = self.workspace.build_layout(taffy, theme, map);
        let profile = self.profile.build_layout(taffy, theme, map);
        let style = Style {
            display: Display::Flex,
            flex_direction: FlexDirection::Column,
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &[bar, tilo, workspace, profile], map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let inner = Point::new(self.rect.x0, self.rect.y0);
        if let [bar, tilo, workspace, profile] = taffy.children(node).unwrap_or_default()[..] {
            self.bar.apply_layout(taffy, bar, inner);
            self.tilo.apply_layout(taffy, tilo, inner);
            self.workspace.apply_layout(taffy, workspace, inner);
            self.profile.apply_layout(taffy, profile, inner);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.bar.draw(scene, transform, text, theme, presence);
        if self.people_open {
            let (rows, _) = tilo_desktop::collaborators::collaborator_roster_rows_named(
                &self.tree,
                ME,
                Some(MY_NAME),
            );
            let workspace = self
                .tree
                .active_workspace()
                .map(|workspace| workspace.name.clone())
                .unwrap_or_default();
            self.people.draw(
                scene,
                transform,
                text,
                theme,
                &CollaboratorsModel {
                    host: self.rect,
                    workspace: &workspace,
                    rows: &rows,
                    unavailable: None,
                    exposure: None,
                    may_invite: true,
                    may_pin: false,
                    followed_id: None,
                    following_pairs: &[],
                    expanded_id: None,
                    watching: 0,
                    waved: &[],
                    spectating: false,
                },
                None,
            );
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.bar, &self.tilo, &self.workspace, &self.profile]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![
            &mut self.bar,
            &mut self.tilo,
            &mut self.workspace,
            &mut self.profile,
        ]
    }

    fn name(&self) -> &'static str {
        "WebBarStage"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn stage() -> Stage {
    stage_with_unread(0)
}

fn stage_with_unread(unread: u32) -> Stage {
    let tree = tree();
    let room = Rc::new(Room {
        tree: RefCell::new(tree.clone()),
        unread,
    });
    let bar = TopBarChrome::from_tree_with_local_user(&tree, ME.into())
        .with_chrome(room)
        .with_tilo_mark();
    Stage {
        bar,
        tilo: TiloMenu::new(AboutVariant::Browser, &AboutLinks::default()),
        workspace: WorkspaceMenu::new(),
        profile: ProfileCard::new(),
        people: CollaboratorsOverlay::default(),
        people_open: false,
        tree,
        rect: Rect::ZERO,
    }
}

fn workspace_model() -> WorkspaceMenuModel {
    WorkspaceMenuModel {
        workspaces: vec![
            WorkspaceChoice {
                key: "1".into(),
                name: "Lounge".into(),
                current: true,
            },
            WorkspaceChoice {
                key: "host:home".into(),
                name: "Home".into(),
                current: false,
            },
        ],
        host: Some("tilo.coffee".into()),
        actions: vec![
            WorkspaceAction {
                id: "join-workspace".into(),
                title: "Join a workspace…".into(),
                destructive: false,
            },
            WorkspaceAction {
                id: "leave-workspace".into(),
                title: "Leave Lounge".into(),
                destructive: true,
            },
        ],
        can_rename: true,
        can_add_strip: true,
    }
}

/// The room's Discord bridge, with your account there in `state`.
fn discord(state: LinkState) -> Vec<LinkedAccount> {
    vec![LinkedAccount {
        app: "so.tilo.discord".into(),
        service: "Discord".into(),
        state,
    }]
}

fn profile_model(linked: Vec<LinkedAccount>) -> ProfileModel {
    ProfileModel {
        linked,
        name: MY_NAME.into(),
        identity: ME.into(),
        colour: Some(vello::peniko::Color::from_rgb8(214, 140, 96)),
        can_rename: true,
        ask_name: false,
        actions: vec![
            ProfileAction {
                id: "keep-identity".into(),
                title: "Keep your name and work".into(),
                group: ProfileGroup::Identity,
                icon: ui::icons::Icon::Fingerprint,
            },
            ProfileAction {
                id: "save-recovery-key".into(),
                title: "Save a recovery key".into(),
                group: ProfileGroup::Identity,
                icon: ui::icons::Icon::Download,
            },
            ProfileAction {
                id: "pair-device".into(),
                title: "Pair a new device".into(),
                group: ProfileGroup::Devices,
                icon: ui::icons::Icon::MonitorSmartphone,
            },
        ],
        ..ProfileModel::default()
    }
}

/// The cursor pills `presence` draws, over an empty window.
struct Pills(PresenceCtx, Rect);
impl Widget for Pills {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.1 = ui::widget::layout_rect(taffy, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        ui::presence::draw_remote_cursors(scene, transform, text, theme, &self.0);
    }
    fn name(&self) -> &'static str {
        "Pills"
    }
    fn get_rect(&self) -> Rect {
        self.1
    }
}

/// Cursor pills of two people called Ada, numbered as everyone sees them.
fn pills(out: &std::path::Path) {
    let mut presence = PresenceCtx::empty();
    presence.set_self_participant(ui::permissions::UserId(ME.into()), MY_NAME);
    presence.set_display_name(ui::permissions::UserId("key-a".into()), "Ada");
    presence.set_display_name(ui::permissions::UserId("key-b".into()), "ada");
    presence.upsert_remote_cursor("key-a".into(), [220.0, 120.0], None);
    presence.upsert_remote_cursor("key-b".into(), [520.0, 200.0], None);
    let mut harness = FrameHarness::for_root(
        State::new(),
        Pills(presence, Rect::ZERO),
        (720, 320),
        Theme::dark(),
    );
    harness.settle_for_capture(Duration::from_secs(1));
    let path = out.join("people-pills.png");
    harness.render_png(&path);
    println!("{}", path.display());
}

fn main() {
    ui::i18n::I18nManager::init("en-US");
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("target/look/web-bar"), PathBuf::from);
    std::fs::create_dir_all(&out).expect("output directory");
    let settle = Duration::from_secs(2);
    let shoot = |harness: &mut FrameHarness, name: &str| {
        harness.settle_for_capture(settle);
        let path = out.join(name);
        harness.render_png(&path);
        println!("{}", path.display());
    };
    for (variant, theme, viewport) in [
        ("dark", Theme::dark(), (1280, 480)),
        ("light", Theme::light(), (1280, 480)),
        ("narrow", Theme::dark(), (390, 480)),
    ] {
        let mut harness = FrameHarness::for_root(State::new(), stage(), viewport, theme);
        shoot(&mut harness, &format!("bar-{variant}.png"));
        if variant != "dark" {
            continue;
        }
        let stage = harness.root_mut::<Stage>();
        let (anchor, area) = (stage.bar.workspace_menu_rect(), stage.rect);
        stage.workspace.toggle(&workspace_model(), anchor, area);
        shoot(&mut harness, "workspace-menu.png");

        let stage = harness.root_mut::<Stage>();
        stage.workspace.close();
        let (anchor, area) = (stage.bar.own_avatar_rect(), stage.rect);
        stage
            .profile
            .toggle(&profile_model(discord(LinkState::Unlinked)), anchor, area);
        shoot(&mut harness, "profile.png");

        harness
            .root_mut::<Stage>()
            .profile
            .set_linked(discord(LinkState::Linking(
                tilo_app::linked_accounts::LinkCode {
                    code: "K7QMW2XRB4TD".into(),
                    instruction: "In Discord, type /link with this code. It works for 10 minutes."
                        .into(),
                },
            )));
        shoot(&mut harness, "profile-linking.png");

        harness
            .root_mut::<Stage>()
            .profile
            .set_linked(discord(LinkState::Linked(
                tilo_app::linked_accounts::AccountName {
                    user_name: "quinn.davis".into(),
                    display_name: Some("Quinn".into()),
                },
            )));
        shoot(&mut harness, "profile-linked.png");

        let stage = harness.root_mut::<Stage>();
        stage.profile.close();
        let _ = stage.profile.take_choice();
        stage.people_open = true;
        shoot(&mut harness, "people.png");
    }
    // The count on the bell, whole inside the bar: on a 1440x900 page, and
    // on a 2560x1440 screen at scale 2, with more than nine unread.
    for (name, unread, viewport, scale) in [
        ("bar-unread.png", 3, (1440, 900), 1),
        ("bar-unread-2x.png", 120, (1280, 720), 2),
    ] {
        ui::offscreen::set_capture_device_scale(scale);
        let mut harness = FrameHarness::for_root(
            State::new(),
            stage_with_unread(unread),
            viewport,
            Theme::dark(),
        );
        shoot(&mut harness, name);
    }
    ui::offscreen::set_capture_device_scale(1);
    pills(&out);
}
