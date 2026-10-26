//! The gallery of the standard motions (`ui::motion`): one film for each,
//! taken where the desktop runs it, every frame at 60 Hz from the input that
//! starts it until it settles. Each film is named for its motion and what
//! shows it, so a review reads the library and the desktop together.
//! Needs the shell's `test-support` feature (see [`super::film`]).
//!
//! As a video: `--mode recording --scenario motion_gallery --fps 60
//! --step-duration 0.1`, once with `TILO_SHOTS_THEME=light` and once with
//! `dark`. A film plays at the pace it ran; each other step shows for a
//! tenth of a second.
//!
//! What the desktop has none of is not here: a peer's just-typed run
//! (`Trace`) and a view scrolling to what it was asked to show
//! (`ScrollTo`) belong to apps' documents.

use vello::kurbo::Point;
use winit::keyboard::NamedKey;

use super::film::{Film, Step, act_film};
use super::motion_review::{
    NONE, REST, SUPER, ch, click_film, key, named, rest, scenario, step, then_film,
};
use super::{Action, BarTab, Scenario};

/// Frames a motion that holds before it goes is filmed for at least: the
/// volume readout stays up, then fades.
const READOUT_FRAMES: u32 = 170;

/// Frames of a motion that loops (a call's ring) or never asks for frames
/// while it holds (desk widgets going away).
const HELD_FRAMES: u32 = 120;

pub(super) fn all() -> Vec<Scenario> {
    vec![gallery()]
}

/// The browser's window with the bar's slots its app declares.
const BROWSER: BarTab = super::BAR_BROWSER;

/// The browser once the person opened a page they can share: its sidebar
/// toggle and a share button arrive in the bar.
fn browser_with_more_slots() -> ui::window_toolbar::WindowToolbar {
    let mut slots = super::browser_slots();
    slots.sidebar = Some(false);
    slots.actions.insert(
        0,
        ui::window_toolbar::ToolbarAction {
            id: "share".into(),
            icon: "upload".into(),
            name: "Share this page".into(),
            enabled: true,
            on: false,
            menu: false,
            items: Vec::new(),
            navigates: false,
            toggles: false,
            destructive: false,
            labelled: false,
        },
    );
    slots
}

fn gallery() -> Scenario {
    let mut a = vec![
        step(Step::LaptopQuickSettings),
        step(Step::NowPlaying),
        Action::Wait(4),
    ];
    top_bar_and_quick_settings(&mut a);
    banners_and_readouts(&mut a);
    windows_and_strip(&mut a);
    window_bar(&mut a);
    people_and_desk(&mut a);
    scenario(
        "motion_gallery",
        "Every standard motion where the desktop runs it, one film each: Appear, Feedback, Disclose and Extent, ControlChange and Mark, Thumb, Drill, PanelSlide, ListChange, Transient, WindowMotion, Snap, Zoom, CrossFade, Attention, Float, Follow, Breathe and Ambient. Every frame at 60 Hz (needs --features test-support).",
        a,
    )
}

/// A popover appearing and going back, a control's hover tint, and Quick
/// Settings: opening, a section opening in place, a switch, the volume's
/// thumb, and drilling into notifications and back.
fn top_bar_and_quick_settings(a: &mut Vec<Action>) {
    click_film(a, "01-appear-workspace-menu", "topbar_workspace_pill");
    key(
        a,
        "02-appear-workspace-menu-back",
        NONE,
        named(NamedKey::Escape),
    );
    then_film(
        a,
        act_film(
            "03-feedback-hover",
            Action::MoveCursorNamed("topbar_tray_pill"),
        ),
    );
    a.push(Action::Film(act_film(
        "04-feedback-hover-goes",
        Action::MoveCursor(REST),
    )));
    click_film(a, "05-appear-quick-settings", "topbar_tray_pill");
    click_film(
        a,
        "06-disclose-and-extent-networks-open",
        "quick_settings_network_details",
    );
    click_film(
        a,
        "07-disclose-and-extent-networks-close",
        "quick_settings_network_details",
    );
    click_film(
        a,
        "08-control-change-and-mark-switch-on",
        "quick_settings_volume_mute",
    );
    click_film(
        a,
        "09-control-change-and-mark-switch-off",
        "quick_settings_volume_mute",
    );
    click_film(a, "10-thumb-volume", "quick_settings_volume");
    click_film(a, "11-drill-notifications", "quick_settings_notifications");
    key(a, "12-drill-back", NONE, named(NamedKey::Escape));
    then_film(
        a,
        act_film("13-appear-quick-settings-close", Action::ClickAt(REST)),
    );
}

/// A banner sliding in from its edge, a second one joining the stack, the
/// newest dismissed, and the volume's readout coming and going on its own.
fn banners_and_readouts(a: &mut Vec<Action>) {
    then_film(
        a,
        Film::new(
            "14-panel-slide-banner-arrives",
            vec![Step::Notify {
                app: "Chat",
                title: "Ada mentioned you",
                body: "Can you look at the launch notes?",
            }],
        ),
    );
    then_film(
        a,
        Film::new(
            "15-list-change-banner-joins",
            vec![Step::Notify {
                app: "Files",
                title: "Lena shared Q3 plan",
                body: "You can edit it.",
            }],
        ),
    );
    then_film(
        a,
        Film::new(
            "16-list-change-banner-dismissed",
            vec![Step::DismissNewestNotification],
        ),
    );
    then_film(
        a,
        act_film("17-transient-volume-readout", Action::SeedOsd("volume")).at_least(READOUT_FRAMES),
    );
}

/// A window opening, maximized and restored, the strip settling on the
/// window the focus steps to, the rail's segment following it, the overview
/// zooming out and back, and a window closing.
fn windows_and_strip(a: &mut Vec<Action>) {
    a.extend([Action::LoneWindow(0), Action::Wait(20)]);
    then_film(
        a,
        act_film(
            "18-window-motion-open",
            Action::OpenWindow {
                app_id: "org.example.second",
                surface_id: 6401,
            },
        ),
    );
    key(
        a,
        "19-window-motion-maximize",
        SUPER,
        named(NamedKey::ArrowUp),
    );
    key(
        a,
        "20-window-motion-restore",
        SUPER,
        named(NamedKey::ArrowDown),
    );
    rest(a);
    a.extend([
        Action::OpenWindow {
            app_id: "org.example.third",
            surface_id: 6402,
        },
        Action::SetWindowWidths(500),
        Action::WaitStripSettled(120),
    ]);
    key(
        a,
        "21-snap-strip-previous",
        SUPER,
        named(NamedKey::ArrowLeft),
    );
    key(a, "22-snap-strip-next", SUPER, named(NamedKey::ArrowRight));
    then_film(
        a,
        Film::new(
            "23-list-change-rail",
            vec![Step::RailSegment {
                index: 0,
                click: true,
            }],
        ),
    );
    key(a, "24-zoom-overview-open", SUPER, named(NamedKey::Tab));
    key(a, "25-zoom-overview-close", SUPER, named(NamedKey::Tab));
    key(a, "26-window-motion-close", SUPER, ch("w"));
}

/// A window of two tabs: the bar's row cross-fading to the other tab's, and
/// the browser's app adding slots to its bar and taking them away.
fn window_bar(a: &mut Vec<Action>) {
    rest(a);
    a.extend([
        Action::SeedWindowBar {
            tabs: &[BROWSER, super::BAR_FILES],
            active: 0,
            width: 700,
        },
        Action::Wait(30),
        Action::Relayout,
        step(Step::LogTargets("window-bar")),
    ]);
    click_film(a, "27-cross-fade-bar-to-files", "tab:Downloads");
    click_film(a, "28-cross-fade-bar-to-browser", "tab:DuckDuckGo");
    then_film(
        a,
        act_film(
            "29-list-change-bar-slots-arrive",
            Action::DeclareBarSlots {
                surface_id: BROWSER.surface_id,
                slots: browser_with_more_slots,
            },
        ),
    );
    then_film(
        a,
        act_film(
            "30-list-change-bar-slots-leave",
            Action::DeclareBarSlots {
                surface_id: BROWSER.surface_id,
                slots: super::browser_slots,
            },
        ),
    );
}

/// People in the workspace: a wave bouncing a face, a reaction rising from
/// a pointer, a pointer gliding between the samples it is heard at, a call's
/// ring breathing, and the desk's widgets stepping away while the person
/// types.
fn people_and_desk(a: &mut Vec<Action>) {
    rest(a);
    a.extend([Action::SeedPresenceRoom, Action::Wait(4)]);
    then_film(a, act_film("31-attention-wave", Action::WaveFrom("ada")));
    then_film(
        a,
        act_film(
            "32-float-reaction",
            Action::RemoteReaction {
                who: "ben",
                emoji: "\u{1f389}",
            },
        ),
    );
    then_film(
        a,
        Film::new(
            "33-follow-peer-pointer",
            vec![Step::PeerPointer {
                who: "ada",
                at: Point::new(900.0, 420.0),
            }],
        ),
    );
    then_film(
        a,
        Film::new("34-breathe-in-a-call", Vec::new()).at_least(HELD_FRAMES),
    );
    rest(a);
    a.extend([Action::SeedDesk, Action::Wait(20)]);
    then_film(
        a,
        act_film("35-ambient-desk-steps-away", Action::DeskTyping).at_least(HELD_FRAMES),
    );
}
