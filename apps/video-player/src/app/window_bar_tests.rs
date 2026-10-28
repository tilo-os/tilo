//! The player's controls live in the window's bar: the library's toggle,
//! the open video's title, Open and History. Nothing of them is drawn over
//! the picture.

use std::path::PathBuf;

use ui::framework::testing::TestApp;
use ui::portal::PortalOutcome;
use ui::window_toolbar::{ToolbarEvent, WindowToolbar};

use super::OPEN;
use crate::actions::VideoEffect;
use crate::app::PlayerApp;
use crate::resource::VideoPlayerShell;

fn toolbar(app: &mut TestApp<VideoPlayerShell>) -> WindowToolbar {
    app.surface_mut::<PlayerApp>().window_toolbar()
}

fn press(app: &mut TestApp<VideoPlayerShell>, event: ToolbarEvent) {
    ui::window_toolbar::deliver(event);
    app.settle();
}

/// Every name and action target in `node`'s subtree.
fn labels(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.name.clone());
    out.extend(node.action_target.clone());
    for child in &node.children {
        labels(child, out);
    }
}

#[test]
fn the_player_draws_no_strip_and_its_controls_are_in_the_window_bar() {
    let mut app = TestApp::<VideoPlayerShell>::capture(Some("paused")).expect("the clip opens");
    let bar = toolbar(&mut app);
    assert_eq!(bar.sidebar, Some(false), "the library is the bar's sidebar");
    assert_eq!(
        bar.location.map(|location| location.label).as_deref(),
        Some("Demo clip"),
        "the bar names the open video"
    );
    let open = bar
        .actions
        .iter()
        .find(|action| action.id == OPEN)
        .expect("Open is a bar action");
    assert!(open.enabled);
    assert_eq!(open.name, "Open video");

    let mut drawn = Vec::new();
    labels(
        &ui::Widget::accessibility(app.surface_mut::<PlayerApp>()).expect("the player's semantics"),
        &mut drawn,
    );
    for own in [
        "Open video",
        OPEN,
        "video-player.library",
        "Show queue and recent videos",
        "Hide library",
        ui::widget::HISTORY_CONTROL_ID,
    ] {
        assert!(
            !drawn.iter().any(|label| label == own),
            "the player draws no {own} of its own: {drawn:?}"
        );
    }
}

#[test]
fn with_nothing_open_the_bar_names_no_video_and_still_opens_one() {
    let mut app = TestApp::<VideoPlayerShell>::new().expect("the player starts");
    let bar = toolbar(&mut app);
    assert_eq!(bar.location, None, "nothing is open to name");
    assert!(bar.actions.iter().any(|action| action.id == OPEN));
}

#[test]
fn the_bars_sidebar_shows_and_hides_the_library_as_its_actions_do() {
    let mut app = TestApp::<VideoPlayerShell>::new().expect("the player starts");
    press(&mut app, ToolbarEvent::ToggleSidebar);
    assert!(app.surface_mut::<PlayerApp>().library_visible());
    assert_eq!(toolbar(&mut app).sidebar, Some(true));
    press(&mut app, ToolbarEvent::ToggleSidebar);
    assert!(!app.surface_mut::<PlayerApp>().library_visible());
    assert_eq!(toolbar(&mut app).sidebar, Some(false));

    app.effect(&VideoEffect::ShowLibrary {
        tab: "recent".into(),
    })
    .expect("an agent shows the library");
    assert_eq!(
        toolbar(&mut app).sidebar,
        Some(true),
        "the bar follows the published action"
    );
    app.effect(&VideoEffect::HideLibrary)
        .expect("an agent hides the library");
    assert_eq!(toolbar(&mut app).sidebar, Some(false));
}

#[test]
fn the_bars_open_runs_the_published_open_and_names_the_chosen_video() {
    let mut app = TestApp::<VideoPlayerShell>::new().expect("the player starts");
    let (services, recorder) = ui::host::HostServices::recording();
    recorder.respond_to_pick(PortalOutcome::Opened(PathBuf::from("/tmp/Chosen.webm")));
    ui::host::scoped(&services, || {
        press(
            &mut app,
            ToolbarEvent::Action {
                id: OPEN.to_owned(),
            },
        );
        app.settle();
    });
    assert_eq!(recorder.pick_requests().len(), 1, "Open asks for one video");
    assert_eq!(
        toolbar(&mut app)
            .location
            .map(|location| location.label)
            .as_deref(),
        Some("Chosen.webm"),
        "the bar names the chosen video"
    );
}

#[test]
fn history_is_the_window_bars_control() {
    let mut app = TestApp::<VideoPlayerShell>::capture(Some("paused")).expect("the clip opens");
    app.harness().present_in_window_bar();
    app.harness().relayout();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Closed),
        "the window's bar offers History"
    );
    app.harness().press_window_history().expect("History opens");
    app.settle();
    assert_eq!(
        app.harness().window_history_control(),
        Some(ui::widget::HistoryControl::Open)
    );
}
