use super::*;
use crate::screen_reader::ScreenReader;
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
        let node = taffy
            .new_with_children(taffy::Style::default(), &kids)
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
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
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
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

// ----- Live regions -----

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
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _s: &mut Scene,
        _t: Affine,
        _tr: &mut TextRenderer,
        _th: &Theme,
        _p: &PresenceCtx,
    ) {
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![]
    }
    fn name(&self) -> &'static str {
        "live-widget"
    }
    fn get_rect(&self) -> KurboRect {
        KurboRect::ZERO
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

// ----- Text review (caret movement over existing content) -----

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
