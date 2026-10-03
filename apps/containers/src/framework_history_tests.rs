include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");

use automerge::{ObjType, ScalarValue, Value};
use tilo_automerge::ApplyIncoming as _;
use ui::headless::test_support::{a11y_node_labelled, find_semantic};
use ui::history_mode::pending_controls::PendingControlStatus;
use ui::widget::Widget;
use ui::widget::{AppModel, ModelPath};

type ArmedReplacement = (
    std::sync::mpsc::Receiver<crate::app::ControlledRuntimeRequest>,
    std::rc::Rc<std::cell::Cell<bool>>,
);

fn test_root() -> Box<dyn Widget> {
    Box::new(crate::history_test_root())
}

fn text(
    doc: &mut AutoCommit,
    parent: &automerge::ObjId,
    key: &str,
    value: &str,
) -> automerge::ObjId {
    let object = doc.put_object(parent, key, ObjType::Text).unwrap();
    doc.splice_text(&object, 0, 0, value).unwrap();
    object
}

fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut doc = AutoCommit::new();
    let view = doc.put_object(ROOT, "view", ObjType::Map).unwrap();
    doc.put(&view, "section", "containers").unwrap();
    doc.put(&view, "selected", "live-selection").unwrap();
    doc.put(&view, "tab", "inspect").unwrap();
    text(&mut doc, &view, "search", "current filter");
    doc.put(&view, "unrelated", "keep current").unwrap();
    let create = doc.put_object(&view, "create", ObjType::Map).unwrap();
    text(&mut doc, &create, "image", "nginx:old");
    text(&mut doc, &create, "ports", "8080:80");
    let unknown = doc.put_object(&create, "future", ObjType::Map).unwrap();
    doc.put(&unknown, "mode", "historical").unwrap();
    let selected = doc.get_heads()[0];
    crate::doc::ViewDoc::set_create_field(&mut doc, crate::doc::CreateField::Image, "nginx:new");
    crate::doc::ViewDoc::set_create_field(&mut doc, crate::doc::CreateField::Name, "current-name");
    let current_create = match doc.get(&view, "create").unwrap().unwrap() {
        (Value::Object(ObjType::Map), id) => id,
        other => panic!("unexpected create value: {other:?}"),
    };
    let current_unknown = match doc.get(&current_create, "future").unwrap().unwrap() {
        (Value::Object(ObjType::Map), id) => id,
        other => panic!("unexpected unknown value: {other:?}"),
    };
    doc.put(&current_unknown, "mode", "current").unwrap();
    doc.put(&view, "section", "images").unwrap();
    doc.put(ROOT, "view_only", "current").unwrap();
    doc.get_heads();
    (doc, selected, "Image: nginx:old".into())
}

fn verify_restored(doc: &AutoCommit) {
    assert_eq!(
        crate::doc::ViewDoc::create_field(doc, crate::doc::CreateField::Image),
        "nginx:old"
    );
    assert_eq!(
        crate::doc::ViewDoc::create_field(doc, crate::doc::CreateField::Ports),
        "8080:80"
    );
    assert_eq!(
        crate::doc::ViewDoc::stored_create_field(doc, crate::doc::CreateField::Name),
        crate::doc::StoredCreateField::Absent
    );
    let view = doc.get(ROOT, "view").unwrap().unwrap().1;
    assert_eq!(
        doc.get(&view, "section").unwrap().unwrap().0.as_str(),
        Some("images")
    );
    assert_eq!(
        doc.get(&view, "selected").unwrap().unwrap().0.as_str(),
        Some("live-selection")
    );
    assert_eq!(
        doc.get(&view, "tab").unwrap().unwrap().0.as_str(),
        Some("inspect")
    );
    assert_eq!(
        doc.text(&doc.get(&view, "search").unwrap().unwrap().1)
            .unwrap(),
        "current filter"
    );
    assert_eq!(
        doc.get(&view, "unrelated").unwrap().unwrap().0.as_str(),
        Some("keep current")
    );
    let create = doc.get(&view, "create").unwrap().unwrap().1;
    let future = doc.get(&create, "future").unwrap().unwrap().1;
    assert_eq!(
        doc.get(&future, "mode").unwrap().unwrap().0.as_str(),
        Some("historical")
    );
}

fn malformed_fixture(doc: &mut AutoCommit) {
    let view = doc.get(ROOT, "view").unwrap().unwrap().1;
    let create = doc.get(&view, "create").unwrap().unwrap().1;
    doc.put(&create, "image", "scalar strings are unsupported")
        .unwrap();
}

#[test]
fn fresh_sync_focus_and_reopen_preserve_absent_draft() {
    let mut state = ui::State::new();
    let heads = state.doc.get_heads();
    let mut root = crate::history_test_root();
    root.sync(&mut state.doc, &state.root);
    assert_eq!(state.doc.get_heads(), heads);
    assert!(state.doc.get(ROOT, "view").unwrap().is_none());

    let mut driver = ui::headless::Driver::new(state, Box::new(root), (1000, 750));
    driver.key("n");
    let heads = driver.state_mut().doc.get_heads();
    let _ = input_id(&driver, "Image");
    assert!(driver.a11y_focus().is_some());
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
    assert!(driver.state_mut().doc.get(ROOT, "view").unwrap().is_none());

    let image = input_id(&driver, "Image");
    driver
        .perform_a11y_action(
            image,
            &ui::a11y::Action::SetValue("quay.io/example/app:1".into()),
        )
        .unwrap();
    assert_eq!(
        crate::doc::ViewDoc::create_field(&driver.state_mut().doc, crate::doc::CreateField::Image),
        "quay.io/example/app:1"
    );
    let view = driver.state_mut().doc.get(ROOT, "view").unwrap().unwrap().1;
    let create = driver
        .state_mut()
        .doc
        .get(&view, "create")
        .unwrap()
        .unwrap()
        .1;
    assert_eq!(
        driver.state_mut().doc.keys(&create).collect::<Vec<_>>(),
        vec!["image"]
    );

    let bytes = driver.state_mut().doc.save();
    let mut reopened = AutoCommit::load(&bytes).unwrap();
    let heads = reopened.get_heads();
    test_root().sync(&mut reopened, &ROOT);
    assert_eq!(reopened.get_heads(), heads);
    assert_eq!(
        crate::doc::ViewDoc::create_field(&reopened, crate::doc::CreateField::Image),
        "quay.io/example/app:1"
    );
    assert_eq!(
        crate::doc::ViewDoc::stored_create_field(&reopened, crate::doc::CreateField::Name),
        crate::doc::StoredCreateField::Absent
    );
}

#[test]
fn compatible_unknown_objects_lists_and_scalars_restore_exactly() {
    let mut doc = AutoCommit::new();
    let view = doc.put_object(ROOT, "view", ObjType::Map).unwrap();
    let create = doc.put_object(&view, "create", ObjType::Map).unwrap();
    text(&mut doc, &create, "image", "nginx:future");
    let future = doc.put_object(&create, "future", ObjType::Map).unwrap();
    let list = doc.put_object(&future, "values", ObjType::List).unwrap();
    let nested = doc.insert_object(&list, 0, ObjType::Map).unwrap();
    doc.put(&nested, "kind", "nested object").unwrap();
    let mut deep = future.clone();
    for _ in 0..40 {
        deep = doc.put_object(&deep, "next", ObjType::Map).unwrap();
    }
    doc.put(&deep, "leaf", "retained past old depth limit")
        .unwrap();
    let values = [
        ScalarValue::Counter(4.into()),
        ScalarValue::Unknown {
            type_code: 15,
            bytes: vec![5, 8],
        },
        ScalarValue::F64(f64::from_bits(0x7ff8_0000_0000_0123)),
    ];
    for (index, value) in values.iter().enumerate() {
        doc.insert(&list, index + 1, value.clone()).unwrap();
    }
    doc.increment(&list, 1, 3).unwrap();
    let target = doc.get_heads();
    doc.delete(&view, "create").unwrap();
    let live = doc.get_heads();

    let restore =
        prepare_content_restore(&doc, &crate::framework_history::SCHEMA, &live, &target).unwrap();
    assert_eq!(
        apply_prepared_content_restore(&mut doc, &restore).unwrap(),
        RestoreOutcome::Changed
    );
    let restored_view = doc.get(ROOT, "view").unwrap().unwrap().1;
    let restored_create = doc.get(&restored_view, "create").unwrap().unwrap().1;
    let restored_future = doc.get(&restored_create, "future").unwrap().unwrap().1;
    let restored_list = doc.get(&restored_future, "values").unwrap().unwrap().1;
    let mut restored_deep = restored_future;
    for _ in 0..40 {
        restored_deep = doc.get(&restored_deep, "next").unwrap().unwrap().1;
    }
    assert_eq!(
        doc.get(&restored_deep, "leaf").unwrap().unwrap().0.as_str(),
        Some("retained past old depth limit")
    );
    assert_eq!(doc.length(&restored_list), 4);
    assert_eq!(
        doc.get(&doc.get(&restored_list, 0).unwrap().unwrap().1, "kind")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("nested object")
    );
    let counter = doc
        .get(&restored_list, 1)
        .unwrap()
        .unwrap()
        .0
        .as_scalar()
        .unwrap()
        .clone();
    assert!(matches!(counter, ScalarValue::Counter(value) if i64::from(value.clone()) == 7));
    let restored_unknown = doc
        .get(&restored_list, 2)
        .unwrap()
        .unwrap()
        .0
        .as_scalar()
        .unwrap()
        .clone();
    assert_eq!(restored_unknown, values[1]);
    let restored_float = doc
        .get(&restored_list, 3)
        .unwrap()
        .unwrap()
        .0
        .as_scalar()
        .unwrap()
        .clone()
        .as_f64()
        .unwrap();
    assert_eq!(restored_float.to_bits(), 0x7ff8_0000_0000_0123);
}

#[test]
fn whole_create_map_deletes_resurrects_and_creates_only_minimal_view() {
    let mut deletion = AutoCommit::new();
    let view = deletion.put_object(ROOT, "view", ObjType::Map).unwrap();
    deletion.put(&view, "section", "images").unwrap();
    let absent = deletion.get_heads();
    crate::doc::ViewDoc::set_create_field(&mut deletion, crate::doc::CreateField::Image, "current");
    let live = deletion.get_heads();
    let restore =
        prepare_content_restore(&deletion, &crate::framework_history::SCHEMA, &live, &absent)
            .unwrap();
    apply_prepared_content_restore(&mut deletion, &restore).unwrap();
    let view = deletion.get(ROOT, "view").unwrap().unwrap().1;
    assert!(deletion.get(&view, "create").unwrap().is_none());
    assert_eq!(
        deletion.get(&view, "section").unwrap().unwrap().0.as_str(),
        Some("images")
    );

    let mut resurrection = AutoCommit::new();
    let view = resurrection.put_object(ROOT, "view", ObjType::Map).unwrap();
    let create = resurrection
        .put_object(&view, "create", ObjType::Map)
        .unwrap();
    text(&mut resurrection, &create, "image", "historical");
    let present = resurrection.get_heads();
    resurrection.delete(ROOT, "view").unwrap();
    resurrection.put(ROOT, "view_only", "preserved").unwrap();
    let live = resurrection.get_heads();
    let restore = prepare_content_restore(
        &resurrection,
        &crate::framework_history::SCHEMA,
        &live,
        &present,
    )
    .unwrap();
    apply_prepared_content_restore(&mut resurrection, &restore).unwrap();
    let view = resurrection.get(ROOT, "view").unwrap().unwrap().1;
    assert_eq!(resurrection.keys(&view).collect::<Vec<_>>(), ["create"]);
    assert_eq!(
        crate::doc::ViewDoc::create_field(&resurrection, crate::doc::CreateField::Image),
        "historical"
    );
    assert_eq!(
        resurrection
            .get(ROOT, "view_only")
            .unwrap()
            .unwrap()
            .0
            .as_str(),
        Some("preserved")
    );
}

#[test]
fn wrong_map_types_and_true_concurrent_field_conflict_refuse_without_writes() {
    for malformed in ["view", "create"] {
        let mut doc = AutoCommit::new();
        if malformed == "view" {
            doc.put(ROOT, "view", "wrong").unwrap();
        } else {
            let view = doc.put_object(ROOT, "view", ObjType::Map).unwrap();
            doc.put(&view, "create", "wrong").unwrap();
        }
        let heads = doc.get_heads();
        let pending = doc.pending_ops();
        assert!(
            prepare_content_restore(&doc, &crate::framework_history::SCHEMA, &heads, &heads,)
                .is_err()
        );
        assert_eq!(doc.get_heads(), heads);
        assert_eq!(doc.pending_ops(), pending);
    }

    let mut left = AutoCommit::new();
    let view = left.put_object(ROOT, "view", ObjType::Map).unwrap();
    left.put_object(&view, "create", ObjType::Map).unwrap();
    let base = left.save();
    let mut right = AutoCommit::load(&base).unwrap();
    let left_view = left.get(ROOT, "view").unwrap().unwrap().1;
    let left_create = left.get(&left_view, "create").unwrap().unwrap().1;
    text(&mut left, &left_create, "image", "left");
    let right_view = right.get(ROOT, "view").unwrap().unwrap().1;
    let right_create = right.get(&right_view, "create").unwrap().unwrap().1;
    right.put(&right_create, "image", "right scalar").unwrap();
    left.merge_incoming(&mut right).unwrap();
    assert_eq!(left.get_all(&left_create, "image").unwrap().len(), 2);
    let heads = left.get_heads();
    let pending = left.pending_ops();
    assert!(
        prepare_content_restore(&left, &crate::framework_history::SCHEMA, &heads, &heads,).is_err()
    );
    assert_eq!(left.get_heads(), heads);
    assert_eq!(left.pending_ops(), pending);
}

fn preview_glyph_count(doc: &AutoCommit, head: automerge::ChangeHash, width: f64) -> usize {
    let mut preview = preview(doc, head, PreviewSide::After);
    let mut text = ui::TextRenderer::new();
    let theme = ui::Theme::dark();
    preview
        .layout(
            vello::kurbo::Rect::new(0.0, 0.0, width, 700.0),
            &mut text,
            &theme,
        )
        .unwrap();
    let mut scene = vello::Scene::new();
    preview.draw(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut text,
        &theme,
    );
    scene.encoding().resources.glyphs.len()
}

#[test]
fn unknown_descendant_metadata_is_visibly_painted_at_wide_and_narrow_widths() {
    let mut plain = AutoCommit::new();
    let view = plain.put_object(ROOT, "view", ObjType::Map).unwrap();
    let create = plain.put_object(&view, "create", ObjType::Map).unwrap();
    text(&mut plain, &create, "image", "nginx");
    let plain_head = plain.get_heads()[0];

    let mut enriched = AutoCommit::load(&plain.save()).unwrap();
    let view = enriched.get(ROOT, "view").unwrap().unwrap().1;
    let create = enriched.get(&view, "create").unwrap().unwrap().1;
    let future = enriched
        .put_object(&create, "future", ObjType::Map)
        .unwrap();
    enriched.put(&future, "mode", "historical").unwrap();
    let list = enriched
        .put_object(&future, "targets", ObjType::List)
        .unwrap();
    enriched.insert(&list, 0, "edge").unwrap();
    let enriched_head = enriched.get_heads()[0];
    assert!(
        preview(&enriched, enriched_head, PreviewSide::After)
            .accessibility()
            .description
            .contains("future.targets (list)")
    );

    for width in [360.0, 1000.0] {
        assert!(
            preview_glyph_count(&enriched, enriched_head, width)
                > preview_glyph_count(&plain, plain_head, width),
            "unknown metadata adds visible glyphs at width {width}"
        );
    }
}

fn managed_driver_with_root(doc: AutoCommit, root: Box<dyn Widget>) -> ui::headless::Driver {
    let definition = &crate::framework_history::HISTORY;
    let registration = ui::history_mode::HistoryRegistration::new(
        definition,
        vec![ui::history_mode::HistorySourceRegistration::HostDocument {
            scope: definition.scopes[0].key,
            resource: ui::history_mode::HistoryResource::new(
                "temporary",
                "containers retention fixture",
                definition.app_id,
                ui::RecordId::for_tab(88),
            ),
        }],
    )
    .unwrap();
    let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/sdk/ui-core/assets/fonts"),
        "en-US",
    );
    let renderer = ui::TextRenderer::with_font_catalog(&catalog).unwrap();
    ui::headless::Driver::from_managed_root_with_controlled_inputs(
        ui::State::from_doc(doc),
        ui::history_mode::ManagedRoot::new(root, registration),
        (1000, 750),
        ui::Theme::dark(),
        renderer,
        ui::permissions::UserId("containers-custody-author".into()),
    )
    .unwrap()
}

fn node_named(driver: &ui::headless::Driver, name: &str) -> ui::accesskit::Node {
    a11y_node_labelled(driver, name, None)
}

/// The text field labelled `name` (its visible label is a separate node).
fn input_id(driver: &ui::headless::Driver, name: &str) -> ui::accesskit::NodeId {
    driver
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label() == Some(name) && node.role() == ui::accesskit::Role::TextInput
        })
        .map_or_else(|| panic!("no text field named {name:?}"), |(id, _)| id)
}

fn node_named_role(
    driver: &ui::headless::Driver,
    name: &str,
    role: ui::accesskit::Role,
) -> ui::accesskit::Node {
    a11y_node_labelled(driver, name, Some(role))
}

/// Select `selected` in open History, restore it and return live.
fn restore_and_return(driver: &mut ui::headless::Driver, selected: automerge::ChangeHash) {
    select_version(driver, selected);
    driver.click_named("history-restore").unwrap();
    driver.key("Enter");
    driver.click_named("history-back-live").unwrap();
}

fn history_round_trip(driver: &mut ui::headless::Driver, selected: automerge::ChangeHash) {
    driver.click_named("history-open").unwrap();
    restore_and_return(driver, selected);
}

fn expect_request(
    requests: &std::sync::mpsc::Receiver<crate::app::ControlledRuntimeRequest>,
    label: &str,
) -> crate::app::ControlledRuntimeRequest {
    requests
        .recv_timeout(tilo_test_env::hang_bound())
        .unwrap_or_else(|error| panic!("missing controlled {label} request: {error}"))
}

/// The armed retention root with its four start-up requests drained, mounted
/// behind `wrap` and armed with a pending restart of `armed-container`.
struct Armed {
    driver: ui::headless::Driver,
    selected: automerge::ChangeHash,
    requests: std::sync::mpsc::Receiver<crate::app::ControlledRuntimeRequest>,
    results: std::sync::mpsc::Sender<Result<(), String>>,
    arm: std::rc::Rc<std::cell::Cell<bool>>,
}

fn armed_driver(
    wrap: impl FnOnce(Box<dyn Widget>) -> Box<dyn Widget>,
) -> (Armed, ArmedReplacement) {
    let (mut doc, selected, _) = fixture();
    crate::doc::ViewDoc::set_selected(&mut doc, Some("armed-container"));
    let (root, requests, results, _observed, arm, replacement_requests, replace_runtime) =
        crate::app::ContainersApp::history_test_armed_retention_root();
    for stage in ["refresh", "pull", "create", "selected detail"] {
        expect_request(&requests, stage);
    }
    let mut driver = managed_driver_with_root(doc, wrap(Box::new(root)));
    arm.set(true);
    driver.relayout();
    (
        Armed {
            driver,
            selected,
            requests,
            results,
            arm,
        },
        (replacement_requests, replace_runtime),
    )
}

fn assert_single_armed_restart(
    controls: &[ui::history_mode::pending_controls::PendingControlReview],
) {
    assert_eq!(controls.len(), 1);
    let invocation = &controls[0].invocation;
    assert_eq!(invocation.address.resource, "container:armed-container");
    assert_eq!(invocation.address.action, "restart");
    assert!(invocation.arguments.is_empty());
}

/// Review the one ready History control, let the runtime succeed, confirm it
/// and expect exactly its restart and follow-up refresh.
fn continue_and_confirm_restart(armed: &mut Armed) {
    let driver = &mut armed.driver;
    let ready = driver.pending_history_controls();
    let review = driver.pending_history_control_token(ready[0].id).unwrap();
    assert_eq!(
        driver.continue_pending_history_control(&review),
        Some(PendingControlStatus::NeedsConfirmation)
    );
    armed.results.send(Ok(())).unwrap();
    let awaiting = driver.pending_history_controls();
    let confirmation = driver
        .pending_history_control_token(awaiting[0].id)
        .unwrap();
    assert_eq!(
        driver.confirm_pending_history_control(&confirmation),
        Some(PendingControlStatus::Consumed)
    );
    assert_restart_dispatched(&armed.requests);
}

fn assert_restart_dispatched(
    requests: &std::sync::mpsc::Receiver<crate::app::ControlledRuntimeRequest>,
) {
    assert!(matches!(
        expect_request(requests, "restart"),
        crate::app::ControlledRuntimeRequest::Immediate { .. }
    ));
    assert!(matches!(
        expect_request(requests, "restart refresh"),
        crate::app::ControlledRuntimeRequest::Refresh(_)
    ));
}

struct CustodyDeniedContainersRoot {
    child: Box<dyn Widget>,
    denied: std::rc::Rc<std::cell::Cell<bool>>,
}

impl Widget for CustodyDeniedContainersRoot {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls
    );

    fn name(&self) -> &'static str {
        "CustodyDeniedContainersRoot"
    }
    fn sync(&mut self, doc: &mut AppModel, path: &ModelPath) {
        self.child.sync(doc, path);
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.child.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.child.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.child.draw(scene, transform, text, theme, presence);
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.child.get_rect()
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.child.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if self.denied.get() {
            Vec::new()
        } else {
            vec![self.child.as_mut()]
        }
    }
}

impl ui::widget::controls::ActionControls for CustodyDeniedContainersRoot {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        if self.denied.get() {
            None
        } else {
            ui::widget::controls::take_control_invocation(self.child.as_mut())
        }
    }
}

impl ui::history_mode::capability::DocumentHistory for CustodyDeniedContainersRoot {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        ui::history_mode::capability::document_history_definition(self.child.as_ref())
    }
}

fn published_palette_control(
    driver: &ui::headless::Driver,
    label: &str,
) -> (String, Option<String>) {
    let node = find_semantic(driver, |node| {
        node.name.as_deref() == Some(label) && node.action_target.is_some()
    })
    .unwrap_or_else(|| panic!("missing published action-palette control {label:?}"));
    (node.action_target.unwrap(), node.description)
}

#[test]
fn managed_history_retains_public_modal_input_selection_and_focus() {
    let (mut doc, selected, _) = fixture();
    crate::doc::ViewDoc::set_create_field(&mut doc, crate::doc::CreateField::Image, "nginx:old");
    let mut driver =
        managed_driver_with_root(doc, Box::new(crate::history_test_open_create_root()));
    let dialog = node_named_role(&driver, "New container", ui::accesskit::Role::Dialog);
    assert_eq!(dialog.role(), ui::accesskit::Role::Dialog);
    let dialog_bounds = dialog.bounds().expect("visible modal has bounds");
    assert!(dialog_bounds.x1 > dialog_bounds.x0 && dialog_bounds.y1 > dialog_bounds.y0);
    let image_id = input_id(&driver, "Image");
    driver
        .perform_a11y_action(image_id, &ui::a11y::Action::Focus)
        .unwrap();
    let image = node_named_role(&driver, "Image", ui::accesskit::Role::TextInput);
    assert_eq!(image.value(), Some("nginx:old"));
    let input_bounds = image.bounds().expect("visible input has bounds");
    assert!(input_bounds.x1 > input_bounds.x0 && input_bounds.y1 > input_bounds.y0);
    assert_eq!(
        driver.a11y_focus().unwrap().1.label(),
        Some("Image"),
        "focus is publicly exported before History"
    );
    let heads = driver.state_mut().doc.get_heads();
    driver.send(&winit::event::WindowEvent::Ime(winit::event::Ime::Preedit(
        "編".into(),
        Some((0, "編".len())),
    )));
    assert_eq!(driver.state_mut().doc.get_heads(), heads);

    history_round_trip(&mut driver, selected);

    let dialog = node_named_role(&driver, "New container", ui::accesskit::Role::Dialog);
    assert_eq!(dialog.role(), ui::accesskit::Role::Dialog);
    let image = node_named_role(&driver, "Image", ui::accesskit::Role::TextInput);
    assert_eq!(image.value(), Some("nginx:old"));
    assert!(image.bounds().is_some());
    assert_eq!(driver.a11y_focus().unwrap().1.label(), Some("Image"));
    driver.send(&winit::event::WindowEvent::Ime(winit::event::Ime::Commit(
        "編".into(),
    )));
    assert!(
        crate::doc::ViewDoc::create_field(&driver.state_mut().doc, crate::doc::CreateField::Image,)
            .contains('編'),
        "retained composition commits through the restored current binding"
    );

    let cancel = driver.a11y_node_named("Cancel").unwrap();
    driver
        .perform_a11y_action(cancel, &ui::a11y::Action::Activate)
        .unwrap();
    let images = node_named(&driver, "Images");
    assert_eq!(images.is_selected(), Some(true));
    let bounds = images
        .bounds()
        .expect("selected navigation target has bounds");
    assert!(bounds.x1 > bounds.x0 && bounds.y1 > bounds.y0);
}

fn assert_public_overlay_retained(root: Box<dyn Widget>, label: &str) {
    let (doc, selected, _) = fixture();
    let mut driver = managed_driver_with_root(doc, root);
    let before = node_named_role(&driver, label, ui::accesskit::Role::Dialog)
        .bounds()
        .expect("visible overlay has public bounds");
    assert!(before.x1 > before.x0 && before.y1 > before.y0);
    history_round_trip(&mut driver, selected);
    let after = node_named_role(&driver, label, ui::accesskit::Role::Dialog)
        .bounds()
        .expect("retained overlay has public bounds");
    assert_eq!(after, before);
}

#[test]
fn managed_history_retains_public_prompt_confirm_and_pull_overlays() {
    assert_public_overlay_retained(Box::new(crate::history_test_prompt_root()), "New volume");
    assert_public_overlay_retained(Box::new(crate::history_test_confirm_root()), "Remove web?");
    assert_public_overlay_retained(
        Box::new(crate::history_test_pull_root()),
        "Pulling quay.io/example/pending:1",
    );
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one managed-history scenario proving every retained private surface"
)]
fn managed_history_retains_private_instances_scroll_engine_intent_and_channels() {
    let (mut doc, selected, _) = fixture();
    crate::doc::ViewDoc::set_section(&mut doc, crate::doc::Section::Containers);
    crate::doc::ViewDoc::set_selected(&mut doc, Some("armed-container"));
    let (root, requests, results, observed, arm, _replacement_requests, _replace_runtime) =
        crate::app::ContainersApp::history_test_armed_retention_root();
    let recv = |label| expect_request(&requests, label);

    let crate::app::ControlledRuntimeRequest::Refresh(initial_refresh) = recv("initial refresh")
    else {
        panic!("expected armed refresh request");
    };
    let crate::app::ControlledRuntimeRequest::Pull(pull) = recv("pull") else {
        panic!("expected armed pull request");
    };
    assert_eq!(pull.reference, "quay.io/example/armed:1");
    assert_eq!(
        pull.arguments,
        crate::podman::pull_args("quay.io/example/armed:1")
    );
    let crate::app::ControlledRuntimeRequest::Create(create) = recv("create") else {
        panic!("expected armed create request");
    };
    assert_eq!(
        create.arguments,
        [
            "run",
            "-d",
            "--name",
            "armed-name",
            "-p",
            "8081:81",
            "quay.io/example/create:1",
        ]
    );
    let crate::app::ControlledRuntimeRequest::SelectedDetails { target } = recv("selected detail")
    else {
        panic!("expected armed selected-detail request");
    };
    assert_eq!(target, "armed-container");
    assert!(requests.try_recv().is_err());

    let mut driver = managed_driver_with_root(doc, Box::new(root));
    arm.set(true);
    driver.relayout();
    let before = observed
        .borrow()
        .clone()
        .expect("initial live retention observation");
    assert_eq!(
        before.pending_control,
        Some(("container:armed-container".into(), "restart".into()))
    );
    assert_eq!(before.background_channels, (true, true));

    driver.click_named("history-open").unwrap();
    let suspended = driver.pending_history_controls();
    assert_single_armed_restart(&suspended);
    assert_eq!(suspended[0].status, PendingControlStatus::Suspended);
    restore_and_return(&mut driver, selected);
    let after = observed
        .borrow()
        .clone()
        .expect("returned live retention observation");
    assert_eq!(after.input_instances, before.input_instances);
    for index in [2, 3, 4, 5] {
        assert_eq!(after.input_values[index], before.input_values[index]);
        assert_eq!(after.input_cursors[index], before.input_cursors[index]);
        assert_eq!(
            after.input_selections[index],
            before.input_selections[index]
        );
    }
    assert_eq!(after.list_offset, before.list_offset);
    assert_eq!(after.detail_offset, before.detail_offset);
    assert_eq!(after.engine_connection, before.engine_connection);
    assert_eq!(after.engine_counts, before.engine_counts);
    assert_eq!(after.last_refresh, before.last_refresh);
    assert_eq!(
        after.pending_control, None,
        "host owns the retained invocation"
    );
    assert_eq!(after.background_channels, before.background_channels);
    assert_eq!(
        crate::doc::ViewDoc::create_field(&driver.state_mut().doc, crate::doc::CreateField::Image,),
        "nginx:old"
    );
    assert_eq!(
        crate::doc::ViewDoc::stored_create_field(
            &driver.state_mut().doc,
            crate::doc::CreateField::Name,
        ),
        crate::doc::StoredCreateField::Absent
    );
    assert_eq!(
        crate::doc::ViewDoc::selected(&driver.state_mut().doc).as_deref(),
        Some("armed-container")
    );
    assert!(
        requests.try_recv().is_err(),
        "managed History emitted no request and did not replay an armed request"
    );
    let ready = driver.pending_history_controls();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].status, PendingControlStatus::Ready);
    let review = driver
        .pending_history_control_token(ready[0].id)
        .expect("fresh exact review token");
    assert_eq!(
        driver.continue_pending_history_control(&review),
        Some(PendingControlStatus::NeedsConfirmation)
    );
    assert!(
        requests.try_recv().is_err(),
        "unconfirmed restart remains inert"
    );

    initial_refresh
        .snapshot(crate::app::ContainersApp::history_test_runtime_engine(
            "retained initial refresh",
            true,
        ))
        .unwrap();
    driver.relayout();
    assert_eq!(
        node_named_role(&driver, "Containers", ui::accesskit::Role::Window).description(),
        Some("retained initial refresh")
    );

    create
        .post_run_snapshot(crate::app::ContainersApp::history_test_runtime_engine(
            "retained create snapshot",
            true,
        ))
        .unwrap();
    driver.relayout();
    assert_eq!(
        node_named_role(&driver, "Containers", ui::accesskit::Role::Window).description(),
        Some("retained create snapshot")
    );

    pull.line("Copying retained armed blob").unwrap();
    pull.done().unwrap();
    driver.relayout();
    let crate::app::ControlledRuntimeRequest::Refresh(pull_refresh) = recv("pull refresh") else {
        panic!("expected refresh after retained pull completion");
    };
    assert_eq!(
        node_named_role(
            &driver,
            "Image pull",
            ui::accesskit::Role::ProgressIndicator,
        )
        .value(),
        Some("100%")
    );

    pull_refresh
        .snapshot(crate::app::ContainersApp::history_test_runtime_engine(
            "retained pull refresh",
            true,
        ))
        .unwrap();
    driver.relayout();
    assert_eq!(
        node_named_role(&driver, "Containers", ui::accesskit::Role::Window).description(),
        Some("retained pull refresh")
    );
    assert!(requests.try_recv().is_err());

    results.send(Ok(())).unwrap();
    let awaiting = driver.pending_history_controls();
    assert_eq!(awaiting.len(), 1);
    assert_eq!(awaiting[0].status, PendingControlStatus::NeedsConfirmation);
    let confirmation = driver
        .pending_history_control_token(awaiting[0].id)
        .expect("fresh exact confirmation token");
    assert_eq!(
        driver.confirm_pending_history_control(&confirmation),
        Some(PendingControlStatus::Consumed)
    );
    assert!(driver.pending_history_controls().is_empty());
    let crate::app::ControlledRuntimeRequest::Immediate {
        operation,
        target,
        arguments,
    } = recv("confirmed restart")
    else {
        panic!("expected one controlled immediate restart");
    };
    assert_eq!(operation, "containers.container.restart");
    assert_eq!(target, "armed-container");
    assert_eq!(arguments, ["restart", "armed-container"]);
    assert!(matches!(
        recv("restart refresh"),
        crate::app::ControlledRuntimeRequest::Refresh(_)
    ));
    assert!(
        requests.try_recv().is_err(),
        "confirmation dispatches exactly once"
    );
    assert!(
        driver
            .confirm_pending_history_control(&confirmation)
            .is_none()
    );

    arm.set(true);
    driver.relayout();
    history_round_trip(&mut driver, selected);
    let ready = driver.pending_history_controls();
    assert_eq!(ready.len(), 1);
    let review = driver.pending_history_control_token(ready[0].id).unwrap();
    assert_eq!(
        driver.continue_pending_history_control(&review),
        Some(PendingControlStatus::NeedsConfirmation)
    );
    driver.set_permission_ctx(ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(None, ui::permissions::LOCAL_USER),
        vec![],
    ));
    let awaiting = driver.pending_history_controls();
    let confirmation = driver
        .pending_history_control_token(awaiting[0].id)
        .expect("fresh revoked confirmation token");
    let refused = driver
        .confirm_pending_history_control(&confirmation)
        .expect("revoked item remains visible");
    assert!(matches!(refused, PendingControlStatus::Refused(_)));
    let retained = driver.pending_history_controls();
    assert_eq!(retained.len(), 1);
    assert_eq!(
        retained[0].invocation.address.resource,
        "container:armed-container"
    );
    assert!(matches!(
        retained[0].status,
        PendingControlStatus::Refused(_)
    ));
    assert!(
        requests.try_recv().is_err(),
        "revoked continuation never reaches runtime"
    );
}

#[test]
fn unreachable_private_route_saves_the_armed_restart_and_drops_the_hidden_emission() {
    let denied = std::rc::Rc::new(std::cell::Cell::new(false));
    let (mut armed, _) = armed_driver(|child| {
        Box::new(CustodyDeniedContainersRoot {
            child,
            denied: denied.clone(),
        })
    });
    denied.set(true);
    armed.driver.click_named("history-open").unwrap();
    // Mounted custody saves the armed restart without private traversal.
    assert_single_armed_restart(&armed.driver.pending_history_controls());

    armed.arm.set(true);
    armed.driver.relayout();
    restore_and_return(&mut armed.driver, armed.selected);
    assert!(
        denied.get(),
        "private traversal stays unavailable through return"
    );
    denied.set(false);
    armed.driver.send(&winit::event::WindowEvent::Focused(true));
    assert!(
        armed.requests.try_recv().is_err(),
        "the hidden emission cannot leak after traversal recovers"
    );
    assert_eq!(
        armed.driver.pending_history_controls().len(),
        1,
        "only the exact saved restart remains"
    );
    continue_and_confirm_restart(&mut armed);
    assert!(
        armed.requests.try_recv().is_err(),
        "the saved restart executes once after explicit confirmation"
    );
}

#[test]
fn mounted_callbacks_drop_hidden_emissions_and_a_fresh_restart_confirms_once() {
    let (mut armed, _) = armed_driver(|root| root);
    armed.driver.click_named("history-open").unwrap();
    assert_single_armed_restart(&armed.driver.pending_history_controls());

    armed.arm.set(true);
    armed.driver.relayout();
    restore_and_return(&mut armed.driver, armed.selected);
    assert_eq!(
        armed.driver.pending_history_controls().len(),
        1,
        "the hidden callback emission is suppressed rather than saved or returned live"
    );
    assert!(
        armed.requests.try_recv().is_err(),
        "History emitted no runtime effect"
    );
    continue_and_confirm_restart(&mut armed);

    let Armed {
        mut driver,
        requests,
        results,
        ..
    } = armed;
    let hide = driver
        .a11y_node_named("Hide")
        .expect("the running pull can be hidden");
    driver
        .perform_a11y_action(hide, &ui::a11y::Action::Activate)
        .unwrap();
    crate::doc::ViewDoc::set_section(&mut driver.state_mut().doc, crate::doc::Section::Containers);
    crate::doc::ViewDoc::set_selected(&mut driver.state_mut().doc, Some("armed-container"));
    driver.relayout();
    let inspection = driver.inspection_json().unwrap();
    assert!(
        inspection.contains("detail-restart-armed-container"),
        "a fresh restart uses the rendered Restart button; inspection: {inspection}"
    );
    let restart = driver
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label() == Some("Restart") && node.role() == ui::accesskit::Role::Button
        })
        .map(|(id, _)| id)
        .expect("the detail offers Restart");
    driver
        .perform_a11y_action(restart, &ui::a11y::Action::Activate)
        .unwrap();
    assert!(
        driver.pending_history_controls().is_empty(),
        "a fresh restart uses the ordinary confirmation surface, not History"
    );
    assert!(
        requests.try_recv().is_err(),
        "the fresh restart waits for confirmation"
    );
    let (field, instruction) = published_palette_control(&driver, "Confirmation");
    let value = instruction
        .as_deref()
        .and_then(|text| text.strip_prefix("Type "))
        .and_then(|text| text.strip_suffix(" to confirm"))
        .filter(|text| !text.is_empty())
        .expect("published confirmation field describes the required text");
    driver
        .perform_semantic_named(
            "action-palette",
            Some(&field),
            &ui::a11y::Action::SetValue(value.into()),
        )
        .unwrap();
    results.send(Ok(())).unwrap();
    let (confirm, _) = published_palette_control(&driver, "Confirm");
    driver
        .perform_semantic_named(
            "action-palette",
            Some(&confirm),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    assert_restart_dispatched(&requests);
    assert!(
        requests.try_recv().is_err(),
        "fresh confirmation dispatches once"
    );
    let coverage = driver.control_coverage();
    assert!(
        coverage.registered >= 2,
        "the window's buttons and the private replace-one producer all hold mounted custody"
    );
    assert_eq!(coverage.unsafe_late_attachments, 0);
}

#[test]
fn retained_restart_refuses_same_address_runtime_replacement_before_confirmation() {
    let (
        Armed {
            mut driver,
            selected,
            requests,
            ..
        },
        (replacement_requests, replace_runtime),
    ) = armed_driver(|root| root);
    history_round_trip(&mut driver, selected);
    let ready = driver.pending_history_controls();
    assert_eq!(ready.len(), 1);
    let review = driver.pending_history_control_token(ready[0].id).unwrap();
    assert_eq!(
        driver.continue_pending_history_control(&review),
        Some(PendingControlStatus::NeedsConfirmation)
    );

    replace_runtime.set(true);
    driver.relayout();
    let changed = driver.pending_history_controls();
    let confirmation = driver
        .pending_history_control_token(changed[0].id)
        .expect("fresh confirmation after runtime replacement");
    let refused = driver
        .confirm_pending_history_control(&confirmation)
        .expect("same-address runtime replacement remains visible");
    assert!(matches!(refused, PendingControlStatus::Refused(_)));
    assert!(
        requests.try_recv().is_err() && replacement_requests.try_recv().is_err(),
        "changed target/runtime identity reaches neither controlled channel"
    );
}

#[test]
fn controlled_pull_stream_crosses_managed_history_once() {
    let (doc, selected, _) = fixture();
    let (mut root, requests, _results) = crate::app::ContainersApp::history_test_runtime_root();
    root.start_pull(" quay.io/example/retained:1 ");
    let crate::app::ControlledRuntimeRequest::Pull(pull) = requests.recv().unwrap() else {
        panic!("expected one controlled pull request");
    };
    assert_eq!(pull.reference, "quay.io/example/retained:1");
    assert_eq!(
        pull.arguments,
        crate::podman::pull_args("quay.io/example/retained:1")
    );
    let mut driver = managed_driver_with_root(doc, Box::new(root));
    history_round_trip(&mut driver, selected);

    pull.line("Copying retained blob").unwrap();
    pull.done().unwrap();
    driver.relayout();
    let progress = node_named_role(
        &driver,
        "Image pull",
        ui::accesskit::Role::ProgressIndicator,
    );
    assert_eq!(progress.value(), Some("100%"));
    assert!(
        driver
            .a11y_node_named("Pulling quay.io/example/retained:1")
            .is_some()
    );
    assert!(matches!(
        requests.recv().unwrap(),
        crate::app::ControlledRuntimeRequest::Refresh(_)
    ));
    driver.relayout();
    assert!(requests.try_recv().is_err());
}

#[test]
fn submitted_create_keeps_copied_args_across_managed_history() {
    let (mut doc, selected, _) = fixture();
    let (mut root, requests, _results) = crate::app::ContainersApp::history_test_runtime_root();
    root.submit_create(&mut doc);
    let crate::app::ControlledRuntimeRequest::Create(create) = requests.recv().unwrap() else {
        panic!("expected one controlled create request");
    };
    assert_eq!(
        create.arguments,
        [
            "run",
            "-d",
            "--name",
            "current-name",
            "-p",
            "8080:80",
            "nginx:new",
        ]
    );
    assert_eq!(
        crate::doc::ViewDoc::stored_create_field(&doc, crate::doc::CreateField::Image),
        crate::doc::StoredCreateField::Absent
    );
    let mut driver = managed_driver_with_root(doc, Box::new(root));
    history_round_trip(&mut driver, selected);
    verify_restored(&driver.state_mut().doc);
    assert_eq!(
        create.arguments.last().map(String::as_str),
        Some("nginx:new")
    );

    let snapshot = crate::model::Engine {
        live: true,
        connection: "retained post-run snapshot".into(),
        ..crate::model::Engine::default()
    };
    create.post_run_snapshot(snapshot).unwrap();
    driver.relayout();
    assert_eq!(
        node_named_role(&driver, "Containers", ui::accesskit::Role::Window).description(),
        Some("retained post-run snapshot")
    );
    driver.relayout();
    assert!(requests.try_recv().is_err());
}
