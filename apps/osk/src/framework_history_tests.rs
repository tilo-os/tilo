use super::*;
use tilo_automerge::ApplyIncoming as _;
use ui::widget::{AppModel, ModelPath};

fn keyboard() -> OskWidget {
    OskWidget::new_with_swipe_model(Box::new(GeometricSwipeModel::english()))
}

fn put_preferences(doc: &mut AutoCommit, layout: &str, suggestions: bool, personalization: bool) {
    doc.put(automerge::ROOT, CONFIG_LAYOUT_KEY, layout).unwrap();
    doc.put(automerge::ROOT, CONFIG_SUGGESTIONS_KEY, suggestions)
        .unwrap();
    doc.put(automerge::ROOT, CONFIG_PERSONALIZATION_KEY, personalization)
        .unwrap();
}

fn saved_layout(doc: &AutoCommit) -> Option<String> {
    read_config_text(doc, &automerge::ROOT, CONFIG_LAYOUT_KEY)
}

fn saved_suggestions(doc: &AutoCommit) -> Option<bool> {
    read_config_bool(doc, &automerge::ROOT, CONFIG_SUGGESTIONS_KEY)
}

#[test]
fn framework_history_absence_adopts_each_default_without_reseeding() {
    for key in [
        CONFIG_LAYOUT_KEY,
        CONFIG_SUGGESTIONS_KEY,
        CONFIG_PERSONALIZATION_KEY,
    ] {
        let mut keyboard = keyboard();
        let mut doc = AutoCommit::new();
        put_preferences(&mut doc, "azerty", false, false);
        doc.commit();
        keyboard.sync(&mut doc, &automerge::ROOT);
        doc.delete(automerge::ROOT, key).unwrap();
        doc.commit();
        let heads = doc.get_heads();
        keyboard.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            keyboard.preferred_text_layout,
            if key == CONFIG_LAYOUT_KEY {
                "qwerty"
            } else {
                "azerty"
            }
        );
        assert_eq!(keyboard.suggestions_enabled, key == CONFIG_SUGGESTIONS_KEY);
        assert_eq!(
            keyboard.personalization_enabled,
            key == CONFIG_PERSONALIZATION_KEY
        );
        assert_eq!(doc.get_heads(), heads);
        assert!(doc.get(automerge::ROOT, key).unwrap().is_none());
    }
}

#[test]
fn framework_history_pending_setter_never_writes_unrelated_roots_from_sync() {
    let mut keyboard = keyboard();
    let mut doc = AutoCommit::new();
    keyboard.set_suggestions_enabled(false);
    keyboard.sync(&mut doc, &automerge::ROOT);
    assert!(
        doc.get(automerge::ROOT, CONFIG_LAYOUT_KEY)
            .unwrap()
            .is_none(),
        "sync cannot overwrite unrelated layout with cached defaults"
    );
    assert_eq!(
        doc.pending_ops(),
        0,
        "drafts must pass through Document action admission"
    );
    assert!(!keyboard.suggestions_enabled());
}

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use ui::headless::test_support::select_timeline_entry;
use ui::history_mode::*;
use ui::tilo_actions::{ActionAddress, ControlInvocation, EncodedAction};

struct LiveFixture {
    keyboard: Rc<RefCell<OskWidget>>,
    queue: Rc<RefCell<VecDeque<ControlInvocation>>>,
    events: Rc<RefCell<VecDeque<String>>>,
    event_count: Rc<Cell<usize>>,
    acknowledgements: Rc<Cell<usize>>,
    reentrant: Option<Rc<RefCell<ReentrantAck>>>,
}
impl LiveFixture {
    fn new(keyboard: Rc<RefCell<OskWidget>>, acknowledgements: Rc<Cell<usize>>) -> Self {
        Self {
            keyboard,
            queue: Rc::default(),
            events: Rc::default(),
            event_count: Rc::default(),
            acknowledgements,
            reentrant: None,
        }
    }
}
impl Widget for LiveFixture {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
    );

    fn sync(&mut self, d: &mut AppModel, p: &ModelPath) {
        self.keyboard.borrow_mut().sync(d, p);
    }
    fn update(&mut self, d: &mut AppModel, p: &ModelPath) -> bool {
        let (d, p) = ui::doc::document(d, p);
        while let Some(event) = self.events.borrow_mut().pop_front() {
            self.keyboard.borrow_mut().on_external_event(&event, d, p);
            self.event_count.set(self.event_count.get() + 1);
        }
        false
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &["osk.suggestions"]
    }
    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        self.keyboard.borrow_mut().start_operation(op, doc, path)
    }
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        h: &Theme,
        n: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.keyboard.borrow_mut().build_layout(t, h, n)
    }
    fn apply_layout(&mut self, t: &TaffyTree<()>, n: NodeId, p: Point) {
        self.keyboard.borrow_mut().apply_layout(t, n, p);
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.keyboard
            .borrow_mut()
            .draw(scene, transform, text, theme, presence);
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.keyboard.borrow_mut().handle_event(event, ctx);
    }
    fn name(&self) -> &'static str {
        "live-osk-fixture"
    }
    fn get_rect(&self) -> Rect {
        self.keyboard.borrow().get_rect()
    }
    fn accessibility(&self) -> Option<AccessNode> {
        self.keyboard.borrow().accessibility()
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.keyboard.borrow_mut().take_structure_dirty()
    }
}

impl ui::resources::PublishesResources for LiveFixture {
    fn resources(&self, d: &AutoCommit) -> Vec<Resource> {
        self.keyboard.borrow().resources(d)
    }
}

impl ui::widget::controls::ActionControls for LiveFixture {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.queue
            .borrow_mut()
            .pop_front()
            .or_else(|| self.keyboard.borrow_mut().take_control_invocation())
    }
}

impl ui::history_mode::capability::DocumentHistory for LiveFixture {
    fn history_restore_readiness(&self, scope: &str) -> Result<(), String> {
        self.keyboard.borrow().history_restore_readiness(scope)
    }
    fn document_history_definition(&self) -> Option<&'static HistoryDefinition> {
        self.keyboard.borrow().document_history_definition()
    }
    fn document_action_applied(
        &mut self,
        a: &EncodedAction,
        b: &[automerge::ChangeHash],
        d: &AutoCommit,
        p: &ObjId,
    ) {
        self.acknowledgements.set(self.acknowledgements.get() + 1);
        let mut keyboard = self.keyboard.borrow_mut();
        if let Some(probe) = &self.reentrant {
            let mut probe = probe.borrow_mut();
            probe
                .envelopes
                .push((a.clone(), b.to_vec(), d.base_heads()));
            if probe.supersede && probe.envelopes.len() == 1 {
                assert_no_reentrant_offer(&mut keyboard, d);
                assert!(keyboard.select_text_layout("qwerty"));
                keyboard.set_suggestions_enabled(true);
                assert_no_reentrant_offer(&mut keyboard, d);
            }
            keyboard.document_action_applied(a, b, d, p);
            assert_no_reentrant_offer(&mut keyboard, d);
            if probe.supersede && probe.envelopes.len() <= 3 {
                assert!(
                    keyboard.drafts.has(0),
                    "newer same-field draft survives old ack"
                );
                assert!(
                    keyboard.drafts.has(1),
                    "unrelated newer draft survives old batch"
                );
            }
        } else {
            keyboard.document_action_applied(a, b, d, p);
        }
    }
}
struct Fixture {
    driver: ui::headless::Driver,
    keyboard: Rc<RefCell<OskWidget>>,
    queue: Rc<RefCell<VecDeque<ControlInvocation>>>,
    events: Rc<RefCell<VecDeque<String>>>,
    event_count: Rc<Cell<usize>>,
    acknowledgements: Rc<Cell<usize>>,
}
fn driver(doc: AutoCommit, keyboard: OskWidget) -> Fixture {
    let definition = ui::history_mode::capability::document_history_definition(&keyboard).unwrap();
    let keyboard = Rc::new(RefCell::new(keyboard));
    let acknowledgements = Rc::new(Cell::new(0));
    let root = LiveFixture::new(keyboard.clone(), acknowledgements.clone());
    let (queue, events, event_count) = (
        root.queue.clone(),
        root.events.clone(),
        root.event_count.clone(),
    );
    let registration = HistoryRegistration::new(
        definition,
        vec![HistorySourceRegistration::HostDocument {
            scope: "preferences",
            resource: HistoryResource::new(
                "osk-fixture",
                "osk-lifetime",
                definition.app_id,
                ui::RecordId::for_tab(351),
            ),
        }],
    )
    .unwrap();
    let mut driver = ui::headless::Driver::from_managed_root(
        ui::State::from_doc(doc),
        ManagedRoot::new(Box::new(root), registration),
        (900, 650),
    )
    .unwrap();
    crate::actions::register(driver.action_registry_mut());
    Fixture {
        driver,
        keyboard,
        queue,
        events,
        event_count,
        acknowledgements,
    }
}
fn document() -> (AutoCommit, automerge::ChangeHash) {
    let mut doc = AutoCommit::new();
    put_preferences(&mut doc, "azerty", false, false);
    doc.commit();
    let selected = doc.get_heads()[0];
    put_preferences(&mut doc, "qwerty", true, true);
    doc.put(automerge::ROOT, "unrelated", "live only").unwrap();
    doc.commit();
    (doc, selected)
}
/// Open History, select `hash` and review its restore, stopping before the
/// confirming Enter.
fn review_restore(driver: &mut ui::headless::Driver, hash: automerge::ChangeHash) {
    driver.click_named("history-open").unwrap();
    select_timeline_entry(driver, hash);
    driver.click_named("history-restore").unwrap();
}
fn restore(driver: &mut ui::headless::Driver, hash: automerge::ChangeHash) {
    review_restore(driver, hash);
    driver.key("Enter");
}
fn invoke(f: &mut Fixture, resource: &str, action: &str) {
    f.queue.borrow_mut().push_back(ControlInvocation {
        address: ActionAddress::new(resource, action),
        arguments: serde_json::Map::default(),
    });
    f.driver.key("Shift");
}
#[test]
fn framework_history_host_pending_draft_refuses_then_settles_once_and_cannot_overwrite_restore() {
    let (doc, selected) = document();
    let mut f = driver(doc, keyboard());
    review_restore(&mut f.driver, selected);
    let heads = f.driver.state_mut().doc.base_heads();
    f.keyboard.borrow_mut().set_suggestions_enabled(false);
    f.driver.key("Enter");
    assert_eq!(f.driver.state_mut().doc.base_heads(), heads);
    assert!(
        f.driver
            .inspection_json()
            .unwrap()
            .contains("Return to live to finish pending keyboard preferences")
    );
    assert!(f.driver.a11y_node_named("Restore version").is_none());
    assert!(f.keyboard.borrow().drafts.pending());
    assert_eq!(f.acknowledgements.get(), 0);
    f.driver.key("Escape");
    f.driver.click_named("history-back-live").unwrap();
    // Return-live's post-discard sync prepares the retained draft; ordinary
    // current host tick dispatches it (no manual latch/draft manipulation).
    f.driver.key("Shift");
    assert_eq!(f.acknowledgements.get(), 1);
    assert!(!f.keyboard.borrow().drafts.pending());
    assert_eq!(saved_suggestions(&f.driver.state_mut().doc), Some(false));
    let changes = f.driver.state_mut().doc.get_changes(&heads);
    assert_eq!(changes.len(), 1);
    let context =
        ui::tilo_actions::ActionContext::from_change_message(changes[0].message().unwrap())
            .unwrap();
    assert_eq!(context.action_label, "Set suggestions");
    assert_eq!(context.resource.unwrap().id, "osk.keyboard.suggestions");
    f.driver.key("Shift");
    assert_eq!(f.acknowledgements.get(), 1);
    restore(&mut f.driver, selected);
    assert_eq!(
        saved_layout(&f.driver.state_mut().doc).as_deref(),
        Some("azerty")
    );
    assert_eq!(f.keyboard.borrow().preferred_text_layout, "azerty");
    assert!(!f.keyboard.borrow().personalization_enabled);
    f.driver.click_named("history-back-live").unwrap();
    f.driver.key("Shift");
    assert_eq!(f.acknowledgements.get(), 1);
    assert!(!f.keyboard.borrow().drafts.pending());
}
#[test]
fn framework_history_granular_actions_noop_settlement_failure_retry_and_supersession() {
    let mut f = driver(AutoCommit::new(), keyboard());
    f.keyboard.borrow_mut().set_suggestions_enabled(false);
    f.driver.relayout();
    // A resource operation is newer than this field's retained setter.
    invoke(&mut f, "osk.keyboard.suggestions", "enable");
    assert!(!f.keyboard.borrow().drafts.pending());
    assert!(f.keyboard.borrow().suggestions_enabled);
    assert!(
        f.driver
            .state_mut()
            .doc
            .get(automerge::ROOT, CONFIG_LAYOUT_KEY)
            .unwrap()
            .is_none()
    );
    assert!(
        f.driver
            .state_mut()
            .doc
            .get(automerge::ROOT, CONFIG_PERSONALIZATION_KEY)
            .unwrap()
            .is_none()
    );
    // Explicit no-op acknowledgement settles a nondefault retained draft.
    invoke(&mut f, "osk.keyboard.suggestions", "disable");
    let heads = f.driver.state_mut().doc.base_heads();
    let acks = f.acknowledgements.get();
    f.keyboard.borrow_mut().set_suggestions_enabled(true);
    f.keyboard.borrow_mut().set_suggestions_enabled(false);
    f.driver.relayout();
    f.driver.key("Shift");
    assert_eq!(f.acknowledgements.get(), acks + 1);
    assert!(!f.keyboard.borrow().drafts.pending());
    assert_eq!(f.driver.state_mut().doc.base_heads(), heads);
    assert!(f.driver.state_mut().undo(None, None).is_some());
    assert_eq!(
        saved_suggestions(&f.driver.state_mut().doc),
        Some(true),
        "no-op settlement adds no undo boundary"
    );
    // A missing decoder is a failed real dispatch, never an acknowledgement.
    let mut failed = driver(AutoCommit::new(), keyboard());
    *failed.driver.action_registry_mut() = ui::tilo_actions::ActionRegistry::new();
    failed
        .keyboard
        .borrow_mut()
        .set_personalization_enabled(false);
    failed.driver.relayout();
    failed.driver.key("Shift");
    assert!(failed.keyboard.borrow().drafts.pending());
    assert_eq!(failed.acknowledgements.get(), 0);
    assert!(
        failed
            .driver
            .state_mut()
            .doc
            .get(automerge::ROOT, CONFIG_PERSONALIZATION_KEY)
            .unwrap()
            .is_none()
    );
    crate::actions::register(failed.driver.action_registry_mut());
    failed.driver.key("Shift");
    assert_eq!(failed.acknowledgements.get(), 1);
    assert!(!failed.keyboard.borrow().drafts.pending());
}
#[test]
fn framework_history_finite_drain_stale_lifetime_and_newer_field_survive_old_ack() {
    let mut root = keyboard();
    let mut doc = AutoCommit::new();
    root.select_text_layout("azerty");
    root.set_suggestions_enabled(false);
    root.set_personalization_enabled(false);
    root.sync(&mut doc, &automerge::ROOT);
    let mut offers = Vec::new();
    while let Some(offer) = ui::widget::controls::take_control_invocation(&mut root) {
        offers.push(offer);
        assert!(offers.len() <= 3);
    }
    assert_eq!(offers.len(), 3);
    assert!(root.drafts.pending());
    ui::widget::controls::refresh_control_action_availability(
        &mut root,
        &std::collections::HashSet::default(),
    );
    root.set_suggestions_enabled(true);
    assert!(
        ui::widget::controls::take_control_invocation(&mut root).is_none(),
        "availability and reentrant setter never rearm consumed offers"
    );
    let old = &offers[1];
    let encoded = ui::tilo_actions::door::encode_json(
        &ui::resources::resources(&root, &doc),
        &old.address,
        &old.arguments,
    )
    .unwrap();
    let mut registry = ui::tilo_actions::ActionRegistry::new();
    crate::actions::register(&mut registry);
    registry.decode(&encoded).unwrap().apply(&mut doc).unwrap();
    ui::history_mode::capability::document_action_applied(
        &mut root,
        &encoded,
        &[],
        &doc,
        &automerge::ROOT,
    );
    assert!(root.suggestions_enabled);
    assert!(root.drafts.has(1));
    assert!(root.drafts.has(0));
    assert!(root.drafts.has(2));
    let mut replacement = keyboard();
    replacement.set_suggestions_enabled(false);
    ui::history_mode::capability::document_action_applied(
        &mut replacement,
        &encoded,
        &[],
        &doc,
        &automerge::ROOT,
    );
    assert!(
        replacement.drafts.pending(),
        "old lifetime acknowledgement cannot settle replacement root"
    );
    root.sync(&mut doc, &automerge::ROOT);
    assert!(ui::widget::controls::take_control_invocation(&mut root).is_some());
}
#[test]
fn framework_history_shared_host_restore_undo_and_absence_preserve_unrelated_roots() {
    for deleted in [
        None,
        Some(CONFIG_LAYOUT_KEY),
        Some(CONFIG_SUGGESTIONS_KEY),
        Some(CONFIG_PERSONALIZATION_KEY),
    ] {
        let (mut doc, _) = document();
        put_preferences(&mut doc, "azerty", false, false);
        if let Some(key) = deleted {
            doc.delete(automerge::ROOT, key).unwrap();
        }
        doc.commit();
        let selected = doc.get_heads()[0];
        put_preferences(&mut doc, "qwerty", true, true);
        doc.commit();
        let mut f = driver(doc, keyboard());
        let heads = f.driver.state_mut().doc.base_heads();
        restore(&mut f.driver, selected);
        let doc = &mut f.driver.state_mut().doc;
        assert_eq!(doc.get_changes(&heads).len(), 1);
        assert_eq!(
            doc.get(automerge::ROOT, "unrelated")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("live only")
        );
        if let Some(key) = deleted {
            assert!(doc.get(automerge::ROOT, key).unwrap().is_none());
        }
        let bytes = doc.save();
        let mut reopened = AutoCommit::load(&bytes).unwrap();
        let before = reopened.get_heads();
        let mut fresh = keyboard();
        fresh.sync(&mut reopened, &automerge::ROOT);
        assert_eq!(reopened.get_heads(), before);
        assert_eq!(reopened.pending_ops(), 0);
        assert_eq!(
            fresh.preferred_text_layout,
            if deleted == Some(CONFIG_LAYOUT_KEY) {
                "qwerty"
            } else {
                "azerty"
            }
        );
        assert_eq!(
            fresh.suggestions_enabled,
            deleted == Some(CONFIG_SUGGESTIONS_KEY)
        );
        assert_eq!(
            fresh.personalization_enabled,
            deleted == Some(CONFIG_PERSONALIZATION_KEY)
        );
        assert!(f.driver.state_mut().undo(None, None).is_some());
        assert_eq!(
            saved_layout(&f.driver.state_mut().doc).as_deref(),
            Some("qwerty")
        );
    }
}

fn frozen_preview(
    root: &OskWidget,
    doc: &AutoCommit,
    hash: automerge::ChangeHash,
) -> Result<Box<dyn HistoryPreview>, String> {
    let mut controller = HistoryController::new(
        HistoryResource::new(
            "preview",
            "lifetime",
            "so.tilo.osk",
            ui::RecordId::for_tab(352),
        ),
        doc,
    )
    .unwrap();
    controller
        .select(
            doc,
            &[ui::history_source::entry_id_from_hash(&hash.to_string())],
        )
        .unwrap();
    ui::history_mode::capability::document_history_definition(root)
        .unwrap()
        .scopes[0]
        .presentation
        .preview_factory()
        .unwrap()(PreviewInput {
        snapshot: controller.snapshot().unwrap().clone(),
        initial_view: PreviewView::default(),
        assets: FrozenPreviewAssets::default(),
    })
}
#[test]
fn framework_history_four_layout_masks_validate_preview_current_target_and_result() {
    for (q, a) in [(true, true), (true, false), (false, true), (false, false)] {
        let mut root = keyboard();
        for (name, canonical) in [("qwerty", q), ("azerty", a)] {
            if !canonical {
                let mut layout = KeyboardLayout::by_name(name).unwrap();
                layout.rows[0][0].label = "custom override".into();
                root = root.with_layout(layout);
            }
        }
        let definition = ui::history_mode::capability::document_history_definition(&root).unwrap();
        let schema = definition.scopes[0].content_schema.unwrap();
        for saved in [
            None,
            Some("qwerty"),
            Some("azerty"),
            Some("symbols"),
            Some("numeric"),
            Some("custom"),
        ] {
            let supported = match saved {
                None | Some("qwerty") => q,
                Some("azerty") => a,
                _ => false,
            };
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "unrelated", "keep").unwrap();
            if let Some(saved) = saved {
                doc.put(automerge::ROOT, CONFIG_LAYOUT_KEY, saved).unwrap();
            }
            doc.commit();
            let selected = doc.get_heads()[0];
            assert_eq!(
                (schema.validate)(&doc).is_ok(),
                supported,
                "{q} {a} {saved:?}"
            );
            assert_eq!((schema.validate_result)(&doc).is_ok(), supported);
            assert_eq!(frozen_preview(&root, &doc, selected).is_ok(), supported);
            let heads = doc.get_heads();
            if !supported {
                assert!(
                    ui::history_mode::restore::prepare_content_restore(
                        &doc,
                        schema,
                        &heads,
                        &[selected]
                    )
                    .is_err()
                );
                assert_eq!(doc.get_heads(), heads);
            }
        }
        for purpose in [
            InputPurpose::Number,
            InputPurpose::Telephone,
            InputPurpose::Text,
            InputPurpose::Password,
        ] {
            root.set_purpose(purpose);
            assert!(std::ptr::eq(
                ui::history_mode::capability::document_history_definition(&root).unwrap(),
                definition
            ));
        }
    }
    let canonical = keyboard().with_layout(KeyboardLayout::qwerty());
    assert!(std::ptr::eq(
        ui::history_mode::capability::document_history_definition(&canonical).unwrap(),
        std::ptr::from_ref(&crate::framework_history::HISTORY)
    ));
    let mut collision = KeyboardLayout::qwerty();
    collision.rows[0][0].width = 2.;
    let recovered = keyboard()
        .with_layout(collision)
        .with_layout(KeyboardLayout::qwerty());
    assert!(std::ptr::eq(
        ui::history_mode::capability::document_history_definition(&recovered).unwrap(),
        std::ptr::from_ref(&crate::framework_history::HISTORY)
    ));
    let mut custom = KeyboardLayout::qwerty();
    custom.name = "custom".into();
    let mut live = keyboard().with_layout(custom).with_active_layout("custom");
    assert_eq!(live.active_layout_name(), "custom");
    live.set_purpose(InputPurpose::Number);
    assert!(live.select_text_layout("numeric"));
    assert_eq!(live.active_layout_name(), "numeric");
}
#[test]
fn framework_history_wrong_types_and_unsupported_current_content_never_partially_restore() {
    let (mut doc, selected) = document();
    let schema = crate::framework_history::HISTORY.scopes[0]
        .content_schema
        .unwrap();
    for key in [
        CONFIG_LAYOUT_KEY,
        CONFIG_SUGGESTIONS_KEY,
        CONFIG_PERSONALIZATION_KEY,
    ] {
        let mut malformed = doc.clone();
        malformed.put(automerge::ROOT, key, 42).unwrap();
        let heads = malformed.get_heads();
        assert!(
            ui::history_mode::restore::prepare_content_restore(
                &malformed,
                schema,
                &heads,
                &[selected]
            )
            .is_err()
        );
        assert_eq!(malformed.get_heads(), heads);
    }
    doc.put(automerge::ROOT, CONFIG_LAYOUT_KEY, "custom")
        .unwrap();
    let heads = doc.get_heads();
    let mut f = driver(doc, keyboard());
    restore(&mut f.driver, selected);
    assert_eq!(f.driver.state_mut().doc.get_heads(), heads);
    assert_eq!(saved_suggestions(&f.driver.state_mut().doc), Some(true));
    assert!(f.driver.a11y_node_named("Refresh review").is_some());
}
#[test]
fn framework_history_passive_rendering_retains_runtime_and_current_im_queue() {
    let keys = Rc::new(Cell::new(0));
    let tools = Rc::new(Cell::new(0));
    let key_probe = keys.clone();
    let tool_probe = tools.clone();
    let root = keyboard()
        .on_output(move |_| key_probe.set(key_probe.get() + 1))
        .on_toolbar(move |_| tool_probe.set(tool_probe.get() + 1));
    let (doc, selected) = document();
    let mut f = driver(doc, root);
    {
        let mut root = f.keyboard.borrow_mut();
        root.apply_action(KeyAction::Char("private".into()));
        root.predictor.learn_transition(Some("private"), "retained");
        root.set_purpose(InputPurpose::Telephone);
        root.shift = ShiftState::CapsLock;
        root.showing_symbols = true;
        root.pressed_key = Some(KeyId::new(1, 2));
        root.pressed_tool = Some(1);
        root.gesture_path = Some(vec![Point::new(20., 30.), Point::new(40., 50.)]);
    }
    let runtime = |root: &OskWidget| {
        (
            root.pending_text().to_owned(),
            root.key_log.clone(),
            root.toolbar_log.clone(),
            root.predictor.predict_next(Some("private"), ""),
            root.gesture_path.clone(),
            root.shift,
            root.pressed_key,
            root.pressed_tool,
            root.purpose,
            root.visible,
            root.showing_symbols,
        )
    };
    let before = runtime(&f.keyboard.borrow());
    let key_calls = keys.get();
    let callback_of = |k: &OskWidget| {
        std::ptr::from_ref::<dyn FnMut(crate::output::OskOutput)>(k.on_output.as_deref().unwrap())
    };
    let callback = callback_of(&f.keyboard.borrow()).cast::<()>();
    let predictor = std::ptr::from_ref(&f.keyboard.borrow().predictor);
    let heads = f.driver.state_mut().doc.get_heads();
    let bytes = f.driver.state_mut().doc.save();
    f.driver.click_named("history-open").unwrap();
    select_timeline_entry(&mut f.driver, selected);
    assert!(
        f.driver
            .inspection_json()
            .unwrap()
            .contains("Saved layout: AZERTY")
    );
    assert!(
        f.driver
            .inspection_json()
            .unwrap()
            .contains("Personalization: off")
    );
    f.driver.click_named("history-before").unwrap();
    f.driver.click_named("history-after").unwrap();
    f.driver.type_text("never delivered");
    f.driver.key("Backspace");
    f.driver.key("Ctrl+k");
    f.driver.key("Escape");
    let center = f.driver.targets()["history-preview"].center();
    f.driver.click_at(center.x, center.y);
    assert_eq!(keys.get(), key_calls);
    assert_eq!(tools.get(), 0);
    assert_eq!(f.acknowledgements.get(), 0);
    assert_eq!(f.driver.state_mut().doc.get_heads(), heads);
    assert_eq!(f.driver.state_mut().doc.save(), bytes);
    assert_eq!(runtime(&f.keyboard.borrow()), before);
    f.driver.click_named("history-back-live").unwrap();
    assert_eq!(runtime(&f.keyboard.borrow()), before);
    assert_eq!(callback_of(&f.keyboard.borrow()).cast::<()>(), callback);
    assert_eq!(
        std::ptr::from_ref(&f.keyboard.borrow().predictor),
        predictor
    );
    f.driver.click_named("history-open").unwrap();
    select_timeline_entry(&mut f.driver, selected);
    f.events.borrow_mut().extend([
        "osk:purpose:number".into(),
        "osk:hide".into(),
        "osk:show".into(),
    ]);
    f.driver.relayout();
    f.driver.relayout();
    assert_eq!(f.event_count.get(), 3);
    assert!(f.events.borrow().is_empty());
    assert_eq!(f.keyboard.borrow().purpose, InputPurpose::Number);
    assert!(f.keyboard.borrow().visible);
    assert_eq!(keys.get(), key_calls);
    assert_eq!(tools.get(), 0);
    assert_eq!(f.acknowledgements.get(), 0);
}
#[test]
fn framework_history_preview_geometry_paints_builtins_without_live_construction_or_controls() {
    let mut scenes = Vec::new();
    for name in ["qwerty", "azerty"] {
        let mut doc = AutoCommit::new();
        put_preferences(&mut doc, name, false, true);
        doc.commit();
        let hash = doc.get_heads()[0];
        let mut preview = frozen_preview(&keyboard(), &doc, hash).unwrap();
        let accessible = preview.accessibility();
        assert!(accessible.pages.is_empty());
        assert!(accessible.description.contains("Suggestions: off"));
        assert!(accessible.description.contains("Personalization: on"));
        assert!(accessible.description.contains("layout definitions"));
        assert!(!preview.navigate(PreviewNavigation::Reveal("osk:key:0:0".into())));
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        for width in [360., 1000.] {
            preview
                .layout(Rect::new(0., 0., width, 640.), &mut text, &theme)
                .unwrap();
            let mut scene = Scene::new();
            preview.draw(&mut scene, Affine::IDENTITY, &mut text, &theme);
            assert!(
                scene.encoding().resources.glyphs.len() > 70,
                "normal keyboard cells and labels are painted"
            );
            scenes.push(
                scene
                    .encoding()
                    .resources
                    .glyphs
                    .iter()
                    .map(|g| g.id)
                    .collect::<Vec<_>>(),
            );
        }
    }
    assert_ne!(
        scenes[0], scenes[2],
        "AZERTY and QWERTY illustrate different actual key labels"
    );
}
#[test]
fn framework_history_personalization_restore_clears_current_cache_but_never_reconstructs_it() {
    let (doc, selected) = document();
    let mut f = driver(doc, keyboard());
    f.keyboard
        .borrow_mut()
        .predictor
        .learn_transition(Some("local"), "word");
    assert_eq!(f.keyboard.borrow().predictor.learned_context_count(), 1);
    restore(&mut f.driver, selected);
    assert_eq!(f.keyboard.borrow().predictor.learned_context_count(), 0);
    assert!(!f.keyboard.borrow().personalization_enabled);
    f.driver.click_named("history-back-live").unwrap();
    invoke(&mut f, "osk.keyboard.suggestions", "enable-personal");
    assert_eq!(f.keyboard.borrow().predictor.learned_context_count(), 0);
    f.keyboard
        .borrow_mut()
        .predictor
        .learn_transition(Some("new"), "word");
    f.driver.relayout();
    assert_eq!(
        f.keyboard.borrow().predictor.learned_context_count(),
        1,
        "unchanged adoption never clears twice"
    );
    invoke(&mut f, "osk.keyboard.suggestions", "disable-personal");
    assert_eq!(f.keyboard.borrow().predictor.learned_context_count(), 0);
}

impl ui::widget::AppWidget for LiveFixture {
    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "OSK controlled root"
    }
    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Named("keyboard".into())
    }
    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        None
    }
}
#[test]
fn framework_history_real_component_tile_settles_through_its_actual_registry() {
    real_owned_tile_settles_and_restores(true);
}
#[test]
fn framework_history_real_legacy_tile_settles_restores_and_reopens_through_enclosing_registry() {
    real_owned_tile_settles_and_restores(false);
}
#[expect(
    clippy::too_many_lines,
    reason = "one owned-tile scenario proving settle and restore end to end"
)]
fn real_owned_tile_settles_and_restores(component: bool) {
    use ui::application::{
        AppComponent, ApplicationDefinition, HostContext, HostContextInputs, LaunchTarget,
    };
    {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("profile");
        let mut owners = ui::record::RecordOwners::new(profile.clone());
        let store = owners.open("so.tilo.osk", "controlled").unwrap();
        let (mut doc, selected) = document();
        let keyboard = Rc::new(RefCell::new(keyboard()));
        let acknowledgements = Rc::new(Cell::new(0));
        let root = LiveFixture::new(keyboard.clone(), acknowledgements.clone());
        let (tile, checkout) = if component {
            let definition = ApplicationDefinition {
                app_id: "so.tilo.osk",
                title: "OSK",
                icon: ui::widget::IconDescriptor::Named("keyboard".into()),
                record_kind: ui::record::RecordKind::new("osk.preferences"),
                required: vec![],
                optional: vec![],
                opens: &[],
                create: |_, _, _| {
                    Err(ui::launch::LaunchError::Operation(
                        "controlled explicit root".into(),
                    ))
                },
            };
            let views = Rc::new(
                ui::application_view::InstanceViewStore::new(
                    profile.clone(),
                    "device",
                    "controlled",
                )
                .unwrap(),
            );
            let permissions = Rc::new(RefCell::new(
                ui::permissions::PermissionContext::from_capabilities(
                    ui::permissions::Actor::person(
                        Some(ui::permissions::Role::Owner),
                        "osk-author",
                    ),
                    vec![],
                ),
            ));
            let mut context = HostContext::prepare(
                &definition,
                HostContextInputs {
                    records: store.clone(),
                    views,
                    device_id: "device".into(),
                    instance_id: 353,
                    permissions,
                    author: ui::permissions::UserId("osk-author".into()),
                    action_context: ui::tilo_actions::ActionContext::default(),
                },
                &LaunchTarget::New,
            )
            .unwrap();
            context
                .initialize_new_record(|target| {
                    target.merge_incoming(&mut doc).map_err(|e| e.to_string())?;
                    Ok(())
                })
                .unwrap();
            let checkout = context.record().clone();
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            crate::actions::register(&mut registry);
            let mut component = AppComponent::new(context, Box::new(root), registry);
            component
                .register_document_history(&crate::framework_history::HISTORY)
                .unwrap();
            (ui::widget::TileHost::from_component(component), checkout)
        } else {
            let checkout = ui::record::RecordCheckout::new(
                ui::RecordId::for_tab(353),
                ui::record::RecordKind::new("osk.preferences"),
            );
            store
                .reserve_record(&checkout)
                .unwrap()
                .initialize(|target| {
                    target.merge_incoming(&mut doc).map_err(|e| e.to_string())?;
                    Ok(())
                })
                .unwrap();
            let mut tile = ui::widget::TileHost::new(ui::widget::TileContent::App(Box::new(root)));
            tile.bind_record(store.clone(), checkout.clone());
            tile.register_document_history(&crate::framework_history::HISTORY)
                .unwrap();
            (tile, checkout)
        };

        let mut driver = ui::headless::Driver::new(ui::State::new(), Box::new(tile), (900, 650));
        crate::actions::register(driver.action_registry_mut());
        review_restore(&mut driver, selected);
        keyboard.borrow_mut().set_suggestions_enabled(false);
        driver.key("Enter");
        assert!(
            driver
                .inspection_json()
                .unwrap()
                .contains("Return to live to finish pending keyboard preferences")
        );
        assert!(keyboard.borrow().drafts.pending());
        assert_eq!(acknowledgements.get(), 0);
        driver.key("Escape");
        driver.click_named("history-back-live").unwrap();
        // Component/tile model forwarding is suppressed in the framework input
        // frame. A subsequent normal model pass prepares, then a live tick admits.
        driver.relayout();
        driver.key("Shift");
        assert!(!keyboard.borrow().drafts.pending());
        assert_eq!(acknowledgements.get(), 1);
        assert_eq!(
            store.read(&checkout.id, saved_suggestions).unwrap(),
            Some(false)
        );
        restore(&mut driver, selected);
        assert_eq!(
            store.read(&checkout.id, saved_layout).unwrap().as_deref(),
            Some("azerty")
        );
        assert_eq!(acknowledgements.get(), 1);
        assert!(!keyboard.borrow().drafts.pending());
        store.try_flush().unwrap();
        drop(driver);
        drop(store);
        drop(owners);
        let mut reopened_owners = ui::record::RecordOwners::new(profile);
        let reopened = reopened_owners.open("so.tilo.osk", "controlled").unwrap();
        reopened.try_open_record(&checkout).unwrap();
        assert_eq!(
            reopened
                .read(&checkout.id, saved_layout)
                .unwrap()
                .as_deref(),
            Some("azerty")
        );
        assert!(
            reopened
                .read(&checkout.id, |doc| doc
                    .get_change_by_hash(&selected)
                    .is_some())
                .unwrap()
        );
    }
}

/// Open scope issue: legacy resource actions need exact-record admission before
/// OSK can safely author preferences in a raw legacy tile. Keep this causal RED.
#[test]
fn framework_history_legacy_tile_document_action_must_not_mutate_outer_state() {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = ui::record::RecordOwners::new(directory.path().join("profile"));
    let store = owners.open("so.tilo.osk", "legacy-regression").unwrap();
    let checkout = ui::record::RecordCheckout::new(
        ui::RecordId::for_tab(355),
        ui::record::RecordKind::new("osk.preferences"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            doc.put(automerge::ROOT, CONFIG_SUGGESTIONS_KEY, true)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .unwrap();
    let root = Rc::new(RefCell::new(keyboard()));
    let acks = Rc::new(Cell::new(0));
    let live = LiveFixture::new(root.clone(), acks.clone());
    let mut tile = ui::widget::TileHost::new(ui::widget::TileContent::App(Box::new(live)));
    tile.bind_record(store.clone(), checkout.clone());
    tile.register_document_history(&crate::framework_history::HISTORY)
        .unwrap();
    let mut outer = ui::State::new();
    outer
        .doc
        .put(automerge::ROOT, "outer-only", "keep")
        .unwrap();
    outer.commit_action(None, None);
    let mut driver = ui::headless::Driver::new(outer, Box::new(tile), (900, 650));
    crate::actions::register(driver.action_registry_mut());
    let outer_heads = driver.state_mut().doc.get_heads();
    let outer_bytes = driver.state_mut().doc.save();
    let record_heads = store.with(&checkout.id, AutoCommit::get_heads).unwrap();
    root.borrow_mut().set_suggestions_enabled(false);
    driver.relayout();
    driver.key("Shift");
    let outer_after = driver.state_mut().doc.get_heads();
    let outer_value = saved_suggestions(&driver.state_mut().doc);
    let record_after = store.with(&checkout.id, AutoCommit::get_heads).unwrap();
    let record_value = store.read(&checkout.id, saved_suggestions).unwrap();
    assert_eq!(
        outer_after,
        outer_heads,
        "legacy action wrote outer preferences={outer_value:?}; record preferences={record_value:?}, record unchanged={}, acknowledgements={}",
        record_after == record_heads,
        acks.get()
    );
    assert_eq!(driver.state_mut().doc.save(), outer_bytes);
    assert_eq!(record_value, Some(false));
    assert_ne!(record_after, record_heads);
    assert_eq!(acks.get(), 1);
    assert!(!root.borrow().drafts.pending());
}

#[derive(Default)]
struct ReentrantAck {
    supersede: bool,
    envelopes: Vec<(
        EncodedAction,
        Vec<automerge::ChangeHash>,
        Vec<automerge::ChangeHash>,
    )>,
}
fn assert_no_reentrant_offer(root: &mut OskWidget, doc: &AutoCommit) {
    let allowed = ui::tilo_actions::door::survey(&ui::resources::resources(root, doc))
        .into_iter()
        .filter(|door| door.contract.availability.is_available())
        .map(|door| door.address)
        .collect();
    ui::widget::controls::refresh_control_action_availability(root, &allowed);
    assert!(
        ui::widget::controls::take_control_invocation(root).is_none(),
        "in-flight enumeration/availability cannot reoffer"
    );
}
thread_local! {
    static NATIVE_ROOT: RefCell<Option<Box<dyn Widget>>> = RefCell::new(None);
}
fn native_root() -> Box<dyn Widget> {
    NATIVE_ROOT.with_borrow_mut(|root| root.take().unwrap())
}
fn native_state(_: &'static str) -> (ui::State, ui::persist::DocWriter) {
    (
        ui::State::new(),
        ui::persist::DocWriter::disabled("reentrant controlled fixture"),
    )
}
fn native_app(root: Box<dyn Widget>) -> ui::App {
    NATIVE_ROOT.with_borrow_mut(|slot| *slot = Some(root));
    let mut app = ui::launch::LaunchSpec::new("dev.tilo.osk-reentrant", native_root)
        .open_state(native_state)
        .build_controlled_app_for_test()
        .unwrap();
    crate::actions::register(app.action_registry_mut());
    app.rebuild_structure();
    app
}
#[test]
fn framework_history_native_app_reentrant_ack_preserves_newer_and_unrelated_drafts() {
    let directory = tempfile::tempdir().unwrap();
    let mut owners = ui::record::RecordOwners::new(directory.path().join("profile"));
    let store = owners.open("so.tilo.osk", "reentrant").unwrap();
    let checkout = ui::record::RecordCheckout::new(
        ui::RecordId::for_tab(357),
        ui::record::RecordKind::new("osk.preferences"),
    );
    store
        .reserve_record(&checkout)
        .unwrap()
        .initialize(|doc| {
            put_preferences(doc, "qwerty", true, true);
            Ok(())
        })
        .unwrap();
    let keyboard = Rc::new(RefCell::new(keyboard()));
    keyboard.borrow_mut().select_text_layout("azerty");
    keyboard.borrow_mut().set_suggestions_enabled(false);
    keyboard.borrow_mut().set_personalization_enabled(false);
    let acks = Rc::new(Cell::new(0));
    let probe = Rc::new(RefCell::new(ReentrantAck {
        supersede: true,
        ..Default::default()
    }));
    let live = LiveFixture {
        reentrant: Some(probe.clone()),
        ..LiveFixture::new(keyboard.clone(), acks.clone())
    };
    let mut tile = ui::widget::TileHost::new(ui::widget::TileContent::App(Box::new(live)));
    tile.bind_record(store.clone(), checkout.clone());
    let mut app = native_app(Box::new(tile));
    let record_heads = || {
        store
            .read(
                &checkout.id,
                automerge::transaction::Transactable::base_heads,
            )
            .unwrap()
    };
    let before = record_heads();
    let outer = app.state_mut_for_test().doc.save();
    assert!(app.controlled_local_idle_for_test());
    assert_eq!(acks.get(), 3);
    assert!(keyboard.borrow().drafts.has(0));
    assert!(keyboard.borrow().drafts.has(1));
    assert!(!keyboard.borrow().drafts.has(2));
    let captured_heads = record_heads();
    assert!(!app.controlled_local_idle_for_test());
    assert_eq!(record_heads(), captured_heads);
    assert_eq!(
        acks.get(),
        3,
        "remaining batch/idle cannot duplicate consumed offers"
    );
    app.rebuild_structure();
    assert!(app.controlled_local_idle_for_test());
    assert_eq!(acks.get(), 5);
    assert!(!keyboard.borrow().drafts.pending());
    let probe = probe.borrow();
    assert_eq!(probe.envelopes[0].1, before);
    for (old, newer) in [(0, 3), (1, 4)] {
        let old = crate::actions::decode(&probe.envelopes[old].0).unwrap();
        let newer = crate::actions::decode(&probe.envelopes[newer].0).unwrap();
        assert_eq!(old.value.index(), newer.value.index());
        assert_ne!(old.value, newer.value);
        assert_ne!(old.draft, newer.draft);
    }
    for (index, (encoded, before, after)) in probe.envelopes.iter().enumerate() {
        assert!(crate::actions::decode(encoded).unwrap().draft.is_some());
        assert_ne!(before, after, "each actual action changes its own value");
        if index > 0 {
            assert_eq!(before, &probe.envelopes[index - 1].2);
        }
    }
    assert_eq!(record_heads(), probe.envelopes.last().unwrap().2);
    store
        .read(&checkout.id, |doc| {
            assert_eq!(
                read_config_text(doc, &automerge::ROOT, CONFIG_LAYOUT_KEY).as_deref(),
                Some("qwerty")
            );
            assert_eq!(
                read_config_bool(doc, &automerge::ROOT, CONFIG_SUGGESTIONS_KEY),
                Some(true)
            );
            assert_eq!(
                read_config_bool(doc, &automerge::ROOT, CONFIG_PERSONALIZATION_KEY),
                Some(false)
            );
        })
        .unwrap();
    assert_eq!(app.state_mut_for_test().doc.save(), outer);
    app.rebuild_structure();
    assert!(!app.controlled_local_idle_for_test());
    assert_eq!(acks.get(), 5);
    assert_eq!(record_heads(), probe.envelopes.last().unwrap().2);
}
#[test]
fn framework_history_native_app_explicit_ack_removes_matching_stale_offer() {
    let keyboard = Rc::new(RefCell::new(keyboard()));
    keyboard.borrow_mut().set_suggestions_enabled(false);
    let queue = Rc::new(RefCell::new(VecDeque::from([ControlInvocation {
        address: ActionAddress::new("osk.keyboard.suggestions", "disable"),
        arguments: serde_json::Map::default(),
    }])));
    let acks = Rc::new(Cell::new(0));
    let probe = Rc::new(RefCell::new(ReentrantAck::default()));
    let live = LiveFixture {
        queue,
        reentrant: Some(probe.clone()),
        ..LiveFixture::new(keyboard.clone(), acks.clone())
    };
    let mut app = native_app(Box::new(live));
    let before = app.state_mut_for_test().doc.get_heads();
    assert!(app.controlled_local_idle_for_test());
    assert_eq!(
        acks.get(),
        1,
        "explicit success removes the prepared stale correlated offer"
    );
    assert!(!keyboard.borrow().drafts.pending());
    assert_eq!(probe.borrow().envelopes[0].1, before);
    assert_eq!(
        probe.borrow().envelopes[0].2,
        app.state_mut_for_test().doc.get_heads()
    );
    assert!(
        crate::actions::decode(&probe.borrow().envelopes[0].0)
            .unwrap()
            .draft
            .is_none()
    );
    let settled_heads = app.state_mut_for_test().doc.get_heads();
    app.rebuild_structure();
    assert!(!app.controlled_local_idle_for_test());
    assert_eq!(acks.get(), 1);
    assert_eq!(app.state_mut_for_test().doc.get_heads(), settled_heads);
}
