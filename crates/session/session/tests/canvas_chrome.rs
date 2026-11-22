//! What a place on the framed canvas looks like and where it stands (`shell.md` §4, §8;
//! `docs/designs/app-language/canvas.md`; the frames preset in `apps/tl/shell/canvas.tl`).
//!
//! Two things a person saw on a live shell and reported:
//!
//! 1. A third place opened beside the conversation squeezed all three into one narrow column and
//!    resized the place already open: opening something never moves or resizes what stands,
//!    and it never changes the person's zoom — it pans when what arrived would stand off-screen.
//! 2. The style menu's band above its first row stood empty.
//!
//! Every step is the production path: the session's own `place_found`/`open` actions, the shell's
//! preset, and the layout the shell reads back.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use tilo_service::Questions;
use tilo_session::{AppSocket, Options, Session};

use crate::common::{
    CANVAS, PERSON, layout, on_screen, private_home, show_preset, until, wait_for,
};

/// A headless session with the shell as the whole screen and the apps a development session seeds,
/// the framed canvas in front.
fn session(root: &std::path::Path) -> Session {
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        services: crate::common::seeded_apps(),
        questions: Some(Questions::new()),
        app_socket: AppSocket::At(root.join("app_wire.sock")),
        mcp_server: Some(root.join("agent.sock")),
        ..Options::default()
    });
    show_preset(&mut session, "so.tilo.shell:frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });
    session
}

/// Point at the canvas's free ground and say `text` there: the person finding or asking where
/// they pointed.
fn enter_text(session: &mut Session, text: &str) {
    let (x, y, w, h) = wait_for(session, "the canvas", |session| on_screen(session, CANVAS));
    crate::common::point_and_say(session, (x + w * 0.5, y + h * 0.3), text);
}

/// Every placed strip's canvas box, by the resource its place shows: `(x, y, w, h)`, as the shell
/// holds it.
fn boxes(session: &Session) -> Vec<(String, i64, i64, i64, i64)> {
    layout(session)
        .places
        .iter()
        .filter_map(|place| {
            let strip = crate::common::strip(session, crate::common::number(&place.strip))?;
            strip.placed.then_some((
                place.resource.clone(),
                strip.canvas_x,
                strip.canvas_y,
                strip.canvas_width,
                strip.canvas_height,
            ))
        })
        .collect()
}

/// The place a resource's root names (a mail root, a chat channel), as the shell stands.
fn place_showing(session: &Session, needle: &str) -> Option<(String, i64, i64, i64, i64)> {
    boxes(session)
        .into_iter()
        .find(|(record, ..)| record.contains(needle))
}

/// The published root of the seeded app `app` (`so.tilo.tasks`): what an agent opens.
fn root_of(session: &Session, app: &str) -> String {
    let prefix = format!("tl:{app}:root-");
    session
        .resources()
        .into_iter()
        .find(|spec| spec.id.as_str().starts_with(&prefix))
        .map_or_else(
            || panic!("{app}'s root is published"),
            |spec| spec.id.as_str().to_owned(),
        )
}

/// Open `resource` beside the place showing `beside`, through the layout's own `open`, as an
/// agent's step does.
fn open_beside(session: &mut Session, resource: &str, beside: &str) {
    session
        .invoke(
            "shell:layout",
            "open",
            serde_json::json!({ "resource": resource, "beside": beside })
                .as_object()
                .expect("arguments"),
        )
        .expect("the open runs");
}

/// Opening a place beside another never moves or resizes what stands: a place's size is the size
/// the record keeps for it (720 × 520, the canvas's frame), and only its own place action sets it.
#[test]
fn opening_a_place_beside_another_keeps_the_sizes_of_what_stands() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());

    // The person's mails on the canvas: the inbox, at the canvas's frame size narrowed to the
    // room in view right of the click, so all of it stays in view.
    enter_text(&mut session, "my mails");
    let inbox = wait_for(&mut session, "the inbox on the canvas", |session| {
        place_showing(session, "so.tilo.mail")
    });
    let (_, _, canvas_width, _) = on_screen(&session, CANVAS).expect("the canvas");
    assert!(
        inbox.4 == 520
            && inbox.3 <= 720
            && i32::try_from(inbox.1 + inbox.3).is_ok_and(|right| f64::from(right) <= canvas_width),
        "the inbox takes the canvas's frame size, as much of it as is in view: {inbox:?} on a \
         canvas {canvas_width} wide"
    );

    // The notes the person keeps, opened beside the inbox.
    let notes = root_of(&session, "so.tilo.notes");
    open_beside(&mut session, &notes, &inbox.0);
    let conversation = wait_for(&mut session, "the notes on the canvas", |session| {
        place_showing(session, "so.tilo.notes")
    });
    assert_eq!(
        (conversation.3, conversation.4),
        (720, 520),
        "the conversation takes the canvas's frame size: {conversation:?}"
    );

    // An agent opens Tasks beside the notes, as a turn's step runs the layout's `open`.
    let resource = root_of(&session, "so.tilo.tasks");
    open_beside(&mut session, &resource, &conversation.0);
    let tasks = wait_for(&mut session, "tasks beside the conversation", |session| {
        place_showing(session, "so.tilo.tasks")
    });
    assert_eq!(
        (tasks.3, tasks.4),
        (720, 520),
        "tasks takes the canvas's frame size: {tasks:?}"
    );

    // Nothing placed moved or shrank: the inbox and the conversation keep their size and their
    // point, and no place crowds another.
    let after = place_showing(&session, "so.tilo.mail").expect("the inbox still stands");
    assert_eq!(
        (after.1, after.2, after.3, after.4),
        (inbox.1, inbox.2, inbox.3, inbox.4),
        "the inbox keeps where it stood and how big it was: {after:?} against {inbox:?}"
    );
    let conversation_after =
        place_showing(&session, "so.tilo.notes").expect("the notes still stand");
    assert_eq!(
        (
            conversation_after.1,
            conversation_after.2,
            conversation_after.3,
            conversation_after.4
        ),
        (
            conversation.1,
            conversation.2,
            conversation.3,
            conversation.4
        ),
        "the conversation keeps where it stood and how big it was: {conversation_after:?} \
         against {conversation:?}"
    );

    // And every pair of places stands clear of every other.
    let all = boxes(&session);
    assert_eq!(all.len(), 3, "three places stand: {all:?}");
    for (index, (a, ax, ay, aw, ah)) in all.iter().enumerate() {
        for (b, bx, by, bw, bh) in all.iter().skip(index + 1) {
            let overlap = ax < &(bx + bw) && bx < &(ax + aw) && ay < &(by + bh) && by < &(ay + ah);
            assert!(!overlap, "{a} and {b} stand over each other: {all:?}");
        }
    }
}

/// The live run's failure, as a person saw it (`shell.md` §3, §4): after "my mails" and a
/// question, an agent opened Tasks and all three places were *drawn* as a narrow column down the
/// left edge, the inbox itself shrunk to a sliver. Opening a place never resizes the places
/// already there: a place is drawn at the size the canvas draws its neighbours at, and the camera
/// pans to what arrived instead of zooming everything into the viewport. A place already open
/// keeps the width it was drawn at, whichever places arrive beside it.
#[test]
fn opening_a_third_place_never_shrinks_what_the_person_is_reading() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());

    // The inbox, lain on the canvas: the frame its title names it by, as wide as the place.
    enter_text(&mut session, "my mails");
    let inbox = wait_for(&mut session, "the inbox on the canvas", |session| {
        place_showing(session, "so.tilo.mail")
    });
    // A place drawn as content goes by its own title, not joined to its app's name.
    let inbox_name = layout(&session)
        .places
        .into_iter()
        .find(|place| place.resource == inbox.0)
        .map(|place| place.title)
        .expect("the inbox stands");
    // The widest box drawn for the place's name is the place itself (its frame's group reads the
    // name too): how wide the place is drawn on the canvas.
    let mut drawn_width = |session: &mut Session| -> Option<f64> {
        session
            .shell_text_bounds_all(&inbox_name)
            .into_iter()
            .map(|(_, _, width, _)| width)
            .fold(None, |held: Option<f64>, width| {
                Some(held.map_or(width, |held| held.max(width)))
            })
    };
    let inbox_width = wait_for(&mut session, "the inbox drawn", &mut drawn_width);

    // The notes, then an agent opening Tasks beside them: two more places arrive.
    let notes = root_of(&session, "so.tilo.notes");
    open_beside(&mut session, &notes, &inbox.0);
    let conversation = wait_for(&mut session, "the notes on the canvas", |session| {
        place_showing(session, "so.tilo.notes")
    });
    let resource = root_of(&session, "so.tilo.tasks");
    open_beside(&mut session, &resource, &conversation.0);
    wait_for(&mut session, "tasks beside the conversation", |session| {
        place_showing(session, "so.tilo.tasks")
    });
    // The inbox is still drawn at the width it was: the camera panned to what arrived, it did not
    // zoom the person's reading into a narrow column.
    let after = wait_for(&mut session, "the inbox still drawn", &mut drawn_width);
    assert!(
        (after - inbox_width).abs() < 1.0,
        "the inbox is drawn at the width it was, {inbox_width}, never shrunk by what arrived \
         beside it: {after}"
    );
    assert!(
        after > 300.0,
        "the inbox is drawn at its place's width, never a narrow column: {after}"
    );
}

/// The style menu has no empty band above its first row: the menu's top is its first row's
/// top, and nothing stands between the panel's edge and the row.
#[test]
fn the_style_shell_menu_has_no_empty_band_above_its_rows() {
    use ui::window_targets::TargetAction;
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    // The canvas keeps no switch on its bar: the person's own menu holds the styles.
    let menu = wait_for(&mut session, "the style menu", |session| {
        crate::common::person_menu(session)
    });
    // Open the style menu by name, as an assistive technology's user does.
    session
        .press_action(&menu, TargetAction::Activate)
        .expect("the style menu opens");
    for _ in 0..4 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    // The menu's first row, Strips, and the button it hangs under.
    let (_, button_y, _, button_h) = on_screen(&session, &menu).expect("the button");
    let (_, row_y, _, _) = wait_for(&mut session, "the menu's first row", |session| {
        on_screen(session, "Strips")
    });
    // The menu opens under its button, `s1` clear of it: no row sits closer to the button than
    // that, and none has an empty band above it.
    let gap = row_y - (button_y + button_h);
    assert!(
        (0.0..=16.0).contains(&gap),
        "the menu's first row stands just under its button, with no empty band between: \
         {gap} px from the button's foot ({}) to the row's top ({row_y}); the button is \
         {button_y}..{}",
        button_y + button_h,
        button_y + button_h
    );
}

/// The frameless style: the same places as the framed canvas, each drawn as its own view with no
/// frame around it. Switching to it keeps every place where it was and draws the map the style
/// names.
#[test]
fn switching_to_the_frameless_style_keeps_every_place() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    enter_text(&mut session, "my mails");
    wait_for(&mut session, "the inbox on the canvas", |session| {
        place_showing(session, "so.tilo.mail")
    });
    let before = boxes(&session);
    show_preset(&mut session, "so.tilo.shell:frameless");
    wait_for(&mut session, "the frameless canvas", |session| {
        on_screen(session, "Frameless")
    });
    assert_eq!(
        boxes(&session),
        before,
        "the frameless style keeps every place where it stood"
    );
}
