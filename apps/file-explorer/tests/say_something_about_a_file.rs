//! Right-click a file and say something about it.
//!
//! `src/reference.rs` proves that a selected file *can* be turned into an
//! address. This proves the gesture reaches it: a real secondary press, through
//! `Widget::handle_event`, over an app that was navigated and laid out, with
//! the compose surface coming up beside the ordinary context menu carrying that
//! file's address — and with the typed sentence and the `Enter` that sends it
//! going through the same door.
//!
//! **Nothing here builds a `Remark`.** Every assertion below is about a message
//! this app composed, because a test that constructs one and pushes it through
//! an encoder proves the encoder works and says nothing at all about whether
//! any app can produce one. That was precisely the state this app was in: the
//! panel opened, drew, and received no event, so the gesture could be described
//! and never performed.
//!
//! Assertions are on the address and the [`Delivery`], never on rendered words.
//! The whole point of the gesture is that the message carries the object rather
//! than a description of it, so a test that read the chip would be checking the
//! description.

mod common;

use std::path::Path;

use common::{Harness, row_center};
use qos_reference::{Delivery, Destination, RoomMember, Target};
use ui::widget::DestinationOption;
use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey, NativeKeyCode, PhysicalKey};

/// The room this app is told about. Structured values, so the assertions are
/// about where a message goes rather than about how it reads.
///
/// The room first, as `ui::App` supplies it — the surface opens on the first
/// option, so the default is the one everybody can read.
fn destinations() -> Vec<DestinationOption> {
    vec![
        DestinationOption::new(
            Destination::channel(qos_reference::ROOM_CONVERSATION, "the room"),
            "the room",
        ),
        DestinationOption::new(
            Destination::participant("agent_claude", "Claude", true),
            "Claude — private",
        ),
    ]
}

/// The same room, as the host holds it for resolving an `@name`.
fn room() -> Vec<RoomMember> {
    vec![
        RoomMember::new("u_xaver", "Xaver", false),
        RoomMember::new("agent_claude", "Claude", true),
    ]
}

/// A directory with a handful of files in a known order.
fn staged() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a staging directory");
    for name in ["alpha.txt", "beta.txt", "harbour.png", "zulu.txt"] {
        std::fs::write(dir.path().join(name), b"x").expect("writes");
    }
    dir
}

/// The address the app publishes for a file — what the remark must equal.
fn address_of(path: &Path) -> String {
    file_explorer::reference::entry_reference(path)
        .resource_id()
        .as_str()
        .to_string()
}

/// SAFETY: the toolkit's established test idiom — winit 0.30 keeps
/// `KeyEventExtra` crate-private, so a struct literal is unavailable. Every
/// public field tolerates the all-zero bit pattern and each is overwritten.
fn key_event(logical: Key, text: Option<&str>) -> KeyEvent {
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.logical_key = logical;
    ev.text = text.map(|t| t.into());
    ev.location = winit::keyboard::KeyLocation::Standard;
    ev.state = ElementState::Pressed;
    ev.repeat = false;
    ev
}

fn press_key(h: &mut Harness, logical: Key, text: Option<&str>) {
    h.dispatch(WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: key_event(logical, text),
        is_synthetic: false,
    });
}

fn type_str(h: &mut Harness, s: &str) {
    for ch in s.chars() {
        let t = ch.to_string();
        press_key(h, Key::Character(t.as_str().into()), Some(&t));
    }
}

fn right_press(h: &mut Harness, at: Point) {
    h.move_cursor(at);
    h.dispatch(WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state: ElementState::Pressed,
        button: MouseButton::Right,
    });
}

/// An app looking at the staged directory, with the row for `index` selected
/// by a real click, and a room to send to.
fn selected(dir: &Path, index: usize) -> (Harness, Point) {
    let at = row_center(dir, index);
    let mut h = Harness::new(dir);
    h.w.set_remark_destinations(destinations());
    h.move_cursor(at);
    h.press();
    h.release();
    h.layout();
    (h, at)
}

/// The headline: the gesture opens a compose surface about the file that was
/// picked, and it is the *address* that is carried.
#[test]
fn right_clicking_a_file_opens_a_compose_about_that_file() {
    let dir = staged();
    // Index 2 of a sorted listing is `harbour.png`.
    let (mut h, at) = selected(dir.path(), 2);
    assert_eq!(
        h.w.selected_indices(),
        vec![2],
        "the click did not select the row the rest of this test is about"
    );

    right_press(&mut h, at);

    assert!(
        h.w.remark_is_open(),
        "right-clicking a selected file offered no way to say anything about it"
    );
    let reference = h.w.remark_reference().expect("the surface is about something");
    assert_eq!(
        reference.resource_id().as_str(),
        address_of(&dir.path().join("harbour.png")),
        "the surface must be about the file that was picked, by address. If the \
         label below reads as a different file, the listing is not sorted the way \
         this test assumes and the row index, not the gesture, is what is wrong: \
         the surface is about {:?}",
        reference.label
    );
    assert_eq!(reference.kind, file_explorer::reference::ENTRY_KIND);
    assert_eq!(
        reference.span, None,
        "a file is referenced whole; a span would claim a part of it"
    );
    // The ordinary menu keeps working — this panel sits beside it, not over it.
    assert!(
        h.w.context_menu_is_open(),
        "the compose surface replaced the context menu instead of joining it"
    );
}

/// The one that was impossible before: a keystroke reaches the message.
///
/// Files drew this panel every frame and routed nothing to it, so the field
/// took no text and the send affordance was never reached. Typing here and
/// finding the words in the composed message is the whole difference.
#[test]
fn what_is_typed_reaches_the_message_and_enter_sends_it() {
    let dir = staged();
    let (mut h, at) = selected(dir.path(), 2);
    let expected = address_of(&dir.path().join("harbour.png"));
    let before = h.w.current_path();

    right_press(&mut h, at);
    assert!(h.w.remark_is_open());

    type_str(&mut h, "make square and put in open document");
    assert_eq!(
        h.w.current_path(),
        before,
        "the sentence was typed into the file view instead of into the compose"
    );

    press_key(&mut h, Key::Named(NamedKey::Enter), None);

    let remark = h.w.take_remark().expect("Enter sends the remark");
    assert_eq!(remark.text, "make square and put in open document");
    assert_eq!(remark.reference.resource_id().as_str(), expected);
    // Asserted on the routing, not on the words the selector renders.
    assert_eq!(remark.destination.target(), Target::Channel(qos_reference::ROOM_CONVERSATION));
    assert!(!h.w.remark_is_open(), "sending closes the surface");
    assert!(
        h.w.take_remark().is_none(),
        "a remark is handed over exactly once"
    );
}

/// The panel takes the keyboard, and the file view stops having it.
///
/// Reported from a live session: right-click, type, and the letters reach
/// *Files* — selecting rows by type-ahead — while the panel sits there
/// looking like it has focus. The panel always asked for focus on open
/// (`RemarkCompose::open_for` sets it); what was missing is that no event ever
/// reached the panel, so the app underneath went on consuming keys beneath an
/// overlay that looked modal and was not.
///
/// Asserted on the app's own state rather than on a focus flag, because the
/// symptom was the app *acting*: a selection that moves while somebody is
/// writing a sentence is the thing a person sees.
#[test]
fn the_panel_takes_the_keyboard_and_the_file_view_stops_acting_on_it() {
    let dir = staged();
    let (mut h, at) = selected(dir.path(), 2);
    let selection_before = h.w.selected_indices();
    let path_before = h.w.current_path();

    right_press(&mut h, at);
    assert!(h.w.remark_is_open());

    // Letters that the file view would otherwise treat as type-ahead — `a`
    // and `z` both name rows in this directory.
    type_str(&mut h, "az make square");

    assert_eq!(
        h.w.selected_indices(),
        selection_before,
        "typing into the compose moved the file view's selection, so the keys \
         reached the application behind the panel"
    );
    assert_eq!(h.w.current_path(), path_before, "typing navigated the file view");
    assert!(
        h.w.remark_text().contains("az make square"),
        "the keys did not reach the message; the field holds {:?}",
        h.w.remark_text()
    );
}

/// Dismissing the menu takes an empty panel with it.
///
/// They open on one gesture. A panel left standing over a menu that has gone
/// is an overlay attached to nothing — reported from a live session, where
/// closing the menu left the compose behind.
#[test]
fn dismissing_the_menu_dismisses_an_unwritten_panel() {
    let dir = staged();
    let (mut h, at) = selected(dir.path(), 2);

    right_press(&mut h, at);
    assert!(h.w.remark_is_open());
    assert!(h.w.context_menu_is_open());

    // A click well away from both, which is how a menu is dismissed.
    h.move_cursor(Point::new(20.0, 700.0));
    h.press();
    h.release();
    // Level-triggered, so it settles on the next event the app sees.
    h.move_cursor(Point::new(21.0, 700.0));

    assert!(!h.w.context_menu_is_open(), "the click did not dismiss the menu");
    assert!(
        !h.w.remark_is_open(),
        "the menu went and the compose panel was left standing over nothing"
    );
}

/// A half-written sentence is not thrown away by a click aimed at something
/// else. This is the case the old always-independent rule was protecting, and
/// it is the only one that keeps the panel open.
#[test]
fn dismissing_the_menu_keeps_a_written_panel() {
    let dir = staged();
    let (mut h, at) = selected(dir.path(), 2);

    right_press(&mut h, at);
    type_str(&mut h, "half a thought");

    h.move_cursor(Point::new(20.0, 700.0));
    h.press();
    h.release();
    h.move_cursor(Point::new(21.0, 700.0));

    assert!(
        h.w.remark_is_open(),
        "a sentence somebody was in the middle of writing was discarded by a \
         click aimed at the menu"
    );
    assert!(h.w.remark_text().contains("half a thought"));
}

/// Escape closes both. The menu is part of the gesture the panel belongs to,
/// so it must not be the visible remnant of something dismissed.
#[test]
fn escape_closes_the_panel_and_the_menu_together() {
    let dir = staged();
    let (mut h, at) = selected(dir.path(), 2);

    right_press(&mut h, at);
    assert!(h.w.remark_is_open() && h.w.context_menu_is_open());

    press_key(&mut h, Key::Named(NamedKey::Escape), None);

    assert!(!h.w.remark_is_open(), "Escape left the compose panel open");
    assert!(!h.w.context_menu_is_open(), "Escape left the menu standing alone");
}

/// Escape leaves without sending. A composer that could only be escaped by
/// sending would make every abandoned sentence a message.
#[test]
fn a_dismissed_compose_sends_nothing() {
    let dir = staged();
    let (mut h, at) = selected(dir.path(), 2);

    right_press(&mut h, at);
    type_str(&mut h, "never mind");
    press_key(&mut h, Key::Named(NamedKey::Escape), None);

    assert!(!h.w.remark_is_open());
    assert!(h.w.take_remark().is_none(), "a dismissed compose sends nothing");
}

/// Nowhere to send is not a compose over nothing. Until the room has told this
/// app who is in it, the gesture is simply not offered — and the ordinary menu
/// still is.
#[test]
fn nothing_opens_before_the_room_has_said_who_is_in_it() {
    let dir = staged();
    let at = row_center(dir.path(), 2);
    let mut h = Harness::new(dir.path());
    h.move_cursor(at);
    h.press();
    h.release();
    h.layout();

    right_press(&mut h, at);

    assert!(!h.w.remark_is_open());
    assert!(
        h.w.context_menu_is_open(),
        "a right-click with nowhere to send must still give you the plain menu"
    );
}

/// The claim beat four rests on, end to end from the app that composes it.
///
/// The remark below is the one the gesture produced — nothing here builds one.
/// It is addressed the way the host addresses it (the room's roster resolves
/// the `@claude` somebody typed), encoded as chat's own `SendMessage`, and read
/// back. What must survive is the **address**: the file's id, recovered from a
/// typed field, with the body still free of it.
///
/// The other half — chat's registry decoding this and chat's own action
/// inserting the row — is proven against the real chat app in
/// `shell/tests/chat_message_crosses.rs`. What that one cannot show, and this
/// one does, is that an app produces the message in the first place.
#[test]
fn the_composed_remark_reaches_an_agent_carrying_the_files_address() {
    let dir = staged();
    let (mut h, at) = selected(dir.path(), 2);
    let expected = address_of(&dir.path().join("harbour.png"));

    right_press(&mut h, at);
    type_str(&mut h, "@claude make square and put in open document");
    press_key(&mut h, Key::Named(NamedKey::Enter), None);

    let remark = h.w.take_remark().expect("the gesture produced a remark");
    // The host's own step: a typed name becomes a participant id. Resolved
    // before the move, because `with_mentions` consumes the remark.
    let mentions = qos_reference::resolve_mentions(&remark.text, &room());
    let remark = remark.with_mentions(mentions);
    assert_eq!(
        remark.delivery(),
        Delivery::DirectedInChannel {
            channel: qos_reference::ROOM_CONVERSATION.to_string(),
            to: vec!["agent_claude".to_string()],
        },
        "the agent must be addressed, and the room must still be able to read it"
    );

    let encoded = remark
        .to_chat_action("m_1", "u_xaver", 1_700_000_000)
        .expect("a remark addressed to the room is deliverable");
    assert_eq!(encoded.type_id, qos_reference::CHAT_ACTION_TYPE);

    let got = qos_reference::read_chat_action(&encoded).expect("a remark is recoverable");
    assert_eq!(
        got.reference.resource_id().as_str(),
        expected,
        "the address must survive as an address"
    );
    assert!(
        !got.text.contains(&expected),
        "the address must not be smuggled through the body"
    );
    assert!(
        got.mentions.iter().any(|m| m.user == "agent_claude" && m.is_agent),
        "the message must say it names software, not a colleague"
    );
    assert_eq!(got.text, "@claude make square and put in open document");
}

/// Under a compositor, the same gesture produces a *request* and no panel.
///
/// The panel opens beside this app's context menu, and beside a menu near the
/// window's edge is outside the window — which a Wayland client cannot paint
/// into. So when a session is drawing this window's chrome the app stops
/// drawing the panel and asks for one instead.
///
/// Asserted on both halves of what crosses, because a request missing either is
/// useless: the address says what is being talked about, and the menu rect is
/// what the compositor places against. Nothing else crosses, and nothing else
/// should — where that rect lands on a desktop and who may receive the message
/// are answers this process does not have.
#[test]
fn a_delegated_app_asks_for_the_surface_instead_of_drawing_one() {
    let dir = staged();
    let (mut h, at) = selected(dir.path(), 2);
    // What a host does for an app it draws the chrome of.
    <file_explorer::FileExplorerWidget as ui::widget::Widget>::delegate_remark_surface(&mut h.w);

    right_press(&mut h, at);

    assert!(
        !h.w.remark_is_open(),
        "a delegated app drew its own panel as well; one gesture must not \
         produce two panels, and this one would be the clipped one"
    );
    assert!(
        h.w.context_menu_is_open(),
        "the ordinary context menu must still open — the compose surface joins \
         it rather than replacing it, and that is unchanged by who draws it"
    );

    let request =
        <file_explorer::FileExplorerWidget as ui::widget::Widget>::take_remark_request(&mut h.w)
            .expect(
                "right-clicking a selected file asked for nothing; the gesture \
                 reached neither a panel nor a request, so it did nothing at all",
            );
    assert_eq!(
        request.reference.resource_id().as_str(),
        address_of(&dir.path().join("harbour.png")),
        "the request must carry the address of the file that was picked; the \
         subject is what the compositor cannot work out for itself"
    );
    assert_eq!(
        request.reference.span, None,
        "a file is referenced whole; a span would claim a part of it"
    );
    assert_eq!(
        request.beside,
        h.w.context_menu_rect_for_tests(),
        "the request must sit beside the menu this gesture opened, reported in \
         this window's own coordinates — that rect is the only thing the \
         compositor has to place against"
    );
    assert!(
        <file_explorer::FileExplorerWidget as ui::widget::Widget>::take_remark_request(&mut h.w)
            .is_none(),
        "a request must be collectable once; a second one would open a second \
         panel for a gesture nobody repeated"
    );
}

/// A delegated app asks even before the room has said who is in it.
///
/// The mirror of `nothing_opens_before_the_room_has_said_who_is_in_it`, and the
/// reason that rule cannot simply be carried across: an app that draws the
/// panel needs somewhere to send, so an empty roster means no panel. A
/// delegated app is never given a roster at all — the compositor fills the
/// selector from the room it already holds — so gating on one would make the
/// gesture do nothing forever.
#[test]
fn a_delegated_app_needs_no_roster_of_its_own() {
    let dir = staged();
    let at = row_center(dir.path(), 2);
    let mut h = Harness::new(dir.path());
    // Deliberately no `set_remark_destinations`.
    <file_explorer::FileExplorerWidget as ui::widget::Widget>::delegate_remark_surface(&mut h.w);
    h.move_cursor(at);
    h.press();
    h.release();
    h.layout();

    right_press(&mut h, at);

    assert!(
        <file_explorer::FileExplorerWidget as ui::widget::Widget>::take_remark_request(&mut h.w)
            .is_some(),
        "a delegated app was gated on a roster it is never given, so the gesture \
         could never reach the compositor that has one"
    );
}
