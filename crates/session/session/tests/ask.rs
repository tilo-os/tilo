//! A question the session puts to the person about themselves: it waits on
//! their own avatar in the top bar, never as a banner over their windows,
//! and their answer takes it away. A bar too narrow for its words opens it
//! as a card under the bar, where it is read and answered.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

mod common;

use std::time::Duration;

use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop_surface::{DesktopNotifications, HostAction, HostActionSource};
use tilo_session::Options;
use tilo_session::Session;
use tilo_session::platform::notice::{Notice, Notices};
use ui::accesskit::{Node, NodeId, Role, TreeUpdate};

const SETTLE: Duration = Duration::from_millis(1);
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// A bar too narrow to hold the ask's words beside the avatar.
const NARROW: i32 = 420;
const REFRESH: Duration = Duration::from_millis(16);

/// A headless session `width` by `height` at its desk, drawn once.
fn session_on(width: i32, height: i32) -> Session {
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, height)],
        Options {
            collab: false,
            ..Options::default()
        },
    );
    session.step(SETTLE).expect("a frame");
    session
}

/// A headless session at its desk, drawn once.
fn session() -> Session {
    session_on(WIDTH, HEIGHT)
}

/// The notices a test host shows, on the desk this session draws them on.
struct Told(DesktopNotifications);

impl Notices for Told {
    fn show(&self, notice: &Notice) {
        self.0.post(notice.shown());
    }

    fn withdraw(&self, _tag: &str) {}
}

/// The notices this session shows, as the person's own.
fn told(session: &Session) -> Told {
    Told(session.notifications())
}

/// Keeping who the person is here: the offer a page makes from their own avatar.
fn keep_identity() -> HostAction {
    HostAction {
        id: "keep-identity".to_owned(),
        title: "Keep with passkey".to_owned(),
        subtitle: None,
        icon: None,
        destructive: false,
        unasked: false,
        source: Some(HostActionSource::Identity),
    }
}

/// The bar as a screen reader reads it: the window's place and its tree.
fn bar(session: &Session) -> (usize, TreeUpdate) {
    session
        .a11y_windows()
        .into_iter()
        .find(|(_, window, _)| window == "Top bar")
        .map(|(place, _, tree)| (place, tree))
        .expect("the session publishes the bar its screen draws")
}

/// The button `label` in `tree` , as a screen reader finds it.
fn button(tree: &TreeUpdate, label: &str) -> (NodeId, Node) {
    tree.nodes
        .iter()
        .find(|(_, node)| {
            node.role() == Role::Button && node.label().is_some_and(|said| said.contains(label))
        })
        .cloned()
        .unwrap_or_else(|| {
            let names: Vec<&str> = tree
                .nodes
                .iter()
                .filter_map(|(_, node)| node.label())
                .collect();
            panic!("no button named {label:?}; the bar says {names:?}")
        })
}

/// Where `node` stands, in the session's own coordinates.
fn place(node: &Node) -> (f64, f64, f64, f64) {
    node.bounds()
        .map(|rect| (rect.x0, rect.y0, rect.x1, rect.y1))
        .expect("a control the person presses names where it stands")
}

/// A press at `(x, y)` on the session's screen, wherever that lands.
fn press(session: &mut Session, (x, y): (f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .expect("the pointer");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
        session.step(REFRESH).expect("a step");
    }
}

/// The person's own avatar in the bar, as a screen reader finds it: its id and where it stands.
fn avatar(tree: &TreeUpdate) -> (NodeId, (f64, f64, f64, f64)) {
    let (id, node) = button(tree, "(you)");
    let at = place(&node);
    (id, at)
}

/// The screen now, kept as `name.png` where `TL_SHELL_SHOTS` names.
fn keep(session: &mut Session, name: &str) {
    let Some(dir) = std::env::var_os("TL_SHELL_SHOTS") else {
        return;
    };
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a screenshot of the display");
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the captures' directory");
    image::save_buffer(
        dir.join(format!("{name}.png")),
        &shot.rgba,
        shot.width,
        shot.height,
        image::ColorType::Rgba8,
    )
    .expect("the capture written");
}

#[test]
fn an_ask_about_the_person_waits_on_their_avatar_until_they_answer_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let quiet = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");

    tilo_session::identity::offer(
        session.handle(),
        &told(&session),
        keep_identity(),
        "Keep your name and work?",
        "A passkey brings you back as yourself on this device.",
    );
    session.step(SETTLE).expect("a frame");

    assert!(
        session
            .accessible_labels()
            .iter()
            .any(|label| label.contains("Keep your name and work?")),
        "the ask waits on the avatar: {:?}",
        session.accessible_labels()
    );
    let asking = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");
    assert_ne!(
        asking.rgba, quiet.rgba,
        "the bar draws the ask on the person's own avatar"
    );
    assert!(
        session
            .notifications()
            .log()
            .active()
            .iter()
            .all(|notice| notice
                .actions
                .iter()
                .all(|(key, _)| key != "host:keep-identity")),
        "an ask about the person is not a banner too"
    );

    // The person answers it on their avatar: the bar's answer asks the desk exactly this.
    session.handle().answer_personal_ask();
    session.step(SETTLE).expect("a frame");
    session.step(SETTLE).expect("a frame");

    assert!(
        session.handle().personal_ask().is_none(),
        "answered, the ask goes"
    );
    assert!(
        session
            .accessible_labels()
            .iter()
            .all(|label| !label.contains("Keep your name and work?")),
        "the bar no longer asks: {:?}",
        session.accessible_labels()
    );
    let answered = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame");
    assert_ne!(
        answered.rgba, asking.rgba,
        "the bar draws no ask once it is answered"
    );
    assert!(
        session.handle().take_host_choices().is_empty(),
        "the session performed the answer rather than leaving it in the desk"
    );
}

/// A bar too narrow for the ask's words beside the avatar opens it as a card under the bar: the
/// question is read there and its answer is pressed there, where the person sees it.
#[test]
fn an_ask_that_does_not_fit_the_bar_opens_as_a_card_under_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_on(NARROW, HEIGHT);
    tilo_session::identity::offer(
        session.handle(),
        &told(&session),
        keep_identity(),
        "Keep your name and work?",
        "A passkey brings you back as yourself on this device.",
    );
    session.step(SETTLE).expect("a frame");
    session.step(REFRESH).expect("a frame");

    let (_, tree) = bar(&session);
    let (_, (_, _, avatar_right, band_bottom)) = avatar(&tree);
    let (_, answer) = button(&tree, "Keep your name and work?");
    let (x0, y0, x1, y1) = place(&answer);
    assert!(
        y0 >= band_bottom,
        "the ask's answer stands in a card under the bar, which ends at {band_bottom}, not at {y0}"
    );
    let margin = ui::widget::floating_surface::viewport_margin(&ui::theme::Theme::dark());
    assert!(
        x0 >= margin && x1 <= f64::from(NARROW) - margin && x1 >= avatar_right,
        "the answer's card keeps the floating layer's {margin}px margin from both edges of the screen \
         and hangs under the avatar, which ends at {avatar_right}: {x0}..{x1}"
    );
    keep(&mut session, "ask-narrow-420");

    press(&mut session, (f64::midpoint(x0, x1), f64::midpoint(y0, y1)));
    assert!(
        session.handle().personal_ask().is_none(),
        "the person answered the ask in the card under the bar"
    );
    assert!(
        session.handle().take_host_choices().is_empty(),
        "the session performed the answer rather than leaving it in the desk"
    );
    keep(&mut session, "ask-narrow-420-answered");
}

/// Folded on a narrow bar, the ask waits as the avatar's dot, and the avatar opens it again: under
/// the bar, where it can be answered.
#[test]
fn a_folded_ask_on_a_narrow_bar_opens_from_its_avatar() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_on(NARROW, HEIGHT);
    tilo_session::identity::offer(
        session.handle(),
        &told(&session),
        keep_identity(),
        "Keep your name and work?",
        "A passkey brings you back as yourself on this device.",
    );
    session.step(SETTLE).expect("a frame");
    assert!(
        session.handle().set_personal_ask_open(false),
        "the ask folds into a dot on the avatar"
    );
    session.step(REFRESH).expect("a frame");
    let (_, tree) = bar(&session);
    let said: Vec<String> = tree
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    assert!(
        said.iter()
            .any(|label| label.contains("Keep your name and work?")),
        "the dot says the ask waits: {said:?}"
    );
    keep(&mut session, "ask-narrow-420-folded");
    let (_, (x0, y0, x1, y1)) = avatar(&tree);
    press(&mut session, (f64::midpoint(x0, x1), f64::midpoint(y0, y1)));
    assert!(
        session.handle().personal_ask().is_some_and(|ask| ask.open),
        "the avatar opens the ask again"
    );
    session.step(REFRESH).expect("a frame");
    let (_, tree) = bar(&session);
    let (_, (_, _, _, band_bottom)) = avatar(&tree);
    let (_, answer) = button(&tree, "Keep your name and work?");
    let (_, y0, _, _) = place(&answer);
    assert!(
        y0 >= band_bottom,
        "the ask opens as a card under the bar, which ends at {band_bottom}, not at {y0}"
    );
    keep(&mut session, "ask-narrow-420-dot-opened");
}
