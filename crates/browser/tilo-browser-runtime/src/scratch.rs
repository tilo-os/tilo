//! A built-in application that proves the component path end to end: a
//! title the input edits and the heading shows, stored as an ordinary record.
//! Every edit of the title is the page's published `rename` action.
use ui::application::{
    AppComponent, ApplicationDefinition, HostContext, LaunchTarget, RestoreInput,
};
use ui::launch::LaunchError;
use ui::record::RecordKind;
use ui::theme::Theme;
use ui::ui_graph::{
    UiBuildContext, UiButtonRole, UiDocument, UiDocumentExt as _, UiNode, UiTextRole, UiTextSource,
    UiValues,
};
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

/// The action that names the page.
const RENAME_TYPE: &str = "browser-scratch.rename";
/// The page, as the resource its actions belong to.
const PAGE: &str = "browser-scratch:page";

/// Name the page `title`: the title's text becomes `title`, spliced where it
/// changed so a collaborator's concurrent typing survives.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Rename {
    title: String,
}

impl tilo_actions::Action for Rename {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        ui::doc::put_text_diffed(doc, &ui::automerge::ROOT, TITLE, &self.title);
        Ok(())
    }
}

fn page_resource() -> tilo_actions::Resource {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, ExecutionLocus, ParamType};
    tilo_actions::Resource::new(PAGE, "browser-scratch.page", "Scratch page").with_action(
        ActionSpec::typed(
            "rename",
            "Rename",
            "Title",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required("title", ParamType::Text, "Title")],
            |args| EncodedAction {
                type_id: RENAME_TYPE.into(),
                payload: serde_json::json!({ "title": args.text("title").unwrap_or_default() })
                    .to_string()
                    .into_bytes(),
            },
        ),
    )
}

static DEFINITION: std::sync::LazyLock<ApplicationDefinition> =
    std::sync::LazyLock::new(|| ApplicationDefinition {
        app_id: "so.tilo.browser-scratch",
        title: "Scratch",
        icon: IconDescriptor::Named("notes".into()),
        record_kind: RecordKind::new("browser-scratch.page"),
        required: vec![],
        optional: vec![],
        opens: &[],
        room_record: false,
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
    let resource = page_resource();
    let action = resource
        .actions
        .first()
        .ok_or_else(|| LaunchError::Operation("the scratch page has no rename action".into()))?;
    let rename = tilo_actions::ActionBinding::new(&resource, action, |title| {
        let mut arguments = serde_json::Map::new();
        arguments.insert("title".into(), serde_json::Value::String(title));
        arguments
    })
    .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let title = ui::LocalState::new(String::new());
    let values = UiValues::default();
    values.route(TITLE, title.clone(), rename);
    let root = surface()
        .build_with(
            &Theme::dark(),
            &UiBuildContext::default().with_values(values),
        )
        .map_err(|error| LaunchError::Operation(error.to_string()))?;
    let mut actions = tilo_actions::ActionRegistry::new();
    actions.register::<Rename>(RENAME_TYPE);
    Ok(AppComponent::new(
        context,
        Box::new(Page {
            root,
            title,
            resource,
        }),
        actions,
    ))
}

/// The graph-built surface as the component's application root. It owns a
/// layout node around the surface's, so traversal pairs widgets with nodes,
/// and forwards input, updates and presence, whose `Widget` defaults do not
/// reach children. It shows the record's title in the title input and
/// publishes the page's `rename`, which the input's edits invoke.
struct Page {
    root: ui::widget::Element,
    /// The record's title, as the title input shows it.
    title: ui::LocalState<String>,
    resource: tilo_actions::Resource,
}

impl ui::resources::PublishesResources for Page {
    fn resources(&self, _doc: &ui::automerge::AutoCommit) -> Vec<tilo_actions::Resource> {
        vec![self.resource.clone()]
    }
}

/// The value path of the title input and the record field it is kept in.
const TITLE: &str = "title";

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
}

impl Widget for Page {
    ui::provides!(dyn ui::resources::PublishesResources);

    fn name(&self) -> &'static str {
        "ScratchPage"
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.root.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![self.root.as_mut()]
    }
    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        {
            let (doc, root) = ui::doc::document_ref(state, path);
            let stored = ui::doc::read_text(doc, root, TITLE).unwrap_or_default();
            if self.title.with(|shown| *shown != stored) {
                self.title.set(stored);
            }
        }
        self.root.sync(state, path);
    }
    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        self.root.update(state, path)
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
        self.root
            .handle_event(event, &mut ctx.reborrow(&path, child));
    }
    fn wants_pointer_capture(&self) -> bool {
        self.root.wants_pointer_capture()
    }
    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        use ui::widget::TaffyTreeExt as _;
        let child = self.root.build_layout(tree, theme, nodes);
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
        let inner = ui::widget::layout_rect(tree, node, origin).origin();
        if let Some(&child) = tree.children(node).unwrap_or_default().first() {
            self.root.apply_layout(tree, child, inner);
        }
    }
    fn get_rect(&self) -> ui::kurbo::Rect {
        self.root.get_rect()
    }
    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.root.draw(scene, transform, text, theme, presence);
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
                tooltip: Some("Report a save request to the app".into()),
                on_click_path: Some("save-requested".into()),
                click_value: Some("yes".into()),
                navigate: None,
                enabled: true,
                role: UiButtonRole::Primary,
            },
        ],
        gap: Some(8.0),
        no_padding: false,
        intrinsic_height: false,
        fill_remaining_height: false,
    })
}
