use super::*;
use crate::screen_reader::ScreenReader;
use crate::widget::TaffyTreeExt;
use crate::widget::{Button, Widget};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use tts::{NullSpeaker, Speaker, Utterance};

#[derive(Clone, Default)]
struct SharedRecorder(Rc<RefCell<NullSpeaker>>);
impl Speaker for SharedRecorder {
    fn speak_utterance(&mut self, u: Utterance) {
        self.0.borrow_mut().speak_utterance(u);
    }
    fn stop(&mut self) {
        self.0.borrow_mut().stop();
    }
    fn set_rate(&mut self, r: f32) {
        self.0.borrow_mut().set_rate(r);
    }
    fn set_voice(&mut self, v: &str) {
        self.0.borrow_mut().set_voice(v);
    }
    fn is_speaking(&self) -> bool {
        self.0.borrow().is_speaking()
    }
    fn backend_name(&self) -> &'static str {
        "shared-null"
    }
}
// SAFETY: test-only, single-threaded, so the !Send Rc is fine.
#[allow(
    clippy::non_send_fields_in_send_ty,
    reason = "the Rc<RefCell<_>> is deliberately asserted Send above — this test double \
              never actually crosses a thread"
)]
unsafe impl Send for SharedRecorder {}

struct TwoButtons {
    kids: Vec<Box<dyn Widget>>,
}
impl TwoButtons {
    fn new() -> Self {
        Self {
            kids: vec![
                Box::new(Button::new("a", "Save")),
                Box::new(Button::new("b", "Cancel")),
            ],
        }
    }
}
impl Widget for TwoButtons {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let kids: Vec<_> = self
            .kids
            .iter_mut()
            .map(|c| c.build_layout(taffy, theme, node_map))
            .collect();
        taffy.widget_node(taffy::Style::default(), &kids, node_map, self)
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.kids.iter().map(Box::as_ref).collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.kids
            .iter_mut()
            .map(|c| &mut **c as &mut (dyn Widget + '_))
            .collect()
    }
    fn name(&self) -> &'static str {
        "two-buttons"
    }
}

fn app_with_reader() -> (App, SharedRecorder) {
    let rec = SharedRecorder::default();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(TwoButtons::new()));
    app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
    (app, rec)
}

#[test]
fn reader_narrates_focus_and_obeys_its_enablement() {
    // Case `autofocus_narrates_the_focused_widget`.
    {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert_eq!(rec.0.borrow().spoken(), &["Save, button".to_string()]);
    }

    // Case `directional_focus_change_narrates_new_widget`.
    {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        let second = app.focusable_order[1];
        let ak_id = accesskit::NodeId(u64::from(second) + 1);
        app.dispatch_a11y_action(ak_id, &crate::a11y::Action::Focus);
        let spoken = rec.0.borrow();
        assert_eq!(
            spoken.spoken(),
            &["Save, button".to_string(), "Cancel, button".to_string()],
            "each focus change narrates the newly focused control"
        );
    }

    // Case `disabling_reader_silences_and_stops`.
    {
        let (mut app, rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert!(app.screen_reader_enabled());
        app.set_screen_reader_enabled(false);
        assert!(!app.screen_reader_enabled());
        assert!(
            rec.0.borrow().stop_count() >= 1,
            "disabling stops in-flight speech"
        );
        let before = rec.0.borrow().spoken().len();
        let second = app.focusable_order[1];
        let ak_id = accesskit::NodeId(u64::from(second) + 1);
        app.dispatch_a11y_action(ak_id, &crate::a11y::Action::Focus);
        assert_eq!(
            rec.0.borrow().spoken().len(),
            before,
            "no narration after the reader is disabled"
        );
    }

    // Case `reader_disabled_by_default_unless_env`.
    {
        let mut app = App::new(State::new(), Box::new(TwoButtons::new()));
        app.autofocus_first = true;
        app.init_layout();
        assert!(
            !app.screen_reader_enabled(),
            "the reader is opt-in; default-off builds are unaffected"
        );
    }

    // Case `screen_reader_key_layer_is_noop_when_disabled`.
    {
        // winit's KeyEvent has no public constructor; the disabled-guard is
        // the first line of the method, so a non-keyboard event exercises it.
        let mut app = App::new(State::new(), Box::new(TwoButtons::new()));
        app.init_layout();
        assert!(!app.screen_reader_enabled());
        let ev = WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        };
        assert!(
            !app.handle_screen_reader_key(&ev),
            "reader-off App must never consume an event in the SR key layer"
        );
    }

    // Case `screen_reader_key_layer_ignores_non_keyboard_when_enabled`.
    {
        let (mut app, _rec) = app_with_reader();
        app.init_layout();
        assert!(app.screen_reader_enabled());
        let ev = WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        };
        assert!(!app.handle_screen_reader_key(&ev));
    }
}

/// One live region whose spoken value is mutable between frames.
struct LiveWidget {
    name: String,
    value: Rc<RefCell<String>>,
    politeness: crate::a11y::Politeness,
}
impl LiveWidget {
    fn new(name: &str, politeness: crate::a11y::Politeness) -> Self {
        Self {
            name: name.to_string(),
            value: Rc::new(RefCell::new(String::new())),
            politeness,
        }
    }
}
impl Widget for LiveWidget {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "live-widget"
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let v = self.value.borrow().clone();
        let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Group)
            .with_name(self.name.clone())
            .with_live(self.politeness);
        if !v.is_empty() {
            node = node.with_value(v);
        }
        Some(node)
    }
}

fn app_with_reader_over(root: Box<dyn Widget>) -> (App, SharedRecorder) {
    let rec = SharedRecorder::default();
    let mut app = App::new_with_controlled_inputs(State::new(), root);
    app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
    app.init_layout();
    (app, rec)
}

#[test]
fn live_regions_announce_with_their_priority_only_when_the_reader_is_on() {
    // Case `live_region_change_announces_without_moving_focus`.
    {
        let live = LiveWidget::new("Notifications", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        let focus_before = app.focused_node;

        // Seed the snapshot, then forget it so we assert purely on the change.
        app.scan_live_regions();
        rec.0.borrow_mut().reset();

        *val.borrow_mut() = "File saved".to_string();
        app.scan_live_regions();
        assert_eq!(
            rec.0.borrow().last(),
            Some("Notifications, group, File saved"),
            "the changed live text is announced (name, role, value)"
        );
        assert_eq!(
            app.focused_node, focus_before,
            "live region must not move focus"
        );

        rec.0.borrow_mut().reset();
        app.scan_live_regions();
        assert!(
            rec.0.borrow().spoken().is_empty(),
            "unchanged live text is not re-announced"
        );
    }

    // Case `assertive_live_region_uses_important_interrupting_priority`.
    {
        let live = LiveWidget::new("Errors", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        *val.borrow_mut() = "Permission denied".to_string();
        app.scan_live_regions();
        let recref = rec.0.borrow();
        let last = recref
            .utterances()
            .last()
            .expect("an utterance was recorded");
        assert_eq!(last.priority, tts::Priority::Important);
        assert!(last.interrupt, "assertive live regions interrupt");
    }

    // Case `polite_live_region_uses_notification_non_interrupting_priority`.
    {
        let live = LiveWidget::new("Status", crate::a11y::Politeness::Polite);
        let val = live.value.clone();
        let (mut app, rec) = app_with_reader_over(Box::new(live));
        *val.borrow_mut() = "Uploading, 40%".to_string();
        app.scan_live_regions();
        let recref = rec.0.borrow();
        let last = recref
            .utterances()
            .last()
            .expect("an utterance was recorded");
        assert_eq!(last.priority, tts::Priority::Notification);
        assert!(!last.interrupt, "polite live regions do not interrupt");
    }

    // Case `live_region_scan_is_noop_when_reader_off`.
    {
        let live = LiveWidget::new("Notifications", crate::a11y::Politeness::Assertive);
        let val = live.value.clone();
        let mut app = App::new(State::new(), Box::new(live));
        app.init_layout();
        assert!(!app.screen_reader_enabled());
        *val.borrow_mut() = "anything".to_string();
        app.scan_live_regions();
    }
}

/// A widget that paints its own subtree and says so: its node carries the
/// live region as a child, as a card's footer sits under its dialog, with no
/// widget of its own to walk to.
struct CardWithFooter {
    text: Rc<RefCell<String>>,
}
impl Widget for CardWithFooter {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "card-with-footer"
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let text = self.text.borrow().clone();
        let footer = crate::a11y::AccessNode::new(crate::a11y::Role::StaticText)
            .with_name(text)
            .with_live(crate::a11y::Politeness::Polite);
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Dialog)
                .with_name("Card")
                .with_children(vec![footer]),
        )
    }
}

#[test]
fn a_live_region_inside_a_widgets_own_subtree_is_announced() {
    let text = Rc::new(RefCell::new("Thumbs up".to_string()));
    let card = CardWithFooter {
        text: Rc::clone(&text),
    };
    let (mut app, rec) = app_with_reader_over(Box::new(card));

    // Seed the snapshot so only the change is asserted.
    app.scan_live_regions();
    rec.0.borrow_mut().reset();

    *text.borrow_mut() = "Pizza".to_string();
    app.scan_live_regions();
    assert_eq!(
        rec.0.borrow().last(),
        Some("Pizza"),
        "the footer the card paints under its dialog is announced as it changes"
    );

    rec.0.borrow_mut().reset();
    app.scan_live_regions();
    assert!(
        rec.0.borrow().spoken().is_empty(),
        "unchanged live text is not re-announced"
    );
}

fn app_with_focused_input(initial: &str) -> (App, SharedRecorder, NodeId) {
    let rec = SharedRecorder::default();
    let input = crate::widget::TextInput::local().with_initial_value(initial);
    let mut app = App::new(State::new(), Box::new(input));
    app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
    app.autofocus_first = true;
    app.init_layout();
    let id = app.focused_node.expect("the input is focused at startup");
    (app, rec, id)
}

#[test]
fn caret_review_announces_characters_and_words_for_text_widgets() {
    // Case `caret_move_one_char_announces_the_character`.
    {
        let (mut app, rec, id) = app_with_focused_input("abc");
        let pre = app.text_review_snapshot();
        assert!(pre.is_some(), "focused text input yields a review snapshot");
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        unsafe {
            crate::undo::set_state(&mut (*(*app.layout.node_map.get(&id).unwrap())), "2");
        }
        app.announce_text_review(pre);
        assert_eq!(
            rec.0.borrow().last(),
            Some("c"),
            "single-step move echoes the char"
        );
    }

    // Case `caret_jump_to_start_announces_the_word`.
    {
        let (mut app, rec, id) = app_with_focused_input("hello world");
        let pre = app.text_review_snapshot();
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        unsafe {
            crate::undo::set_state(&mut (*(*app.layout.node_map.get(&id).unwrap())), "0");
        }
        app.announce_text_review(pre);
        assert_eq!(
            rec.0.borrow().last(),
            Some("hello"),
            "a long jump echoes the word"
        );
    }

    // Case `no_caret_move_announces_nothing`.
    {
        let (mut app, rec, _id) = app_with_focused_input("abc");
        let pre = app.text_review_snapshot();
        rec.0.borrow_mut().reset(); // forget the autofocus narration
        app.announce_text_review(pre);
        assert!(
            rec.0.borrow().spoken().is_empty(),
            "no move, no announcement"
        );
    }

    // Case `text_review_snapshot_is_none_for_non_text_widget`.
    {
        let (mut app, _rec) = app_with_reader();
        app.autofocus_first = true;
        app.init_layout();
        assert!(app.focused_node.is_some());
        assert!(
            app.text_review_snapshot().is_none(),
            "a button has no caret to review"
        );
    }
}

/// Ctrl+Super+Enter pressed, as winit delivers it.
#[cfg(feature = "native")]
fn ctrl_super_enter(app: &mut App) -> WindowEvent {
    use winit::keyboard::ModifiersState;
    app.input.modifiers =
        winit::event::Modifiers::from(ModifiersState::CONTROL | ModifiersState::SUPER);
    crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter)
}

#[cfg(feature = "native")]
#[test]
fn the_chord_toggles_the_reader_and_says_so_even_when_turning_it_off() {
    let (mut app, rec) = app_with_reader();
    app.init_layout();
    let told = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&told);
    app.on_screen_reader_chord(move |on| seen.borrow_mut().push(on));
    let before = rec.0.borrow().spoken().len();

    let chord = ctrl_super_enter(&mut app);
    assert!(app.handle_screen_reader_chord(&chord));
    assert!(!app.screen_reader_enabled());
    assert_eq!(
        rec.0.borrow().spoken().get(before..),
        Some(&["Screen reader off".to_string()][..]),
        "turning the reader off is heard"
    );
    assert!(
        app.accessibility.parting_screen_reader.is_some(),
        "the narrator saying so is not dropped mid-sentence"
    );
    assert!(app.handle_screen_reader_chord(&chord));
    assert!(app.screen_reader_enabled());
    assert!(app.accessibility.parting_screen_reader.is_none());
    assert_eq!(*told.borrow(), [false, true]);

    app.input.modifiers = winit::event::Modifiers::from(winit::keyboard::ModifiersState::CONTROL);
    let enter = crate::synthetic_input::named_key(winit::keyboard::NamedKey::Enter);
    assert!(!app.handle_screen_reader_chord(&enter));
    assert!(app.screen_reader_enabled());
}

/// A keypad that paints its keys itself and lists them as synthetic
/// buttons, as the passcode keyboard does: keyed, placed and pressed
/// through their accessible action.
struct Keypad {
    rect: vello::kurbo::Rect,
    pressed: Rc<RefCell<Vec<String>>>,
}

impl Keypad {
    const KEYS: [&'static str; 3] = ["1", "2", "3"];

    fn key_rect(&self, index: usize) -> vello::kurbo::Rect {
        let width = self.rect.width() / 3.0;
        #[allow(clippy::cast_precision_loss, reason = "a key's index, below four")]
        let x0 = self.rect.x0 + width * index as f64;
        vello::kurbo::Rect::new(x0, self.rect.y0, x0 + width, self.rect.y1)
    }
}

impl Widget for Keypad {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(300.0),
                height: taffy::Dimension::length(100.0),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
    fn name(&self) -> &'static str {
        "keypad"
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let keys = Self::KEYS
            .iter()
            .enumerate()
            .map(|(index, key)| {
                AccessNode::new(Role::Button)
                    .with_name(*key)
                    .with_hashed_key(*key)
                    .with_bounds(self.key_rect(index))
                    .with_state(AccessState::new().focusable(true))
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_name("Keypad")
                .with_children(keys)
                .with_keyed_action_routes("test.keypad"),
        )
    }
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        let ([key], crate::a11y::Action::Activate) = (key_path, action) else {
            return false;
        };
        let Some(name) = Self::KEYS
            .iter()
            .find(|name| crate::a11y::hash_key(*name) == *key)
        else {
            return false;
        };
        self.pressed.borrow_mut().push((*name).to_owned());
        true
    }
}

fn app_with_keypad() -> (App, SharedRecorder, Rc<RefCell<Vec<String>>>) {
    let pressed = Rc::new(RefCell::new(Vec::new()));
    // A Save button above a keypad.
    let root = crate::widget::Column::new()
        .push(Box::new(Button::new("save", "Save")))
        .push(Box::new(Keypad {
            rect: vello::kurbo::Rect::ZERO,
            pressed: Rc::clone(&pressed),
        }));
    let rec = SharedRecorder::default();
    let mut app = App::new_with_controlled_inputs(State::new(), Box::new(root));
    app.set_screen_reader(Some(ScreenReader::with_speaker(Box::new(rec.clone()))));
    app.init_layout();
    (app, rec, pressed)
}

/// What the reader said since `from`.
fn said_since(rec: &SharedRecorder, from: usize) -> Vec<String> {
    rec.0
        .borrow()
        .spoken()
        .get(from..)
        .unwrap_or_default()
        .to_vec()
}

#[cfg(feature = "native")]
#[test]
fn reading_next_and_previous_walks_into_a_widgets_synthetic_keys() {
    let (mut app, rec, _) = app_with_keypad();
    let insert = crate::synthetic_input::key_input(
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Insert),
        winit::event::ElementState::Pressed,
    );
    let next = crate::synthetic_input::named_key(winit::keyboard::NamedKey::ArrowRight);
    let prev = crate::synthetic_input::named_key(winit::keyboard::NamedKey::ArrowLeft);
    let start = rec.0.borrow().spoken().len();
    app.handle_screen_reader_key(&insert);
    for _ in 0..5 {
        assert!(app.handle_screen_reader_key(&next));
    }
    assert_eq!(
        said_since(&rec, start),
        [
            "Save, button",
            "Keypad, group",
            "1, button",
            "2, button",
            "3, button"
        ]
    );
    assert!(app.handle_screen_reader_key(&next));
    assert_eq!(
        said_since(&rec, start).len(),
        5,
        "the last key ends the order"
    );
    assert!(app.handle_screen_reader_key(&prev));
    assert_eq!(said_since(&rec, start).last().unwrap(), "2, button");
}

/// A touchscreen at twice the logical resolution, as a host that reads
/// the touchscreen itself shows the app.
#[cfg(feature = "native")]
#[derive(Default)]
struct Touchscreen {
    emulator: std::cell::RefCell<crate::interaction::TouchPointerEmulator>,
}

#[cfg(feature = "native")]
impl AppSurface for Touchscreen {
    fn scale_factor(&self) -> f64 {
        2.0
    }
    fn inner_size(&self) -> winit::dpi::PhysicalSize<u32> {
        winit::dpi::PhysicalSize::new(1200, 800)
    }
    fn request_redraw(&self) {}
    fn set_cursor(&self, _cursor: CursorIcon) {}
}

#[cfg(feature = "native")]
impl Touchscreen {
    /// Lay the app out on the screen.
    fn frame(&self, app: &mut App) {
        let mut scene = Scene::new();
        for _ in 0..3 {
            scene.reset();
            app.surface_frame(self, &mut scene);
        }
    }

    /// A finger at `(x, y)` logical pixels, `t` ms after `zero`, through
    /// the host's touch path. Whether the app took it.
    fn finger(
        &self,
        app: &mut App,
        zero: crate::time::Instant,
        (id, phase): (u64, winit::event::TouchPhase),
        (x, y): (f64, f64),
        t: u64,
    ) -> bool {
        let touch = winit::event::Touch {
            device_id: winit::event::DeviceId::dummy(),
            phase,
            location: winit::dpi::PhysicalPosition::new(x * 2.0, y * 2.0),
            force: None,
            id,
        };
        let at = zero + Duration::from_millis(t);
        crate::time::testing::with_monotonic_time(at, || {
            app.surface_touch(self, touch, &mut self.emulator.borrow_mut())
        })
    }
}

#[cfg(feature = "native")]
#[test]
fn touch_explores_keys_without_pressing_them_and_a_double_tap_presses_the_one_read() {
    use winit::event::TouchPhase::{Ended, Moved, Started};
    let (mut app, rec, pressed) = app_with_keypad();
    let screen = Touchscreen::default();
    screen.frame(&mut app);
    let finger = |app: &mut App, zero, contact, at, t| screen.finger(app, zero, contact, at, t);
    let zero = crate::time::now();
    let tick = |app: &mut App, t: u64| app.tick_screen_reader(zero + Duration::from_millis(t));
    app.refresh_semantic_tree();
    let key = |app: &App, name: &str| {
        app.semantic_tree()
            .find_by_name(name)
            .and_then(|node| node.bounds)
            .map(|rect| (rect.center().x, rect.center().y))
            .unwrap()
    };
    let (one, two, three) = (key(&app, "1"), key(&app, "2"), key(&app, "3"));
    let start = rec.0.borrow().spoken().len();

    // A resting finger reads the key under it once the delay is up.
    assert!(finger(&mut app, zero, (1, Started), two, 0));
    assert_eq!(
        app.screen_reader_deadline(),
        Some(zero + crate::screen_reader_touch::EXPLORE_DELAY)
    );
    assert!(!tick(&mut app, 100));
    assert!(tick(&mut app, 200));
    assert_eq!(said_since(&rec, start), ["2, button"]);
    // Dragging reads each key once as the finger reaches it.
    finger(&mut app, zero, (1, Moved), (two.0 + 5.0, two.1), 250);
    finger(&mut app, zero, (1, Moved), three, 300);
    finger(&mut app, zero, (1, Moved), (three.0 + 5.0, three.1), 350);
    finger(&mut app, zero, (1, Ended), three, 400);
    assert_eq!(said_since(&rec, start), ["2, button", "3, button"]);
    assert!(pressed.borrow().is_empty(), "exploring presses nothing");

    // A double tap far from the key presses the key the cursor is on.
    finger(&mut app, zero, (2, Started), (10.0, 10.0), 1_000);
    finger(&mut app, zero, (2, Ended), (10.0, 10.0), 1_060);
    finger(&mut app, zero, (3, Started), (12.0, 10.0), 1_200);
    finger(&mut app, zero, (3, Ended), (12.0, 10.0), 1_260);
    assert_eq!(*pressed.borrow(), ["3"]);
    assert!(!tick(&mut app, 3_000), "the double tap's taps read nothing");

    // A swipe left moves to the previous key.
    finger(&mut app, zero, (4, Started), (200.0, 300.0), 4_000);
    finger(&mut app, zero, (4, Moved), (150.0, 300.0), 4_050);
    finger(&mut app, zero, (4, Ended), (100.0, 300.0), 4_100);
    assert_eq!(said_since(&rec, start).last().unwrap(), "2, button");

    // A tap reads the key under it once no second tap follows.
    finger(&mut app, zero, (5, Started), one, 5_000);
    finger(&mut app, zero, (5, Ended), one, 5_050);
    assert!(tick(&mut app, 5_400));
    assert_eq!(said_since(&rec, start).last().unwrap(), "1, button");
    assert_eq!(*pressed.borrow(), ["3"]);

    // With the reader off, touch is the widgets' again.
    app.set_screen_reader_enabled(false);
    assert!(!finger(&mut app, zero, (6, Started), two, 6_000));
    assert_eq!(app.screen_reader_deadline(), None);
}
