//! A fixed desktop to show: what the overview pictures in a capture, a test
//! and its own window, where no shell lends it the real one.
//!
//! Four workspaces: the one you are in with two strips (one window of
//! three tabs, people in two of its windows, something shared out of it),
//! a shared one with someone in it, a site's workspace that can be left,
//! and an empty one.

use tilo_action_policy::Role;
use tilo_desktop::overview::{
    OverviewGroup, OverviewStrip, OverviewTab, RoomCard, RoomPerson, RoomShare,
};
use tilo_desktop::overview_panel::OverviewDesk;
use tilo_desktop::window_switcher::WindowApp;
use tilo_desktop_model::{StripId, WorkspaceId};
use ui::icons::Icon;
use ui::widget::AppKind;

/// The workspace you are in.
pub const HERE: WorkspaceId = WorkspaceId(1);
/// Its strips, top to bottom.
pub const WORK: StripId = StripId(11);
pub const READING: StripId = StripId(12);

fn app(key: &str, kind: AppKind, glyph: Icon) -> WindowApp {
    WindowApp {
        key: key.to_owned(),
        kind,
        icon: None,
        glyph,
    }
}

fn person(id: &str, name: &str, present: bool, is_you: bool) -> RoomPerson {
    RoomPerson {
        id: id.to_owned(),
        name: name.to_owned(),
        role: if is_you { Role::Owner } else { Role::Editor },
        present,
        is_you,
    }
}

fn tab(id: u64, title: &str, app: WindowApp, active: bool) -> OverviewTab {
    OverviewTab {
        id,
        title: title.to_owned(),
        is_active: active,
        app,
        people: Vec::new(),
    }
}

fn group(id: u64, width_permille: u32, tabs: Vec<OverviewTab>) -> OverviewGroup {
    OverviewGroup {
        id,
        width_permille,
        is_current: false,
        tabs,
    }
}

fn notes() -> WindowApp {
    app("so.tilo.notes", AppKind::Editor, Icon::FileText)
}

fn terminal() -> WindowApp {
    app("so.tilo.terminal", AppKind::Terminal, Icon::Terminal)
}

fn browser() -> WindowApp {
    app("org.mozilla.firefox", AppKind::Browser, Icon::Globe)
}

fn calendar() -> WindowApp {
    app(
        "so.tilo.calendar",
        AppKind::App(Icon::Calendar),
        Icon::Calendar,
    )
}

fn chat() -> WindowApp {
    app(
        "so.tilo.chat",
        AppKind::App(Icon::MessageSquare),
        Icon::MessageSquare,
    )
}

/// A workspace card with one strip, "Main", holding `groups`.
fn single_strip(
    id: u64,
    name: &str,
    people: Vec<RoomPerson>,
    strip: u64,
    groups: Vec<OverviewGroup>,
) -> RoomCard {
    RoomCard {
        id: WorkspaceId(id),
        name: name.to_owned(),
        is_current: false,
        people,
        shares: Vec::new(),
        strips: vec![OverviewStrip {
            id: StripId(strip),
            name: "Main".to_owned(),
            is_current: false,
            groups,
        }],
    }
}

/// The workspace this desktop is on: writing beside reading, shared.
fn launch(maya: RoomPerson, theo: RoomPerson, you: RoomPerson) -> RoomCard {
    let mut review = tab(101, "Design review.md", notes(), true);
    review.people = vec![maya.clone()];
    let mut writing = group(
        1001,
        600,
        vec![
            review,
            tab(102, "Roadmap 2027.md", notes(), false),
            tab(103, "Q3 notes.md", notes(), false),
        ],
    );
    writing.is_current = true;
    let work_groups = vec![
        writing,
        group(1002, 400, vec![tab(104, "~/src/tilo", terminal(), true)]),
    ];
    let reading_groups = vec![
        group(
            1003,
            500,
            vec![tab(105, "Tilo release notes", browser(), true)],
        ),
        group(1004, 500, vec![tab(106, "Week 44", calendar(), true)]),
    ];
    RoomCard {
        id: HERE,
        name: "Launch".to_owned(),
        is_current: true,
        people: vec![you, maya, theo],
        shares: vec![RoomShare {
            what: "Press kit".to_owned(),
            with: vec!["Ada".to_owned(), "Bo".to_owned()],
            writable: true,
        }],
        strips: vec![
            OverviewStrip {
                id: WORK,
                name: "Writing".to_owned(),
                is_current: true,
                groups: work_groups,
            },
            OverviewStrip {
                id: READING,
                name: "Reading".to_owned(),
                is_current: false,
                groups: reading_groups,
            },
        ],
    }
}

/// The desktop the overview shows on its own.
pub fn desk() -> OverviewDesk {
    let maya = person("maya", "Maya", true, false);
    let theo = person("theo", "Theo", false, false);
    let you = person("you", "You", true, true);
    let ada = || person("ada", "Ada", true, false);

    let here = launch(maya, theo, you.clone());
    let mut standup = tab(201, "Team chat", chat(), true);
    standup.people = vec![ada()];
    let research = single_strip(
        2,
        "Research",
        vec![you.clone(), ada()],
        21,
        vec![group(2001, 1000, vec![standup])],
    );
    let community = single_strip(
        3,
        "Typography club",
        Vec::new(),
        31,
        vec![group(
            3001,
            1000,
            vec![tab(301, "Kerning", browser(), true)],
        )],
    );
    let empty = single_strip(4, "Taxes", vec![you], 41, Vec::new());
    let mut desk = OverviewDesk::of(vec![here, research, community, empty]);
    desk.renamable = [WORK, READING].into_iter().collect();
    desk.colors = [
        ("maya".to_owned(), (232, 121, 80)),
        ("ada".to_owned(), (94, 160, 230)),
        ("theo".to_owned(), (150, 110, 220)),
    ]
    .into_iter()
    .collect();
    desk.sites = [(WorkspaceId(3), "typography.example community".to_owned())]
        .into_iter()
        .collect();
    desk.leavable = [WorkspaceId(3)].into_iter().collect();
    desk.can_join = true;
    desk
}

/// The desktop with `count` workspaces more than fit: a list that scrolls.
pub fn crowded(count: u64) -> OverviewDesk {
    let mut desk = desk();
    desk.cards
        .extend((10..count.saturating_add(10)).map(|id| RoomCard {
            id: WorkspaceId(id),
            name: format!("Project {id}"),
            is_current: false,
            people: Vec::new(),
            shares: Vec::new(),
            strips: Vec::new(),
        }));
    desk
}
