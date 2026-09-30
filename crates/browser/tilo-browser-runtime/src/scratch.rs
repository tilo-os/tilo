//! A built-in application that proves the component path end to end: a
//! title the input edits and the heading shows, stored as an ordinary record.
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::launch::LaunchError;
use ui::record::RecordKind;
use ui::theme::Theme;
use ui::ui_graph::{
    UiButtonStyle, UiDocument, UiDocumentExt as _, UiNode, UiTextRole, UiTextSource,
};
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

static DEFINITION: std::sync::LazyLock<ApplicationDefinition> =
    std::sync::LazyLock::new(|| ApplicationDefinition {
        app_id: "so.tilo.browser-scratch",
        title: "Scratch",
        icon: IconDescriptor::Named("notes".into()),
        record_kind: RecordKind::new("browser-scratch.page"),
        required: vec![],
        optional: vec![],
        opens: &[],
        create: |context, target, _restore: RestoreInput| create(context, &target),
    });

pub(crate) fn definition() -> &'static ApplicationDefinition {
    &DEFINITION
}

fn create(mut context: HostContext, target: &LaunchTarget) -> Result<AppComponent, LaunchError> {
    use ui::automerge::transaction::Transactable as _;
    if matches!(target, LaunchTarget::New) {
        context
            .initialize_new_record(|doc| {
                let title = doc
                    .put_object(ui::automerge::ROOT, "title", ui::automerge::ObjType::Text)
                    .map_err(|error| error.to_string())?;
                doc.splice_text(&title, 0, 0, "Tilo in the browser")
                    .map_err(|error| error.to_string())?;
                Ok(())
            })
            .map_err(LaunchError::Record)?;
    }
    let root = surface()
        .build(&Theme::dark())
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    Ok(AppComponent::new(
        context,
        Box::new(Page(root)),
        tilo_actions::ActionRegistry::new(),
    ))
}

/// The graph-built surface as the component's application root. It owns a
/// layout node around the surface's, so traversal pairs widgets with nodes,
/// and forwards input, updates and presence, whose `Widget` defaults do not
/// reach children.
struct Page(ui::widget::Element);

impl AppWidget for Page {
    fn app_kind(&self) -> AppKind {
        AppKind::Editor
    }
    fn app_title(&self) -> &'static str {
        "Scratch"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Named("notes".into())
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

impl Widget for Page {
    fn name(&self) -> &'static str {
        "ScratchPage"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.0.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.0.as_mut()]
    }
    fn sync(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.0.sync(state, path);
    }
    fn update(&mut self, state: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.0.update(state, path)
    }
    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
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
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.0
            .draw_with_presence(scene, transform, text, theme, presence, permissions);
    }
    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        use ui::widget::TaffyTreeExt as _;
        let child = self.0.build_layout(tree, theme, nodes);
        let node = tree.parent_node(
            ui::taffy::Style {
                display: ui::taffy::Display::Flex,
                flex_direction: ui::taffy::FlexDirection::Column,
                size: ui::taffy::Size {
                    width: ui::taffy::Dimension::percent(1.0),
                    height: ui::taffy::Dimension::percent(1.0),
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
        tree: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        let location = tree.layout(node).map(|l| l.location).unwrap_or_default();
        let inner = ui::kurbo::Point::new(
            origin.x + f64::from(location.x),
            origin.y + f64::from(location.y),
        );
        if let Some(&child) = tree.children(node).unwrap_or_default().first() {
            self.0.apply_layout(tree, child, inner);
        }
    }
    fn get_rect(&self) -> ui::kurbo::Rect {
        self.0.get_rect()
    }
    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.0.draw(scene, transform, text, theme, presence);
    }
}

fn surface() -> UiDocument {
    UiDocument::new(UiNode::Column {
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
            UiNode::Button {
                id: "save".into(),
                label: "Save".into(),
                accessible_name: None,
                tooltip: Some("Record a save request in the document".into()),
                on_click_path: Some("save-requested".into()),
                click_value: Some("yes".into()),
                navigate: None,
                enabled: true,
                style: UiButtonStyle::Primary,
            },
        ],
        gap: Some(8.0),
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    })
}
