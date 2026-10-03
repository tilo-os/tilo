//! A component's text input driven the way a live host drives it: pointer
//! and keyboard platform events through `send_host_event`, text through IME
//! commits.

use tilo_ui::application::{AppComponent, HostContext, LaunchTarget, RestoreInput};
use tilo_ui::headless::FrameHarness;
use tilo_ui::launch::LaunchError;
use tilo_ui::ui_graph::{UiDocument, UiDocumentExt as _, UiNode, UiTextRole, UiTextSource};
use tilo_ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};
use tilo_ui::winit::event::{ElementState, Ime, MouseButton, WindowEvent};

use crate::support::application::{ProbeHost, definition, runner_inputs};

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
    fn sync(&mut self, state: &mut tilo_ui::widget::AppModel, path: &tilo_ui::widget::ModelPath) {
        self.0.sync(state, path);
    }
    fn update(
        &mut self,
        state: &mut tilo_ui::widget::AppModel,
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
    fn draw_with_presence(
        &mut self,
        scene: &mut tilo_ui::vello::Scene,
        transform: tilo_ui::kurbo::Affine,
        text: &mut tilo_ui::TextRenderer,
        theme: &tilo_ui::Theme,
        presence: &tilo_ui::presence::PresenceCtx,
        permissions: &tilo_ui::permissions::PermissionContext,
    ) {
        self.0
            .draw_with_presence(scene, transform, text, theme, presence, permissions);
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
        let location = tree.layout(node).map(|l| l.location).unwrap_or_default();
        let inner = tilo_ui::kurbo::Point::new(
            origin.x + f64::from(location.x),
            origin.y + f64::from(location.y),
        );
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
    use automerge::transaction::Transactable as _;
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
                let title = doc
                    .put_object(automerge::ROOT, "title", automerge::ObjType::Text)
                    .map_err(|e| e.to_string())?;
                doc.splice_text(&title, 0, 0, "Hello")
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
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
                size: None,
            },
            UiNode::TextInput {
                id: "title-input".into(),
                label: Some("Title".into()),
                accessible_name: String::new(),
                placeholder: "Untitled".into(),
                value_path: Some("title".into()),
                initial_value: String::new(),
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
    let (key, payload) = frame
        .focus_presence()
        .expect("the focused input publishes its caret");
    assert_eq!(
        key,
        tilo_ui::presence::PresenceKey::Widget {
            path: vec![tilo_ui::presence::PathSegment::Field("title".into())]
        }
    );
    assert_eq!(
        payload,
        tilo_ui::collaboration::PresencePayload::Caret {
            pos: "Hello world".len()
        }
    );
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
