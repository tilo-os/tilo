//! A component's text input driven the way a live host drives it: pointer
//! and keyboard platform events through `send_host_event`, text through IME
//! commits.

use tilo_ui::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
use tilo_ui::headless::FrameHarness;
use tilo_ui::launch::LaunchError;
use tilo_ui::ui_graph::{UiDocument, UiDocumentExt as _, UiNode, UiTextRole, UiTextSource};
use tilo_ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};
use tilo_ui::winit::event::{ElementState, Ime, MouseButton, WindowEvent};

use crate::support::application::{ProbeHost, definition, editor_permissions, runner_inputs};

struct Page(tilo_ui::widget::Element);

impl AppWidget for Page {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Page"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("P")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

impl Widget for Page {
    fn name(&self) -> &'static str {
        "Page"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.0.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.0.as_mut()]
    }
    fn sync(&mut self, state: &tilo_ui::widget::AppModel, path: &tilo_ui::widget::ModelPath) {
        self.0.sync(state, path);
    }
    fn update(
        &mut self,
        state: &tilo_ui::widget::AppModel,
        path: &tilo_ui::widget::ModelPath,
    ) -> bool {
        self.0.update(state, path)
    }
    fn handle_event(
        &mut self,
        event: &tilo_ui::winit::event::WindowEvent,
        ctx: &mut tilo_ui::widget::EventCtx<'_>,
    ) {
        let Some(&child) = ctx.taffy.children(ctx.node_id).unwrap_or_default().first() else {
            return;
        };
        let path = ctx.path.clone();
        self.0.handle_event(event, &mut ctx.reborrow(&path, child));
    }
    fn wants_pointer_capture(&self) -> bool {
        self.0.wants_pointer_capture()
    }
    fn build_layout(
        &mut self,
        tree: &mut tilo_ui::taffy::TaffyTree<()>,
        theme: &tilo_ui::Theme,
        nodes: &mut std::collections::HashMap<tilo_ui::taffy::NodeId, *mut dyn Widget>,
    ) -> tilo_ui::taffy::NodeId {
        use tilo_ui::widget::TaffyTreeExt as _;
        let child = self.0.build_layout(tree, theme, nodes);
        let node = tree.parent_node(
            tilo_ui::taffy::Style {
                display: tilo_ui::taffy::Display::Flex,
                flex_direction: tilo_ui::taffy::FlexDirection::Column,
                size: tilo_ui::taffy::Size {
                    width: tilo_ui::taffy::Dimension::percent(1.0),
                    height: tilo_ui::taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[child],
        );
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(
        &mut self,
        tree: &tilo_ui::taffy::TaffyTree<()>,
        node: tilo_ui::taffy::NodeId,
        origin: tilo_ui::kurbo::Point,
    ) {
        let inner = tilo_ui::widget::layout_rect(tree, node, origin).origin();
        if let Some(&child) = tree.children(node).unwrap_or_default().first() {
            self.0.apply_layout(tree, child, inner);
        }
    }
    fn get_rect(&self) -> tilo_ui::kurbo::Rect {
        self.0.get_rect()
    }
    fn draw(
        &mut self,
        scene: &mut tilo_ui::vello::Scene,
        transform: tilo_ui::kurbo::Affine,
        text: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::Theme,
        presence: &tilo_ui::presence::PresenceCtx,
    ) {
        self.0.draw(scene, transform, text, theme, presence);
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a component factory takes its inputs by value"
)]
fn create(
    mut context: HostContext,
    target: LaunchTarget,
    _: RestoreInput,
) -> Result<AppComponent, LaunchError> {
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|_| Ok(()))
            .map_err(LaunchError::Record)?;
    }
    let root = UiDocument::new(UiNode::Column {
        id: "root".into(),
        children: vec![
            UiNode::Text {
                id: "title-view".into(),
                text: UiTextSource::Document {
                    path: "title".into(),
                },
                role: UiTextRole::Title,
            },
            UiNode::TextInput {
                id: "title-input".into(),
                label: Some("Title".into()),
                accessible_name: String::new(),
                placeholder: "Untitled".into(),
                value_path: Some("title".into()),
                initial_value: "Hello".into(),
                multiline: false,
                disabled: false,
            },
        ],
        gap: Some(8.0),
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    })
    .build(&tilo_ui::Theme::dark())
    .map_err(|e| LaunchError::Operation(e.to_string()))?;
    Ok(AppComponent::new(
        context,
        Box::new(Page(root)),
        tilo_actions::ActionRegistry::new(),
    ))
}

fn harness(host: &ProbeHost) -> FrameHarness {
    static DEFINITION: std::sync::OnceLock<tilo_ui::application::ApplicationDefinition> =
        std::sync::OnceLock::new();
    let definition = DEFINITION.get_or_init(|| definition(create));
    let context = host.prepare(definition, 1, &LaunchTarget::New).unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let mut frame = FrameHarness::from_component(component, (800, 600), runner_inputs()).unwrap();
    frame.relayout();
    frame
}

fn input_rect(frame: &FrameHarness) -> tilo_ui::kurbo::Rect {
    *frame
        .targets()
        .get("title-input")
        .expect("the input is a target")
}

fn press(frame: &mut FrameHarness, x: f64, y: f64) {
    let device_id = tilo_ui::winit::event::DeviceId::dummy();
    frame.send_host_event(&WindowEvent::CursorMoved {
        device_id,
        position: tilo_ui::winit::dpi::PhysicalPosition::new(x, y),
    });
    for state in [ElementState::Pressed, ElementState::Released] {
        frame.send_host_event(&WindowEvent::MouseInput {
            device_id,
            state,
            button: MouseButton::Left,
        });
    }
}

fn title(frame: &FrameHarness) -> String {
    frame.inspection_json().unwrap()
}

#[test]
fn a_pointer_press_focuses_a_components_text_input_and_ime_commits_edit_it() {
    let host = ProbeHost::new();
    let mut frame = harness(&host);
    let rect = input_rect(&frame);
    press(&mut frame, rect.center().x, rect.center().y);
    assert!(
        frame.ime_window().is_some(),
        "the input takes focus and asks for IME"
    );
    frame.send_host_event(&WindowEvent::Ime(Ime::Enabled));
    frame.send_host_event(&WindowEvent::Ime(Ime::Commit(" world".into())));
    assert!(title(&frame).contains("Hello world"), "{}", title(&frame));
    // What a graph input types is this window's own view state, not a
    // shared field: there is no peer to show its caret to.
    assert!(frame.focus_presence().is_none());
}

#[test]
fn tab_reaches_a_components_text_input() {
    let host = ProbeHost::new();
    let mut frame = harness(&host);
    let order = frame.tab_through();
    assert!(
        order
            .iter()
            .any(|(role, _)| *role == tilo_ui::a11y::Role::TextInput),
        "{order:?}"
    );
}

const CREATE_LOCUS: tilo_ui::application::ComponentFactory =
    |context, target, _| locus_component(context, &target, 1);

const CREATE_AMBIGUOUS_LOCUS: tilo_ui::application::ComponentFactory =
    |context, target, _| locus_component(context, &target, 2);

fn locus_component(
    context: HostContext,
    target: &LaunchTarget,
    count: usize,
) -> Result<AppComponent, LaunchError> {
    locus_component_with_scroll(context, target, count, None)
}

const CREATE_PARTIAL_SCROLL_LOCUS: tilo_ui::application::ComponentFactory =
    |context, target, _| locus_component_with_scroll(context, &target, 1, Some(80.0));

const CREATE_HIDDEN_SCROLL_LOCUS: tilo_ui::application::ComponentFactory =
    |context, target, _| locus_component_with_scroll(context, &target, 1, Some(140.0));

fn locus_component_with_scroll(
    mut context: HostContext,
    target: &LaunchTarget,
    count: usize,
    scroll_gap: Option<f32>,
) -> Result<AppComponent, LaunchError> {
    use tilo_ui::widget::{Button, Column, resource_actions};
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|_| Ok(()))
            .map_err(LaunchError::Record)?;
    }
    let resource = resource_actions::resource("locus", "Exact locus", "edit", "Edit");
    let binding = tilo_actions::ActionBinding::unit_for(&resource, "edit").unwrap();
    let mut column = Column::new();
    if let Some(gap) = scroll_gap {
        column = column
            .with_intrinsic_height()
            .with_no_padding()
            .with_gap(0.0)
            .push(Box::new(tilo_ui::widget::Spacer::fixed(gap)));
    }
    for index in 0..count {
        column = column.push(Box::new(
            Button::new(&format!("locus-button-{index}"), "Exact locus").on_action(binding.clone()),
        ));
    }
    let scroll_root: tilo_ui::widget::Element = if scroll_gap.is_some() {
        Box::new(
            tilo_ui::widget::Scrollable::new_vertical()
                .with_height(100.0)
                .with_scrollbar(false)
                .with_child(Box::new(column)),
        )
    } else {
        Box::new(column)
    };
    let root = resource_actions::immediate(scroll_root, resource, |_| {
        Ok(tilo_actions::ActionOutput::default())
    });
    Ok(AppComponent::new(
        context,
        Box::new(Page(root)),
        tilo_actions::ActionRegistry::new(),
    ))
}

#[test]
fn component_publication_locus_respects_scrollable_clipping() {
    for (factory, visible_edge, outer_clip) in [
        (
            CREATE_PARTIAL_SCROLL_LOCUS as tilo_ui::application::ComponentFactory,
            Some(100.0),
            None,
        ),
        (
            CREATE_PARTIAL_SCROLL_LOCUS as tilo_ui::application::ComponentFactory,
            Some(90.0),
            Some(90.0),
        ),
        (
            CREATE_HIDDEN_SCROLL_LOCUS as tilo_ui::application::ComponentFactory,
            None,
            None,
        ),
    ] {
        let host = ProbeHost::new();
        let definition = definition(factory);
        let context = host
            .prepare_with_permissions(&definition, 12, &LaunchTarget::New, editor_permissions())
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let record = component.record().id.as_str().to_owned();
        let mut frame = if let Some(height) = outer_clip {
            FrameHarness::new(
                tilo_ui::State::new(),
                Box::new(
                    tilo_ui::widget::Scrollable::new_vertical()
                        .with_height(height)
                        .with_scrollbar(false)
                        .with_child(Box::new(tilo_ui::widget::TileHost::from_component(
                            component,
                        ))),
                ),
                (800, 600),
            )
        } else {
            FrameHarness::from_component(component, (800, 600), runner_inputs()).unwrap()
        };
        frame.relayout();
        let address =
            tilo_actions::door::survey(&frame.published_bound_resources_of(Some(12)).unwrap())
                .remove(0)
                .address;
        let raw = frame.targets()["locus-button-0"];
        // Both raw controls lie inside the mount; only the scroll viewport
        // determines which of their pixels were painted.
        assert!(raw.y1 < 600.0);
        let before = frame.inspection_json().unwrap();
        let locus = frame.published_action_locus(12, definition.app_id, &record, &address);
        if let Some(edge) = visible_edge {
            let visible = locus.expect("partially visible control");
            assert_eq!(visible.x0, raw.x0);
            assert_eq!(visible.x1, raw.x1);
            assert_eq!(visible.y0, raw.y0);
            assert!(
                visible.y1 < raw.y1 && visible.height() > 0.0,
                "{raw:?} -> {visible:?}"
            );
            assert_eq!(visible.y1, edge);
        } else {
            assert!(locus.is_none(), "offscreen control {raw:?}");
        }
        assert_eq!(frame.inspection_json().unwrap(), before);
    }
}

#[test]
fn component_publication_locus_checks_owner_record_and_lifetime_without_input() {
    let host = ProbeHost::new();
    let definition = definition(CREATE_LOCUS);
    let context = host
        .prepare_with_permissions(&definition, 12, &LaunchTarget::New, editor_permissions())
        .unwrap();
    let component = definition
        .instantiate(context, LaunchTarget::New, RestoreInput::None)
        .unwrap();
    let app = definition.app_id.to_owned();
    let record_id = component.record().id.clone();
    let record = record_id.as_str().to_owned();
    let mut frame = FrameHarness::from_component(component, (800, 600), runner_inputs()).unwrap();
    frame.relayout();
    let resources = frame.published_bound_resources_of(Some(12)).unwrap();
    let address = tilo_actions::door::survey(&resources)
        .into_iter()
        .find(|d| d.address.action == "edit")
        .unwrap()
        .address;
    assert!(address.resource.starts_with("tilo-ui-bound:v1:"));
    let before = frame.inspection_json().unwrap();
    let local_before = frame.state().doc.clone().save();
    let record_before = host
        .records
        .read(&record_id, automerge::transaction::Transactable::base_heads)
        .unwrap();
    let bounds = frame
        .published_action_locus(12, &app, &record, &address)
        .expect("mounted bound button locus");
    assert_eq!(bounds, frame.targets()["locus-button-0"]);
    assert_eq!(frame.inspection_json().unwrap(), before);
    assert_eq!(frame.state().doc.clone().save(), local_before);
    assert_eq!(
        host.records
            .read(&record_id, automerge::transaction::Transactable::base_heads)
            .unwrap(),
        record_before
    );
    assert!(
        frame
            .published_action_locus(13, &app, &record, &address)
            .is_none()
    );
    assert!(
        frame
            .published_action_locus(12, "foreign-app", &record, &address)
            .is_none()
    );
    assert!(
        frame
            .published_action_locus(12, &app, "foreign-record", &address)
            .is_none()
    );
    for index in [2, 3] {
        let mut parts = address
            .resource
            .split(':')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        parts[index] = uuid::Uuid::new_v4().to_string();
        let forged = tilo_actions::ActionAddress::new(parts.join(":"), "edit");
        assert!(
            frame
                .published_action_locus(12, &app, &record, &forged)
                .is_none()
        );
    }
    let invoked = frame
        .invoke_bound_action(
            &tilo_ui::bound_dispatch::BoundActionAddress {
                instance_id: 12,
                address: address.clone(),
            },
            &serde_json::Map::new(),
        )
        .unwrap();
    assert!(matches!(
        invoked,
        tilo_ui::host_operation::ResourceOperationStart::Completed(_)
    ));
    frame.resize((320, 240)).unwrap();
    assert_eq!(
        frame.published_action_locus(12, &app, &record, &address),
        Some(frame.targets()["locus-button-0"])
    );
    frame.open_caller_search("").unwrap();
    assert!(
        frame
            .published_action_locus(12, &app, &record, &address)
            .is_none()
    );
    frame.close_components();
    assert!(
        frame
            .published_action_locus(12, &app, &record, &address)
            .is_none()
    );
}

#[test]
fn component_publication_locus_refuses_ambiguous_controls_and_replaced_mounts() {
    for factory in [
        CREATE_LOCUS as tilo_ui::application::ComponentFactory,
        CREATE_AMBIGUOUS_LOCUS,
    ] {
        let host = ProbeHost::new();
        let definition = definition(factory);
        let context = host
            .prepare_with_permissions(&definition, 12, &LaunchTarget::New, editor_permissions())
            .unwrap();
        let component = definition
            .instantiate(context, LaunchTarget::New, RestoreInput::None)
            .unwrap();
        let app = definition.app_id.to_owned();
        let checkout = component.record().clone();
        let mut frame =
            FrameHarness::from_component(component, (800, 600), runner_inputs()).unwrap();
        frame.relayout();
        let address =
            tilo_actions::door::survey(&frame.published_bound_resources_of(Some(12)).unwrap())
                .remove(0)
                .address;
        let rect = frame.published_action_locus(12, &app, checkout.id.as_str(), &address);
        assert_eq!(
            rect.is_some(),
            std::ptr::fn_addr_eq(
                factory,
                CREATE_LOCUS as tilo_ui::application::ComponentFactory
            )
        );
        frame.close_components();
        let target = crate::support::application::existing(&checkout);
        let context = host
            .prepare_with_permissions(&definition, 12, &target, editor_permissions())
            .unwrap();
        let replacement = definition
            .instantiate(context, target, RestoreInput::None)
            .unwrap();
        let mut replacement =
            FrameHarness::from_component(replacement, (800, 600), runner_inputs()).unwrap();
        replacement.relayout();
        replacement.published_bound_resources_of(Some(12)).unwrap();
        assert!(
            replacement
                .published_action_locus(12, &app, checkout.id.as_str(), &address)
                .is_none()
        );
    }
}

struct WithdrawableResource {
    resource: tilo_actions::Resource,
    published: std::sync::Arc<std::sync::atomic::AtomicBool>,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl AppWidget for WithdrawableResource {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Withdrawable resource"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("W")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

impl Widget for WithdrawableResource {
    tilo_ui::provides!(dyn tilo_ui::resources::PublishesResources);
    fn name(&self) -> &'static str {
        "WithdrawableResource"
    }
    fn build_layout(
        &mut self,
        tree: &mut tilo_ui::taffy::TaffyTree<()>,
        _: &tilo_ui::Theme,
        nodes: &mut std::collections::HashMap<tilo_ui::taffy::NodeId, *mut dyn Widget>,
    ) -> tilo_ui::taffy::NodeId {
        let node = tree.new_leaf(tilo_ui::taffy::Style::default()).unwrap();
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[tilo_ui::widget::resource_actions::OPERATION_TYPE]
    }
    fn start_operation(
        &mut self,
        _: &tilo_ui::host_operation::ResourceOperation<'_>,
        _: &mut tilo_ui::widget::Edits<'_>,
        _: &automerge::ObjId,
    ) -> Result<Option<tilo_ui::host_operation::ResourceOperationStart>, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some(
            tilo_ui::host_operation::ResourceOperationStart::Completed(
                tilo_actions::ActionOutput::default(),
            ),
        ))
    }
}

impl tilo_ui::resources::PublishesResources for WithdrawableResource {
    fn resources(&self, _: &automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.published.load(std::sync::atomic::Ordering::SeqCst) {
            vec![self.resource.clone()]
        } else {
            Vec::new()
        }
    }
}

#[test]
fn component_publication_invocation_cannot_cross_same_instance_owners() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tilo_ui::widget::{Column, TileHost, resource_actions};
    let host = ProbeHost::new();
    let definition = definition(CREATE_LOCUS);
    let mut column = Column::new();
    let mut owners = Vec::new();
    for _ in 0..2 {
        let mut context = host
            .prepare_with_permissions(&definition, 12, &LaunchTarget::New, editor_permissions())
            .unwrap();
        context.initialize_new_record(|_| Ok(())).unwrap();
        let published = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(AtomicUsize::new(0));
        let resource = resource_actions::resource("locus", "Exact locus", "edit", "Edit");
        let component = AppComponent::new(
            context,
            Box::new(WithdrawableResource {
                resource,
                published: published.clone(),
                calls: calls.clone(),
            }),
            tilo_actions::ActionRegistry::new(),
        );
        column = column.push(Box::new(TileHost::from_component(component)));
        owners.push((published, calls));
    }
    let mut frame = FrameHarness::new(tilo_ui::State::new(), Box::new(column), (800, 600));
    let addresses =
        tilo_actions::door::survey(&frame.published_bound_resources_of(Some(12)).unwrap());
    assert_eq!(addresses.len(), 2);
    assert_ne!(addresses[0].address.resource, addresses[1].address.resource);
    let invoke = |frame: &mut FrameHarness, address: &tilo_actions::ActionAddress| {
        frame.invoke_bound_action(
            &tilo_ui::bound_dispatch::BoundActionAddress {
                instance_id: 12,
                address: address.clone(),
            },
            &serde_json::Map::new(),
        )
    };
    // Learn which independently owned alias belongs to the first component.
    invoke(&mut frame, &addresses[0].address).unwrap();
    let first = usize::from(owners[0].1.load(Ordering::SeqCst) != 1);
    let other = 1 - first;
    assert_eq!(owners[other].1.load(Ordering::SeqCst), 0);
    owners[first].0.store(false, Ordering::SeqCst);
    assert!(invoke(&mut frame, &addresses[0].address).is_err());
    assert_eq!(owners[first].1.load(Ordering::SeqCst), 1);
    assert_eq!(owners[other].1.load(Ordering::SeqCst), 0);
    invoke(&mut frame, &addresses[1].address).unwrap();
    assert_eq!(owners[other].1.load(Ordering::SeqCst), 1);
}
