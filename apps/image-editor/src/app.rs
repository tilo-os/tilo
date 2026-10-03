use crate::{
    actions::{
        ADD_ADJUSTMENT_TYPE, AddAdjustment, AdjustmentUpdate, RECONNECT_INPUT_TYPE,
        REMOVE_ADJUSTMENT_TYPE, ReconnectInput, RemoveAdjustment, UPDATE_ADJUSTMENT_TYPE,
        UpdateAdjustment,
    },
    doc::ProjectDoc,
    i18n::{t, t_args},
    model::{Operation, Project},
};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use std::{cell::Cell, rc::Rc};
use ui::widget::scrollbar::{ScrollMetrics, Scrollbar};
use ui::widget::{AppModel, ModelPath};
use ui::{
    a11y::{AccessNode, AccessState, Role},
    graph_editor::{ConnectionTarget, GraphEditor},
    taffy::{Dimension, NodeId, Position, Size, Style, TaffyTree},
    vello::{
        Scene,
        kurbo::{Affine, BezPath, Point, Rect, Stroke},
        peniko::{Blob, Fill, ImageAlphaType, ImageData, ImageFormat},
    },
    winit::{
        event::{ElementState, MouseButton, WindowEvent},
        keyboard::{Key, NamedKey},
    },
};
use ui::{
    paint::PaintCtx,
    presence::PresenceCtx,
    surface::Region,
    text_render::TextRenderer,
    theme::Theme,
    tokens::TextRole,
    widget::{
        AvatarFace, Button, ButtonFace, ButtonRole, ControlSize, CropSelection, EmptyState, Image,
        ImageCropEditor, ListRowFace, RowLeading, Slider, SliderFace, TaffyTreeExt, Toolbar,
        Widget, toolbar::Orientation,
    },
};

/// The operation a toolkit button's invocation carries.
const CONTROL_OPERATION_TYPE: &str = "tilo.ui.control.invoke";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ToolbarCommand {
    Open,
    Undo,
    Redo,
    Exposure,
    Contrast,
    Colour,
    Mono,
    Rotate,
    Crop,
    Fit,
    Compare,
    Graph,
    Export,
}

impl ToolbarCommand {
    /// The catalogue key of the command's name.
    fn label_key(self) -> &'static str {
        match self {
            Self::Open => "image-editor-open",
            Self::Undo => "image-editor-undo",
            Self::Redo => "image-editor-redo",
            Self::Exposure => "image-editor-exposure",
            Self::Contrast => "image-editor-contrast",
            Self::Colour => "image-editor-colour",
            Self::Mono => "image-editor-mono",
            Self::Rotate => "image-editor-rotate",
            Self::Crop => "image-editor-crop",
            Self::Fit => "image-editor-fit",
            Self::Compare => "image-editor-compare",
            Self::Graph => "image-editor-graph",
            Self::Export => "image-editor-export",
        }
    }

    /// The stable semantic target assistive technology and agents address.
    fn target(self) -> &'static str {
        match self {
            Self::Open => "image-editor.open",
            Self::Undo => "image-editor.undo",
            Self::Redo => "image-editor.redo",
            Self::Exposure => "image-editor.exposure",
            Self::Contrast => "image-editor.contrast",
            Self::Colour => "image-editor.colour",
            Self::Mono => "image-editor.b&w",
            Self::Rotate => "image-editor.rotate",
            Self::Crop => "image-editor.crop",
            Self::Fit => "image-editor.fit",
            Self::Compare => "image-editor.compare",
            Self::Graph => "image-editor.graph",
            Self::Export => "image-editor.export",
        }
    }

    const ALL: [Self; 13] = [
        Self::Open,
        Self::Undo,
        Self::Redo,
        Self::Exposure,
        Self::Contrast,
        Self::Colour,
        Self::Mono,
        Self::Rotate,
        Self::Crop,
        Self::Fit,
        Self::Compare,
        Self::Graph,
        Self::Export,
    ];
}

/// The commands that add an adjustment, in toolbar order.
const ADJUSTMENT_COMMANDS: [ToolbarCommand; 6] = [
    ToolbarCommand::Exposure,
    ToolbarCommand::Contrast,
    ToolbarCommand::Colour,
    ToolbarCommand::Mono,
    ToolbarCommand::Rotate,
    ToolbarCommand::Crop,
];

/// The adjustment's name as a person reads it, from what it does.
pub(crate) fn operation_label(operation: Option<&Operation>) -> Option<String> {
    let key = match operation? {
        Operation::Exposure { .. } => "image-editor-exposure",
        Operation::Contrast { .. } => "image-editor-contrast",
        Operation::Saturation { .. } => "image-editor-colour",
        Operation::Grayscale => "image-editor-mono",
        Operation::Rotate { .. } => "image-editor-rotate",
        Operation::Crop { .. } => "image-editor-crop",
        Operation::Source { .. } => "image-editor-original",
        Operation::Output => "image-editor-result",
    };
    Some(t(key))
}

/// The header's toolbar: the adjustments, then the view commands. With no
/// image there is nothing to adjust or view, so it holds no commands.
fn build_toolbar(
    pending: &Rc<Cell<Option<ToolbarCommand>>>,
    has_image: bool,
    compare: bool,
    graph: bool,
) -> Toolbar {
    let mut toolbar = Toolbar::new(Orientation::Horizontal);
    if !has_image {
        return toolbar;
    }
    let sink = |command: ToolbarCommand| {
        let pending = Rc::clone(pending);
        move || pending.set(Some(command))
    };
    for command in ADJUSTMENT_COMMANDS {
        toolbar = toolbar.push_button(&t(command.label_key()), sink(command));
    }
    toolbar
        .push_separator()
        .push_button(
            &t(ToolbarCommand::Fit.label_key()),
            sink(ToolbarCommand::Fit),
        )
        .push_toggle(
            &t(ToolbarCommand::Compare.label_key()),
            compare,
            sink(ToolbarCommand::Compare),
        )
        .push_toggle(
            &t(ToolbarCommand::Graph.label_key()),
            graph,
            sink(ToolbarCommand::Graph),
        )
}

/// Where the Compare and Graph toggles sit in [`build_toolbar`]'s toolbar.
const COMPARE_ITEM: usize = ADJUSTMENT_COMMANDS.len() + 2;
const GRAPH_ITEM: usize = ADJUSTMENT_COMMANDS.len() + 3;

/// The header's trailing buttons, in order.
fn header_button(pending: &Rc<Cell<Option<ToolbarCommand>>>, command: ToolbarCommand) -> Button {
    let sink = Rc::clone(pending);
    let id = command.target();
    let label = t(command.label_key());
    let button = match command {
        ToolbarCommand::Undo => Button::icon_only(id, ui::icons::Icon::Undo2, &label),
        ToolbarCommand::Redo => Button::icon_only(id, ui::icons::Icon::Redo2, &label),
        ToolbarCommand::Export => Button::new(id, &label, None).role(ButtonRole::Primary),
        _ => Button::new(id, &label, None),
    };
    button.on_view(move || sink.set(Some(command)))
}

/// A signed value set with a true minus sign, as wide as the plus it
/// stands in for.
fn signed(value: &str) -> String {
    value.replacen('-', "\u{2212}", 1)
}

/// A step image kept small for the adjustment list and the graph.
fn thumbnail(image: &image::RgbaImage) -> ImageData {
    let small = image::imageops::thumbnail(image, 192, 192);
    let (width, height) = small.dimensions();
    ImageData {
        data: Blob::new(std::sync::Arc::new(small.into_raw())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    }
}

/// What the selected step lets a person change in the inspector.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepControls {
    None,
    Slider,
    Rotate,
    Crop,
    RemoveOnly,
}

pub struct ImageEditorApp {
    project: Project,
    search: crate::search::ProjectSearch,
    indexed_project: Project,
    editor: GraphEditor,
    rect: Rect,
    header_rect: Rect,
    title: String,
    title_origin: Point,
    graph_rect: Rect,
    cursor: Point,
    serial: u64,
    /// The last thing that happened, spoken to assistive technology and
    /// shown once in the status bubble.
    notice: String,
    status: ui::StatusBubble,
    picker: Option<ui::portal::PortalChannel>,
    /// A chooser delivering contents, where files have no path (the web).
    contents_picker: Option<ui::portal::ContentsChannel>,
    save_contents: Option<ui::portal::SaveChannel>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    preview: Option<Image>,
    crop_editor: Option<ImageCropEditor>,
    crop_selection: ui::LocalState<CropSelection>,
    crop_node: Option<String>,
    original_preview: Option<Image>,
    rendered_project: Option<Project>,
    canvas_rect: Rect,
    inspector_rect: Rect,
    /// The scrolling viewport of the adjustment list.
    list_rect: Rect,
    /// The selected step's settings under the list.
    controls_rect: Rect,
    slider_rect: Rect,
    rotate_rects: (Rect, Rect),
    remove_rect: Rect,
    inspector_dragging: bool,
    inspector_focused: bool,
    inspector_touch: Option<u64>,
    graph_touch: Option<u64>,
    graph_pan_touch: Option<u64>,
    graph_connection_touch: Option<u64>,
    graph_keyboard_target: Option<ConnectionTarget>,
    /// The graph is centred in its canvas until the person pans or zooms it.
    graph_moved: bool,
    /// The graph's how-to line shows until the person first works in it.
    graph_hint_seen: bool,
    step_rects: Vec<(String, Rect)>,
    stack_scroll: f64,
    stack_content: f64,
    stack_touch: Option<u64>,
    stack_touch_y: f64,
    advanced_graph: bool,
    compare_original: bool,
    zoom: f64,
    pan: ui::vello::kurbo::Vec2,
    panning_from: Option<Point>,
    touches: HashMap<u64, Point>,
    pinch_distance: Option<f64>,
    step_previews: Vec<(String, ImageData)>,
    toolbar: Toolbar,
    toolbar_has_image: bool,
    toolbar_toggles: (bool, bool),
    inspector_text: InspectorText,
    header_buttons: Vec<(ToolbarCommand, Button)>,
    /// The History button: the app owns the window's one header row, so
    /// History lives in it instead of a bar of its own.
    history: Button,
    empty_state: EmptyState,
    toolbar_pending: Rc<Cell<Option<ToolbarCommand>>>,
    /// A screenshot asked for the toolbar's More menu; opened once the
    /// toolbar has been measured.
    staged_overflow: bool,
    undo_stack: Vec<Project>,
    redo_stack: Vec<Project>,
    gesture_checkpoint: Option<Project>,
    /// The authoritative dispatch boundary for every shared graph mutation.
    /// Toolbar, inspector, keyboard and assistive-technology controls all use
    /// this host, matching the actions exposed through global search.
    actions: crate::resource::ActionSink,
}
impl ImageEditorApp {
    pub fn new() -> Self {
        let pending = Rc::new(Cell::new(None));
        let header_buttons = [
            ToolbarCommand::Undo,
            ToolbarCommand::Redo,
            ToolbarCommand::Open,
            ToolbarCommand::Export,
        ]
        .into_iter()
        .map(|command| (command, header_button(&pending, command)))
        .collect();
        let empty_state = EmptyState::new(t("image-editor-empty-title"))
            .with_icon(ui::icons::Icon::Image)
            .with_message(t("image-editor-empty-message"))
            .with_action(t("image-editor-open-image"))
            .with_action_id("image-editor.empty.open");
        let project = Project::blank();
        let search = crate::search::ProjectSearch::new(&project);
        Self {
            project: project.clone(),
            search,
            indexed_project: project,
            editor: GraphEditor::default(),
            rect: Rect::ZERO,
            header_rect: Rect::ZERO,
            title: String::new(),
            title_origin: Point::ZERO,
            graph_rect: Rect::ZERO,
            cursor: Point::ZERO,
            serial: 0,
            notice: String::new(),
            status: ui::StatusBubble::new(),
            picker: None,
            contents_picker: None,
            save_contents: None,
            resource_invocation: None,
            resource_result: None,
            preview: None,
            crop_editor: None,
            crop_selection: ui::LocalState::new(CropSelection::default()),
            crop_node: None,
            original_preview: None,
            rendered_project: None,
            canvas_rect: Rect::ZERO,
            inspector_rect: Rect::ZERO,
            list_rect: Rect::ZERO,
            controls_rect: Rect::ZERO,
            slider_rect: Rect::ZERO,
            rotate_rects: (Rect::ZERO, Rect::ZERO),
            remove_rect: Rect::ZERO,
            inspector_dragging: false,
            inspector_focused: false,
            inspector_touch: None,
            graph_touch: None,
            graph_pan_touch: None,
            graph_connection_touch: None,
            graph_keyboard_target: None,
            graph_moved: false,
            graph_hint_seen: false,
            step_rects: Vec::new(),
            stack_scroll: 0.0,
            stack_content: 0.0,
            stack_touch: None,
            stack_touch_y: 0.0,
            advanced_graph: false,
            compare_original: false,
            zoom: 1.0,
            pan: ui::vello::kurbo::Vec2::ZERO,
            panning_from: None,
            touches: HashMap::new(),
            pinch_distance: None,
            step_previews: Vec::new(),
            toolbar: build_toolbar(&pending, false, false, false),
            toolbar_has_image: false,
            toolbar_toggles: (false, false),
            inspector_text: InspectorText::default(),
            header_buttons,
            history: ui::widget::history_button(),
            empty_state,
            toolbar_pending: pending,
            staged_overflow: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            gesture_checkpoint: None,
            actions: crate::resource::ActionSink::default(),
        }
    }

    /// Stage the view state of a screenshot scenario over the project its
    /// actions built: which step is selected and which view shows.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        let select = match scenario {
            "crop" => "crop-sample",
            "long-stack" => "touch-7",
            _ => "colour-sample",
        };
        if self.project.graph.nodes.contains_key(select) {
            self.editor.select_only(select);
            self.rebuild_crop_editor();
        }
        self.advanced_graph = scenario.contains("graph");
        self.compare_original = scenario == "compare";
        self.staged_overflow = scenario == "narrow-more";
    }

    /// Whether an image is open.
    fn has_image(&self) -> bool {
        !crate::actions::source_path(&self.project).is_empty()
    }

    /// Say what just happened: assistive technology reads it from the
    /// canvas, and the status bubble shows it once.
    fn say(&mut self, notice: impl Into<String>) {
        self.notice = notice.into();
        if !self.notice.is_empty() {
            self.status.show(&self.notice, ui::time::now());
        }
    }

    /// The graph node size, in graph units.
    fn node_size() -> (f64, f64) {
        (168.0, 138.0)
    }

    fn node_rect(&self, id: &str) -> Option<Rect> {
        let n = self.project.graph.nodes.get(id)?;
        let p = self.editor.screen_position(n.position);
        let (width, height) = Self::node_size();
        let (width, height) = (width * self.editor.zoom, height * self.editor.zoom);
        Some(Rect::new(
            self.graph_rect.x0 + p.x,
            self.graph_rect.y0 + p.y,
            self.graph_rect.x0 + p.x + width,
            self.graph_rect.y0 + p.y + height,
        ))
    }

    /// A port's touch target: a finger's width around its small dot.
    fn port_target(center: Point) -> Rect {
        Rect::from_center_size(center, (44.0, 44.0))
    }

    fn input_port_center(&self, id: &str) -> Option<Point> {
        let node = self.project.graph.nodes.get(id)?;
        (!node.inputs.is_empty()).then(|| {
            let rect = self.node_rect(id).unwrap_or(Rect::ZERO);
            Point::new(rect.x0, rect.center().y)
        })
    }

    fn output_port_center(&self, id: &str) -> Option<Point> {
        let node = self.project.graph.nodes.get(id)?;
        (!node.outputs.is_empty()).then(|| {
            let rect = self.node_rect(id).unwrap_or(Rect::ZERO);
            Point::new(rect.x1, rect.center().y)
        })
    }

    fn graph_port_at(&self, point: Point, output: bool) -> Option<(String, String)> {
        self.project.graph.nodes.keys().find_map(|id| {
            let center = if output {
                self.output_port_center(id)
            } else {
                self.input_port_center(id)
            }?;
            Self::port_target(center)
                .contains(point)
                .then(|| {
                    let port = if output {
                        self.project.graph.nodes[id].outputs.first()
                    } else {
                        self.project.graph.nodes[id].inputs.first()
                    }?;
                    Some((id.clone(), port.id.clone()))
                })
                .flatten()
        })
    }

    fn begin_keyboard_connection(&mut self) -> bool {
        let Some(node) = self.editor.selection.iter().next().cloned() else {
            self.say(t("image-editor-connect-select"));
            return false;
        };
        let Some(port) = self
            .project
            .graph
            .nodes
            .get(&node)
            .and_then(|node| node.outputs.first())
            .map(|port| port.id.clone())
        else {
            self.say(t("image-editor-connect-no-output"));
            return false;
        };
        let Some(center) = self.output_port_center(&node) else {
            return false;
        };
        if !self.editor.begin_connection(
            &self.project.graph,
            &node,
            &port,
            ui::graph_editor::GraphPoint {
                x: center.x - self.graph_rect.x0,
                y: center.y - self.graph_rect.y0,
            },
        ) {
            return false;
        }
        self.graph_keyboard_target =
            GraphEditor::next_connection_target(&self.project.graph, &node, &port, None, false);
        self.update_keyboard_connection_pointer();
        self.say(t("image-editor-connect-choose"));
        true
    }

    fn cycle_keyboard_connection(&mut self, backwards: bool) -> bool {
        let Some((node, port, _)) = self.editor.connection_preview() else {
            return false;
        };
        let node = node.to_owned();
        let port = port.to_owned();
        self.graph_keyboard_target = GraphEditor::next_connection_target(
            &self.project.graph,
            &node,
            &port,
            self.graph_keyboard_target.as_ref(),
            backwards,
        );
        self.update_keyboard_connection_pointer();
        true
    }

    fn update_keyboard_connection_pointer(&mut self) {
        let center = self
            .graph_keyboard_target
            .as_ref()
            .and_then(|target| self.input_port_center(&target.node));
        if let Some(center) = center {
            self.editor.update_connection(ui::graph_editor::GraphPoint {
                x: center.x - self.graph_rect.x0,
                y: center.y - self.graph_rect.y0,
            });
        }
    }

    fn finish_keyboard_connection(&mut self, state: &mut AutoCommit) -> bool {
        let Some(target) = self.graph_keyboard_target.take() else {
            self.editor.cancel_connection();
            self.say(t("image-editor-connect-no-input"));
            return true;
        };
        self.begin_gesture();
        match self
            .editor
            .finish_connection(&mut self.project.graph, &target.node, &target.port)
        {
            Ok(_) => {
                self.commit_connection(state, &target.node, &target.port);
                self.say(t("image-editor-connected"));
            }
            Err(_) => {
                self.say(t("image-editor-connect-refused"));
            }
        }
        self.finish_gesture();
        true
    }

    fn push_undo(&mut self, project: Project) {
        if self.undo_stack.last() == Some(&project) {
            return;
        }
        if self.undo_stack.len() == 100 {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(project);
    }

    fn checkpoint(&mut self) {
        self.push_undo(self.project.clone());
        self.redo_stack.clear();
    }

    fn begin_gesture(&mut self) {
        if self.gesture_checkpoint.is_none() {
            self.gesture_checkpoint = Some(self.project.clone());
        }
    }

    fn finish_gesture(&mut self) {
        if let Some(before) = self.gesture_checkpoint.take()
            && before != self.project
        {
            self.push_undo(before);
            self.redo_stack.clear();
        }
    }

    /// Apply a shared graph edit through the same action executor used by the
    /// resource tree and global command palette, then immediately materialize
    /// the CRDT result.  The latter is important: an on-screen edit should
    /// render the converged graph, never a private optimistic copy that could
    /// overwrite a collaborator's independent parameter change on the next
    /// frame.
    fn dispatch_graph_action<A>(&mut self, state: &mut AutoCommit, type_id: &str, action: &A)
    where
        A: tilo_actions::Action + serde::Serialize + crate::actions::AsEditorAction,
    {
        self.actions.dispatch_with_context(
            state,
            type_id,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.image-editor", "Image Editor", action),
        );
        self.project = ProjectDoc::read(state);
        self.rendered_project = None;
        self.rebuild_crop_editor();
    }

    /// Show the project graph the framework resource already holds.
    /// An empty / blank projection does not wipe a graph that has not been
    /// published yet (local source path or adjustments that never reached the
    /// shared document).
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::model::Project) {
        if Self::is_blank_projection(state) && self.has_unpublished_local_edits() {
            return;
        }
        self.project = state.clone();
        self.rendered_project = None;
        self.rebuild_crop_editor();
    }

    fn is_blank_projection(project: &Project) -> bool {
        crate::actions::source_path(project).is_empty()
            && !project
                .graph
                .nodes
                .keys()
                .any(|id| id.as_str() != "source" && id.as_str() != "output")
    }

    fn has_unpublished_local_edits(&self) -> bool {
        !Self::is_blank_projection(&self.project)
    }

    /// Whether [`PublishesResources`](ui::resources::PublishesResources) is
    /// deferring the project door to `tilo_resource::publish`.
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test / attach override so [`PublishesResources`] can keep only live
    /// extras beside `ResourceRoot`.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_framework_hosted(&mut self, hosted: bool) {
        self.actions.set_hosted_for_test(hosted);
    }

    /// Seed a project that has not been written through the framework resource.
    #[cfg(test)]
    pub(crate) fn seed_unpublished_project_for_test(&mut self, project: Project) {
        self.project = project;
        self.rendered_project = None;
        self.rebuild_crop_editor();
    }

    /// Adjustment node ids currently held on the local project surface.
    #[cfg(test)]
    pub(crate) fn local_adjustment_ids_for_test(&self) -> Vec<String> {
        self.project
            .graph
            .nodes
            .keys()
            .filter(|id| id.as_str() != "source" && id.as_str() != "output")
            .cloned()
            .collect()
    }

    /// Source path currently held on the local project surface.
    #[cfg(test)]
    pub(crate) fn local_source_path_for_test(&self) -> String {
        crate::actions::source_path(&self.project).to_owned()
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::ImageEditor>,
    ) {
        self.actions.attach(dispatcher);
    }

    /// Persist a graph position after the editor has provided fluid local drag
    /// feedback. Only the final coordinates become a document operation, so a
    /// drag does not flood collaboration/history while still converging with
    /// keyboard moves and remote edits.
    fn commit_node_position(&mut self, state: &mut AutoCommit, id: &str) {
        let Some((x, y)) = self
            .project
            .graph
            .nodes
            .get(id)
            .map(|node| (node.position.x, node.position.y))
        else {
            return;
        };
        self.dispatch_graph_action(
            state,
            UPDATE_ADJUSTMENT_TYPE,
            &UpdateAdjustment {
                id: id.into(),
                update: AdjustmentUpdate::Position { x, y },
            },
        );
    }

    fn commit_connection(&mut self, state: &mut AutoCommit, node: &str, port: &str) {
        let Some(edge) = self
            .project
            .graph
            .edges
            .iter()
            .find(|edge| edge.to_node == node && edge.to_port == port)
            .cloned()
        else {
            return;
        };
        self.dispatch_graph_action(state, RECONNECT_INPUT_TYPE, &ReconnectInput { edge });
    }

    fn restore_project(&mut self, state: &mut AutoCommit, project: Project, label: &str) {
        let selection_survives = self
            .editor
            .selection
            .iter()
            .any(|id| project.graph.nodes.contains_key(id));
        self.dispatch_graph_action(
            state,
            crate::actions::RESTORE_PROJECT_TYPE,
            &crate::actions::RestoreProject { project },
        );
        if !selection_survives {
            self.editor.select_only(self.project.output.clone());
        }
        self.say(t(label));
    }

    fn undo(&mut self, state: &mut AutoCommit) {
        self.finish_gesture();
        let Some(previous) = self.undo_stack.pop() else {
            self.say(t("image-editor-nothing-to-undo"));
            return;
        };
        self.redo_stack.push(self.project.clone());
        self.restore_project(state, previous, "image-editor-undone");
    }

    fn redo(&mut self, state: &mut AutoCommit) {
        self.finish_gesture();
        let Some(next) = self.redo_stack.pop() else {
            self.say(t("image-editor-nothing-to-redo"));
            return;
        };
        self.push_undo(self.project.clone());
        self.restore_project(state, next, "image-editor-redone");
    }

    fn add(&mut self, state: &mut AutoCommit, op: Operation) {
        self.checkpoint();
        let title = operation_label(Some(&op)).unwrap_or_default();
        // The id's stem names the operation, never the localized title, so
        // ids read the same for every collaborator.
        let stem = match op {
            Operation::Exposure { .. } => "exposure",
            Operation::Contrast { .. } => "contrast",
            Operation::Saturation { .. } => "colour",
            Operation::Grayscale => "mono",
            Operation::Rotate { .. } => "rotate",
            Operation::Crop { .. } => "crop",
            Operation::Source { .. } | Operation::Output => "step",
        };
        let actor = state.get_actor().to_hex_string();
        loop {
            self.serial = self.serial.saturating_add(1);
            let candidate = format!("{stem}-{actor}-{}", self.serial);
            if !self.project.graph.nodes.contains_key(&candidate) {
                break;
            }
        }
        let id = format!("{stem}-{actor}-{}", self.serial);
        self.dispatch_graph_action(
            state,
            ADD_ADJUSTMENT_TYPE,
            &AddAdjustment {
                id: id.clone(),
                title: title.clone(),
                operation: op,
            },
        );
        if self.project.graph.nodes.contains_key(&id) {
            self.editor.select_only(id);
            self.rebuild_crop_editor();
            self.say(t_args("image-editor-added", &[("step", title.into())]));
        } else {
            // Refusal is already logged by ActionHost; don't leave a stale
            // local undo checkpoint behind when no shared graph change landed.
            let _ = self.undo_stack.pop();
        }
    }
    fn open_path(&mut self, state: &mut AutoCommit, path: &std::path::Path) -> bool {
        if image::image_dimensions(path).is_err() {
            self.say(t("image-editor-unsupported"));
            return false;
        }
        self.checkpoint();
        let source = path.to_string_lossy().into_owned();
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map_or_else(|| t("image-editor-untitled"), str::to_owned);
        self.dispatch_graph_action(
            state,
            crate::actions::SET_SOURCE_TYPE,
            &crate::actions::SetSource { path: source, name },
        );
        self.say(t("image-editor-opened"));
        true
    }
    /// Adopt an image chosen by contents: kept in the app's state and named
    /// by the project's source step.
    fn open_contents(&mut self, state: &mut AutoCommit, name: &str, bytes: &[u8]) -> bool {
        let readable = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()
            .and_then(|reader| reader.into_dimensions().ok())
            .is_some();
        if !readable {
            self.say(t("image-editor-unsupported"));
            return false;
        }
        crate::web_file::remember(name, bytes);
        self.checkpoint();
        let source = format!("{}{name}", crate::engine::OPENED_SOURCE);
        let title = std::path::Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .map_or_else(|| t("image-editor-untitled"), str::to_owned);
        self.dispatch_graph_action(
            state,
            crate::actions::SET_SOURCE_TYPE,
            &crate::actions::SetSource {
                path: source,
                name: title,
            },
        );
        // The kept bytes may be a new image under the same name.
        self.rendered_project = None;
        self.say(t("image-editor-opened"));
        true
    }
    fn request_open(&mut self) {
        if self.picker.is_some() || self.contents_picker.is_some() {
            return;
        }
        self.notice.clear();
        let request =
            ui::portal::PickRequest::open(&t("image-editor-pick-title"), &t("image-editor-open"))
                .filter(
                    ui::portal::FileFilter::new(&t("image-editor-pick-filter"))
                        .mimetype("image/png")
                        .mimetype("image/jpeg")
                        .mimetype("image/webp"),
                );
        if !crate::web_file::native_paths() {
            self.contents_picker = Some(request.send_contents());
            return;
        }
        self.picker = Some(
            request
                .thread_name("image-editor-portal-open")
                .all_files()
                .send(),
        );
    }

    /// The file name an export of this project gets.
    fn export_name(&self) -> String {
        let stem = match self.project.operations.get("source") {
            Some(Operation::Source { path }) if !path.is_empty() => {
                let name = crate::engine::source_file_name(path);
                std::path::Path::new(name)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or(name)
                    .to_owned()
            }
            _ => self.project.name.clone(),
        };
        format!("{stem}-edited.png")
    }
    fn poll_contents(&mut self, state: &mut AutoCommit) {
        if let Some(rx) = self.contents_picker.as_ref() {
            match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Ok(ui::portal::ContentsOutcome::Picked(files)) => {
                    self.contents_picker = None;
                    if let Some(file) = files.into_iter().next() {
                        let _ = self.open_contents(state, &file.name, &file.bytes);
                    }
                }
                Ok(ui::portal::ContentsOutcome::Cancelled) => {
                    self.contents_picker = None;
                    self.notice.clear();
                }
                Ok(ui::portal::ContentsOutcome::Failed(message)) => {
                    self.contents_picker = None;
                    self.say(t_args(
                        "image-editor-picker-failed",
                        &[("error", message.as_str().into())],
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.contents_picker = None,
            }
        }
        if let Some(rx) = self.save_contents.as_ref() {
            match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Ok(ui::portal::SaveOutcome::Saved) => {
                    self.save_contents = None;
                    self.say(t_args(
                        "image-editor-exported",
                        &[("file", self.export_name().into())],
                    ));
                }
                Ok(ui::portal::SaveOutcome::Cancelled) => {
                    self.save_contents = None;
                    self.notice.clear();
                }
                Ok(ui::portal::SaveOutcome::Failed(message)) => {
                    self.save_contents = None;
                    self.say(t_args(
                        "image-editor-export-failed",
                        &[("error", message.as_str().into())],
                    ));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.save_contents = None,
            }
        }
    }
    fn poll_picker(&mut self, state: &mut AutoCommit) {
        self.poll_contents(state);
        let Some(rx) = self.picker.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Ok(ui::portal::PortalOutcome::Opened(path)) => {
                self.picker = None;
                let opened = self.open_path(state, &path);
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(if opened {
                        tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        }
                    } else {
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "image-editor.unsupported-image".into(),
                                message: "That file is not a supported image".into(),
                                details: None,
                                retryable: false,
                            },
                        }
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Cancelled) => {
                self.picker = None;
                self.notice.clear();
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("image selection was cancelled".into()),
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Failed(message)) => {
                self.picker = None;
                self.say(t_args(
                    "image-editor-picker-failed",
                    &[("error", message.as_str().into())],
                ));
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "image-editor.portal-failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.picker = None;
                self.say(t("image-editor-picker-stopped"));
            }
        }
    }
    fn export_beside_source(&self) -> Result<String, String> {
        let source = match self.project.operations.get("source") {
            Some(Operation::Source { path })
                if !path.is_empty()
                    && !path.starts_with(crate::engine::OPENED_SOURCE)
                    && path != crate::engine::SAMPLE_SOURCE =>
            {
                std::path::Path::new(path)
            }
            _ => {
                return Err(t("image-editor-export-needs-image"));
            }
        };
        let out = source.with_file_name(self.export_name());
        crate::engine::export(&self.project, &out)
            .map(|()| {
                t_args(
                    "image-editor-exported",
                    &[("file", self.export_name().into())],
                )
            })
            .map_err(|error| {
                t_args(
                    "image-editor-export-failed",
                    &[("error", format!("{error:?}").into())],
                )
            })
    }

    fn export_and_notice(&mut self) {
        let has_file = self.project.operations.get("source").is_some_and(|source| {
            matches!(source, Operation::Source { path }
                if !path.starts_with(crate::engine::OPENED_SOURCE)
                    && path != crate::engine::SAMPLE_SOURCE)
        });
        if !crate::web_file::native_paths() || (self.has_image() && !has_file) {
            if self.save_contents.is_none() {
                match crate::engine::export_png(&self.project) {
                    Ok(bytes) => {
                        self.save_contents = Some(
                            ui::portal::PickRequest::save(
                                &t("image-editor-save-title"),
                                &t("image-editor-export"),
                                &self.export_name(),
                            )
                            .send_save_contents(bytes),
                        );
                    }
                    Err(error) => self.say(t_args(
                        "image-editor-export-failed",
                        &[("error", format!("{error:?}").into())],
                    )),
                }
            }
            return;
        }
        match self.export_beside_source() {
            Ok(notice) | Err(notice) => self.say(notice),
        }
    }
    fn rebuild_preview(&mut self) {
        if self.rendered_project.as_ref() == Some(&self.project) {
            return;
        }
        self.preview = crate::engine::evaluate(&self.project).ok().map(|rgba| {
            let (w, h) = rgba.dimensions();
            Image::from_rgba(std::sync::Arc::from(rgba.into_raw()), w, h)
                .with_alt(t("image-editor-edited-preview"))
        });
        self.original_preview = match self.project.operations.get("source") {
            Some(Operation::Source { path }) if !path.is_empty() => {
                crate::engine::open_source(path).ok().map(|rgba| {
                    let (w, h) = rgba.dimensions();
                    Image::from_rgba(std::sync::Arc::from(rgba.into_raw()), w, h)
                        .with_alt(t("image-editor-original-preview"))
                })
            }
            _ => None,
        };
        // The graph promises an image state at every point, not just at
        // adjustment nodes. Evaluate the source and the result as well so
        // the original and final image never become blank endpoint cards.
        self.step_previews = self
            .project
            .graph
            .evaluation_order()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| {
                crate::engine::evaluate_at(&self.project, &id)
                    .ok()
                    .map(|rgba| (id, thumbnail(&rgba)))
            })
            .collect();
        self.rendered_project = Some(self.project.clone());
        self.crop_node = None;
        self.rebuild_crop_editor();
    }

    fn rebuild_crop_editor(&mut self) {
        let Some(id) = self.editor.selection.iter().next().cloned() else {
            self.crop_editor = None;
            self.crop_node = None;
            return;
        };
        let Some(Operation::Crop {
            x,
            y,
            width,
            height,
        }) = self.project.operations.get(&id)
        else {
            self.crop_editor = None;
            self.crop_node = None;
            return;
        };
        if self.crop_node.as_deref() == Some(id.as_str()) {
            return;
        }
        let Some(upstream) = self
            .project
            .graph
            .edges
            .iter()
            .find(|edge| edge.to_node == id && edge.to_port == "image")
            .map(|edge| edge.from_node.clone())
        else {
            self.crop_editor = None;
            return;
        };
        let Ok(image) = crate::engine::evaluate_at(&self.project, &upstream) else {
            self.crop_editor = None;
            return;
        };
        let selection = CropSelection {
            left: f64::from(*x),
            top: f64::from(*y),
            right: f64::from((*x + *width).clamp(0.0, 1.0)),
            bottom: f64::from((*y + *height).clamp(0.0, 1.0)),
        };
        self.crop_selection.set(selection);
        let (width, height) = image.dimensions();
        self.crop_editor = Some(ImageCropEditor::new(
            std::sync::Arc::from(image.into_raw()),
            width,
            height,
            self.crop_selection.clone(),
        ));
        self.crop_node = Some(id);
    }

    fn commit_crop_editor(&mut self, state: &mut AutoCommit) {
        let Some(id) = self.crop_node.clone() else {
            return;
        };
        let crop = self.crop_selection.get();
        let Some(Operation::Crop {
            x,
            y,
            width,
            height,
        }) = self.project.operations.get(&id)
        else {
            return;
        };
        let next = (
            crop.left as f32,
            crop.top as f32,
            (crop.right - crop.left) as f32,
            (crop.bottom - crop.top) as f32,
        );
        if (*x, *y, *width, *height) == next {
            return;
        }
        if self.gesture_checkpoint.is_none() {
            // Detached/programmatic crop commits are still independently
            // undoable; pointer/touch drags use their coalesced checkpoint.
            self.checkpoint();
        }
        // Crop is intentionally four scalar actions instead of a project
        // replacement. This keeps a collaborator's change to (for example)
        // the vertical bounds when this client commits a horizontal drag.
        for (field, value) in [
            ("x", next.0),
            ("y", next.1),
            ("width", next.2),
            ("height", next.3),
        ] {
            self.dispatch_graph_action(
                state,
                UPDATE_ADJUSTMENT_TYPE,
                &UpdateAdjustment {
                    id: id.clone(),
                    update: AdjustmentUpdate::Number {
                        field: field.into(),
                        value: f64::from(value),
                    },
                },
            );
        }
        self.notice.clear();
    }
    fn steps(&self) -> Vec<String> {
        self.project
            .graph
            .evaluation_order()
            .unwrap_or_default()
            .into_iter()
            .filter(|id| id != "source" && id != "output")
            .collect()
    }

    /// The furthest the adjustment list scrolls.
    fn max_stack_scroll(&self) -> f64 {
        (self.stack_content - self.list_rect.height().max(0.0)).max(0.0)
    }

    fn scroll_stack_by(&mut self, delta: f64) {
        self.stack_scroll = (self.stack_scroll + delta).clamp(0.0, self.max_stack_scroll());
    }

    /// What the selected step offers in the inspector.
    fn step_controls(&self) -> StepControls {
        let Some(id) = self.editor.selection.iter().next() else {
            return StepControls::None;
        };
        if id == "source" || id == &self.project.output {
            return StepControls::None;
        }
        match self.project.operations.get(id) {
            Some(
                Operation::Exposure { .. }
                | Operation::Contrast { .. }
                | Operation::Saturation { .. },
            ) => StepControls::Slider,
            Some(Operation::Rotate { .. }) => StepControls::Rotate,
            Some(Operation::Crop { .. }) => StepControls::Crop,
            Some(Operation::Grayscale) => StepControls::RemoveOnly,
            _ => StepControls::None,
        }
    }

    fn selected_is_removable(&self) -> bool {
        self.editor.selection.iter().next().is_some_and(|id| {
            id != "source" && id != &self.project.output && self.project.operations.contains_key(id)
        })
    }

    fn remove_selected(&mut self, state: &mut AutoCommit) -> bool {
        let Some(id) = self.editor.selection.iter().next().cloned() else {
            return false;
        };
        if id == "source" || id == self.project.output {
            return false;
        }
        self.checkpoint();
        self.dispatch_graph_action(
            state,
            REMOVE_ADJUSTMENT_TYPE,
            &RemoveAdjustment { id: id.clone() },
        );
        if self.project.graph.nodes.contains_key(&id) {
            let _ = self.undo_stack.pop();
            return false;
        }
        if let Some(latest) = self.steps().last().cloned() {
            self.editor.select_only(latest);
        } else {
            self.editor.select_only(self.project.output.clone());
        }
        self.say(t_args("image-editor-removed", &[("count", 1.into())]));
        true
    }

    fn selected_slider(&self) -> Option<(f64, String)> {
        let id = self.editor.selection.iter().next()?;
        match self.project.operations.get(id)? {
            Operation::Exposure { stops } => {
                let value = f64::from((*stops + 5.0) / 10.0);
                Some((value, signed(&format!("{stops:+.1} EV"))))
            }
            Operation::Contrast { amount } => {
                let value = f64::from((*amount + 1.0) / 3.0);
                Some((value, signed(&format!("{:+.0}%", *amount * 100.0))))
            }
            Operation::Saturation { amount } => {
                let value = f64::from(*amount / 3.0);
                Some((value, format!("{:.0}%", *amount * 100.0)))
            }
            _ => None,
        }
    }

    fn inspector_value_at(&self, x: f64) -> f64 {
        let track = SliderFace::new(self.slider_rect, 0.0).track_rect();
        ((x - track.x0) / track.width().max(1.0)).clamp(0.0, 1.0)
    }

    fn set_selected_slider(&mut self, state: &mut AutoCommit, normalized: f64) -> bool {
        let Some(id) = self.editor.selection.iter().next().cloned() else {
            return false;
        };
        if !matches!(
            self.project.operations.get(&id),
            Some(
                Operation::Exposure { .. }
                    | Operation::Contrast { .. }
                    | Operation::Saturation { .. }
            )
        ) {
            return false;
        }
        let normalized = normalized.clamp(0.0, 1.0) as f32;
        let Some(operation) = self.project.operations.get(&id) else {
            return false;
        };
        let (field, next, changed) = match operation {
            Operation::Exposure { stops } => {
                let next = normalized * 10.0 - 5.0;
                ("stops", next, (*stops - next).abs() > 0.001)
            }
            Operation::Contrast { amount } => {
                let next = normalized * 3.0 - 1.0;
                ("amount", next, (*amount - next).abs() > 0.001)
            }
            Operation::Saturation { amount } => {
                let next = normalized * 3.0;
                ("amount", next, (*amount - next).abs() > 0.001)
            }
            _ => return false,
        };
        if !changed {
            return false;
        }
        if self.gesture_checkpoint.is_none() {
            self.checkpoint();
        }
        self.dispatch_graph_action(
            state,
            UPDATE_ADJUSTMENT_TYPE,
            &UpdateAdjustment {
                id,
                update: AdjustmentUpdate::Number {
                    field: field.into(),
                    value: f64::from(next),
                },
            },
        );
        self.notice.clear();
        true
    }
    fn select_relative_step(&mut self, direction: isize) {
        let mut visual = self.steps();
        visual.reverse();
        if visual.is_empty() {
            return;
        }
        let current = visual
            .iter()
            .position(|id| self.editor.selection.contains(id))
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(direction)
            .min(visual.len().saturating_sub(1));
        if let Some(id) = visual.get(next) {
            self.editor.select_only(id.clone());
            self.rebuild_crop_editor();
        }
    }
    fn adjust_selected(&mut self, state: &mut AutoCommit, direction: f32) {
        let Some(id) = self.editor.selection.iter().next().cloned() else {
            return;
        };
        if !matches!(
            self.project.operations.get(&id),
            Some(
                Operation::Exposure { .. }
                    | Operation::Contrast { .. }
                    | Operation::Saturation { .. }
                    | Operation::Rotate { .. }
            )
        ) {
            return;
        }
        let (field, next) = match self.project.operations.get(&id) {
            Some(Operation::Exposure { stops }) => {
                ("stops", (*stops + direction * 0.1).clamp(-5.0, 5.0))
            }
            Some(Operation::Contrast { amount }) => {
                ("amount", (*amount + direction * 0.05).clamp(-1.0, 2.0))
            }
            Some(Operation::Saturation { amount }) => {
                ("amount", (*amount + direction * 0.05).clamp(0.0, 3.0))
            }
            Some(Operation::Rotate { quarter_turns }) if direction > 0.0 => {
                ("quarter_turns", ((*quarter_turns + 1) % 4) as f32)
            }
            Some(Operation::Rotate { quarter_turns }) => {
                ("quarter_turns", ((*quarter_turns + 3) % 4) as f32)
            }
            _ => return,
        };
        self.checkpoint();
        self.dispatch_graph_action(
            state,
            UPDATE_ADJUSTMENT_TYPE,
            &UpdateAdjustment {
                id,
                update: AdjustmentUpdate::Number {
                    field: field.into(),
                    value: f64::from(next),
                },
            },
        );
    }
    /// The header buttons, which the app holds rather than exposing as
    /// children. Control custody still reaches them here.
    fn retained_buttons_mut(&mut self) -> Vec<&mut Button> {
        self.header_buttons
            .iter_mut()
            .map(|(_, button)| button)
            .chain(std::iter::once(&mut self.history))
            .collect()
    }
    fn run_toolbar(&mut self, state: &mut AutoCommit, command: ToolbarCommand) {
        match command {
            ToolbarCommand::Open => self.request_open(),
            ToolbarCommand::Undo => self.undo(state),
            ToolbarCommand::Redo => self.redo(state),
            ToolbarCommand::Exposure => self.add(state, Operation::Exposure { stops: 0.5 }),
            ToolbarCommand::Contrast => self.add(state, Operation::Contrast { amount: 0.15 }),
            ToolbarCommand::Colour => self.add(state, Operation::Saturation { amount: 1.2 }),
            ToolbarCommand::Mono => self.add(state, Operation::Grayscale),
            ToolbarCommand::Rotate => self.add(state, Operation::Rotate { quarter_turns: 1 }),
            ToolbarCommand::Crop => self.add(
                state,
                Operation::Crop {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
            ),
            ToolbarCommand::Fit => {
                self.zoom = 1.0;
                self.pan = ui::vello::kurbo::Vec2::ZERO;
                self.graph_moved = false;
            }
            ToolbarCommand::Compare => self.compare_original = !self.compare_original,
            ToolbarCommand::Graph => self.advanced_graph = !self.advanced_graph,
            ToolbarCommand::Export => self.export_and_notice(),
        }
        self.sync_toolbar();
    }

    /// Whether the processing graph shows in place of the canvas.
    fn graph_shown(&self) -> bool {
        self.advanced_graph && self.has_image()
    }

    /// A press in the inspector: a step in the list, the slider, the rotate
    /// buttons or Remove. `touch` names the finger that drags the slider.
    fn press_inspector(&mut self, state: &mut AutoCommit, point: Point, touch: Option<u64>) {
        let (left, right) = self.rotate_rects;
        if self.selected_is_removable() && self.remove_rect.contains(point) {
            self.inspector_focused = false;
            self.remove_selected(state);
        } else if self.selected_slider().is_some() && self.slider_rect.contains(point) {
            self.begin_gesture();
            match touch {
                Some(id) => self.inspector_touch = Some(id),
                None => self.inspector_dragging = true,
            }
            self.inspector_focused = true;
            self.panning_from = None;
            self.set_selected_slider(state, self.inspector_value_at(point.x));
        } else if left.contains(point) {
            self.inspector_focused = false;
            self.adjust_selected(state, -1.0);
        } else if right.contains(point) {
            self.inspector_focused = false;
            self.adjust_selected(state, 1.0);
        } else if self.list_rect.contains(point)
            && let Some((id, _)) = self
                .step_rects
                .iter()
                .find(|(_, rect)| rect.contains(point))
                .cloned()
        {
            self.inspector_focused = false;
            self.editor.select_only(id);
            self.rebuild_crop_editor();
        }
    }

    /// Keep the toolbar's commands and toggles, and the header buttons'
    /// availability, in step with the project.
    fn sync_toolbar(&mut self) {
        let has_image = self.has_image();
        if has_image != self.toolbar_has_image {
            self.toolbar = build_toolbar(
                &self.toolbar_pending,
                has_image,
                self.compare_original,
                self.advanced_graph,
            );
            self.toolbar_has_image = has_image;
        } else if has_image && self.toolbar_toggles != (self.compare_original, self.advanced_graph)
        {
            self.toolbar.set_active(COMPARE_ITEM, self.compare_original);
            self.toolbar.set_active(GRAPH_ITEM, self.advanced_graph);
        }
        self.toolbar_toggles = (self.compare_original, self.advanced_graph);
        let (can_undo, can_redo) = (!self.undo_stack.is_empty(), !self.redo_stack.is_empty());
        for (command, button) in &mut self.header_buttons {
            button.set_enabled(match command {
                ToolbarCommand::Undo => can_undo,
                ToolbarCommand::Redo => can_redo,
                ToolbarCommand::Export => has_image,
                _ => true,
            });
        }
    }

    /// Place the header, the canvas or graph, the inspector and the selected
    /// step's settings for this frame.
    fn lay_out(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let (s1, s2, s3, s4, s6) = (
            f64::from(spacing.s1),
            f64::from(spacing.s2),
            f64::from(spacing.s3),
            f64::from(spacing.s4),
            f64::from(spacing.s6),
        );
        let rect = self.rect;
        let has_image = self.has_image();

        // The header: the project's name, its toolbar, then undo and redo,
        // Open and Export at the trailing edge.
        self.header_rect = Rect::new(
            rect.x0,
            rect.y0,
            rect.x1,
            rect.y0 + f64::from(metrics.header_height),
        );
        let band = self.header_rect;
        let control_h = ButtonFace::height(ControlSize::Regular, theme);
        let control_y = band.center().y - control_h / 2.0;
        let mut x = band.x1 - s4;
        for (command, button) in self.header_buttons.iter_mut().rev() {
            let label = t(command.label_key());
            let face = match command {
                ToolbarCommand::Undo => {
                    ButtonFace::icon_only(Rect::ZERO, ui::icons::Icon::Undo2, &label)
                }
                ToolbarCommand::Redo => {
                    ButtonFace::icon_only(Rect::ZERO, ui::icons::Icon::Redo2, &label)
                }
                _ => ButtonFace::new(Rect::ZERO, &label),
            };
            let width = face.intrinsic_width(tr, theme);
            button.apply_layout_external(Rect::new(x - width, control_y, x, control_y + control_h));
            x -= width
                + match command {
                    ToolbarCommand::Open => s4,
                    ToolbarCommand::Redo => s1,
                    _ => s2,
                };
        }
        // History shows its name while the toolbar has room for every
        // command, and only its icon when that keeps a command out of More.
        let history_label = ui::i18n::I18nManager::lookup_plain("standalone-history-title");
        let trailing_end = x - (s4 - s2);
        let title_style = theme.text(TextRole::Title);
        let toolbar_h = f64::from(metrics.min_pointer_target);
        for compact in [false, true] {
            self.history.set_compact_toolbar_presentation(compact);
            let history_face = if compact {
                ButtonFace::icon_only(Rect::ZERO, ui::icons::Icon::History, &history_label)
            } else {
                ButtonFace::new(Rect::ZERO, &history_label).icon(ui::icons::Icon::History)
            };
            let history_w = history_face.intrinsic_width(tr, theme);
            self.history.apply_layout_external(Rect::new(
                trailing_end - history_w,
                control_y,
                trailing_end,
                control_y + control_h,
            ));
            let trailing_x0 = trailing_end - history_w;
            // The project's name, unless the window is too narrow to read it.
            let room = (trailing_x0 - band.x0) * 0.4;
            self.title = if has_image && room >= f64::from(metrics.min_pointer_target) * 3.0 {
                tr.elide_to_width_style(&self.project.name, room as f32, &title_style)
            } else {
                String::new()
            };
            let title_w = f64::from(tr.measure_style(&self.title, None, &title_style).0);
            self.title_origin = Point::new(band.x0 + s4, band.y0);
            let toolbar_x0 = if self.title.is_empty() {
                band.x0 + s4
            } else {
                band.x0 + s4 + title_w + s6
            };
            let toolbar_rect = Rect::new(
                toolbar_x0,
                band.center().y - toolbar_h / 2.0,
                (trailing_x0 - s4).max(toolbar_x0),
                band.center().y + toolbar_h / 2.0,
            );
            self.toolbar.lay_out_in(toolbar_rect, tr, theme);
            if self.toolbar.visible_count() == 0 && self.toolbar.has_overflow() {
                // Only the More button fits: it leads the row, where the
                // commands would have started.
                let chip = Rect::new(
                    toolbar_rect.x0,
                    toolbar_rect.y0,
                    toolbar_rect.x0 + toolbar_h,
                    toolbar_rect.y1,
                );
                self.toolbar.lay_out_in(chip, tr, theme);
            }
            if !self.toolbar.has_overflow() {
                break;
            }
        }

        // The body: the canvas (or the graph) and, once there is an image,
        // the inspector beside it, or under it in a narrow window.
        let body = Rect::new(rect.x0, band.y1, rect.x1, rect.y1.max(band.y1));
        let inspector_w = f64::from(metrics.inspector_width);
        let (main, inspector) = if !has_image {
            (body, Rect::ZERO)
        } else if body.width() >= f64::from(metrics.content_min_width) + inspector_w {
            (
                Rect::new(body.x0, body.y0, body.x1 - inspector_w, body.y1),
                Rect::new(body.x1 - inspector_w, body.y0, body.x1, body.y1),
            )
        } else {
            let height = (body.height() * 0.45).round();
            (
                Rect::new(body.x0, body.y0, body.x1, body.y1 - height),
                Rect::new(body.x0, body.y1 - height, body.x1, body.y1),
            )
        };
        self.inspector_rect = inspector;
        if self.graph_shown() {
            self.graph_rect = main;
            self.canvas_rect = Rect::ZERO;
        } else {
            self.graph_rect = Rect::ZERO;
            self.canvas_rect = main;
        }
        self.empty_state.apply_layout_external(main);

        // The inspector: the "Adjustments" heading, the list of steps, and
        // the selected step's settings under it.
        self.slider_rect = Rect::ZERO;
        self.rotate_rects = (Rect::ZERO, Rect::ZERO);
        self.remove_rect = Rect::ZERO;
        self.controls_rect = Rect::ZERO;
        self.list_rect = Rect::ZERO;
        self.inspector_text = InspectorText::default();
        if inspector.area() <= 0.0 {
            return;
        }
        let content_x0 = inspector.x0 + s2 + s3;
        let content_x1 = inspector.x1 - s2 - s3;
        let heading_h = f64::from(
            tr.measure_style("Ag", None, &theme.text(TextRole::Heading))
                .1,
        );
        self.inspector_text.heading = Point::new(content_x0, inspector.y0 + s4);
        let list_top = inspector.y0 + s4 + heading_h + s2;

        let controls = self.step_controls();
        let line = |role: TextRole, tr: &mut TextRenderer| {
            f64::from(tr.measure_style("Ag", None, &theme.text(role)).1)
        };
        let row_h = f64::from(metrics.min_pointer_target);
        let remove_h = control_h;
        // A parameter is one line naming it and showing its value, then
        // its control across the whole column.
        let label_line = line(TextRole::Label, tr);
        let body_h = match controls {
            StepControls::None => 0.0,
            StepControls::RemoveOnly => remove_h,
            StepControls::Crop => {
                let hint = t("image-editor-crop-hint");
                let (_, h) = tr.measure_style(
                    &hint,
                    Some((content_x1 - content_x0).max(1.0) as f32),
                    &theme.text(TextRole::Caption),
                );
                f64::from(h) + s3 + remove_h
            }
            StepControls::Slider | StepControls::Rotate => label_line + s2 + row_h + s4 + remove_h,
        };
        if body_h <= 0.0 {
            self.list_rect = Rect::new(inspector.x0, list_top, inspector.x1, inspector.y1 - s2);
            return;
        }
        let controls_top = (inspector.y1 - body_h - 2.0 * s4).max(list_top);
        self.controls_rect = Rect::new(inspector.x0, controls_top, inspector.x1, inspector.y1);
        self.list_rect = Rect::new(inspector.x0, list_top, inspector.x1, controls_top);
        let mut y = controls_top + s4;
        match controls {
            StepControls::Slider | StepControls::Rotate => {
                let band = Rect::new(content_x0, y, content_x1, y + label_line);
                self.inspector_text.label = Some(band);
                self.inspector_text.value = Some(band);
                y = band.y1 + s2;
                let row = Rect::new(content_x0, y, content_x1, y + row_h);
                if controls == StepControls::Slider {
                    self.slider_rect = row;
                } else {
                    let left = Rect::new(row.x0, row.y0, row.x0 + row_h, row.y1);
                    let right = Rect::new(left.x1 + s2, row.y0, left.x1 + s2 + row_h, row.y1);
                    self.rotate_rects = (left, right);
                }
                y = row.y1 + s4;
            }
            StepControls::Crop => {
                let hint = t("image-editor-crop-hint");
                let width = (content_x1 - content_x0).max(1.0);
                let (_, h) =
                    tr.measure_style(&hint, Some(width as f32), &theme.text(TextRole::Caption));
                self.inspector_text.hint =
                    Some(Rect::new(content_x0, y, content_x1, y + f64::from(h)));
                y += f64::from(h) + s3;
            }
            StepControls::RemoveOnly | StepControls::None => {}
        }
        if controls != StepControls::None {
            let label = t("image-editor-remove");
            let width = ButtonFace::new(Rect::ZERO, &label)
                .icon(ui::icons::Icon::Trash2)
                .intrinsic_width(tr, theme);
            self.remove_rect = Rect::new(content_x0, y, content_x0 + width, y + remove_h);
        }
    }

    /// The name of the selected step's one parameter.
    fn selected_parameter_label(&self) -> String {
        let operation = self
            .editor
            .selection
            .iter()
            .next()
            .and_then(|id| self.project.operations.get(id));
        match operation {
            Some(Operation::Rotate { .. }) => t("image-editor-rotation"),
            Some(Operation::Saturation { .. }) => t("image-editor-saturation"),
            other => operation_label(other).unwrap_or_default(),
        }
    }

    /// The selected rotation, as a person reads it.
    fn selected_rotation(&self) -> Option<String> {
        let id = self.editor.selection.iter().next()?;
        match self.project.operations.get(id)? {
            Operation::Rotate { quarter_turns } => Some(format!("{}°", (quarter_turns % 4) * 90)),
            _ => None,
        }
    }

    /// Fit the graph into its canvas, centred, until the person moves it.
    /// A graph too long for the canvas, even at its smallest legible size,
    /// starts at its first step and pans from there.
    fn centre_graph(&mut self, theme: &Theme) {
        if self.graph_moved || self.graph_rect.area() <= 0.0 {
            return;
        }
        let mut bounds: Option<Rect> = None;
        let (width, height) = Self::node_size();
        for node in self.project.graph.nodes.values() {
            let r = Rect::new(
                node.position.x,
                node.position.y,
                node.position.x + width,
                node.position.y + height,
            );
            bounds = Some(bounds.map_or(r, |b| b.union(r)));
        }
        let Some(bounds) = bounds else {
            return;
        };
        let margin = f64::from(theme.tokens.spacing.s8);
        let area = self.graph_rect;
        let room_w = (area.width() - 2.0 * margin).max(1.0);
        let room_h = (area.height() - 2.0 * margin).max(1.0);
        let zoom = (room_w / bounds.width())
            .min(room_h / bounds.height())
            .clamp(0.6, 1.0);
        self.editor.zoom = zoom;
        let fit_x = (area.width() - bounds.width() * zoom) / 2.0;
        let fit_y = (area.height() - bounds.height() * zoom) / 2.0;
        self.editor.pan.x = fit_x.max(margin) - bounds.x0 * zoom;
        self.editor.pan.y = fit_y.max(margin) - bounds.y0 * zoom;
    }
}

/// Where the inspector's text goes this frame.
#[derive(Clone, Copy, Debug, Default)]
struct InspectorText {
    heading: Point,
    /// The parameter's name, centred on its control's line.
    label: Option<Rect>,
    /// The parameter's value, trailing, centred on the same line.
    value: Option<Rect>,
    /// The crop instructions.
    hint: Option<Rect>,
}

impl ImageEditorApp {
    /// The image on the window ground: the edited result, the original while
    /// comparing, or the crop editor over the step before a selected crop.
    fn draw_canvas(&mut self, s: &mut Scene, a: Affine, theme: &Theme) {
        let inset = f64::from(theme.tokens.spacing.s6);
        let viewport = self.canvas_rect.inset(-inset);
        if viewport.width() <= 0.0 || viewport.height() <= 0.0 {
            return;
        }
        let target = Rect::from_center_size(
            viewport.center(),
            (viewport.width() * self.zoom, viewport.height() * self.zoom),
        ) + self.pan;
        s.push_clip_layer(Fill::NonZero, a, &self.canvas_rect);
        if !self.compare_original
            && let Some(editor) = self.crop_editor.as_mut()
        {
            editor.paint_at(s, a, theme, target);
        } else if let Some(image) = if self.compare_original {
            self.original_preview.as_mut()
        } else {
            self.preview.as_mut()
        } {
            image.paint_at_on(s, a, target, theme);
        }
        s.pop_layer();
    }

    /// The inspector region: the adjustment list, latest step first, and the
    /// selected step's settings under it.
    fn draw_inspector(&mut self, s: &mut Scene, a: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let ground = ui::surface::region(s, a, self.inspector_rect, Region::Inspector, theme);
        let local = theme.in_region(Region::Inspector);
        let spacing = &local.tokens.spacing;
        let (s2, s3) = (f64::from(spacing.s2), f64::from(spacing.s3));
        let gutter = f64::from(local.tokens.metrics.scrollbar_gutter);
        let mut ctx = PaintCtx::new(s, a, tr, &local);
        ctx.text_role(
            &t("image-editor-adjustments"),
            TextRole::Heading,
            local.text_primary,
            self.inspector_text.heading,
            None,
        );

        // The list, latest step first.
        let steps = self.steps();
        let count = steps.len();
        let rows: Vec<(String, String, String)> = steps
            .iter()
            .rev()
            .enumerate()
            .map(|(index, id)| {
                let title = operation_label(self.project.operations.get(id))
                    .or_else(|| self.project.graph.nodes.get(id).map(|n| n.title.clone()))
                    .unwrap_or_default();
                let step = t_args("image-editor-step", &[("number", (count - index).into())]);
                (id.clone(), title, step)
            })
            .collect();
        let list = self.list_rect;
        let selected: Vec<bool> = rows
            .iter()
            .map(|(id, _, _)| self.editor.selection.contains(id))
            .collect();
        let faces: Vec<ListRowFace<'_>> = rows
            .iter()
            .enumerate()
            .map(|(index, (id, title, step))| {
                let thumb = self
                    .step_previews
                    .iter()
                    .find(|(step_id, _)| step_id == id)
                    .map(|(_, image)| image);
                let face = ListRowFace::new(title)
                    .secondary(step)
                    .leading(RowLeading::Avatar(
                        AvatarFace::new(title, id).image(thumb).square(),
                    ))
                    .selected(selected[index]);
                match selected.get(index + 1) {
                    Some(next) => face.separator(*next),
                    None => face,
                }
            })
            .collect();
        let heights: Vec<f64> = faces
            .iter()
            .map(|face| face.height(ctx.text, ctx.theme))
            .collect();
        self.stack_content = heights.iter().sum();
        self.stack_scroll = self.stack_scroll.clamp(0.0, self.max_stack_scroll());
        let overflows = self.stack_content > list.height();
        let row_x0 = list.x0 + s2;
        let row_x1 = if overflows {
            list.x1 - gutter
        } else {
            list.x1 - s2
        };
        self.step_rects.clear();
        ctx.scene.push_clip_layer(Fill::NonZero, a, &list);
        let mut y = list.y0 - self.stack_scroll;
        for ((face, height), (id, _, _)) in faces.iter().zip(&heights).zip(&rows) {
            let row = Rect::new(row_x0, y, row_x1, y + height);
            y += height;
            if row.y1 < list.y0 || row.y0 > list.y1 {
                continue;
            }
            let hovered = row.contains(self.cursor) && list.contains(self.cursor);
            face.clone()
                .interaction(ui::widget::Interaction {
                    hovered,
                    ..Default::default()
                })
                .paint(&mut ctx, row);
            self.step_rects.push((id.clone(), row));
        }
        ctx.scene.pop_layer();
        if overflows {
            let track = Rect::new(list.x1 - gutter, list.y0, list.x1, list.y1);
            Scrollbar::vertical().draw(
                ctx.scene,
                a,
                track,
                ScrollMetrics::new(self.stack_content, list.height(), self.stack_scroll),
                &local,
                false,
            );
        }

        // The selected step's settings.
        if self.controls_rect.area() <= 0.0 {
            return;
        }
        ui::surface::separator(
            ctx.scene,
            a,
            self.controls_rect.x0,
            self.controls_rect.x1,
            self.controls_rect.y0,
            s2 + s3,
            ground,
        );
        if let Some(band) = self.inspector_text.label {
            let label = self.selected_parameter_label();
            ctx.text_role_in_band(
                &label,
                TextRole::Label,
                local.text_primary,
                band.x0,
                band,
                None,
            );
        }
        let value = self
            .selected_slider()
            .map(|(_, value)| value)
            .or_else(|| self.selected_rotation());
        if let (Some(band), Some(value)) = (self.inspector_text.value, value) {
            let style = local.text(TextRole::Label).numeric();
            let width = f64::from(ctx.text.measure_style(&value, None, &style).0);
            let height = f64::from(ctx.text.measure_style("Ag", None, &style).1);
            let top = band.center().y - height / 2.0;
            ctx.text(
                &value,
                &style,
                local.text_dim,
                Point::new(band.x1 - width, top),
                None,
            );
        }
        if let Some(hint) = self.inspector_text.hint {
            ctx.text_role(
                &t("image-editor-crop-hint"),
                TextRole::Caption,
                local.text_dim,
                Point::new(hint.x0, hint.y0),
                Some(hint.width() as f32),
            );
        }
        if let Some((value, _)) = self.selected_slider()
            && self.slider_rect.area() > 0.0
        {
            Slider::paint(
                ctx.scene,
                a,
                &local,
                &SliderFace::new(self.slider_rect, value)
                    .hovered(self.slider_rect.contains(self.cursor))
                    .dragging(self.inspector_dragging)
                    .focused(self.inspector_focused),
            );
        }
        let (left, right) = self.rotate_rects;
        for (rect, icon, key) in [
            (left, ui::icons::Icon::RotateCcw, "image-editor-rotate-left"),
            (
                right,
                ui::icons::Icon::RotateCw,
                "image-editor-rotate-right",
            ),
        ] {
            if rect.area() > 0.0 {
                let label = t(key);
                Button::paint(
                    ctx.scene,
                    a,
                    ctx.text,
                    &local,
                    &ButtonFace::icon_only(rect, icon, &label)
                        .role(ButtonRole::Secondary)
                        .hovered(rect.contains(self.cursor)),
                );
            }
        }
        if self.remove_rect.area() > 0.0 {
            let label = t("image-editor-remove");
            Button::paint(
                ctx.scene,
                a,
                ctx.text,
                &local,
                &ButtonFace::new(self.remove_rect, &label)
                    .icon(ui::icons::Icon::Trash2)
                    .role(ButtonRole::Destructive)
                    .hovered(self.remove_rect.contains(self.cursor)),
            );
        }
    }

    /// The processing graph: every step as a card with its image, joined
    /// from each output to the inputs it feeds.
    fn draw_graph(&self, s: &mut Scene, a: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let colors = &theme.tokens.colors;
        let spacing = &theme.tokens.spacing;
        let (s2, s3, s4) = (
            f64::from(spacing.s2),
            f64::from(spacing.s3),
            f64::from(spacing.s4),
        );
        s.push_clip_layer(Fill::NonZero, a, &self.graph_rect);
        let contributing = self.project.graph.contributing_nodes("output");
        let curve = |from: Point, to: Point| {
            let reach = ((to.x - from.x).abs() / 2.0).max(s4 * 3.0);
            let mut path = BezPath::new();
            path.move_to(from);
            path.curve_to((from.x + reach, from.y), (to.x - reach, to.y), (to.x, to.y));
            path
        };
        for e in &self.project.graph.edges {
            if let (Some(from), Some(to)) = (
                self.output_port_center(&e.from_node),
                self.input_port_center(&e.to_node),
            ) {
                let active =
                    contributing.contains(&e.from_node) && contributing.contains(&e.to_node);
                s.stroke(
                    &Stroke::new(if active { 2.0 } else { 1.0 }),
                    a,
                    if active {
                        theme.text_dim
                    } else {
                        theme.text_tertiary
                    },
                    None,
                    &curve(from, to),
                );
            }
        }
        if let Some((from_node, _, pointer)) = self.editor.connection_preview()
            && let Some(from) = self.output_port_center(from_node)
        {
            let to = Point::new(
                self.graph_rect.x0 + pointer.x,
                self.graph_rect.y0 + pointer.y,
            );
            s.stroke(
                &Stroke::new(2.0),
                a,
                colors.intent.you,
                None,
                &curve(from, to),
            );
        }
        let radius = f64::from(theme.tokens.radii.tile);
        let order = self.steps();
        let mut ctx = PaintCtx::new(s, a, tr, theme);
        for (id, n) in &self.project.graph.nodes {
            let Some(r) = self.node_rect(id) else {
                continue;
            };
            let selected = self.editor.selection.contains(id);
            let active = contributing.contains(id);
            let raised = colors.surfaces.raised;
            let fill = if selected {
                colors.lift_on(raised, 1.0)
            } else {
                raised
            };
            ui::surface::card(ctx.scene, a, r, radius, fill);
            let title = match id.as_str() {
                "source" => self.project.name.clone(),
                _ => operation_label(self.project.operations.get(id))
                    .unwrap_or_else(|| n.title.clone()),
            };
            let state = match id.as_str() {
                "source" => t("image-editor-original"),
                "output" => String::new(),
                _ if !active => t("image-editor-unused"),
                _ => order
                    .iter()
                    .position(|step| step == id)
                    .map(|index| t_args("image-editor-step", &[("number", (index + 1).into())]))
                    .unwrap_or_default(),
            };
            let ink = if active {
                theme.text_primary
            } else {
                theme.text_tertiary
            };
            let width = (r.width() - 2.0 * s3) as f32;
            let title_style = theme.text(TextRole::Label).strong();
            let title = ctx.text.elide_to_width_style(&title, width, &title_style);
            let title_h = f64::from(ctx.text.measure_style("Ag", None, &title_style).1);
            ctx.text(
                &title,
                &title_style,
                ink,
                Point::new(r.x0 + s3, r.y0 + s3),
                None,
            );
            let caption_h = ctx.line_box(TextRole::Caption);
            if !state.is_empty() {
                ctx.text_role(
                    &state,
                    TextRole::Caption,
                    theme.text_dim,
                    Point::new(r.x0 + s3, r.y0 + s3 + title_h),
                    Some(width),
                );
            }
            let thumb_top = r.y0 + s3 + title_h + caption_h + s2;
            if let Some((_, preview)) = self.step_previews.iter().find(|(step, _)| step == id) {
                let thumb = Rect::new(r.x0 + s2, thumb_top, r.x1 - s2, r.y1 - s2);
                ui::image_store::paint_image_cover(
                    ctx.scene,
                    a,
                    thumb,
                    preview,
                    theme
                        .tokens
                        .radii
                        .concentric(theme.tokens.radii.tile, spacing.s2)
                        .into(),
                );
            }
            for (center, focused) in [
                (
                    self.input_port_center(id),
                    self.graph_keyboard_target
                        .as_ref()
                        .is_some_and(|target| target.node == *id),
                ),
                (self.output_port_center(id), false),
            ]
            .into_iter()
            .filter_map(|(center, focused)| center.map(|center| (center, focused)))
            {
                // A port is a dot on the card's edge, hollow on the canvas
                // ground and ringed in the ink of its connection.
                let port = s2 * 0.75;
                let dot = Rect::from_center_size(center, (2.0 * port, 2.0 * port));
                ctx.fill_round(dot, port, colors.surfaces.content);
                ctx.stroke_round(
                    dot,
                    port,
                    if focused {
                        colors.intent.you
                    } else if active {
                        theme.text_dim
                    } else {
                        theme.text_tertiary
                    },
                    2.0,
                );
            }
        }
        if !self.graph_hint_seen {
            let hint = t("image-editor-graph-hint");
            let width = (self.graph_rect.width() - 2.0 * s4).max(1.0) as f32;
            ctx.text_role(
                &hint,
                TextRole::Caption,
                theme.text_dim,
                Point::new(self.graph_rect.x0 + s4, self.graph_rect.y0 + s4),
                Some(width),
            );
        }
        ctx.scene.pop_layer();
    }
}

impl Widget for ImageEditorApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        ProjectDoc::open_shared(state);
        self.project = ProjectDoc::read(state);
        if self.indexed_project != self.project {
            self.search.refresh(&self.project);
            self.indexed_project = self.project.clone();
        }
        // A button's action arrives through the host between events.
        if let Some(command) = self.toolbar_pending.take() {
            self.run_toolbar(state, command);
        }
        if self.empty_state.take_action() {
            self.request_open();
        }
        self.poll_picker(state);
        self.rebuild_preview();
        self.sync_toolbar();
    }
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        _theme: &Theme,
        m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let n = t.leaf_node(Style {
            position: Position::Absolute,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        m.insert(n, std::ptr::from_mut::<dyn Widget>(self));
        n
    }
    fn apply_layout(&mut self, t: &TaffyTree<()>, n: NodeId, o: Point) {
        let l = t.layout(n).copied().unwrap_or_default();
        self.rect = Rect::new(
            o.x + f64::from(l.location.x),
            o.y + f64::from(l.location.y),
            o.x + f64::from(l.location.x + l.size.width),
            o.y + f64::from(l.location.y + l.size.height),
        );
    }
    fn draw(
        &mut self,
        s: &mut Scene,
        a: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.sync_toolbar();
        self.lay_out(tr, theme);
        if self.staged_overflow && self.toolbar.has_overflow() {
            self.staged_overflow = false;
            let mut scratch = AutoCommit::new();
            let _ = self.toolbar.perform_semantic_action(
                "toolbar.overflow",
                &ui::a11y::Action::Activate,
                &mut scratch,
                &automerge::ROOT,
            );
        }

        // Header.
        ui::widget::toolbar::paint_band(s, a, theme, self.header_rect);
        if !self.title.is_empty() {
            PaintCtx::new(s, a, tr, theme).text_role_in_band(
                &self.title,
                TextRole::Title,
                theme.text_primary,
                self.title_origin.x,
                self.header_rect,
                None,
            );
        }
        self.toolbar.draw(s, a, tr, theme, presence);
        for (_, button) in &mut self.header_buttons {
            button.draw(s, a, tr, theme, presence);
        }
        self.history.draw(s, a, tr, theme, presence);

        // The canvas, the graph, or the empty state.
        let main = if self.graph_shown() {
            self.graph_rect
        } else {
            self.canvas_rect
        };
        if self.graph_shown() {
            self.centre_graph(theme);
            self.draw_graph(s, a, tr, theme);
        } else if self.has_image() && self.preview.is_some() {
            self.draw_canvas(s, a, theme);
        } else {
            self.empty_state.draw(s, a, tr, theme, presence);
        }
        if self.inspector_rect.area() > 0.0 {
            self.draw_inspector(s, a, tr, theme);
        }
        self.status.paint(s, a, tr, theme, main, ui::time::now());
    }

    fn draw_overlay(
        &mut self,
        s: &mut Scene,
        a: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.toolbar.draw_overlay(s, a, tr, theme, presence);
    }

    fn needs_animation_frame(&self) -> bool {
        self.status.needs_frame(ui::time::now())
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.toolbar.modal_capture()
    }

    fn traps_focus(&self) -> bool {
        self.toolbar.traps_focus()
    }

    /// The History button is shown to the window, which then draws no
    /// History bar of its own.
    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.history);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.history]
    }

    fn handle_event(&mut self, e: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // The toolbar's More menu is modal while it is open.
        let menu_was_open = self.toolbar.is_overflow_open();
        self.toolbar
            .handle_event(e, &mut ctx.reborrow(ctx.path, ctx.node_id));
        if menu_was_open {
            if let Some(command) = self.toolbar_pending.take() {
                self.run_toolbar(ctx.state, command);
            }
            *ctx.handled = true;
            return;
        }
        for button in self.retained_buttons_mut() {
            button.handle_event(
                e,
                &mut ui::widget::EventCtx {
                    modifiers: &ui::winit::event::Modifiers::default(),
                    ..ctx.reborrow(&automerge::ROOT, ctx.node_id)
                },
            );
        }
        if !self.graph_shown() && self.preview.is_none() {
            self.empty_state
                .handle_event(e, &mut ctx.reborrow(&automerge::ROOT, ctx.node_id));
            if self.empty_state.take_action() {
                self.request_open();
                *ctx.handled = true;
            }
        }
        if let Some(command) = self.toolbar_pending.take() {
            self.run_toolbar(ctx.state, command);
            *ctx.handled = true;
            return;
        }
        if *ctx.handled {
            return;
        }
        let starts_direct_gesture = matches!(
            e,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } | WindowEvent::Touch(ui::winit::event::Touch {
                phase: ui::winit::event::TouchPhase::Started,
                ..
            })
        );
        let ends_direct_gesture = matches!(
            e,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } | WindowEvent::Touch(ui::winit::event::Touch {
                phase: ui::winit::event::TouchPhase::Ended
                    | ui::winit::event::TouchPhase::Cancelled,
                ..
            })
        );
        if starts_direct_gesture
            && !self.graph_shown()
            && !self.compare_original
            && self.crop_editor.is_some()
        {
            self.begin_gesture();
        }
        if !self.graph_shown()
            && !self.compare_original
            && let Some(editor) = self.crop_editor.as_mut()
        {
            editor.handle_event(e, &mut ctx.reborrow(&automerge::ROOT, ctx.node_id));
            if *ctx.handled {
                self.commit_crop_editor(ctx.state);
                if ends_direct_gesture {
                    self.finish_gesture();
                }
                return;
            }
        }
        match e {
            WindowEvent::DroppedFile(path) => {
                let _ = self.open_path(ctx.state, path);
                *ctx.handled = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                let next = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.graph_shown() && self.editor.is_connecting() {
                    self.editor.update_connection(ui::graph_editor::GraphPoint {
                        x: next.x - self.graph_rect.x0,
                        y: next.y - self.graph_rect.y0,
                    });
                    *ctx.handled = true;
                } else if self.graph_shown() && self.editor.is_dragging_node() {
                    let point = ui::graph_editor::GraphPoint {
                        x: next.x - self.graph_rect.x0,
                        y: next.y - self.graph_rect.y0,
                    };
                    if self.editor.update_node_drag(&mut self.project.graph, point) {
                        *ctx.handled = true;
                    }
                } else if self.graph_shown() && self.editor.is_panning() {
                    if self.editor.update_pan(ui::graph_editor::GraphPoint {
                        x: next.x - self.graph_rect.x0,
                        y: next.y - self.graph_rect.y0,
                    }) {
                        *ctx.handled = true;
                    }
                } else if self.inspector_dragging {
                    self.set_selected_slider(ctx.state, self.inspector_value_at(next.x));
                    *ctx.handled = true;
                } else if let Some(last) = self.panning_from {
                    self.pan += next - last;
                    self.panning_from = Some(next);
                }
                self.cursor = next
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.graph_shown() && self.graph_rect.contains(self.cursor) =>
            {
                let amount = match delta {
                    ui::winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    ui::winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 80.0,
                };
                self.graph_moved = true;
                self.editor.zoom_about(
                    ui::graph_editor::GraphPoint {
                        x: self.cursor.x - self.graph_rect.x0,
                        y: self.cursor.y - self.graph_rect.y0,
                    },
                    1.12_f64.powf(amount),
                );
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.list_rect.contains(self.cursor) && self.max_stack_scroll() > 0.0 =>
            {
                let amount = match delta {
                    ui::winit::event::MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * 44.0,
                    ui::winit::event::MouseScrollDelta::PixelDelta(point) => -point.y,
                };
                self.scroll_stack_by(amount);
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } if self.canvas_rect.contains(self.cursor) => {
                let amount = match delta {
                    ui::winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    ui::winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 80.0,
                };
                self.zoom = (self.zoom * 1.12_f64.powf(amount)).clamp(0.2, 8.0);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.editor.is_connecting() {
                    if let Some((node, port)) = self.graph_port_at(self.cursor, false) {
                        match self
                            .editor
                            .finish_connection(&mut self.project.graph, &node, &port)
                        {
                            Ok(_) => {
                                self.commit_connection(ctx.state, &node, &port);
                                self.say(t("image-editor-connected"));
                            }
                            Err(_) => {
                                self.say(t("image-editor-connect-refused"));
                            }
                        }
                    } else {
                        self.editor.cancel_connection();
                    }
                    *ctx.handled = true;
                } else if let Some(id) = self.editor.end_node_drag() {
                    self.commit_node_position(ctx.state, &id);
                    *ctx.handled = true;
                }
                if self.editor.end_pan() {
                    *ctx.handled = true;
                }
                if self.inspector_dragging {
                    self.inspector_dragging = false;
                    *ctx.handled = true;
                }
                self.finish_gesture();
                self.panning_from = None;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if self.header_rect.contains(self.cursor) {
                    *ctx.handled = true;
                } else if self.inspector_rect.contains(self.cursor) {
                    self.press_inspector(ctx.state, self.cursor, None);
                } else if !self.graph_shown() {
                    if self.canvas_rect.contains(self.cursor) {
                        self.inspector_focused = false;
                        self.panning_from = Some(self.cursor);
                    }
                } else if self.graph_touch.is_none()
                    && self.graph_pan_touch.is_none()
                    && self.graph_connection_touch.is_none()
                    && let Some((node, port)) = self.graph_port_at(self.cursor, true)
                {
                    self.graph_hint_seen = true;
                    self.begin_gesture();
                    self.editor.begin_connection(
                        &self.project.graph,
                        &node,
                        &port,
                        ui::graph_editor::GraphPoint {
                            x: self.cursor.x - self.graph_rect.x0,
                            y: self.cursor.y - self.graph_rect.y0,
                        },
                    );
                } else if self.graph_touch.is_none()
                    && self.graph_pan_touch.is_none()
                    && self.graph_connection_touch.is_none()
                    && let Some(id) = self
                        .project
                        .graph
                        .nodes
                        .keys()
                        .find(|id| self.node_rect(id).is_some_and(|r| r.contains(self.cursor)))
                        .cloned()
                {
                    self.graph_hint_seen = true;
                    self.graph_moved = true;
                    self.begin_gesture();
                    let point = ui::graph_editor::GraphPoint {
                        x: self.cursor.x - self.graph_rect.x0,
                        y: self.cursor.y - self.graph_rect.y0,
                    };
                    self.editor.begin_node_drag(&self.project.graph, &id, point);
                    self.rebuild_crop_editor();
                } else if self.graph_rect.contains(self.cursor) {
                    self.graph_hint_seen = true;
                    self.graph_moved = true;
                    self.editor.begin_pan(ui::graph_editor::GraphPoint {
                        x: self.cursor.x - self.graph_rect.x0,
                        y: self.cursor.y - self.graph_rect.y0,
                    });
                }
                *ctx.handled = true
            }
            WindowEvent::Touch(touch) => {
                let point = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                match touch.phase {
                    ui::winit::event::TouchPhase::Started => {
                        if self.header_rect.contains(point) {
                            *ctx.handled = true;
                            return;
                        }
                        if self.inspector_rect.contains(point) {
                            self.touches.insert(touch.id, point);
                            if self.list_rect.contains(point) {
                                self.stack_touch = Some(touch.id);
                                self.stack_touch_y = point.y;
                            }
                            self.press_inspector(ctx.state, point, Some(touch.id));
                            *ctx.handled = true;
                            return;
                        }
                        if self.graph_shown() {
                            self.graph_hint_seen = true;
                            if self.graph_touch.is_none()
                                && self.graph_pan_touch.is_none()
                                && self.graph_connection_touch.is_none()
                            {
                                let graph_point = ui::graph_editor::GraphPoint {
                                    x: point.x - self.graph_rect.x0,
                                    y: point.y - self.graph_rect.y0,
                                };
                                if let Some((node, port)) = self.graph_port_at(point, true) {
                                    self.begin_gesture();
                                    if self.editor.begin_connection(
                                        &self.project.graph,
                                        &node,
                                        &port,
                                        graph_point,
                                    ) {
                                        self.graph_connection_touch = Some(touch.id);
                                    }
                                } else {
                                    let node = self
                                        .project
                                        .graph
                                        .nodes
                                        .keys()
                                        .find(|id| {
                                            self.node_rect(id)
                                                .is_some_and(|rect| rect.contains(point))
                                        })
                                        .cloned();
                                    if let Some(id) = node {
                                        self.begin_gesture();
                                        if self.editor.begin_node_drag(
                                            &self.project.graph,
                                            &id,
                                            graph_point,
                                        ) {
                                            self.graph_touch = Some(touch.id);
                                            self.rebuild_crop_editor();
                                        }
                                    } else if self.graph_rect.contains(point)
                                        && self.editor.begin_pan(graph_point)
                                    {
                                        self.graph_moved = true;
                                        self.graph_pan_touch = Some(touch.id);
                                    }
                                }
                            }
                            *ctx.handled = true;
                            return;
                        }
                        self.touches.insert(touch.id, point);
                        if self.canvas_rect.contains(point) {
                            self.inspector_focused = false;
                            self.panning_from = Some(point);
                        }
                        if self.touches.len() == 2 {
                            let points = self.touches.values().copied().collect::<Vec<_>>();
                            if let [a, b] = points.as_slice() {
                                self.pinch_distance = Some(a.distance(*b).max(1.0));
                                self.panning_from = None;
                            }
                        }
                        *ctx.handled = true;
                    }
                    ui::winit::event::TouchPhase::Moved => {
                        if self.graph_shown() {
                            let graph_point = ui::graph_editor::GraphPoint {
                                x: point.x - self.graph_rect.x0,
                                y: point.y - self.graph_rect.y0,
                            };
                            if self.graph_connection_touch == Some(touch.id) {
                                self.editor.update_connection(graph_point);
                            } else if self.graph_touch == Some(touch.id) {
                                self.editor
                                    .update_node_drag(&mut self.project.graph, graph_point);
                            } else if self.graph_pan_touch == Some(touch.id) {
                                self.editor.update_pan(graph_point);
                            }
                            *ctx.handled = true;
                            return;
                        }
                        self.touches.insert(touch.id, point);
                        if self.stack_touch == Some(touch.id) {
                            let delta = self.stack_touch_y - point.y;
                            self.stack_touch_y = point.y;
                            self.scroll_stack_by(delta);
                        } else if self.graph_touch == Some(touch.id) {
                            let graph_point = ui::graph_editor::GraphPoint {
                                x: point.x - self.graph_rect.x0,
                                y: point.y - self.graph_rect.y0,
                            };
                            self.editor
                                .update_node_drag(&mut self.project.graph, graph_point);
                        } else if self.inspector_touch == Some(touch.id) {
                            self.set_selected_slider(ctx.state, self.inspector_value_at(point.x));
                        } else if self.touches.len() == 2 {
                            let points = self.touches.values().copied().collect::<Vec<_>>();
                            if let [a, b] = points.as_slice() {
                                let distance = a.distance(*b).max(1.0);
                                if let Some(previous) = self.pinch_distance.replace(distance) {
                                    self.zoom = (self.zoom * distance / previous).clamp(0.2, 8.0);
                                }
                            }
                        } else if let Some(previous) = self.panning_from
                            && self.canvas_rect.contains(point)
                        {
                            self.pan += point - previous;
                            self.panning_from = Some(point);
                        }
                        *ctx.handled = true;
                    }
                    ui::winit::event::TouchPhase::Ended
                    | ui::winit::event::TouchPhase::Cancelled => {
                        if self.graph_shown() {
                            if self.graph_connection_touch == Some(touch.id) {
                                self.graph_connection_touch = None;
                                if matches!(touch.phase, ui::winit::event::TouchPhase::Cancelled) {
                                    self.editor.cancel_connection();
                                } else if let Some((node, port)) = self.graph_port_at(point, false)
                                {
                                    match self.editor.finish_connection(
                                        &mut self.project.graph,
                                        &node,
                                        &port,
                                    ) {
                                        Ok(_) => {
                                            self.commit_connection(ctx.state, &node, &port);
                                            self.say(t("image-editor-connected"));
                                        }
                                        Err(_) => {
                                            self.say(t("image-editor-connect-refused"));
                                        }
                                    }
                                } else {
                                    self.editor.cancel_connection();
                                }
                            } else if self.graph_touch == Some(touch.id) {
                                self.graph_touch = None;
                                if let Some(id) = self.editor.end_node_drag() {
                                    self.commit_node_position(ctx.state, &id);
                                }
                            }
                            if self.graph_pan_touch == Some(touch.id) {
                                self.graph_pan_touch = None;
                                self.editor.end_pan();
                            }
                            self.finish_gesture();
                            *ctx.handled = true;
                            return;
                        }
                        if self.graph_touch == Some(touch.id) {
                            self.graph_touch = None;
                            if let Some(id) = self.editor.end_node_drag() {
                                self.commit_node_position(ctx.state, &id);
                            }
                        }
                        if self.stack_touch == Some(touch.id) {
                            self.stack_touch = None;
                        }
                        if self.inspector_touch == Some(touch.id) {
                            self.inspector_touch = None;
                            self.finish_gesture();
                        }
                        self.touches.remove(&touch.id);
                        self.pinch_distance = None;
                        self.panning_from = self.touches.values().next().copied();
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if ctx.modifiers.state().control_key()
                    && matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("z"))
                {
                    if ctx.modifiers.state().shift_key() {
                        self.redo(ctx.state);
                    } else {
                        self.undo(ctx.state);
                    }
                    *ctx.handled = true;
                } else if ctx.modifiers.state().control_key()
                    && matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("y"))
                {
                    self.redo(ctx.state);
                    *ctx.handled = true;
                } else if self.graph_shown()
                    && self.editor.is_connecting()
                    && self.graph_keyboard_target.is_some()
                    && matches!(event.logical_key, Key::Named(NamedKey::Tab))
                {
                    self.cycle_keyboard_connection(ctx.modifiers.state().shift_key());
                    *ctx.handled = true;
                } else if self.graph_shown()
                    && self.editor.is_connecting()
                    && self.graph_keyboard_target.is_some()
                    && matches!(event.logical_key, Key::Named(NamedKey::Enter))
                {
                    self.finish_keyboard_connection(ctx.state);
                    *ctx.handled = true;
                } else if self.graph_shown()
                    && self.editor.is_connecting()
                    && matches!(event.logical_key, Key::Named(NamedKey::Escape))
                {
                    self.editor.cancel_connection();
                    self.graph_keyboard_target = None;
                    self.say(t("image-editor-connect-cancelled"));
                    *ctx.handled = true;
                } else if self.graph_shown()
                    && ctx.modifiers.state().control_key()
                    && matches!(event.logical_key, Key::Named(NamedKey::Enter))
                {
                    self.begin_keyboard_connection();
                    *ctx.handled = true;
                } else if self.graph_shown()
                    && ctx.modifiers.state().alt_key()
                    && matches!(
                        event.logical_key,
                        Key::Named(
                            NamedKey::ArrowLeft
                                | NamedKey::ArrowRight
                                | NamedKey::ArrowUp
                                | NamedKey::ArrowDown
                        )
                    )
                {
                    let step = if ctx.modifiers.state().shift_key() {
                        32.0
                    } else {
                        8.0
                    };
                    let (dx, dy) = match event.logical_key {
                        Key::Named(NamedKey::ArrowLeft) => (-step, 0.0),
                        Key::Named(NamedKey::ArrowRight) => (step, 0.0),
                        Key::Named(NamedKey::ArrowUp) => (0.0, -step),
                        Key::Named(NamedKey::ArrowDown) => (0.0, step),
                        _ => (0.0, 0.0),
                    };
                    let movable = self
                        .editor
                        .selection
                        .iter()
                        .filter(|id| self.project.graph.nodes.contains_key(*id))
                        .cloned()
                        .collect::<Vec<_>>();
                    if !movable.is_empty() {
                        self.checkpoint();
                    }
                    for id in movable {
                        let Some(node) = self.project.graph.nodes.get(&id) else {
                            continue;
                        };
                        let (x, y) = (node.position.x + dx, node.position.y + dy);
                        self.dispatch_graph_action(
                            ctx.state,
                            UPDATE_ADJUSTMENT_TYPE,
                            &UpdateAdjustment {
                                id,
                                update: AdjustmentUpdate::Position { x, y },
                            },
                        );
                    }
                    *ctx.handled = true;
                } else if matches!(&event.logical_key,Key::Character(s) if s.eq_ignore_ascii_case("g"))
                    && self.has_image()
                {
                    self.advanced_graph = !self.advanced_graph;
                    *ctx.handled = true;
                } else if matches!(&event.logical_key,Key::Character(s) if s=="\\") {
                    self.compare_original = true;
                    *ctx.handled = true;
                } else if matches!(&event.logical_key,Key::Character(s) if s.eq_ignore_ascii_case("f"))
                {
                    self.zoom = 1.0;
                    self.pan = ui::vello::kurbo::Vec2::ZERO;
                    *ctx.handled = true;
                } else if matches!(event.logical_key, Key::Named(NamedKey::ArrowUp)) {
                    self.select_relative_step(-1);
                    *ctx.handled = true;
                } else if matches!(event.logical_key, Key::Named(NamedKey::ArrowDown)) {
                    self.select_relative_step(1);
                    *ctx.handled = true;
                } else if matches!(event.logical_key, Key::Named(NamedKey::ArrowLeft)) {
                    if let Some((value, _)) = self.selected_slider() {
                        let step = if ctx.modifiers.state().shift_key() {
                            0.05
                        } else {
                            0.01
                        };
                        self.set_selected_slider(ctx.state, value - step);
                    } else {
                        self.adjust_selected(ctx.state, -1.0);
                    }
                    *ctx.handled = true;
                } else if matches!(event.logical_key, Key::Named(NamedKey::ArrowRight)) {
                    if let Some((value, _)) = self.selected_slider() {
                        let step = if ctx.modifiers.state().shift_key() {
                            0.05
                        } else {
                            0.01
                        };
                        self.set_selected_slider(ctx.state, value + step);
                    } else {
                        self.adjust_selected(ctx.state, 1.0);
                    }
                    *ctx.handled = true;
                } else if matches!(event.logical_key, Key::Named(NamedKey::Delete)) {
                    let deletable = self
                        .editor
                        .selection
                        .iter()
                        .filter(|id| id.as_str() != "source" && id.as_str() != "output")
                        .cloned()
                        .collect::<Vec<_>>();
                    if !deletable.is_empty() {
                        self.checkpoint();
                    }
                    let mut removed = 0usize;
                    for id in deletable {
                        self.dispatch_graph_action(
                            ctx.state,
                            REMOVE_ADJUSTMENT_TYPE,
                            &RemoveAdjustment { id: id.clone() },
                        );
                        if !self.project.graph.nodes.contains_key(&id) {
                            removed += 1;
                        }
                    }
                    if removed > 0 {
                        self.editor.select_only(self.project.output.clone());
                        self.rendered_project = None;
                        self.rebuild_crop_editor();
                        self.say(t_args("image-editor-removed", &[("count", removed.into())]));
                    }
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Released => {
                if matches!(&event.logical_key,Key::Character(s) if s=="\\") {
                    self.compare_original = false;
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }
    fn name(&self) -> &str {
        "Image Editor"
    }
    fn test_id(&self) -> Option<&str> {
        Some("image-editor.graph")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn cursor_shape(&self, point: Point) -> Option<ui::winit::window::CursorIcon> {
        if self.graph_shown() && self.graph_rect.contains(point) {
            Some(
                if self.editor.is_connecting() || self.graph_port_at(point, true).is_some() {
                    ui::winit::window::CursorIcon::Crosshair
                } else if self.editor.is_dragging_node() || self.editor.is_panning() {
                    ui::winit::window::CursorIcon::Grabbing
                } else {
                    ui::winit::window::CursorIcon::Grab
                },
            )
        } else {
            None
        }
    }
    fn wants_pointer_capture(&self) -> bool {
        self.editor.is_dragging_node()
            || self.editor.is_connecting()
            || self.inspector_dragging
            || self.inspector_touch.is_some()
            || self.graph_touch.is_some()
            || self.graph_pan_touch.is_some()
            || self.graph_connection_touch.is_some()
            || self.editor.is_panning()
            || self.panning_from.is_some()
            || self.toolbar.wants_pointer_capture()
    }
    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut out = vec![
            ("canvas".into(), self.canvas_rect),
            ("adjustments".into(), self.list_rect),
            ("inspector".into(), self.inspector_rect),
        ];
        out.extend(
            self.step_rects
                .iter()
                .map(|(id, r)| (format!("step.{id}"), *r)),
        );
        out
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::OPERATION_TYPE, CONTROL_OPERATION_TYPE]
    }
    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if request.type_id == CONTROL_OPERATION_TYPE {
            let started = {
                let mut found = None;
                for button in self.retained_buttons_mut() {
                    if let Some(started) = button.start_operation(op, doc, path)? {
                        found = Some(started);
                        break;
                    }
                }
                found
            };
            if let Some(started) = started {
                // View/effect handlers queue into `toolbar_pending`; run them
                // before returning so a host click opens the picker in the
                // same tick (sync may not run until the next relayout).
                if let Some(command) = self.toolbar_pending.take() {
                    self.run_toolbar(doc, command);
                }
                return Ok(Some(started));
            }
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _: bool,
        _: &AutoCommit,
        _: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self
            .resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let mut children = self
            .header_buttons
            .iter()
            .map(|(_, button)| button)
            .chain(std::iter::once(&self.history))
            .filter_map(Widget::accessibility)
            .collect::<Vec<_>>();
        if let Some(toolbar) = self.toolbar.accessibility()
            && !toolbar.children.is_empty()
        {
            children.push(toolbar);
        }
        let graph = self.graph_shown();
        if !graph && self.preview.is_none() {
            if let Some(mut empty) = self.empty_state.accessibility() {
                let action = self.empty_state.action_rect();
                for child in &mut empty.children {
                    child.bounds = Some(action.into());
                }
                children.push(empty.with_bounds(self.canvas_rect));
            }
        } else if !graph {
            children.push(
                AccessNode::new(Role::Image)
                    .with_name(t(if self.compare_original {
                        "image-editor-original-preview"
                    } else {
                        "image-editor-edited-preview"
                    }))
                    .with_bounds(self.canvas_rect),
            );
        }
        if !graph
            && let Some(crop) = self.crop_editor.as_ref()
            && let Some(access) = crop.accessibility()
        {
            children.push(access);
        }
        let contributing = self.project.graph.contributing_nodes("output");
        let name_of = |id: &str| -> String {
            match id {
                "source" => self.project.name.clone(),
                _ => operation_label(self.project.operations.get(id))
                    .or_else(|| self.project.graph.nodes.get(id).map(|n| n.title.clone()))
                    .unwrap_or_default(),
            }
        };
        children.extend(
            self.project
                .graph
                .nodes
                .iter()
                .filter(|(id, _)| graph || (id.as_str() != "source" && id.as_str() != "output"))
                .map(|(id, node)| {
                    let bounds = if graph {
                        self.node_rect(id)
                    } else {
                        self.step_rects
                            .iter()
                            .find_map(|(candidate, rect)| (candidate == id).then_some(*rect))
                    };
                    let mut access = AccessNode::new(Role::Button)
                        .with_name(name_of(id))
                        .with_value(t(if !contributing.contains(id) {
                            "image-editor-unused"
                        } else if node.enabled {
                            "image-editor-enabled"
                        } else {
                            "image-editor-bypassed"
                        }))
                        .with_action_target(format!("image-editor.step.{id}"))
                        .with_state(AccessState {
                            focusable: true,
                            selected: self.editor.selection.contains(id),
                            ..Default::default()
                        });
                    if let Some(bounds) = bounds {
                        access = access.with_bounds(bounds);
                    }
                    access
                })
                .collect::<Vec<_>>(),
        );
        if graph {
            for (id, node) in &self.project.graph.nodes {
                let step = name_of(id);
                if let (Some(_), Some(center)) = (node.outputs.first(), self.output_port_center(id))
                {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(t_args(
                                "image-editor-connect-from",
                                &[("step", step.as_str().into())],
                            ))
                            .with_action_target(format!("image-editor.output.{id}"))
                            .with_bounds(Self::port_target(center))
                            .with_state(AccessState::new().focusable(true)),
                    );
                }
                if let (Some(_), Some(center)) = (node.inputs.first(), self.input_port_center(id)) {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(t_args(
                                "image-editor-connect-to",
                                &[("step", step.as_str().into())],
                            ))
                            .with_action_target(format!("image-editor.input.{id}"))
                            .with_bounds(Self::port_target(center))
                            .with_state(AccessState::new().focusable(true)),
                    );
                }
            }
        }
        if let Some((_, value)) = self.selected_slider()
            && self.slider_rect.area() > 0.0
        {
            children.push(
                AccessNode::new(Role::Slider)
                    .with_name(self.selected_parameter_label())
                    .with_value(value)
                    .with_action_target("image-editor.adjust.slider")
                    .with_bounds(self.slider_rect)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.inspector_focused),
                    ),
            );
        }
        let (left, right) = self.rotate_rects;
        if left.area() > 0.0 {
            children.extend([
                AccessNode::new(Role::Button)
                    .with_name(t("image-editor-rotate-left"))
                    .with_action_target("image-editor.adjust.decrease")
                    .with_bounds(left)
                    .with_state(AccessState::new().focusable(true)),
                AccessNode::new(Role::Button)
                    .with_name(t("image-editor-rotate-right"))
                    .with_action_target("image-editor.adjust.increase")
                    .with_bounds(right)
                    .with_state(AccessState::new().focusable(true)),
            ]);
        }
        if self.selected_is_removable() && self.remove_rect.area() > 0.0 {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(t("image-editor-remove"))
                    .with_action_target("image-editor.adjust.remove")
                    .with_bounds(self.remove_rect)
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        // Action targets already have stable keys. Key passive/container
        // siblings too so this custom canvas never mixes identity- and
        // position-addressed children.
        for (index, child) in children.iter_mut().enumerate() {
            if child.key.is_none() {
                child.key = Some(ui::a11y::hash_key(&(
                    "image-editor.root-child",
                    child.role,
                    child.name.as_deref(),
                    index,
                )));
            }
        }
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(t(if graph {
                    "image-editor-graph-canvas"
                } else {
                    "image-editor-canvas"
                }))
                .with_value(self.notice.clone())
                .with_state(AccessState {
                    focusable: true,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        if target.starts_with("toolbar.") {
            let handled = self
                .toolbar
                .perform_semantic_action(target, action, state, path);
            if let Some(command) = self.toolbar_pending.take() {
                let (state, _) = ui::doc::document(state, path);
                self.run_toolbar(state, command);
            }
            return handled;
        }
        let (state, path) = ui::doc::document(state, path);
        if target.starts_with("crop.")
            && let Some(editor) = self.crop_editor.as_mut()
            && editor.perform_semantic_action(target, action, state, path)
        {
            self.commit_crop_editor(state);
            return true;
        }
        if target == "image-editor.adjust.slider" {
            let Some((current, _)) = self.selected_slider() else {
                return false;
            };
            let next = match action {
                ui::a11y::Action::Increment => current + 0.01,
                ui::a11y::Action::Decrement => current - 0.01,
                ui::a11y::Action::SetValue(value) => {
                    let Ok(mut parsed) = value.trim().trim_end_matches('%').parse::<f64>() else {
                        return false;
                    };
                    if !parsed.is_finite() {
                        return false;
                    }
                    if parsed > 1.0 {
                        parsed /= 100.0;
                    }
                    parsed
                }
                ui::a11y::Action::Focus => {
                    self.inspector_focused = true;
                    return true;
                }
                _ => return false,
            };
            return self.set_selected_slider(state, next.clamp(0.0, 1.0));
        }
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let toolbar_command = ToolbarCommand::ALL
            .into_iter()
            .find(|command| command.target() == target);
        if let Some(command) = toolbar_command {
            self.run_toolbar(state, command);
            return true;
        }
        if let Some(id) = target.strip_prefix("image-editor.output.") {
            if !self.project.graph.nodes.contains_key(id) {
                return false;
            }
            self.editor.select_only(id.to_owned());
            return self.begin_keyboard_connection();
        }
        if let Some(id) = target.strip_prefix("image-editor.input.") {
            if !self.editor.is_connecting() || !self.project.graph.nodes.contains_key(id) {
                return false;
            }
            let Some(port) = self.project.graph.nodes[id].inputs.first() else {
                return false;
            };
            self.graph_keyboard_target = Some(ConnectionTarget {
                node: id.to_owned(),
                port: port.id.clone(),
            });
            return self.finish_keyboard_connection(state);
        }
        match target {
            "image-editor.empty.open" => self.request_open(),
            "image-editor.adjust.decrease" => self.adjust_selected(state, -1.0),
            "image-editor.adjust.increase" => self.adjust_selected(state, 1.0),
            "image-editor.adjust.remove" => return self.remove_selected(state),
            _ => {
                let Some(id) = target.strip_prefix("image-editor.step.") else {
                    return false;
                };
                if !self.project.graph.nodes.contains_key(id) {
                    return false;
                }
                self.editor.select_only(id.to_string());
                self.rebuild_crop_editor();
            }
        }
        true
    }
}

impl ui::search::capability::SearchSource for ImageEditorApp {
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search.clone())]
    }
}

impl ui::resources::PublishesResources for ImageEditorApp {
    /// Standalone / `--shots` / unit tests publish through
    /// [`tilo_resource::publish`]. When the framework hosts the surface, that
    /// door already owns the project. A project that matches the document is
    /// read from the document; an edit that has not been written yet stays on
    /// the widget so the palette still shows it.
    ///
    /// Retained toolbar / empty-state buttons are not layout children, so their
    /// `tilo.control:*` resources must be published here or host clicks never
    /// resolve after `ActionEmitter` emit.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut out = if self.actions.is_hosted() {
            Vec::new()
        } else {
            vec![self.project_resource(doc)]
        };
        out.extend(self.control_resources(doc));
        out
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.actions.is_hosted() && id == crate::actions::PROJECT_RESOURCE_ID {
            return None;
        }
        self.control_resources(doc)
            .into_iter()
            .find(|resource| resource.id.as_str() == id)
            .or_else(|| find_resource(&self.project_resource(doc), id))
    }
}

impl ImageEditorApp {
    fn control_resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let mut out = Vec::new();
        for (_, button) in &self.header_buttons {
            out.extend(ui::resources::resources(button, doc));
        }
        out.extend(ui::resources::resources(&self.history, doc));
        out
    }

    fn project_resource(&self, doc: &AutoCommit) -> tilo_actions::Resource {
        let stored = <crate::resource::ImageEditor as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_else(|_| self.project.clone());
        let project = if stored == self.project {
            stored
        } else {
            self.project.clone()
        };
        let label = if project.name.is_empty() {
            "Image Editor".to_string()
        } else {
            project.name.clone()
        };
        ui::tilo_resource::publish::<crate::resource::ImageEditor>(
            crate::actions::PROJECT_RESOURCE_ID,
            &label,
            &project,
        )
    }
}

impl ui::widget::controls::ActionControls for ImageEditorApp {
    /// The toolbar's retained buttons are not all laid-out children.
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        for button in self.retained_buttons_mut() {
            ui::widget::controls::install_control_custody(button, domain, attachment);
        }
        for child in self.children_mut() {
            ui::widget::controls::install_control_custody(child, domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self
            .retained_buttons_mut()
            .into_iter()
            .find_map(|button| ui::widget::controls::take_control_invocation(button))
        {
            return Some(invocation);
        }
        self.children_mut()
            .into_iter()
            .find_map(ui::widget::controls::take_control_invocation)
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for button in self.retained_buttons_mut() {
            ui::widget::controls::refresh_control_action_availability(button, allowed);
        }
        for child in self.children_mut() {
            ui::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut bindings = ui::widget::semantic_action_bindings(self.accessibility());
        for (_, button) in &self.header_buttons {
            if button.get_rect().area() > 0.0
                && let Some(id) = button.test_id()
            {
                bindings.push(ui::widget::ControlBindingMetadata::semantic(id));
            }
        }
        bindings
    }
}

impl ui::undo::CommittedActions for ImageEditorApp {
    /// Expose the shared-action boundary to the shell so these edits appear
    /// in the same undo/history and collaboration stream as palette and agent
    /// operations.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ImageEditorApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _: &AutoCommit,
        _: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        if self.picker.is_some() {
            return Err("an image picker is already open".into());
        }
        match operation {
            crate::actions::EditorOperation::Open => self.request_open(),
            crate::actions::EditorOperation::Export => {
                return Err(
                    "export operations must be started through the invocation broker".into(),
                );
            }
            crate::actions::EditorOperation::Select { id } => {
                if !self.project.graph.nodes.contains_key(&id) {
                    return Err("that image-editor resource no longer exists".into());
                }
                self.editor.select_only(id);
                self.rebuild_crop_editor();
                self.notice.clear();
            }
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Document
            && let Some(crate::actions::EditorOperation::Export) =
                crate::actions::decode_operation(type_id, payload)?
        {
            self.resource_result = Some(match self.export_beside_source() {
                Ok(notice) => {
                    self.say(notice);
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    }
                }
                Err(message) => {
                    self.say(message.clone());
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "image-editor.export-failed".into(),
                            message,
                            details: None,
                            retryable: true,
                        },
                    }
                }
            });
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        self.resource_invocation = Some(invocation);
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

fn find_resource(resource: &tilo_actions::Resource, id: &str) -> Option<tilo_actions::Resource> {
    if resource.id.as_str() == id {
        return Some(resource.clone());
    }
    for child in &resource.children {
        if let Some(found) = find_resource(child, id) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod portal_tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use ui::headless::Framed;

    /// Responsive layout: the header never overlaps and sends what does not fit
    /// to the toolbar's More menu, and every published target (empty-state
    /// open, header, inspector, graph ports) is placed, disjoint and
    /// semantically reachable.
    #[test]
    fn responsive_layout_publishes_touch_sized_disjoint_targets() {
        empty_state_open_is_a_real_touch_sized_semantic_button();
        phone_toolbar_targets_are_touch_sized_and_never_overlap();
        phone_more_exposes_every_tool_as_disjoint_touch_targets();
        compact_inspector_controls_remain_touch_sized_and_disjoint();
        advanced_graph_exposes_touch_sized_accessible_ports();
    }

    fn empty_state_open_is_a_real_touch_sized_semantic_button() {
        let mut app = framed(ImageEditorApp::new(), 390.0, 760.0);
        let rect = app.empty_state.action_rect();
        let target = f64::from(ui::Theme::dark().tokens.metrics.min_pointer_target);
        assert!(rect.width() >= target && rect.height() >= target);
        assert!(app.canvas_rect.contains(rect.center()));

        let access = app.accessibility().unwrap();
        assert!(access.children.iter().any(|child| {
            child.children.iter().any(|action| {
                action.action_target.as_deref() == Some("image-editor.empty.open")
                    && action.role == Role::Button
                    && action.bounds.is_some()
            })
        }));
        let mut state = AutoCommit::new();
        assert!(app.perform_semantic_action(
            "image-editor.empty.open",
            &ui::a11y::Action::Activate,
            &mut state,
            &automerge::ROOT,
        ));
        assert!(app.picker.is_some());
    }

    #[test]
    fn clicking_open_through_the_host_asks_for_an_image() {
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut harness = ui::headless::FrameHarness::new(
            ui::State::new(),
            Box::new(ImageEditorApp::new()),
            (1280, 800),
        );
        harness.relayout();
        let centre = harness
            .controls()
            .into_iter()
            .find(|control| control.name.as_deref() == Some("Open image"))
            .and_then(|control| control.bounds)
            .expect("the empty state's Open image button is placed")
            .center();
        harness.click_at(centre.x, centre.y);
        harness.relayout();
        assert_eq!(recorder.pick_requests().len(), 1);
    }

    /// Mount `app` in the shared frame harness and run its first frame at
    /// `width`x`height`.
    fn framed(app: ImageEditorApp, width: f32, height: f32) -> Framed<ImageEditorApp> {
        Framed::new(
            ui::State::new(),
            app,
            ui::headless::viewport((width, height)),
            ui::Theme::dark(),
        )
    }

    /// The bundled sample with `steps` added in order.
    fn sample_project(steps: &[(&str, Operation)]) -> Project {
        let mut project = Project::blank();
        if let Some(Operation::Source { path }) = project.operations.get_mut("source") {
            *path = crate::engine::SAMPLE_SOURCE.into();
        }
        project.name = "Harbour light".into();
        for (id, operation) in steps {
            let title = operation_label(Some(operation)).unwrap_or_default();
            assert!(project.insert_before_output(id, &title, operation.clone()));
        }
        project
    }

    /// `project` open in the editor, its first frame run at `width`x`height`.
    fn framed_project(project: &Project, width: f32, height: f32) -> Framed<ImageEditorApp> {
        let mut state = ui::State::new();
        ProjectDoc::write(&mut state.doc, project);
        framed_state(state, width, height)
    }

    fn framed_state(state: ui::State, width: f32, height: f32) -> Framed<ImageEditorApp> {
        Framed::new(
            state,
            ImageEditorApp::new(),
            ui::headless::viewport((width, height)),
            ui::Theme::dark(),
        )
    }

    /// Select `id` and run a frame so the inspector lays its settings out.
    fn select(app: &mut Framed<ImageEditorApp>, id: &str) {
        app.editor.select_only(id);
        app.rebuild_crop_editor();
        Framed::harness_mut(app).relayout();
    }

    fn phone_toolbar_targets_are_touch_sized_and_never_overlap() {
        let project = sample_project(&[("exposure", Operation::Exposure { stops: 0.2 })]);
        for width in [360.0, 390.0, 800.0, 1_280.0] {
            let app = framed_project(&project, width, 640.0);
            let control = ButtonFace::height(ControlSize::Regular, &ui::Theme::dark());
            let mut placed = app
                .header_buttons
                .iter()
                .map(|(_, button)| button.get_rect())
                .collect::<Vec<_>>();
            assert_eq!(placed.len(), 4);
            assert!(placed.iter().all(|rect| rect.height() >= control));
            assert!(placed.windows(2).all(|pair| pair[0].x1 < pair[1].x0));
            placed.push(app.toolbar.get_rect());
            placed.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            assert!(
                placed.windows(2).all(|pair| pair[0].x1 <= pair[1].x0),
                "header controls overlap at {width}: {placed:?}"
            );
            assert!(
                placed
                    .iter()
                    .all(|rect| app.header_rect.contains(rect.center()))
            );
        }
    }

    /// Direct edits (toolbar, slider, remove, keyboard connection, crop) dispatch the
    /// canonical document actions and undo/redo as whole gestures.
    #[test]
    fn direct_edits_dispatch_canonical_actions_and_undo_as_gestures() {
        undo_and_redo_restore_the_complete_non_destructive_project();
        direct_editor_affordances_dispatch_canonical_document_actions();
        continuous_adjustment_is_one_undoable_gesture();
        keyboard_connection_flow_selects_a_real_port_and_commits();
        direct_crop_edits_the_graph_node_and_recomputes_later_steps();
    }

    fn keyboard_connection_flow_selects_a_real_port_and_commits() {
        let mut app = framed(ImageEditorApp::new(), 1_280.0, 800.0);
        app.advanced_graph = true;
        app.editor.select_only("source");
        let mut state = AutoCommit::new();

        assert!(app.begin_keyboard_connection());
        assert!(app.editor.is_connecting());
        assert_eq!(
            app.graph_keyboard_target
                .as_ref()
                .map(|target| target.node.as_str()),
            Some("output")
        );
        assert!(app.finish_keyboard_connection(&mut state));
        assert!(!app.editor.is_connecting());
        assert_eq!(
            app.actions.last_dispatched().unwrap().type_id,
            RECONNECT_INPUT_TYPE
        );
        assert!(ui::undo::take_committed_action(&mut *app).is_some());
        assert!(
            app.project
                .graph
                .edges
                .iter()
                .any(|edge| edge.from_node == "source" && edge.to_node == "output")
        );
    }

    fn undo_and_redo_restore_the_complete_non_destructive_project() {
        let mut app = ImageEditorApp::new();
        let mut state = AutoCommit::new();
        let original = app.project.clone();

        app.add(&mut state, Operation::Exposure { stops: 0.5 });
        let edited = app.project.clone();
        assert_ne!(edited, original);
        assert_eq!(app.undo_stack.len(), 1);

        app.undo(&mut state);
        assert_eq!(app.project, original);
        assert_eq!(ProjectDoc::read(&state), original);
        assert_eq!(app.redo_stack.len(), 1);

        app.redo(&mut state);
        assert_eq!(app.project, edited);
        assert_eq!(ProjectDoc::read(&state), edited);
    }

    fn direct_editor_affordances_dispatch_canonical_document_actions() {
        let mut app = ImageEditorApp::new();
        let mut document = AutoCommit::new();
        ProjectDoc::seed(&mut document);

        app.add(&mut document, Operation::Exposure { stops: 0.5 });
        assert_eq!(
            app.actions.last_dispatched().unwrap().type_id,
            ADD_ADJUSTMENT_TYPE
        );
        assert!(ui::undo::take_committed_action(&mut app).is_some());

        assert!(app.set_selected_slider(&mut document, 0.8));
        assert_eq!(
            app.actions.last_dispatched().unwrap().type_id,
            UPDATE_ADJUSTMENT_TYPE
        );
        assert!(ui::undo::take_committed_action(&mut app).is_some());

        assert!(app.remove_selected(&mut document));
        assert_eq!(
            app.actions.last_dispatched().unwrap().type_id,
            REMOVE_ADJUSTMENT_TYPE
        );
        assert!(ui::undo::take_committed_action(&mut app).is_some());
    }

    fn continuous_adjustment_is_one_undoable_gesture() {
        let mut app = ImageEditorApp::new();
        let mut state = AutoCommit::new();
        app.add(&mut state, Operation::Exposure { stops: 0.5 });
        app.undo_stack.clear();
        let before = app.project.clone();

        app.begin_gesture();
        assert!(app.set_selected_slider(&mut state, 0.2));
        assert!(app.set_selected_slider(&mut state, 0.4));
        assert!(app.set_selected_slider(&mut state, 0.8));
        app.finish_gesture();
        assert_eq!(app.undo_stack.len(), 1);

        app.undo(&mut state);
        assert_eq!(app.project, before);
    }

    fn advanced_graph_exposes_touch_sized_accessible_ports() {
        let project = sample_project(&[("exposure", Operation::Exposure { stops: 0.2 })]);
        let mut app = framed_project(&project, 1_280.0, 800.0);
        app.advanced_graph = true;
        Framed::harness_mut(&mut app).relayout();
        let access = app.accessibility().unwrap();
        let ports = access
            .children
            .iter()
            .filter(|child| {
                child.action_target.as_deref().is_some_and(|target| {
                    target.starts_with("image-editor.output.")
                        || target.starts_with("image-editor.input.")
                })
            })
            .collect::<Vec<_>>();
        assert!(ports.len() >= 2);
        for port in ports {
            let bounds = port
                .bounds
                .expect("graph ports publish their hit target")
                .to_rect();
            assert!(bounds.width() >= 44.0);
            assert!(bounds.height() >= 44.0);
            assert!(app.graph_rect.contains(bounds.center()));
        }
    }

    fn phone_more_exposes_every_tool_as_disjoint_touch_targets() {
        let project = sample_project(&[("exposure", Operation::Exposure { stops: 0.2 })]);
        let wide = framed_project(&project, 1_280.0, 800.0);
        assert!(
            !wide.toolbar.has_overflow(),
            "every command fits a wide window"
        );
        for width in [360.0, 390.0] {
            let app = framed_project(&project, width, 640.0);
            assert!(
                app.toolbar.has_overflow(),
                "a phone keeps the commands that do not fit in More"
            );
            let access = app.toolbar.accessibility().unwrap();
            assert!(
                access
                    .children
                    .iter()
                    .any(|child| { child.action_target.as_deref() == Some("toolbar.overflow") })
            );
        }
    }

    #[test]
    fn host_picker_opens_validated_images_and_cancels_quietly() {
        portal_selection_opens_through_the_same_validated_path_as_drop();
        cancelling_the_host_picker_is_quiet_and_retryable();
    }

    fn portal_selection_opens_through_the_same_validated_path_as_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chosen.png");
        RgbaImage::from_pixel(2, 2, Rgba([20, 40, 60, 255]))
            .save(&path)
            .unwrap();
        let mut app = ImageEditorApp::new();
        let mut doc = AutoCommit::new();
        ProjectDoc::seed(&mut doc);
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path.clone()));
        app.request_open();
        assert_eq!(recorder.pick_requests()[0].title(), "Open an image");
        app.poll_picker(&mut doc);
        let project = ProjectDoc::read(&doc);
        assert!(
            matches!(project.operations.get("source"), Some(Operation::Source { path: opened }) if opened == &path.to_string_lossy())
        );
        assert!(app.picker.is_none());
    }

    fn cancelling_the_host_picker_is_quiet_and_retryable() {
        let mut app = ImageEditorApp::new();
        let mut doc = AutoCommit::new();
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        recorder.respond_to_pick(ui::portal::PortalOutcome::Cancelled);
        app.request_open();
        app.poll_picker(&mut doc);
        assert!(app.notice.is_empty(), "a cancelled pick says nothing");
        assert!(app.picker.is_none());
    }

    fn compact_inspector_controls_remain_touch_sized_and_disjoint() {
        let project = sample_project(&[("rotate", Operation::Rotate { quarter_turns: 1 })]);
        let mut app = framed_project(&project, 390.0, 760.0);
        select(&mut app, "rotate");
        let (minus, plus) = app.rotate_rects;
        let remove = app.remove_rect;
        let target = f64::from(ui::Theme::dark().tokens.metrics.min_pointer_target);
        for rect in [minus, plus] {
            assert!(rect.width() >= target && rect.height() >= target);
        }
        assert!(remove.height() >= target);
        assert!(minus.x1 < plus.x0);
        assert!(plus.y1 < remove.y0 || remove.y1 < minus.y0);
        for rect in [minus, plus, remove] {
            assert!(app.inspector_rect.contains(rect.center()));
        }
    }

    /// The inspector slider and its assistive counterparts do real graph work.
    #[test]
    fn inspector_and_assistive_controls_edit_the_graph() {
        shared_inspector_slider_maps_every_adjustment_across_its_full_range();
        inspector_semantics_offer_only_controls_that_do_real_work();
        assistive_remove_preserves_later_steps_and_is_undoable();
        assistive_slider_changes_the_graph_operation();
    }

    fn shared_inspector_slider_maps_every_adjustment_across_its_full_range() {
        let cases = [
            ("exposure", Operation::Exposure { stops: 0.0 }),
            ("contrast", Operation::Contrast { amount: 0.0 }),
            ("saturation", Operation::Saturation { amount: 1.0 }),
        ];
        for (id, operation) in cases {
            let mut app = ImageEditorApp::new();
            let mut document = AutoCommit::new();
            ProjectDoc::seed(&mut document);
            assert!(app.project.insert_before_output(id, id, operation));
            ProjectDoc::write(&mut document, &app.project);
            app.editor.select_only(id);
            assert!(app.set_selected_slider(&mut document, 0.0));
            let low = app.selected_slider().unwrap().0;
            assert!(low.abs() < 0.001, "{id} low endpoint was {low}");
            assert!(app.set_selected_slider(&mut document, 1.0));
            let high = app.selected_slider().unwrap().0;
            assert!((high - 1.0).abs() < 0.001, "{id} high endpoint was {high}");
        }
    }

    fn inspector_semantics_offer_only_controls_that_do_real_work() {
        let project = sample_project(&[
            ("exposure", Operation::Exposure { stops: 0.0 }),
            ("mono", Operation::Grayscale),
        ]);
        let mut app = framed_project(&project, 1_280.0, 800.0);
        select(&mut app, "exposure");
        let access = app.accessibility().unwrap();
        let slider = access
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("image-editor.adjust.slider"))
            .expect("numeric adjustment has one real slider");
        assert_eq!(slider.role, Role::Slider);
        let target = f64::from(ui::Theme::dark().tokens.metrics.min_pointer_target);
        assert!(slider.bounds.unwrap().to_rect().height() >= target);
        assert!(
            slider.bounds.unwrap().to_rect().width() >= 120.0,
            "the track keeps its minimum travel"
        );

        select(&mut app, "mono");
        let access = app.accessibility().unwrap();
        assert!(access.children.iter().all(|node| {
            !node.action_target.as_deref().is_some_and(|target| {
                matches!(
                    target,
                    "image-editor.adjust.slider"
                        | "image-editor.adjust.decrease"
                        | "image-editor.adjust.increase"
                )
            })
        }));
        assert!(access.children.iter().any(|node| {
            node.action_target.as_deref() == Some("image-editor.adjust.remove")
                && node.bounds.unwrap().to_rect().height() >= target
        }));
    }

    fn assistive_remove_preserves_later_steps_and_is_undoable() {
        let mut app = ImageEditorApp::new();
        let mut document = AutoCommit::new();
        ProjectDoc::seed(&mut document);
        assert!(app.project.insert_before_output(
            "exposure",
            "Exposure",
            Operation::Exposure { stops: 1.0 },
        ));
        assert!(app.project.insert_before_output(
            "contrast",
            "Contrast",
            Operation::Contrast { amount: 0.2 },
        ));
        ProjectDoc::write(&mut document, &app.project);
        app.editor.select_only("exposure");

        assert!(app.perform_semantic_action(
            "image-editor.adjust.remove",
            &ui::a11y::Action::Activate,
            &mut document,
            &automerge::ROOT,
        ));
        assert_eq!(
            app.project.graph.evaluation_order().unwrap(),
            ["source", "contrast", "output"]
        );
        assert!(app.editor.selection.contains("contrast"));
        app.undo(&mut document);
        assert_eq!(
            app.project.graph.evaluation_order().unwrap(),
            ["source", "exposure", "contrast", "output"]
        );
    }

    fn assistive_slider_changes_the_graph_operation() {
        let mut app = ImageEditorApp::new();
        let mut document = AutoCommit::new();
        ProjectDoc::seed(&mut document);
        assert!(app.project.insert_before_output(
            "contrast",
            "Contrast",
            Operation::Contrast { amount: 0.0 },
        ));
        ProjectDoc::write(&mut document, &app.project);
        app.editor.select_only("contrast");
        assert!(app.perform_semantic_action(
            "image-editor.adjust.slider",
            &ui::a11y::Action::SetValue("75%".into()),
            &mut document,
            &automerge::ROOT,
        ));
        assert!(matches!(
            app.project.operations.get("contrast"),
            Some(Operation::Contrast { amount }) if (*amount - 1.25).abs() < 0.001
        ));
    }

    #[test]
    fn adjustment_stack_navigates_scrolls_and_previews_every_step() {
        keyboard_step_navigation_follows_the_visible_latest_first_order();
        long_adjustment_stacks_scroll_to_every_step_and_clamp();
        graph_previews_include_original_intermediate_and_final_states();
    }

    fn keyboard_step_navigation_follows_the_visible_latest_first_order() {
        let mut app = ImageEditorApp::new();
        let _ =
            app.project
                .insert_before_output("first", "First", Operation::Exposure { stops: 0.1 });
        let _ = app.project.insert_before_output(
            "latest",
            "Latest",
            Operation::Contrast { amount: 0.1 },
        );
        app.editor.select_only("latest");
        app.select_relative_step(1);
        assert!(app.editor.selection.contains("first"));
        app.select_relative_step(-1);
        assert!(app.editor.selection.contains("latest"));
    }

    fn long_adjustment_stacks_scroll_to_every_step_and_clamp() {
        let steps = (0..12)
            .map(|index| {
                (
                    format!("exposure-{index}"),
                    Operation::Exposure { stops: 0.0 },
                )
            })
            .collect::<Vec<_>>();
        let steps = steps
            .iter()
            .map(|(id, operation)| (id.as_str(), operation.clone()))
            .collect::<Vec<_>>();
        let project = sample_project(&steps);
        let mut app = framed_project(&project, 390.0, 760.0);

        let maximum = app.max_stack_scroll();
        assert!(maximum > 0.0);
        app.scroll_stack_by(f64::MAX);
        assert_eq!(app.stack_scroll, maximum);
        app.scroll_stack_by(-f64::MAX);
        assert_eq!(app.stack_scroll, 0.0);
    }

    fn graph_previews_include_original_intermediate_and_final_states() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.png");
        RgbaImage::from_pixel(3, 2, Rgba([40, 80, 120, 255]))
            .save(&source)
            .unwrap();
        let mut app = ImageEditorApp::new();
        let mut document = AutoCommit::new();
        ProjectDoc::seed(&mut document);
        assert!(app.open_path(&mut document, &source));
        app.add(&mut document, Operation::Exposure { stops: 1.0 });

        app.rebuild_preview();
        let preview_ids = app
            .step_previews
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(preview_ids.len(), 3);
        assert_eq!(preview_ids[0], "source");
        assert!(preview_ids[1].starts_with("exposure-"));
        assert_eq!(preview_ids[2], "output");
    }

    fn direct_crop_edits_the_graph_node_and_recomputes_later_steps() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.png");
        RgbaImage::from_pixel(8, 6, Rgba([40, 80, 120, 255]))
            .save(&source)
            .unwrap();
        let mut app = ImageEditorApp::new();
        let mut document = AutoCommit::new();
        ProjectDoc::seed(&mut document);
        assert!(app.open_path(&mut document, &source));
        app.run_toolbar(&mut document, ToolbarCommand::Crop);
        app.add(&mut document, Operation::Contrast { amount: 0.2 });
        let crop = app
            .project
            .operations
            .iter()
            .find_map(|(id, operation)| {
                matches!(operation, Operation::Crop { .. }).then(|| id.clone())
            })
            .unwrap();
        app.editor.select_only(crop.clone());
        app.rebuild_preview();
        app.rebuild_crop_editor();
        assert!(app.crop_editor.is_some());

        app.crop_selection.set(CropSelection {
            left: 0.25,
            top: 0.0,
            right: 0.75,
            bottom: 0.5,
        });
        app.commit_crop_editor(&mut document);
        assert!(matches!(
            app.project.operations.get(&crop),
            Some(Operation::Crop { x, y, width, height })
                if (*x, *y, *width, *height) == (0.25, 0.0, 0.5, 0.5)
        ));
        assert_eq!(
            crate::engine::evaluate(&app.project).unwrap().dimensions(),
            (4, 3)
        );
        assert!(
            app.project
                .operations
                .values()
                .any(|operation| matches!(operation, Operation::Contrast { .. }))
        );
        assert!(app.notice.is_empty(), "a crop drag says nothing");
    }
}
