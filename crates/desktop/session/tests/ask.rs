//! A question the session puts to the person about themselves: it waits on
//! their own avatar in the top bar, never as a banner over their windows,
//! and their answer takes it away. A bar too narrow for its words opens it
//! as its card under the avatar, where it is read and answered, and folded it
//! waits as a dot on the avatar while the avatar itself opens the person's own
//! card.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::time::Duration;

use tilo_compositor::{InputEvent, OutputConfig};
use tilo_session::Options;
use tilo_session::Session;
use tilo_session::platform::notice::{Notice, Notices};
use tilo_session_core::desktop::state::{DesktopNotifications, HostAction, HostActionSource};
use ui::a11y::Action;
use ui::accesskit::{Node, NodeId, Role, TreeUpdate};

const SETTLE: Duration = Duration::from_millis(1);
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// A bar too narrow to hold the ask's words beside the avatar.
const NARROW: i32 = 420;
const REFRESH: Duration = Duration::from_millis(16);
/// The window the shell draws this device's screen as, as the shell names it.
const SHELL_WINDOW: &str = "Desktop shell";

/// A headless session `width` by `height` at its shell, drawn once.
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

/// A headless session at its shell, drawn once.
fn session() -> Session {
    session_on(WIDTH, HEIGHT)
}

/// The notices a test host shows, on the shell this session draws them on.
struct Told(DesktopNotifications);

impl Notices for Told {
    fn show(&self, notice: &Notice) {
        self.0
            .post(tilo_session_core::desktop::notifications::Notification::of_notice(notice));
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
        label: None,
        subtitle: None,
        icon: None,
        destructive: false,
        unasked: false,
        source: Some(HostActionSource::Identity),
    }
}

/// The shell's window as a screen reader reads it: the place it publishes and its tree. The bar is
/// inside it, so the test waits for the shell to draw rather than reading a window of its own.
fn bar(session: &mut Session) -> (usize, TreeUpdate) {
    crate::common::until(session, "the shell's window", |session| {
        session
            .a11y_windows()
            .iter()
            .any(|(_, window, _)| window == SHELL_WINDOW)
    });
    session
        .a11y_windows()
        .into_iter()
        .find(|(_, window, _)| window == SHELL_WINDOW)
        .map(|(place, _, tree)| (place, tree))
        .expect("the session publishes the shell its screen draws")
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

/// The person's own avatar in the bar, as a screen reader finds it: its id and where it stands. The
/// shell names it by what it opens: "You: open your card", or their name before it.
fn avatar(tree: &TreeUpdate) -> (NodeId, (f64, f64, f64, f64)) {
    let (id, node) = button(tree, "open your card");
    let at = place(&node);
    (id, at)
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

    // The person answers it on their avatar: the bar's answer asks the shell exactly this.
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
        "the session performed the answer rather than leaving it in the shell"
    );
}

/// A bar too narrow for the ask's words beside its own controls opens it as its card under the
/// avatar: the question is read there and its answer is pressed there, where the person sees it,
/// clear of the person's own face.
#[test]
fn an_ask_that_does_not_fit_the_bar_opens_clear_of_the_person_face() {
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

    let (_, tree) = bar(&mut session);
    assert!(
        tree.nodes.iter().any(|(_, node)| {
            [node.label(), node.description()]
                .into_iter()
                .flatten()
                .any(|said| said.contains("Keep your name and work?"))
        }),
        "the question is read in the shell's window"
    );
    let (_, (face_x0, face_y0, face_x1, face_y1)) = avatar(&tree);
    let (_, answer) = button(&tree, "Keep with passkey");
    let (x0, y0, x1, y1) = place(&answer);
    assert!(
        x0 >= 0.0 && y0 >= 0.0 && x1 <= f64::from(NARROW) && y1 <= f64::from(HEIGHT),
        "the ask's answer stands on the display where the person sees it: {x0}..{x1}, {y0}..{y1}"
    );
    assert!(
        x1 <= face_x0 || x0 >= face_x1 || y1 <= face_y0 || y0 >= face_y1,
        "the ask's answer stands clear of the person's own face, which stands at \
         {face_x0}..{face_x1}, {face_y0}..{face_y1}: {x0}..{x1}, {y0}..{y1}"
    );
    crate::common::shot(&mut session, "ask-narrow-420");

    press(&mut session, (f64::midpoint(x0, x1), f64::midpoint(y0, y1)));
    assert!(
        session.handle().personal_ask().is_none(),
        "the person answered the ask where it stands"
    );
    assert!(
        session.handle().take_host_choices().is_empty(),
        "the session performed the answer rather than leaving it in the shell"
    );
    crate::common::shot(&mut session, "ask-narrow-420-answered");
}

/// Folded, the ask waits as the avatar's dot, and a press on the avatar opens the person's own card,
/// which offers what the ask asks: the ask itself stays folded.
#[test]
fn a_folded_ask_leaves_the_avatar_to_open_the_own_card() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_on(NARROW, HEIGHT);
    session.handle().set_host_actions(vec![keep_identity()]);
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
    let (place, tree) = bar(&mut session);
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
    let (face, _) = tree
        .nodes
        .iter()
        .find(|(_, node)| {
            !node.is_hidden()
                && node
                    .label()
                    .is_some_and(|said| said.ends_with(": open your card"))
        })
        .cloned()
        .unwrap_or_else(|| panic!("the avatar says what it opens: {said:?}"));
    crate::common::shot(&mut session, "ask-narrow-420-folded");
    assert!(
        session.a11y_perform(place, face, &Action::Activate),
        "the shell took the press on the person's own face"
    );
    assert!(
        session.handle().personal_ask().is_some_and(|ask| !ask.open),
        "the avatar leaves the ask folded"
    );
    // The shell opens the person's own card where the person pressed, as the session reads it: the
    // card's own words are what a screen reader hears.
    crate::common::wait_for(&mut session, "the person's own card", |session| {
        session
            .shell_says("How everyone here sees you")
            .then_some(())
    });
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Keep with passkey"),
        "and the card offers what the ask asks: {labels:?}"
    );
    crate::common::shot(&mut session, "ask-narrow-420-folded-card");
}
