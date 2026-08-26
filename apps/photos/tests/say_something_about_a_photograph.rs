//! Right-click a photograph and say something about it.
//!
//! `src/reference.rs` proves that an open photograph *can* be turned into an
//! address. This proves the gesture reaches it: a real right-click, through
//! `Widget::handle_event`, over an app that was laid out and drawn, with the
//! compose surface coming up beside the ordinary context menu carrying the
//! picture's address.
//!
//! Everything here goes through the production path. Nothing calls the compose
//! widget directly — if the app stopped wiring it into the right-click, or
//! stopped routing keystrokes to it, these fail.
//!
//! Assertions are on the address and the [`Delivery`], never on rendered words:
//! the whole point of the gesture is that the message carries the object rather
//! than a description of it, and a test that read the chip would be checking
//! the description.

use std::collections::HashMap;

use photos::PhotosApp;
use photos::doc::PhotosDoc;
use taffy::prelude::*;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{DestinationOption, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey, NativeKeyCode, PhysicalKey};

use qos_reference::{Delivery, Destination, Target};

const VIEWPORT: (f32, f32) = (1280.0, 800.0);

/// An app with a photograph open, decoded through the ordinary path from a
/// real file — the same route the chooser and the command line take, so the
/// viewer this produces is the one a session has.
fn opened() -> (ui::State, PhotosApp, tempfile::TempDir) {
    opened_sized(1200, 800)
}

/// The same, at a chosen aspect ratio. A tall photograph in a wide window
/// leaves real letterbox bands, which is the case the placement rule is about.
fn opened_sized(w: u32, h: u32) -> (ui::State, PhotosApp, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a staging directory");
    let path = dir.path().join("harbour.png");
    let png = photos::pixels::encode_png(&photos::shots_pattern(w, h)).expect("encodes");
    std::fs::write(&path, png).expect("writes");

    let mut state = ui::State::new();
    let mut app = PhotosApp::new();
    PhotosDoc::seed(&mut state.doc);
    app.open_path(&mut state.doc, &path.to_string_lossy());
    assert!(
        PhotosDoc::image(&state.doc).is_some(),
        "the fixture failed to open a photograph, so nothing below proves anything"
    );
    (state, app, dir)
}

/// The destinations a host would supply. Structured values, so the assertions
/// are about where a message goes rather than about how it reads.
fn destinations() -> Vec<DestinationOption> {
    vec![
        DestinationOption::new(
            Destination::channel("c_shoot", "harbour-shoot"),
            "# harbour-shoot",
        ),
        DestinationOption::new(
            Destination::participant("agent_claude", "Claude", true),
            "Claude — private",
        ),
    ]
}

/// Lay the app out at the viewport size, the way `ui::App` does every frame,
/// and hand back the tree the event routing needs.
///
/// The layout is not decoration: the viewer's framing — which decides both what
/// the photograph covers and where the panel can go — is only real once a
/// layout has happened, and `handle_event` routes to the toolbar and canvas
/// through this very tree.
fn layout(app: &mut PhotosApp, state: &mut ui::State) -> (TaffyTree<()>, NodeId) {
    let (w, h) = VIEWPORT;
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    app.sync(&mut state.doc, &state.root);
    let child = app.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                },
                ..Default::default()
            },
            &[child],
        )
        .unwrap();
    {
        let measure_map = node_map.clone();
        let trr = &mut tr;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(w),
                    height: AvailableSpace::Definite(h),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, trr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
    }
    app.apply_layout(&taffy, child, Point::ORIGIN);
    (taffy, child)
}

/// A laid-out frame that is also drawn, as the first frame of a session is
/// before anybody clicks anything.
fn lay_out_and_draw(app: &mut PhotosApp, state: &mut ui::State) {
    let _ = layout(app, state);
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    app.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &Theme::dark(),
        &PresenceCtx::empty(),
        &PermissionContext::permissive(),
    );
}

/// An app that is open, laid out, drawn, and has somewhere to send.
fn ready() -> (ui::State, PhotosApp, tempfile::TempDir) {
    ready_sized(1200, 800)
}

fn ready_sized(w: u32, h: u32) -> (ui::State, PhotosApp, tempfile::TempDir) {
    let (mut state, mut app, dir) = opened_sized(w, h);
    app.set_remark_destinations(destinations());
    lay_out_and_draw(&mut app, &mut state);
    (state, app, dir)
}

/// Deliver one event, against a freshly laid-out tree — which is what a real
/// session hands `handle_event`, because a frame precedes every input.
fn dispatch(app: &mut PhotosApp, state: &mut ui::State, event: &WindowEvent, mods: &Modifiers) {
    let (taffy, node) = layout(app, state);
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    app.handle_event(
        event, 1.0, mods, &mut state.doc, &state.root, &mut handled, &mut focus, node, &taffy,
    );
}

fn move_to(app: &mut PhotosApp, state: &mut ui::State, pt: Point) {
    dispatch(
        app,
        state,
        &WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: (pt.x, pt.y).into(),
        },
        &Modifiers::default(),
    );
}

fn button(app: &mut PhotosApp, state: &mut ui::State, b: MouseButton, s: ElementState) {
    dispatch(
        app,
        state,
        &WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: s,
            button: b,
        },
        &Modifiers::default(),
    );
}

/// A real secondary press at `pt`, preceded by the pointer move that carries
/// the position — the app tracks the cursor, it does not read the press.
fn right_click(app: &mut PhotosApp, state: &mut ui::State, pt: Point) {
    move_to(app, state, pt);
    button(app, state, MouseButton::Right, ElementState::Pressed);
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

fn press(
    app: &mut PhotosApp,
    state: &mut ui::State,
    logical: Key,
    text: Option<&str>,
    mods: &Modifiers,
) {
    dispatch(
        app,
        state,
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: key_event(logical, text),
            is_synthetic: false,
        },
        mods,
    );
}

fn type_str(app: &mut PhotosApp, state: &mut ui::State, s: &str) {
    let mods = Modifiers::default();
    for ch in s.chars() {
        let t = ch.to_string();
        press(app, state, Key::Character(t.as_str().into()), Some(&t), &mods);
    }
}

/// A point on the photograph, at the given fractions of its drawn extent.
fn on_the_picture(app: &PhotosApp, fx: f64, fy: f64) -> Point {
    let r = app.photograph_rect();
    Point::new(r.x0 + r.width() * fx, r.y0 + r.height() * fy)
}

/// Right-click the photograph itself, at the given fractions of its extent —
/// where somebody looking at the picture would point.
fn right_click_picture(app: &mut PhotosApp, state: &mut ui::State, fx: f64, fy: f64) {
    let at = on_the_picture(app, fx, fy);
    right_click(app, state, at);
}

/// The address the document itself publishes for the open photograph, which is
/// what the remark must equal.
fn open_address(state: &ui::State) -> String {
    let image = PhotosDoc::image(&state.doc).expect("something is open");
    photos::actions::resource_id(&image)
}

fn overlap(a: Rect, b: Rect) -> f64 {
    let i = a.intersect(b);
    if i.width() > 0.0 && i.height() > 0.0 {
        i.width() * i.height()
    } else {
        0.0
    }
}

/// The headline property: a right-click on the photograph opens the compose
/// surface, and what it carries is the picture's address — not its filename,
/// and not the words on the chip a person reads.
#[test]
fn a_right_click_on_a_photograph_opens_a_compose_carrying_its_address() {
    let (mut state, mut app, _dir) = ready();
    let expected = open_address(&state);

    right_click_picture(&mut app, &mut state, 0.34, 0.42);

    assert!(app.remark_is_open(), "the compose surface did not open");
    let r = app
        .remark_reference()
        .expect("an open compose is about something");
    assert_eq!(
        r.resource_id().as_str(),
        expected,
        "the remark must carry the photograph's address"
    );
    assert_eq!(
        r.span, None,
        "a photograph is referenced whole; a span would claim a part of it"
    );
    assert!(
        !r.label.contains(&expected),
        "the label is for a person and is never the address, was {:?}",
        r.label
    );
}

/// Beside the menu, not instead of it. "Say something about this" is a second
/// thing to do with the photograph, not a replacement for rotating or cropping
/// it, and the two surfaces must not sit on top of each other.
#[test]
fn the_ordinary_menu_still_opens_beside_the_compose() {
    let (mut state, mut app, _dir) = ready();

    right_click_picture(&mut app, &mut state, 0.34, 0.42);

    let labels = app.context_menu_labels();
    assert!(
        labels.len() >= 4,
        "the right-click menu lost its rows: {labels:?}"
    );
    assert!(app.remark_is_open());
}

/// The gesture must not damage its own subject. In a document that means the
/// text selection; here it means the framing — the region of the picture the
/// person was looking at when they decided to say something about it.
#[test]
fn a_right_click_does_not_disturb_what_is_being_looked_at() {
    let (mut state, mut app, _dir) = ready();
    app.set_zoom(2.2);
    lay_out_and_draw(&mut app, &mut state);
    let framed = app.visible_rect();
    let before = PhotosDoc::edits(&state.doc);

    right_click_picture(&mut app, &mut state, 0.34, 0.42);

    let after = app.visible_rect();
    assert!(
        (after.x - framed.x).abs() < 1e-9
            && (after.y - framed.y).abs() < 1e-9
            && (after.w - framed.w).abs() < 1e-9,
        "the framing moved under the click: {framed:?} -> {after:?}"
    );
    assert_eq!(
        PhotosDoc::edits(&state.doc),
        before,
        "opening a message box edited the photograph"
    );
}

/// The roster is the host's. Until one arrives the gesture is not offered —
/// the app must never invent somewhere to send to.
#[test]
fn nothing_opens_until_a_host_says_where_a_remark_could_go() {
    let (mut state, mut app, _dir) = opened();
    lay_out_and_draw(&mut app, &mut state);

    right_click_picture(&mut app, &mut state, 0.34, 0.42);

    assert!(
        !app.remark_is_open(),
        "a compose opened with nowhere to send"
    );
    assert!(
        !app.context_menu_labels().is_empty(),
        "the ordinary menu must still work without a roster"
    );
}

/// Nothing open, nothing to be about. A compose over an empty canvas would ask
/// for a sentence about nothing.
#[test]
fn nothing_opens_over_an_empty_app() {
    let mut state = ui::State::new();
    let mut app = PhotosApp::new();
    app.set_remark_destinations(destinations());
    lay_out_and_draw(&mut app, &mut state);

    right_click(&mut app, &mut state, Point::new(600.0, 400.0));

    assert!(!app.remark_is_open());
    assert!(
        app.context_menu_labels().is_empty(),
        "there is nothing to do to a photograph that is not open"
    );
}

/// Typing lands in the message, and `Enter` hands the host a remark addressed
/// to the destination the selector is showing.
#[test]
fn what_is_typed_reaches_the_message_and_enter_sends_it() {
    let (mut state, mut app, _dir) = ready();
    let expected = open_address(&state);
    let before = PhotosDoc::edits(&state.doc);

    right_click_picture(&mut app, &mut state, 0.34, 0.42);
    assert!(app.remark_is_open());

    type_str(&mut app, &mut state, "this one, but it needs to be square");
    assert_eq!(
        PhotosDoc::edits(&state.doc),
        before,
        "the message was typed into the photograph instead of into the compose"
    );

    press(
        &mut app,
        &mut state,
        Key::Named(NamedKey::Enter),
        None,
        &Modifiers::default(),
    );

    let remark = app.take_remark().expect("Enter sends the remark");
    assert_eq!(remark.text, "this one, but it needs to be square");
    assert_eq!(remark.reference.resource_id().as_str(), expected);
    // Asserted on the routing, not on the words the selector renders.
    assert_eq!(
        remark.delivery(),
        Delivery::Channel {
            channel: "c_shoot".to_string()
        }
    );
    assert!(!app.remark_is_open(), "sending closes the surface");
    assert!(
        app.take_remark().is_none(),
        "a remark is handed over exactly once"
    );
    // The frame that follows the keystroke, which is where the status line is
    // recomputed — the same one a running session draws next.
    lay_out_and_draw(&mut app, &mut state);
    assert!(
        app.status().contains("harbour-shoot"),
        "a send whose only evidence is that the panel vanished is indistinguishable \
         from one that failed; status was {:?}",
        app.status()
    );
}

/// Escape puts the compose away without sending, and without touching the
/// photograph underneath.
#[test]
fn escape_dismisses_without_sending() {
    let (mut state, mut app, _dir) = ready();
    let before = PhotosDoc::edits(&state.doc);

    right_click_picture(&mut app, &mut state, 0.34, 0.42);
    type_str(&mut app, &mut state, "never mind");
    press(
        &mut app,
        &mut state,
        Key::Named(NamedKey::Escape),
        None,
        &Modifiers::default(),
    );

    assert!(!app.remark_is_open(), "Escape must dismiss the compose");
    assert!(app.take_remark().is_none(), "a dismissed compose sends nothing");
    assert_eq!(PhotosDoc::edits(&state.doc), before);
}

/// The chord raises the same surface over the same photograph, minting the
/// same address — the gesture is not mouse-only. An address is exactly what
/// somebody working without a pointer cannot produce by describing.
#[test]
fn the_chord_raises_the_same_surface_as_the_right_click() {
    let mods: Modifiers =
        (winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT).into();

    let (mut state, mut app, _dir) = ready();
    let expected = open_address(&state);

    press(&mut app, &mut state, Key::Character("m".into()), Some("m"), &mods);

    assert!(app.remark_is_open(), "Ctrl+Shift+M did not raise the compose");
    assert_eq!(
        app.remark_reference()
            .expect("about something")
            .resource_id()
            .as_str(),
        expected,
        "the chord must mint the same address the pointer does"
    );
    // And it is showing where it would go before anything is typed.
    assert!(matches!(
        app.remark_destination().map(|d| d.target()),
        Some(Target::Channel("c_shoot"))
    ));
}

/// A press inside the message field must not reach the canvas underneath.
///
/// The photograph fills the window, so every surface floating over it is over
/// the viewer too; a press that fell through would begin a pan and drag the
/// subject away while its own sentence was being written.
#[test]
fn typing_into_the_message_cannot_pan_the_photograph_behind_it() {
    let (mut state, mut app, _dir) = ready();
    app.set_zoom(2.2);
    lay_out_and_draw(&mut app, &mut state);
    right_click_picture(&mut app, &mut state, 0.34, 0.42);
    assert!(app.remark_is_open());

    let framed = app.visible_rect();
    // Press inside the panel, then drag across it the way selecting text does.
    let panel = app.remark_rect();
    let inside = Point::new(panel.x0 + panel.width() * 0.5, panel.y0 + panel.height() * 0.7);
    move_to(&mut app, &mut state, inside);
    button(&mut app, &mut state, MouseButton::Left, ElementState::Pressed);
    move_to(&mut app, &mut state, Point::new(inside.x + 120.0, inside.y + 40.0));
    button(&mut app, &mut state, MouseButton::Left, ElementState::Released);

    let after = app.visible_rect();
    assert!(
        (after.x - framed.x).abs() < 1e-9 && (after.y - framed.y).abs() < 1e-9,
        "the photograph panned under the compose: {framed:?} -> {after:?}"
    );
}

/// The panel moves to a side that leaves the photograph clear.
///
/// A text selection runs back from the click, which is why the word processor
/// keeps the menu's left clear. A picture has no such direction — what has to
/// stay visible is the picture itself — and this is the case that tells the two
/// rules apart: a tall photograph in a wide window leaves a letterbox band on
/// either side, and a right-click near its left edge has free room on the left
/// while the room on the right is over the picture. A fixed right-first order
/// would cover it.
#[test]
fn the_panel_moves_to_a_side_that_clears_the_picture() {
    // 3:8 — tall enough that the contain fit leaves bands wider than the panel.
    let (mut state, mut app, _dir) = ready_sized(300, 800);
    let picture = app.photograph_rect();
    let window = Rect::new(0.0, 0.0, VIEWPORT.0 as f64, VIEWPORT.1 as f64);
    assert!(
        picture.x0 - window.x0 > 392.0 && window.x1 - picture.x1 > 392.0,
        "the fixture stopped leaving room beside the picture ({picture:?}), so this \
         test no longer distinguishes the rule from a fixed order"
    );

    right_click_picture(&mut app, &mut state, 0.25, 0.5);
    assert!(app.remark_is_open());
    assert_eq!(
        overlap(app.remark_rect(), picture),
        0.0,
        "the panel covered the photograph at {:?} when a free band was available \
         beside it ({picture:?})",
        app.remark_rect()
    );

    // And from the middle, where either side would do: it still clears it.
    let (mut state, mut app, _dir) = ready_sized(300, 800);
    right_click_picture(&mut app, &mut state, 0.5, 0.5);
    assert_eq!(
        overlap(app.remark_rect(), app.photograph_rect()),
        0.0,
        "the panel covered the photograph from a click in its middle"
    );
}

/// Where the photograph fills the window there is no free band, and the panel
/// has to sit over it somewhere — but never over most of it.
#[test]
fn a_full_bleed_photograph_is_not_mostly_covered() {
    for (fx, fy) in [(0.2, 0.5), (0.5, 0.5), (0.8, 0.5)] {
        let (mut state, mut app, _dir) = ready();
        let picture = app.photograph_rect();
        right_click_picture(&mut app, &mut state, fx, fy);
        assert!(app.remark_is_open(), "nothing opened at ({fx}, {fy})");
        let area = picture.width() * picture.height();
        let hidden = overlap(app.remark_rect(), picture);
        assert!(
            hidden < area * 0.5,
            "a right-click at ({fx}, {fy}) parked the panel over {:.0}% of the picture",
            100.0 * hidden / area
        );
    }
}

/// Wherever it lands, the panel stays on the window. A floating surface that
/// runs off the edge is the one case where it stops being usable at all.
#[test]
fn the_panel_stays_inside_the_window() {
    let window = Rect::new(0.0, 0.0, VIEWPORT.0 as f64, VIEWPORT.1 as f64);

    for (fx, fy) in [
        (0.02, 0.02),
        (0.98, 0.02),
        (0.02, 0.98),
        (0.98, 0.98),
        (0.5, 0.5),
    ] {
        let (mut state, mut app, _dir) = ready();
        right_click_picture(&mut app, &mut state, fx, fy);
        assert!(app.remark_is_open(), "nothing opened at ({fx}, {fy})");
        let p = app.remark_rect();
        assert!(
            p.x0 >= window.x0 && p.y0 >= window.y0 && p.x1 <= window.x1 && p.y1 <= window.y1,
            "a right-click at ({fx}, {fy}) put the panel at {p:?}, outside {window:?}"
        );
    }
}

/// The menu beside it keeps working: a row runs the operation it names, and it
/// runs the same one the toolbar button does.
#[test]
fn a_menu_row_performs_its_operation() {
    let (mut state, mut app, _dir) = ready();
    right_click_picture(&mut app, &mut state, 0.34, 0.42);

    let label = photos::strings::tr("photos-rotate-right");
    let at = app
        .context_menu_point_for(&label)
        .expect("the menu offers a right turn, on a row that can be clicked");
    move_to(&mut app, &mut state, at);
    button(&mut app, &mut state, MouseButton::Left, ElementState::Pressed);

    assert_eq!(
        PhotosDoc::edits(&state.doc).rotate_quarters,
        1,
        "the menu row did not turn the photograph"
    );
}
