//! Films of the shell's motions for review: every frame at 60 Hz, from the
//! input that starts a motion until it settles and two frames after, by
//! pointer and by keyboard where a shortcut exists. Each scenario starts from
//! a fresh desktop. Needs the shell's `test-support` feature (see
//! [`super::film`]).

use vello::kurbo::Point;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::film::{Film, Step, act_film, key_film};
use super::{Action, Scenario};

/// Where the pointer rests between motions: over a window, clear of the bar,
/// the ribbon and the rail.
const REST: Point = Point::new(800.0, 500.0);

/// Beside search's card and below it, over a window: a click outside it.
const OUTSIDE_SEARCH: Point = Point::new(300.0, 780.0);

/// Frames a hover-driven film records at least: the ribbon waits for the
/// pointer to dwell before it lays its strips out.
const DWELL_FRAMES: u32 = 90;

const NONE: ModifiersState = ModifiersState::empty();
const SUPER: ModifiersState = ModifiersState::SUPER;
const SUPER_CTRL: ModifiersState = ModifiersState::SUPER.union(ModifiersState::CONTROL);
const SUPER_SHIFT: ModifiersState = ModifiersState::SUPER.union(ModifiersState::SHIFT);
const CTRL_ALT: ModifiersState = ModifiersState::CONTROL.union(ModifiersState::ALT);

fn ch(c: &str) -> Key {
    Key::Character(c.into())
}

fn named(key: NamedKey) -> Key {
    Key::Named(key)
}

fn step(step: Step) -> Action {
    Action::Step(step)
}

/// The pointer at rest and the scene settled, before a film.
fn rest(actions: &mut Vec<Action>) {
    actions.extend([
        Action::MoveCursor(REST),
        Action::Wait(20),
        Action::WaitSettled,
    ]);
}

/// Settle, then film `film`.
fn then_film(actions: &mut Vec<Action>, film: Film) {
    rest(actions);
    actions.push(Action::Film(film));
}

/// Settle with the pointer already on `target` (so its hover is not in the
/// film), then film a click on it.
fn click_film(actions: &mut Vec<Action>, prefix: &'static str, target: &'static str) {
    rest(actions);
    actions.extend([
        Action::MoveCursorNamed(target),
        Action::Wait(30),
        Action::WaitSettled,
    ]);
    actions.push(Action::Film(act_film(prefix, Action::ClickNamed(target))));
}

fn key(actions: &mut Vec<Action>, prefix: &'static str, modifiers: ModifiersState, k: Key) {
    then_film(actions, key_film(prefix, modifiers, k));
}

fn scenario(name: &'static str, description: &'static str, actions: Vec<Action>) -> Scenario {
    Scenario {
        name,
        description,
        actions,
    }
}

pub(super) fn all() -> Vec<Scenario> {
    vec![
        targets(),
        quick_settings(),
        notifications(),
        windows(),
        overview_and_switching(),
        popovers(),
        tabs(),
        sheets(),
        ribbon_and_rail(),
        launcher(),
    ]
}

/// Quick Settings on a laptop with music playing: opened from the tray,
/// Wi-Fi's networks opened in place and closed, closed by a click outside;
/// then Super+Q and Escape.
fn quick_settings() -> Scenario {
    let mut a = vec![
        step(Step::LaptopQuickSettings),
        step(Step::NowPlaying),
        Action::Wait(4),
    ];
    click_film(&mut a, "01-pointer-open", "topbar_tray_pill");
    a.push(step(Step::LogTargets("quick-settings")));
    click_film(
        &mut a,
        "02-pointer-networks-open",
        "quick_settings_network_details",
    );
    click_film(
        &mut a,
        "03-pointer-networks-close",
        "quick_settings_network_details",
    );
    then_film(&mut a, act_film("04-pointer-close", Action::ClickAt(REST)));
    key(&mut a, "05-key-open", SUPER, ch("q"));
    key(&mut a, "06-key-close", NONE, named(NamedKey::Escape));
    scenario(
        "motion_review_quick_settings",
        "Quick Settings with music playing beside it: opened from the tray, Wi-Fi's networks opened in place and closed, closed by a click outside; Super+Q and Escape. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// A banner arriving, a second one arriving over it, the newest dismissed.
fn notifications() -> Scenario {
    let mut a = Vec::new();
    then_film(
        &mut a,
        Film::new(
            "01-first-arrives",
            vec![Step::Notify {
                app: "Chat",
                title: "Ada mentioned you",
                body: "Can you look at the launch notes?",
            }],
        ),
    );
    then_film(
        &mut a,
        Film::new(
            "02-second-arrives",
            vec![Step::Notify {
                app: "Files",
                title: "Lena shared Q3 plan",
                body: "You can edit it.",
            }],
        ),
    );
    then_film(
        &mut a,
        Film::new("03-newest-dismissed", vec![Step::DismissNewestNotification]),
    );
    a.push(step(Step::LogTargets("banner")));
    scenario(
        "motion_review_notifications",
        "A notification banner arriving, a second arriving, the newest dismissed. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// A window opening beside the first, maximised and restored by its button
/// and by Super+Up/Down, full screen by Super+F and back, closed by its
/// button and by Super+W.
fn windows() -> Scenario {
    let mut a = vec![Action::LoneWindow(0), Action::Wait(20)];
    then_film(
        &mut a,
        act_film(
            "01-open",
            Action::OpenWindow {
                app_id: "org.example.right",
                surface_id: 6201,
            },
        ),
    );
    a.push(step(Step::LogTargets("two-windows")));
    click_film(&mut a, "02-pointer-maximize", "window-maximize");
    click_film(&mut a, "03-pointer-restore", "window-maximize");
    key(&mut a, "04-key-maximize", SUPER, named(NamedKey::ArrowUp));
    key(&mut a, "05-key-restore", SUPER, named(NamedKey::ArrowDown));
    key(&mut a, "06-key-fullscreen", SUPER, ch("f"));
    key(&mut a, "07-key-fullscreen-exit", SUPER, ch("f"));
    click_film(&mut a, "08-pointer-close", "window-close");
    key(
        &mut a,
        "09-key-open-terminal",
        SUPER,
        named(NamedKey::Enter),
    );
    key(&mut a, "10-key-close", SUPER, ch("w"));
    scenario(
        "motion_review_windows",
        "A window opening beside another, maximised and restored by its button and by Super+Up/Down, full screen by Super+F and back, closed by its button; Super+Enter opening a terminal and Super+W closing it. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// The overview by Super+Tab and by the workspace menu; a workspace switch
/// by Super+PageDown/PageUp and by the menu; a strip switch by the ribbon's
/// wheel, by a click on a strip's name and by Ctrl+Super+Down.
fn overview_and_switching() -> Scenario {
    let mut a = vec![Action::SeedStripsWithPeople, Action::Wait(4)];
    key(&mut a, "01-key-overview-open", SUPER, named(NamedKey::Tab));
    key(&mut a, "02-key-overview-close", SUPER, named(NamedKey::Tab));
    rest(&mut a);
    a.extend([
        Action::ClickNamed("topbar_workspace_pill"),
        Action::Wait(30),
    ]);
    // "Overview", the menu's sixth row below its workspaces.
    then_film(
        &mut a,
        act_film(
            "03-pointer-overview-open",
            Action::ClickAt(Point::new(70.0, 296.0)),
        ),
    );
    key(
        &mut a,
        "04-key-overview-close",
        NONE,
        named(NamedKey::Escape),
    );
    key(
        &mut a,
        "05-key-next-workspace",
        SUPER,
        named(NamedKey::PageDown),
    );
    key(
        &mut a,
        "06-key-previous-workspace",
        SUPER,
        named(NamedKey::PageUp),
    );
    rest(&mut a);
    a.extend([
        Action::ClickNamed("topbar_workspace_pill"),
        Action::Wait(30),
    ]);
    // "Research", the menu's second workspace.
    then_film(
        &mut a,
        act_film(
            "07-pointer-workspace",
            Action::ClickAt(Point::new(70.0, 86.0)),
        ),
    );
    key(
        &mut a,
        "08-key-previous-workspace",
        SUPER,
        named(NamedKey::PageUp),
    );
    rest(&mut a);
    a.extend([
        Action::MoveCursorNamed("strip_switcher"),
        Action::WaitWall(1500),
        Action::WaitSettled,
    ]);
    a.push(Action::Film(act_film(
        "09-ribbon-wheel-next-strip",
        Action::MouseWheel { dx: 0.0, dy: -1.0 },
    )));
    a.extend([Action::WaitWall(500), Action::WaitSettled]);
    a.push(Action::Film(act_film(
        "10-ribbon-click-strip",
        Action::ClickNamed("strip_switcher.strip.3"),
    )));
    key(
        &mut a,
        "11-key-previous-strip",
        SUPER_CTRL,
        named(NamedKey::ArrowUp),
    );
    key(
        &mut a,
        "12-key-next-strip",
        SUPER_CTRL,
        named(NamedKey::ArrowDown),
    );
    scenario(
        "motion_review_overview",
        "The overview by Super+Tab and by the workspace menu; switching workspace by Super+PageDown/PageUp and by the menu; switching strip by the ribbon's wheel, a click on a strip's name and Ctrl+Super+Up/Down. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// The bar's workspace menu, profile card and people list, each opened by a
/// click and closed by Escape; the tab menu by a secondary click and by
/// Super+F10.
fn popovers() -> Scenario {
    let mut a = vec![Action::SeedPresenceRoom, Action::Wait(4)];
    let esc = || named(NamedKey::Escape);
    click_film(&mut a, "01-workspace-menu-open", "topbar_workspace_pill");
    key(&mut a, "02-workspace-menu-close", NONE, esc());
    click_film(&mut a, "03-profile-open", "topbar_profile");
    key(&mut a, "04-profile-close", NONE, esc());
    click_film(&mut a, "05-people-open", "topbar_avatar_stack");
    then_film(
        &mut a,
        act_film("06-people-close-click-outside", Action::ClickAt(REST)),
    );
    rest(&mut a);
    a.extend([
        Action::MoveCursorNamed("tab:Files"),
        Action::Wait(30),
        Action::WaitSettled,
    ]);
    a.push(Action::Film(Film::new(
        "07-tab-menu-secondary-click",
        vec![Step::RightClickNamed("tab:Files")],
    )));
    a.push(step(Step::LogTargets("tab-menu")));
    key(&mut a, "08-tab-menu-close", NONE, esc());
    key(&mut a, "09-tab-menu-key-open", SUPER, named(NamedKey::F10));
    key(&mut a, "10-tab-menu-key-close", NONE, esc());
    scenario(
        "motion_review_popovers",
        "The bar's workspace menu, profile card and people list opened by a click and closed; the tab menu by a secondary click on a tab and by Super+F10. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// A tab duplicated by Super+T and closed by Super+W; the + chip; a tab
/// split into a window of its own by Super+Shift+Enter and merged back by
/// Super+[; a tab's close button.
fn tabs() -> Scenario {
    let mut a = vec![Action::SelectGroup(Some(0)), Action::Wait(4)];
    key(&mut a, "01-key-new-tab", SUPER, ch("t"));
    key(&mut a, "02-key-close-tab", SUPER, ch("w"));
    click_film(&mut a, "03-plus-chip", "tabbar_new_tab");
    key(
        &mut a,
        "04-plus-chip-dismissed",
        NONE,
        named(NamedKey::Escape),
    );
    key(
        &mut a,
        "05-key-split-tab-out",
        SUPER_SHIFT,
        named(NamedKey::Enter),
    );
    key(&mut a, "06-key-merge-tab-left", SUPER, ch("["));
    rest(&mut a);
    a.extend([
        Action::MoveCursorNamed("tab:Files"),
        Action::Wait(30),
        Action::WaitSettled,
    ]);
    a.push(step(Step::LogTargets("tab-hovered")));
    scenario(
        "motion_review_tabs",
        "A tab duplicated by Super+T and closed by Super+W; the + chip opening search aimed at the window; a tab split out by Super+Shift+Enter and merged back by Super+[. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// The power menu by Ctrl+Alt+Delete and from Quick Settings; "Uninstall
/// Notes?" arriving and cancelled; a portal's file picker arriving and
/// cancelled.
fn sheets() -> Scenario {
    let mut a = Vec::new();
    let esc = || named(NamedKey::Escape);
    key(
        &mut a,
        "01-power-menu-key-open",
        CTRL_ALT,
        named(NamedKey::Delete),
    );
    key(&mut a, "02-power-menu-key-close", NONE, esc());
    rest(&mut a);
    a.extend([Action::ClickNamed("topbar_tray_pill"), Action::Wait(30)]);
    click_film(
        &mut a,
        "03-power-menu-from-quick-settings",
        "quick_settings_power",
    );
    then_film(
        &mut a,
        act_film("04-power-menu-click-outside", Action::ClickAt(REST)),
    );
    then_film(
        &mut a,
        Film::new("05-uninstall-arrives", vec![Step::AskUninstall]),
    );
    click_film(&mut a, "06-uninstall-cancel", "app-uninstall.secondary");
    then_film(
        &mut a,
        Film::new("07-uninstall-again", vec![Step::AskUninstall]),
    );
    key(&mut a, "08-uninstall-escape", NONE, esc());
    then_film(
        &mut a,
        act_film("09-file-picker-arrives", Action::SeedFilePicker("open")),
    );
    a.push(step(Step::LogTargets("file-picker")));
    key(&mut a, "10-file-picker-escape", NONE, esc());
    // Escape took it down: asked again, for its Cancel button.
    a.extend([Action::SeedFilePicker("open"), Action::Wait(6)]);
    rest(&mut a);
    // The picker's Cancel button.
    let cancel = Point::new(1080.0, 740.0);
    a.extend([
        Action::MoveCursor(cancel),
        Action::Wait(30),
        Action::WaitSettled,
    ]);
    a.push(Action::Film(act_film(
        "11-file-picker-cancel",
        Action::ClickAt(cancel),
    )));
    scenario(
        "motion_review_sheets",
        "The power menu by Ctrl+Alt+Delete and from Quick Settings; Uninstall Notes? arriving, cancelled and dismissed by Escape; a portal file picker arriving, Escape, and its Cancel button. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// The strip ribbon laying its strips out under a resting pointer and
/// folding when it leaves; the window rail under the pointer and a click on
/// a segment, over a strip of three windows.
fn ribbon_and_rail() -> Scenario {
    let mut a = vec![Action::SeedStripsWithPeople, Action::Wait(4)];
    then_film(
        &mut a,
        Film::new(
            "01-ribbon-hover-expand",
            vec![Step::Act(Box::new(Action::MoveCursorNamed(
                "strip_switcher",
            )))],
        )
        .at_least(DWELL_FRAMES),
    );
    a.push(Action::Film(
        Film::new(
            "02-ribbon-leave",
            vec![Step::Act(Box::new(Action::MoveCursor(REST)))],
        )
        .at_least(DWELL_FRAMES),
    ));
    rest(&mut a);
    a.extend([
        Action::LoneWindow(0),
        Action::OpenWindow {
            app_id: "org.example.second",
            surface_id: 6301,
        },
        Action::OpenWindow {
            app_id: "org.example.third",
            surface_id: 6302,
        },
        Action::SetWindowWidths(500),
        Action::WaitStripSettled(120),
    ]);
    then_film(
        &mut a,
        Film::new(
            "03-rail-hover",
            vec![Step::RailSegment {
                index: 0,
                click: false,
            }],
        )
        .at_least(30),
    );
    a.push(Action::Film(Film::new(
        "04-rail-click-first-window",
        vec![Step::RailSegment {
            index: 0,
            click: true,
        }],
    )));
    a.push(Action::Film(Film::new(
        "05-rail-click-last-window",
        vec![Step::RailSegment {
            index: 2,
            click: true,
        }],
    )));
    scenario(
        "motion_review_ribbon",
        "The strip ribbon laying its strips out under a resting pointer and folding when it leaves; the window rail under the pointer and clicks on its first and last segments. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// Search by Super+Space, a query typed, Escape clearing it and Escape
/// closing; opened again and closed by a click outside.
fn launcher() -> Scenario {
    let mut a = Vec::new();
    key(&mut a, "01-key-open", SUPER, named(NamedKey::Space));
    a.push(Action::Film(act_film("02-type-t", Action::Type("t"))));
    a.push(Action::Film(act_film("03-type-erm", Action::Type("erm"))));
    a.push(Action::Film(key_film(
        "04-escape-clears",
        NONE,
        named(NamedKey::Escape),
    )));
    a.push(Action::Film(key_film(
        "05-escape-closes",
        NONE,
        named(NamedKey::Escape),
    )));
    key(&mut a, "06-key-open-again", SUPER, named(NamedKey::Space));
    // Search's card spans the middle of the display, REST included (a click
    // there opens the row under it): the pointer goes beside the card first,
    // so the film is of the click alone.
    a.extend([
        Action::MoveCursor(OUTSIDE_SEARCH),
        Action::Wait(20),
        Action::WaitSettled,
    ]);
    a.push(Action::Film(act_film(
        "07-click-outside",
        Action::ClickAt(OUTSIDE_SEARCH),
    )));
    scenario(
        "motion_review_launcher",
        "Search opened by Super+Space, a query typed, Escape clearing it and closing it; opened again and closed by a click outside. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// Every state the films reach, with the named targets on screen in each
/// printed to stderr: the names the films click and hover.
fn targets() -> Scenario {
    let mut a = Vec::new();
    let mut state = |label: &'static str, open: Vec<Action>| {
        rest(&mut a);
        a.extend(open);
        a.extend([
            Action::Wait(40),
            step(Step::LogTargets(label)),
            Action::Snapshot(label),
            step(Step::Key(NONE, named(NamedKey::Escape))),
        ]);
    };
    state("00-desktop", vec![]);
    state(
        "01-quick-settings",
        vec![Action::ClickNamed("topbar_tray_pill")],
    );
    state(
        "02-workspace-menu",
        vec![Action::ClickNamed("topbar_workspace_pill")],
    );
    state("03-profile", vec![Action::ClickNamed("topbar_profile")]);
    state(
        "04-power-menu",
        vec![step(Step::Key(CTRL_ALT, named(NamedKey::Delete)))],
    );
    state(
        "05-tab-menu",
        vec![step(Step::Key(SUPER, named(NamedKey::F10)))],
    );
    state("06-launcher", vec![Action::OpenPalette]);
    scenario(
        "motion_review_targets",
        "The states the motion review films reach, with every named target on screen in each printed to stderr.",
        a,
    )
}
