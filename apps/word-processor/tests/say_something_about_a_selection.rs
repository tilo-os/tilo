//! Right-click a selection in the document and say something about it.
//!
//! `src/reference.rs` proves that a selection *can* be turned into an address.
//! This proves the gesture reaches it: a real right-click, through
//! `Widget::handle_event`, over a document that was laid out and drawn, with
//! the compose surface coming up beside the ordinary context menu carrying
//! that paragraph's address.
//!
//! Everything here goes through the production path. Nothing calls the
//! compose widget directly — if the editor stopped wiring it into the
//! right-click, or stopped routing keystrokes to it, these fail.

use std::collections::HashMap;

use taffy::prelude::*;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{DestinationOption, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey, NativeKeyCode, PhysicalKey};

use qos_reference::{Delivery, Destination, Target};
use word_processor::WordProcessorWidget;

const VIEWPORT: (f32, f32) = (1400.0, 860.0);

/// The paragraph the tests select inside, and the run they select.
const PARAGRAPH: &str =
    "Sound check runs from four to five. The desk is locked after that, so anything the band wants changed has to be said before then.";
const PHRASE: &str = "The desk is locked after that";

/// A document with a heading and three paragraphs, typed in through the
/// editing model the keyboard drives.
fn document() -> (ui::State, WordProcessorWidget) {
    let mut state = ui::State::new();
    let mut wp = WordProcessorWidget::new();
    wp.sync(&mut state.doc, &state.root);

    let blocks: [(&str, &str); 4] = [
        ("heading", "Run of show"),
        (
            "paragraph",
            "Doors at seven, the support act at half past, and the hall is ours until eleven.",
        ),
        ("paragraph", PARAGRAPH),
        ("paragraph", "Two hundred and forty seats are sold; the balcony stays shut."),
    ];
    for (i, (kind, text)) in blocks.iter().enumerate() {
        if i > 0 {
            wp.model_mut().enter(&mut state.doc);
        }
        let level = if *kind == "heading" { Some(1) } else { None };
        wp.model_mut().set_paragraph_style(&mut state.doc, kind, level, None);
        wp.model_mut().insert_str(&mut state.doc, text);
    }
    wp.sync(&mut state.doc, &state.root);
    (state, wp)
}

/// The destinations a host would supply. Structured values, so the assertions
/// can be made about where a message goes rather than about how it reads.
fn destinations() -> Vec<DestinationOption> {
    vec![
        DestinationOption::new(Destination::channel("c_run", "run-of-show"), "# run-of-show"),
        DestinationOption::new(
            Destination::participant("u_xaver", "Xaver", false),
            "Xaver — private",
        ),
    ]
}

/// Lay the widget out at the viewport size and draw one frame.
///
/// The draw is not decoration: the shaped geometry a click resolves a caret
/// against is produced by drawing, exactly as it is in a running session.
fn lay_out_and_draw(wp: &mut WordProcessorWidget, state: &mut ui::State) {
    let (w, h) = VIEWPORT;
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    wp.sync(&mut state.doc, &state.root);
    let child = wp.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size { width: Dimension::length(w), height: Dimension::length(h) },
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
    wp.apply_layout(&taffy, child, Point::ORIGIN);
    let mut scene = Scene::new();
    wp.draw_with_presence(
        &mut scene,
        Affine::IDENTITY,
        &mut tr,
        &theme,
        &PresenceCtx::empty(),
        &PermissionContext::permissive(),
    );
}

/// Select [`PHRASE`] inside the third block, through the caret movements a
/// keyboard produces.
fn select_the_phrase(state: &mut ui::State, wp: &mut WordProcessorWidget) {
    let byte = PARAGRAPH.find(PHRASE).expect("the phrase is in the paragraph");
    let start = PARAGRAPH[..byte].chars().count();
    let len = PHRASE.chars().count();
    let doc = &state.doc;
    let m = wp.model_mut();
    m.move_to_block(doc, 2);
    m.move_home();
    m.clear_anchor();
    for _ in 0..start {
        m.move_right(doc);
    }
    m.set_anchor();
    for _ in 0..len {
        m.move_right(doc);
    }
}

fn dispatch(wp: &mut WordProcessorWidget, state: &mut ui::State, event: &WindowEvent, mods: &Modifiers) {
    let taffy: TaffyTree<()> = TaffyTree::new();
    let node = NodeId::new(0);
    let mut handled = false;
    let mut focus: Option<NodeId> = None;
    wp.handle_event(
        event, 1.0, mods, &mut state.doc, &state.root, &mut handled, &mut focus, node, &taffy,
    );
}

/// A real secondary press at `pt`, preceded by the pointer move that carries
/// the position — the editor tracks the cursor, it does not read the press.
fn right_click(wp: &mut WordProcessorWidget, state: &mut ui::State, pt: Point) {
    let mods = Modifiers::default();
    dispatch(
        wp,
        state,
        &WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: (pt.x, pt.y).into(),
        },
        &mods,
    );
    dispatch(
        wp,
        state,
        &WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Right,
        },
        &mods,
    );
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

fn press(wp: &mut WordProcessorWidget, state: &mut ui::State, logical: Key, text: Option<&str>, mods: &Modifiers) {
    dispatch(
        wp,
        state,
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: key_event(logical, text),
            is_synthetic: false,
        },
        mods,
    );
}

fn type_str(wp: &mut WordProcessorWidget, state: &mut ui::State, s: &str) {
    let mods = Modifiers::default();
    for ch in s.chars() {
        let t = ch.to_string();
        press(wp, state, Key::Character(t.as_str().into()), Some(&t), &mods);
    }
}

/// The id the document itself publishes for the selected block, which is what
/// the remark's address must equal.
fn selected_block_address(state: &ui::State, wp: &WordProcessorWidget) -> String {
    let (block, _, _) = wp
        .model()
        .selection_blocks(&state.doc)
        .into_iter()
        .next()
        .expect("something is selected");
    format!("word-processor.block:{block}")
}

/// A right-click on a selection opens the compose surface, and what it carries
/// is the block's address — not the words that were selected, and not the chip
/// label a person reads.
#[test]
fn a_right_click_on_a_selection_opens_a_compose_carrying_its_address() {
    let (mut state, mut wp) = document();
    select_the_phrase(&mut state, &mut wp);
    wp.set_remark_destinations(destinations());
    lay_out_and_draw(&mut wp, &mut state);

    let expected = selected_block_address(&state, &wp);
    let at = wp.debug_selection_point();
    right_click(&mut wp, &mut state, at);

    assert!(wp.remark_is_open(), "the compose surface did not open");
    let r = wp.remark_reference().expect("an open compose is about something");
    assert_eq!(
        r.resource_id().as_str(),
        expected,
        "the remark must carry the block's address"
    );
    let span = r.span.expect("a partial selection carries its range");
    assert_eq!(
        span.end - span.start,
        PHRASE.chars().count(),
        "the span must cover the run that was selected"
    );
    assert!(
        !r.label.contains(&expected),
        "the label is for a person and is never the address, was {:?}",
        r.label
    );
}

/// The right-click keeps the selection it landed in. Seating the caret at the
/// click would throw away the subject of both the menu and the compose.
#[test]
fn a_right_click_inside_a_selection_keeps_it() {
    let (mut state, mut wp) = document();
    select_the_phrase(&mut state, &mut wp);
    wp.set_remark_destinations(destinations());
    lay_out_and_draw(&mut wp, &mut state);

    let at = wp.debug_selection_point();
    right_click(&mut wp, &mut state, at);

    assert_eq!(
        wp.model().selection_text(&state.doc),
        PHRASE,
        "the selection under the click was lost"
    );
}

/// The roster is the host's. Until one arrives the gesture is not offered —
/// the editor must never invent somewhere to send to.
#[test]
fn nothing_opens_until_a_host_says_where_a_remark_could_go() {
    let (mut state, mut wp) = document();
    select_the_phrase(&mut state, &mut wp);
    lay_out_and_draw(&mut wp, &mut state);

    let at = wp.debug_selection_point();
    right_click(&mut wp, &mut state, at);

    assert!(
        !wp.remark_is_open(),
        "a compose opened over a document with nowhere to send"
    );
}

/// A right-click that had to invent the selection does not raise the surface.
///
/// The editor snaps a selection around the word under a bare right-click so
/// Cut and Copy have something to act on — but that is the editor's guess, not
/// somebody pointing at something they want to talk about. Raising a message
/// box every time a misspelling is right-clicked would make the gesture noise.
#[test]
fn a_right_click_that_had_to_pick_the_word_itself_does_not_raise_the_surface() {
    let (mut state, mut wp) = document();
    wp.set_remark_destinations(destinations());
    lay_out_and_draw(&mut wp, &mut state);
    wp.model_mut().clear_anchor();
    assert!(!wp.model().has_selection());

    // Straight at a word in the body, with nothing selected first.
    right_click(&mut wp, &mut state, Point::new(400.0, 320.0));

    assert!(
        wp.model().has_selection(),
        "the menu still snaps a selection around the clicked word"
    );
    assert!(!wp.remark_is_open(), "a compose opened over a selection nobody made");
}

/// Typing lands in the message, not in the document, and `Enter` hands the
/// host a remark addressed to the destination the selector is showing.
#[test]
fn what_is_typed_reaches_the_message_and_enter_sends_it() {
    let (mut state, mut wp) = document();
    select_the_phrase(&mut state, &mut wp);
    wp.set_remark_destinations(destinations());
    lay_out_and_draw(&mut wp, &mut state);

    let expected = selected_block_address(&state, &wp);
    let before = wp.model().current_text(&state.doc);
    let at = wp.debug_selection_point();
    right_click(&mut wp, &mut state, at);
    assert!(wp.remark_is_open());

    type_str(&mut wp, &mut state, "who has the key");
    assert_eq!(
        wp.model().current_text(&state.doc),
        before,
        "the message was typed into the document instead of into the compose"
    );

    press(&mut wp, &mut state, Key::Named(NamedKey::Enter), None, &Modifiers::default());

    let remark = wp.take_remark().expect("Enter sends the remark");
    assert_eq!(remark.text, "who has the key");
    assert_eq!(remark.reference.resource_id().as_str(), expected);
    // Asserted on the routing, not on the words the selector renders.
    assert_eq!(
        remark.delivery(),
        Delivery::Channel { channel: "c_run".to_string() }
    );
    assert!(!wp.remark_is_open(), "sending closes the surface");
    assert!(wp.take_remark().is_none(), "a remark is handed over exactly once");
}

/// Escape puts the compose away without sending, and without touching the
/// document underneath.
#[test]
fn escape_dismisses_without_sending() {
    let (mut state, mut wp) = document();
    select_the_phrase(&mut state, &mut wp);
    wp.set_remark_destinations(destinations());
    lay_out_and_draw(&mut wp, &mut state);

    let at = wp.debug_selection_point();
    right_click(&mut wp, &mut state, at);
    type_str(&mut wp, &mut state, "never mind");
    press(&mut wp, &mut state, Key::Named(NamedKey::Escape), None, &Modifiers::default());

    assert!(!wp.remark_is_open(), "Escape must dismiss the compose");
    assert!(wp.take_remark().is_none(), "a dismissed compose sends nothing");
}

/// The chord raises the same surface over the same selection, minting the
/// same address — the gesture is not mouse-only.
#[test]
fn the_chord_raises_the_same_surface_as_the_right_click() {
    let mods: Modifiers =
        (winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT).into();

    let (mut state, mut wp) = document();
    select_the_phrase(&mut state, &mut wp);
    wp.set_remark_destinations(destinations());
    lay_out_and_draw(&mut wp, &mut state);
    let expected = selected_block_address(&state, &wp);

    press(&mut wp, &mut state, Key::Character("m".into()), Some("m"), &mods);

    assert!(wp.remark_is_open(), "Ctrl+Shift+M did not raise the compose");
    assert_eq!(
        wp.remark_reference().expect("about something").resource_id().as_str(),
        expected,
        "the chord must mint the same address the pointer does"
    );
    // And it is showing where it would go before anything is typed.
    assert!(matches!(
        wp.remark_destination().map(|d| d.target()),
        Some(Target::Channel("c_run"))
    ));
}
