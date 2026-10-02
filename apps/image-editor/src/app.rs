use crate::{
    actions::{
        ADD_ADJUSTMENT_TYPE, AddAdjustment, AdjustmentUpdate, RECONNECT_INPUT_TYPE,
        REMOVE_ADJUSTMENT_TYPE, ReconnectInput, RemoveAdjustment, UPDATE_ADJUSTMENT_TYPE,
        UpdateAdjustment,
    },
    doc::ProjectDoc,
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
        kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke},
        peniko::Fill,
    },
    winit::{
        event::{ElementState, MouseButton, WindowEvent},
        keyboard::{Key, NamedKey},
    },
};
use ui::{
    presence::PresenceCtx,
    text_render::{TextRenderer, TextWeight},
    theme::Theme,
    widget::{
        Button, ButtonStyle, CropSelection, Image, ImageCropEditor, Slider, SliderFace,
        TaffyTreeExt, Widget,
    },
};

const COMPACT_TOOLBAR_BREAKPOINT: f64 = 1_060.0;
/// The operation a toolkit button's invocation carries.
const CONTROL_OPERATION_TYPE: &str = "tilo.ui.control.invoke";

fn compact_toolbar_for(width: f64) -> bool {
    width < COMPACT_TOOLBAR_BREAKPOINT
}

#[derive(Clone, Copy)]
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
    More,
    Export,
}
pub struct ImageEditorApp {
    project: Project,
    search: crate::search::ProjectSearch,
    indexed_project: Project,
    editor: GraphEditor,
    rect: Rect,
    text_scale: f64,
    graph_rect: Rect,
    cursor: Point,
    serial: u64,
    notice: String,
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
    stack_rect: Rect,
    inspector_rect: Rect,
    inspector_dragging: bool,
    inspector_focused: bool,
    inspector_touch: Option<u64>,
    graph_touch: Option<u64>,
    graph_pan_touch: Option<u64>,
    graph_connection_touch: Option<u64>,
    graph_keyboard_target: Option<ConnectionTarget>,
    step_rects: Vec<(String, Rect)>,
    stack_scroll: f64,
    stack_touch: Option<u64>,
    stack_touch_y: f64,
    advanced_graph: bool,
    compare_original: bool,
    zoom: f64,
    pan: ui::vello::kurbo::Vec2,
    panning_from: Option<Point>,
    touches: HashMap<u64, Point>,
    pinch_distance: Option<f64>,
    step_previews: Vec<(String, Image)>,
    toolbar: Vec<(ToolbarCommand, Button)>,
    empty_open: Button,
    toolbar_pending: Rc<Cell<Option<ToolbarCommand>>>,
    more_open: bool,
    more_rect: Rect,
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
        let specs = [
            (ToolbarCommand::Open, "Open", ui::icons::Icon::FolderOpen),
            (ToolbarCommand::Undo, "Undo", ui::icons::Icon::Undo2),
            (ToolbarCommand::Redo, "Redo", ui::icons::Icon::Redo2),
            (
                ToolbarCommand::Exposure,
                "Exposure",
                ui::icons::Icon::Sparkles,
            ),
            (ToolbarCommand::Contrast, "Contrast", ui::icons::Icon::Eye),
            (ToolbarCommand::Colour, "Colour", ui::icons::Icon::Settings2),
            (ToolbarCommand::Mono, "B&W", ui::icons::Icon::Image),
            (ToolbarCommand::Rotate, "Rotate", ui::icons::Icon::RotateCw),
            (ToolbarCommand::Crop, "Crop", ui::icons::Icon::Scissors),
            (ToolbarCommand::Fit, "Fit", ui::icons::Icon::Maximize2),
            (ToolbarCommand::Compare, "Compare", ui::icons::Icon::Eye),
            (ToolbarCommand::Graph, "Graph", ui::icons::Icon::Layers),
            (
                ToolbarCommand::More,
                "More",
                ui::icons::Icon::MoreHorizontal,
            ),
            (ToolbarCommand::Export, "Export", ui::icons::Icon::Download),
        ];
        let toolbar = specs
            .into_iter()
            .map(|(command, label, icon)| {
                let sink = pending.clone();
                let button = Button::new(
                    &format!("image-editor.{}", label.to_lowercase()),
                    label,
                    None,
                )
                .with_icon(icon)
                .with_size(
                    if matches!(command, ToolbarCommand::Open | ToolbarCommand::Export) {
                        88.0
                    } else {
                        82.0
                    },
                    34.0,
                )
                .with_style(if matches!(command, ToolbarCommand::Export) {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Ghost
                })
                .on_view(move || sink.set(Some(command)));
                (command, button)
            })
            .collect();
        let empty_sink = pending.clone();
        let empty_open = Button::new("image-editor.empty.open", "Open image", None)
            .with_icon(ui::icons::Icon::FolderOpen)
            .with_size(140.0, 44.0)
            .primary()
            .on_view(move || empty_sink.set(Some(ToolbarCommand::Open)));
        let project = Project::blank();
        let search = crate::search::ProjectSearch::new(&project);
        Self {
            project: project.clone(),
            search,
            indexed_project: project,
            editor: GraphEditor::default(),
            rect: Rect::ZERO,
            text_scale: 1.0,
            graph_rect: Rect::ZERO,
            cursor: Point::ZERO,
            serial: 0,
            notice: "Every step is editable. Changes update the final image automatically.".into(),
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
            stack_rect: Rect::ZERO,
            inspector_rect: Rect::ZERO,
            inspector_dragging: false,
            inspector_focused: false,
            inspector_touch: None,
            graph_touch: None,
            graph_pan_touch: None,
            graph_connection_touch: None,
            graph_keyboard_target: None,
            step_rects: Vec::new(),
            stack_scroll: 0.0,
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
            toolbar,
            empty_open,
            toolbar_pending: pending,
            more_open: false,
            more_rect: Rect::ZERO,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            gesture_checkpoint: None,
            actions: crate::resource::ActionSink::default(),
        }
    }
    pub fn seed_shot(&mut self, doc: &mut AutoCommit, path: &std::path::Path, advanced: bool) {
        self.project = Project::blank();
        if let Some(Operation::Source { path: source }) = self.project.operations.get_mut("source")
        {
            *source = path.to_string_lossy().into_owned();
        }
        self.project.name = "Harbour light".into();
        let _ = self.project.insert_before_output(
            "exposure-demo",
            "Exposure",
            Operation::Exposure { stops: 0.4 },
        );
        let _ = self.project.insert_before_output(
            "colour-demo",
            "Colour",
            Operation::Saturation { amount: 1.25 },
        );
        let _ = self.project.insert_before_output(
            "crop-demo",
            "Crop",
            Operation::Crop {
                x: 0.06,
                y: 0.06,
                width: 0.88,
                height: 0.82,
            },
        );
        self.editor.select_only("colour-demo");
        self.advanced_graph = advanced;
        ProjectDoc::write(doc, &self.project);
        self.rendered_project = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.gesture_checkpoint = None;
    }
    pub fn open_more_for_shot(&mut self) {
        self.more_open = true;
    }
    pub fn seed_long_stack_for_shot(&mut self, doc: &mut AutoCommit) {
        for index in 0..9 {
            let id = format!("review-{index}");
            let title = if index % 2 == 0 {
                "Exposure"
            } else {
                "Contrast"
            };
            let operation = if index % 2 == 0 {
                Operation::Exposure { stops: 0.1 }
            } else {
                Operation::Contrast { amount: 0.1 }
            };
            let _ = self.project.insert_before_output(&id, title, operation);
        }
        ProjectDoc::write(doc, &self.project);
        self.rendered_project = None;
    }
    fn node_rect(&self, id: &str) -> Option<Rect> {
        let n = self.project.graph.nodes.get(id)?;
        let p = self.editor.screen_position(n.position);
        let (width, height) = if self.advanced_graph {
            (168.0, 138.0)
        } else {
            (148.0, 72.0)
        };
        Some(Rect::new(
            self.graph_rect.x0 + p.x,
            self.graph_rect.y0 + p.y,
            self.graph_rect.x0 + p.x + width,
            self.graph_rect.y0 + p.y + height,
        ))
    }

    fn input_port_center(&self, id: &str) -> Option<Point> {
        let node = self.project.graph.nodes.get(id)?;
        (!node.inputs.is_empty()).then(|| {
            let rect = self.node_rect(id).unwrap_or(Rect::ZERO);
            Point::new(rect.x0, rect.y0 + 54.0)
        })
    }

    fn output_port_center(&self, id: &str) -> Option<Point> {
        let node = self.project.graph.nodes.get(id)?;
        (!node.outputs.is_empty()).then(|| {
            let rect = self.node_rect(id).unwrap_or(Rect::ZERO);
            Point::new(rect.x1, rect.y0 + 54.0)
        })
    }

    fn graph_port_at(&self, point: Point, output: bool) -> Option<(String, String)> {
        self.project.graph.nodes.keys().find_map(|id| {
            let center = if output {
                self.output_port_center(id)
            } else {
                self.input_port_center(id)
            }?;
            Rect::from_center_size(center, (44.0, 44.0))
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
            self.notice = "Select a step before connecting it".into();
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
            self.notice = "This step has no output to connect".into();
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
        self.notice = "Choose an input with Tab, then press Enter to connect".into();
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
            self.notice = "No compatible input is available".into();
            return true;
        };
        self.begin_gesture();
        match self
            .editor
            .finish_connection(&mut self.project.graph, &target.node, &target.port)
        {
            Ok(_) => {
                self.commit_connection(state, &target.node, &target.port);
                self.notice = "Connection updated".into();
            }
            Err(_) => {
                self.notice =
                    "Those steps cannot be connected; the previous path is unchanged".into();
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
    /// extras beside ResourceRoot.
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
        self.notice = format!("{label}. The final image was recomputed.");
    }

    fn undo(&mut self, state: &mut AutoCommit) {
        self.finish_gesture();
        let Some(previous) = self.undo_stack.pop() else {
            self.notice = "Nothing to undo".into();
            return;
        };
        self.redo_stack.push(self.project.clone());
        self.restore_project(state, previous, "Undid the last change");
    }

    fn redo(&mut self, state: &mut AutoCommit) {
        self.finish_gesture();
        let Some(next) = self.redo_stack.pop() else {
            self.notice = "Nothing to redo".into();
            return;
        };
        self.push_undo(self.project.clone());
        self.restore_project(state, next, "Redid the change");
    }

    fn add(&mut self, state: &mut AutoCommit, op: Operation, title: &str) {
        self.checkpoint();
        let stem = title.to_lowercase();
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
                title: title.into(),
                operation: op,
            },
        );
        if self.project.graph.nodes.contains_key(&id) {
            self.editor.select_only(id);
            self.notice = format!("Added {title}. Earlier steps remain editable.");
        } else {
            // Refusal is already logged by ActionHost; don't leave a stale
            // local undo checkpoint behind when no shared graph change landed.
            let _ = self.undo_stack.pop();
        }
    }
    fn open_path(&mut self, state: &mut AutoCommit, path: &std::path::Path) -> bool {
        if image::image_dimensions(path).is_err() {
            self.notice = "That file is not a supported image".into();
            return false;
        }
        self.checkpoint();
        let source = path.to_string_lossy().into_owned();
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled image")
            .to_string();
        self.dispatch_graph_action(
            state,
            crate::actions::SET_SOURCE_TYPE,
            &crate::actions::SetSource { path: source, name },
        );
        self.notice = "Image opened. Add an adjustment or revise any existing step.".into();
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
            self.notice = "That file is not a supported image".into();
            return false;
        }
        crate::web_file::remember(name, bytes);
        self.checkpoint();
        let source = format!("{}{name}", crate::engine::OPENED_SOURCE);
        let title = std::path::Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled image")
            .to_string();
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
        self.notice = "Image opened. Add an adjustment or revise any existing step.".into();
        true
    }
    fn request_open(&mut self) {
        if self.picker.is_some() || self.contents_picker.is_some() {
            return;
        }
        self.notice = "Waiting for the system file picker…".into();
        if !crate::web_file::native_paths() {
            self.contents_picker = Some(
                ui::portal::PickRequest::open("Open an image", "Open")
                    .filter(
                        ui::portal::FileFilter::new("Images")
                            .mimetype("image/png")
                            .mimetype("image/jpeg")
                            .mimetype("image/webp"),
                    )
                    .send_contents(),
            );
            return;
        }
        self.picker = Some(
            ui::portal::PickRequest::open("Open an image", "Open")
                .thread_name("image-editor-portal-open")
                .filter(
                    ui::portal::FileFilter::new("Images")
                        .mimetype("image/png")
                        .mimetype("image/jpeg")
                        .mimetype("image/webp"),
                )
                .all_files()
                .send(),
        );
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
                    self.notice = "Open cancelled".into();
                }
                Ok(ui::portal::ContentsOutcome::Failed(message)) => {
                    self.contents_picker = None;
                    self.notice = format!("Could not open file picker: {message}");
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.contents_picker = None,
            }
        }
        if let Some(rx) = self.save_contents.as_ref() {
            match rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Ok(ui::portal::SaveOutcome::Saved) => {
                    self.save_contents = None;
                    self.notice = "Exported".into();
                }
                Ok(ui::portal::SaveOutcome::Cancelled) => {
                    self.save_contents = None;
                    self.notice = "Export cancelled".into();
                }
                Ok(ui::portal::SaveOutcome::Failed(message)) => {
                    self.save_contents = None;
                    self.notice = format!("Export failed: {message}");
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
                self.notice = "Open cancelled".into();
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("image selection was cancelled".into()),
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Failed(message)) => {
                self.picker = None;
                self.notice = format!("Could not open file picker: {message}");
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
                self.notice = "The system file picker stopped responding".into();
            }
        }
    }
    fn export_beside_source(&self) -> Result<String, String> {
        let source = match self.project.operations.get("source") {
            Some(Operation::Source { path }) if !path.is_empty() => std::path::Path::new(path),
            _ => {
                return Err("Open an image before exporting".into());
            }
        };
        let out = source.with_file_name(format!(
            "{}-edited.png",
            source
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("image")
        ));
        crate::engine::export(&self.project, &out)
            .map(|()| format!("Exported {}", out.display()))
            .map_err(|error| format!("Export failed: {error:?}"))
    }

    fn export_and_notice(&mut self) {
        if !crate::web_file::native_paths() {
            if self.save_contents.is_none() {
                match crate::engine::export_png(&self.project) {
                    Ok(bytes) => {
                        let name = format!("{}-edited.png", self.project.name);
                        self.save_contents = Some(
                            ui::portal::PickRequest::save("Export image", "Export", &name)
                                .send_save_contents(bytes),
                        );
                    }
                    Err(error) => self.notice = format!("Export failed: {error:?}"),
                }
            }
            return;
        }
        match self.export_beside_source() {
            Ok(notice) => self.notice = notice,
            Err(error) => self.notice = error,
        }
    }
    fn rebuild_preview(&mut self) {
        if self.rendered_project.as_ref() == Some(&self.project) {
            return;
        }
        self.preview = crate::engine::evaluate(&self.project).ok().map(|rgba| {
            let (w, h) = rgba.dimensions();
            Image::from_rgba(std::sync::Arc::from(rgba.into_raw()), w, h)
                .with_alt("Edited image preview")
        });
        self.original_preview = match self.project.operations.get("source") {
            Some(Operation::Source { path }) if !path.is_empty() => {
                crate::engine::open_source(path).ok().map(|rgba| {
                    let (w, h) = rgba.dimensions();
                    Image::from_rgba(std::sync::Arc::from(rgba.into_raw()), w, h)
                        .with_alt("Original image preview")
                })
            }
            _ => None,
        };
        // The advanced graph promises an image state at every point, not just
        // at adjustment nodes. Evaluate Source and Export as well so the
        // original and final result never become blank endpoint cards.
        self.step_previews = self
            .project
            .graph
            .evaluation_order()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| {
                crate::engine::evaluate_at(&self.project, &id)
                    .ok()
                    .map(|rgba| {
                        let (w, h) = rgba.dimensions();
                        let title = self
                            .project
                            .graph
                            .nodes
                            .get(&id)
                            .map_or_else(|| id.clone(), |node| node.title.clone());
                        (
                            id,
                            Image::from_rgba(std::sync::Arc::from(rgba.into_raw()), w, h)
                                .with_alt(format!("Image after {title}")),
                        )
                    })
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
        self.notice = "Crop updated. Later steps were recomputed.".into();
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

    fn stack_row_geometry(&self) -> (f64, f64) {
        let scale = self.text_scale.max(1.0);
        if self.rect.width() < 820.0 {
            (52.0 * scale, 46.0 * scale)
        } else {
            (50.0 * scale, 42.0 * scale)
        }
    }

    fn stack_viewport(&self) -> Rect {
        Rect::new(
            self.stack_rect.x0,
            self.stack_rect.y0 + 58.0 * self.text_scale.max(1.0),
            self.stack_rect.x1,
            self.inspector_rect.y0 - 6.0,
        )
    }

    fn max_stack_scroll(&self) -> f64 {
        let (row_step, row_height) = self.stack_row_geometry();
        let content = self.steps().len().saturating_sub(1) as f64 * row_step + row_height;
        (content - self.stack_viewport().height().max(0.0)).max(0.0)
    }

    fn scroll_stack_by(&mut self, delta: f64) {
        self.stack_scroll = (self.stack_scroll + delta).clamp(0.0, self.max_stack_scroll());
    }

    fn empty_card_rect(&self) -> Rect {
        let viewport = self.canvas_rect.inflate(-28.0, -28.0);
        let height = if viewport.width() < 500.0 {
            240.0
        } else {
            220.0
        };
        Rect::from_center_size(viewport.center(), (viewport.width().min(460.0), height))
    }

    fn empty_open_rect(&self) -> Rect {
        let card = self.empty_card_rect();
        let top = if card.width() < 400.0 {
            card.y0 + 166.0
        } else {
            card.y0 + 142.0
        };
        Rect::new(card.x0 + 32.0, top, card.x0 + 172.0, top + 44.0)
    }
    fn inspector_controls(&self) -> (Rect, Rect) {
        (
            Rect::new(
                self.inspector_rect.x1 - 156.0,
                self.inspector_rect.y0 + 8.0,
                self.inspector_rect.x1 - 112.0,
                self.inspector_rect.y1 - 8.0,
            ),
            Rect::new(
                self.inspector_rect.x1 - 104.0,
                self.inspector_rect.y0 + 8.0,
                self.inspector_rect.x1 - 60.0,
                self.inspector_rect.y1 - 8.0,
            ),
        )
    }

    fn inspector_remove_rect(&self) -> Rect {
        Rect::new(
            self.inspector_rect.x1 - 52.0,
            self.inspector_rect.y0 + 8.0,
            self.inspector_rect.x1 - 8.0,
            self.inspector_rect.y1 - 8.0,
        )
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
        self.notice = "Adjustment removed. Later steps remain connected.".into();
        true
    }

    fn selected_slider(&self) -> Option<(f64, String)> {
        let id = self.editor.selection.iter().next()?;
        match self.project.operations.get(id)? {
            Operation::Exposure { stops } => {
                let value = f64::from((*stops + 5.0) / 10.0);
                Some((value, format!("{stops:+.1} EV")))
            }
            Operation::Contrast { amount } => {
                let value = f64::from((*amount + 1.0) / 3.0);
                Some((value, format!("{:+.0}%", *amount * 100.0)))
            }
            Operation::Saturation { amount } => {
                let value = f64::from(*amount / 3.0);
                Some((value, format!("{:.0}%", *amount * 100.0)))
            }
            _ => None,
        }
    }

    fn inspector_slider_rect(&self) -> Rect {
        Rect::new(
            self.inspector_rect.x0 + 142.0,
            self.inspector_rect.y0 + 8.0,
            self.inspector_rect.x1 - 128.0,
            self.inspector_rect.y1 - 8.0,
        )
    }

    fn inspector_value_at(&self, x: f64) -> f64 {
        let rect = self.inspector_slider_rect();
        ((x - (rect.x0 + 5.0)) / (rect.width() - 10.0).max(1.0)).clamp(0.0, 1.0)
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
        self.notice = "Adjustment updated. Later steps were recomputed.".into();
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
    /// The toolbar and empty-state buttons, which the app holds rather than
    /// exposing as children. Control custody still reaches them here.
    fn retained_buttons_mut(&mut self) -> Vec<&mut Button> {
        self.toolbar
            .iter_mut()
            .map(|(_, button)| button)
            .chain(std::iter::once(&mut self.empty_open))
            .collect()
    }
    fn run_toolbar(&mut self, state: &mut AutoCommit, command: ToolbarCommand) {
        if self.more_open && !matches!(command, ToolbarCommand::More) {
            self.more_open = false;
        }
        match command {
            ToolbarCommand::Open => self.request_open(),
            ToolbarCommand::Undo => self.undo(state),
            ToolbarCommand::Redo => self.redo(state),
            ToolbarCommand::Exposure => {
                self.add(state, Operation::Exposure { stops: 0.5 }, "Exposure")
            }
            ToolbarCommand::Contrast => {
                self.add(state, Operation::Contrast { amount: 0.15 }, "Contrast")
            }
            ToolbarCommand::Colour => {
                self.add(state, Operation::Saturation { amount: 1.2 }, "Colour")
            }
            ToolbarCommand::Mono => self.add(state, Operation::Grayscale, "B&W"),
            ToolbarCommand::Rotate => {
                self.add(state, Operation::Rotate { quarter_turns: 1 }, "Rotate")
            }
            ToolbarCommand::Crop => self.add(
                state,
                Operation::Crop {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                "Crop",
            ),
            ToolbarCommand::Fit => {
                self.zoom = 1.0;
                self.pan = ui::vello::kurbo::Vec2::ZERO
            }
            ToolbarCommand::Compare => self.compare_original = !self.compare_original,
            ToolbarCommand::Graph => self.advanced_graph = !self.advanced_graph,
            ToolbarCommand::More => self.more_open = !self.more_open,
            ToolbarCommand::Export => self.export_and_notice(),
        }
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
        self.poll_picker(state);
        self.rebuild_preview();
    }
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        theme: &Theme,
        m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale);
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
        let compact = compact_toolbar_for(self.rect.width());
        // At large type sizes toolbar labels cannot remain legible in the
        // five compact cells. Keep the familiar glyphs, but preserve the full
        // name for AT and the shared tooltip host.
        let icon_first_toolbar = self.text_scale > 1.25;
        // Normal type keeps the familiar 44px toolbar origin. Enlarged type
        // reserves a full title line before the controls instead of drawing
        // the first row through the project name.
        let toolbar_top = self.rect.y0 + 44.0 + (self.text_scale - 1.0).max(0.0) * 64.0;
        let header_extra = (toolbar_top + 48.0 - (self.rect.y0 + 92.0)).max(0.0);
        let gap = 6.0;
        let top_count = if compact { 5 } else { self.toolbar.len() - 1 };
        let advanced_graph = self.advanced_graph;
        let width = ((self.rect.width() - 24.0 - gap * (top_count - 1) as f64) / top_count as f64)
            .min(if compact { 104.0 } else { 96.0 });
        let mut x = self.rect.x0 + 12.0;
        for (command, button) in &mut self.toolbar {
            button.set_compact_toolbar_presentation(icon_first_toolbar);
            let top_level = if compact {
                matches!(
                    command,
                    ToolbarCommand::Open
                        | ToolbarCommand::Undo
                        | ToolbarCommand::Redo
                        | ToolbarCommand::More
                        | ToolbarCommand::Export
                )
            } else {
                !matches!(command, ToolbarCommand::More)
            };
            if top_level {
                button.apply_layout_external(Rect::new(
                    x,
                    toolbar_top,
                    x + width,
                    toolbar_top + 44.0,
                ));
                x += width + gap;
            } else {
                button.apply_layout_external(Rect::ZERO);
            }
        }
        self.more_rect = Rect::ZERO;
        if compact && self.more_open {
            let more_width = (self.rect.width() - 24.0).min(420.0);
            self.more_rect = Rect::new(
                self.rect.x0 + 12.0,
                toolbar_top + 52.0,
                self.rect.x0 + 12.0 + more_width,
                (toolbar_top + 284.0).min(self.rect.y1 - 12.0),
            );
            let cell_gap = 6.0;
            let cell_width = (self.more_rect.width() - 24.0 - cell_gap) / 2.0;
            let mut index = 0;
            for (command, button) in &mut self.toolbar {
                if matches!(
                    command,
                    ToolbarCommand::Contrast
                        | ToolbarCommand::Colour
                        | ToolbarCommand::Mono
                        | ToolbarCommand::Rotate
                        | ToolbarCommand::Crop
                        | ToolbarCommand::Fit
                        | ToolbarCommand::Compare
                        | ToolbarCommand::Graph
                ) && !(advanced_graph && matches!(command, ToolbarCommand::Graph))
                {
                    let column = index % 2;
                    let row = index / 2;
                    let x0 = self.more_rect.x0 + 12.0 + column as f64 * (cell_width + cell_gap);
                    let y0 = self.more_rect.y0 + 32.0 + row as f64 * 48.0;
                    button.apply_layout_external(Rect::new(x0, y0, x0 + cell_width, y0 + 44.0));
                    index += 1;
                }
            }
        }
        let status_extra = 48.0 * (self.text_scale - 1.0).max(0.0);
        let adaptive = ui::graph_editor::AdaptiveEditorLayout::resolve(
            self.rect.width(),
            (self.rect.height() - header_extra - status_extra).max(0.0),
        );
        let cv = adaptive.canvas;
        let st = adaptive.stack;
        let ins = adaptive.inspector;
        let mut canvas = Rect::new(
            self.rect.x0 + cv.x0,
            self.rect.y0 + cv.y0 + header_extra,
            self.rect.x0 + cv.x1,
            self.rect.y0 + cv.y1 + header_extra,
        );
        let mut stack = Rect::new(
            self.rect.x0 + st.x0,
            self.rect.y0 + st.y0 + header_extra,
            self.rect.x0 + st.x1,
            self.rect.y0 + st.y1 + header_extra,
        );
        if self.rect.width() < 820.0 && self.text_scale > 1.0 {
            // Favour a scrollable adjustment list over a nearly unusable
            // preview when large type is requested. The canvas remains
            // available for direct crop/zoom work above it.
            let wanted_stack_height =
                (300.0 + 48.0 * (self.text_scale - 1.0)).min((stack.y1 - canvas.y0 - 1.0).max(0.0));
            stack.y0 = stack.y1 - wanted_stack_height;
            canvas.y1 = stack.y0 - 1.0;
        }
        if self.advanced_graph {
            self.graph_rect = Rect::new(
                self.rect.x0,
                self.rect.y0 + 92.0 + header_extra,
                self.rect.x1,
                self.rect.y1 - 48.0 - status_extra,
            );
            self.canvas_rect = Rect::ZERO;
            self.stack_rect = Rect::ZERO;
        } else {
            self.stack_rect = stack;
            self.canvas_rect = canvas;
        }
        self.inspector_rect = Rect::new(
            self.rect.x0 + ins.x0,
            self.rect.y0 + ins.y0 + header_extra,
            self.rect.x0 + ins.x1,
            self.rect.y0 + ins.y1 + header_extra,
        );
        if !self.advanced_graph && self.text_scale > 1.0 {
            let inspector_height =
                (68.0 * self.text_scale).min((stack.height() - 58.0 * self.text_scale).max(44.0));
            self.inspector_rect = Rect::new(
                stack.x0 + 12.0,
                stack.y1 - 8.0 - inspector_height,
                stack.x1 - 12.0,
                stack.y1 - 8.0,
            );
        }
        let empty_open_rect = self.empty_open_rect();
        self.empty_open.apply_layout_external(empty_open_rect);
    }
    fn draw(
        &mut self,
        s: &mut Scene,
        a: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        s.fill(Fill::NonZero, a, c.surfaces.base, None, &self.rect);
        let title = tr.elide_to_width_weighted(
            &self.project.name,
            20.0,
            (self.rect.width() - 36.0).max(0.0) as f32,
            TextWeight::Semibold,
        );
        tr.draw_text_weighted(
            s,
            &title,
            20.0,
            c.text_primary,
            a.pre_translate((18.0 + self.rect.x0, 8.0 * self.text_scale + self.rect.y0).into()),
            None,
            TextWeight::Semibold,
        );
        for (command, button) in &mut self.toolbar {
            button.set_enabled(match command {
                ToolbarCommand::Undo => !self.undo_stack.is_empty(),
                ToolbarCommand::Redo => !self.redo_stack.is_empty(),
                _ => true,
            });
            button.set_style(if matches!(command, ToolbarCommand::Export) {
                ButtonStyle::Primary
            } else if (matches!(command, ToolbarCommand::Graph) && self.advanced_graph)
                || (matches!(command, ToolbarCommand::Compare) && self.compare_original)
                || (matches!(command, ToolbarCommand::More) && self.more_open)
            {
                ButtonStyle::Neutral
            } else {
                ButtonStyle::Ghost
            });
            if button.get_rect().area() > 0.0
                && !self.more_rect.contains(button.get_rect().center())
            {
                button.draw(s, a, tr, theme, &PresenceCtx::empty());
            }
        }
        if !self.advanced_graph {
            s.fill(Fill::NonZero, a, c.neutral[1], None, &self.canvas_rect);
            let viewport = self.canvas_rect.inflate(-28.0, -28.0);
            let target = Rect::from_center_size(
                viewport.center(),
                (viewport.width() * self.zoom, viewport.height() * self.zoom),
            ) + self.pan;
            if !self.compare_original
                && let Some(editor) = self.crop_editor.as_mut()
            {
                editor.paint_at(s, a, theme, target);
                s.stroke(&Stroke::new(1.0), a, c.border_standard, None, &target);
            } else if let Some(image) = if self.compare_original {
                self.original_preview.as_mut()
            } else {
                self.preview.as_mut()
            } {
                image.paint_at(s, a, target);
                s.stroke(&Stroke::new(1.0), a, c.border_standard, None, &target);
            } else {
                let card = self.empty_card_rect();
                s.fill(
                    Fill::NonZero,
                    a,
                    c.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(card, 18.0),
                );
                tr.draw_text_weighted(
                    s,
                    "Make something beautiful",
                    if card.width() < 400.0 { 22.0 } else { 24.0 },
                    c.text_primary,
                    a.pre_translate(
                        (
                            card.x0 + 32.0,
                            card.y0 + if card.width() < 400.0 { 44.0 } else { 58.0 },
                        )
                            .into(),
                    ),
                    Some((card.width() - 64.0) as f32),
                    TextWeight::Semibold,
                );
                tr.draw_text_weighted(s,"Open an image or drop it anywhere. Every adjustment remains editable, in any order.",14.0,c.text_dim,a.pre_translate((card.x0+32.0,card.y0+if card.width()<400.0 {108.0}else{98.0}).into()),Some((card.width()-64.0) as f32),TextWeight::Regular);
                self.empty_open.draw(s, a, tr, theme, &PresenceCtx::empty());
            }
            s.fill(Fill::NonZero, a, c.surfaces.raised, None, &self.stack_rect);
            tr.draw_text_weighted(
                s,
                "Adjustments",
                17.0,
                c.text_primary,
                a.pre_translate(
                    (
                        self.stack_rect.x0 + 16.0,
                        self.stack_rect.y0 + 28.0 * self.text_scale.max(1.0),
                    )
                        .into(),
                ),
                None,
                TextWeight::Semibold,
            );
            tr.draw_text_weighted(
                s,
                if self.text_scale > 1.25 {
                    "Select any step to revise."
                } else {
                    "Latest result at top. Select any step to revise."
                },
                11.0,
                c.text_dim,
                a.pre_translate(
                    (
                        self.stack_rect.x0 + 16.0,
                        self.stack_rect.y0 + 48.0 * self.text_scale.max(1.0),
                    )
                        .into(),
                ),
                Some((self.stack_rect.width() - 32.0) as f32),
                TextWeight::Regular,
            );
            self.step_rects.clear();
            let steps = self.steps();
            self.stack_scroll = self.stack_scroll.clamp(0.0, self.max_stack_scroll());
            let viewport = self.stack_viewport();
            s.push_clip_layer(Fill::NonZero, a, &viewport);
            for (index, id) in steps.iter().rev().enumerate() {
                let (row_step, row_height) = self.stack_row_geometry();
                let y =
                    self.stack_rect.y0 + 62.0 * self.text_scale.max(1.0) + index as f64 * row_step
                        - self.stack_scroll;
                if y + row_height < viewport.y0 || y > viewport.y1 {
                    continue;
                }
                let r = Rect::new(
                    self.stack_rect.x0 + 10.0,
                    y,
                    self.stack_rect.x1 - 10.0,
                    y + row_height,
                );
                self.step_rects.push((id.clone(), r));
                let selected = self.editor.selection.contains(id);
                s.fill(
                    Fill::NonZero,
                    a,
                    if selected {
                        c.overlay_medium
                    } else {
                        c.lift(0.35)
                    },
                    None,
                    &RoundedRect::from_rect(r, 9.0),
                );
                if selected {
                    s.stroke(
                        &Stroke::new(2.0),
                        a,
                        c.accent[5],
                        None,
                        &RoundedRect::from_rect(r, 9.0),
                    );
                }
                let label = self
                    .project
                    .graph
                    .nodes
                    .get(id)
                    .map_or(id.as_str(), |n| n.title.as_str());
                tr.draw_text_weighted(
                    s,
                    label,
                    14.0,
                    c.text_primary,
                    a.pre_translate((r.x0 + 12.0, r.y0 + 7.0 * self.text_scale.max(1.0)).into()),
                    Some((r.width() - 92.0) as f32),
                    TextWeight::Semibold,
                );
                tr.draw_text_weighted(
                    s,
                    &format!("Step {}, editable", steps.len() - index),
                    10.0,
                    c.text_dim,
                    a.pre_translate((r.x0 + 12.0, r.y0 + 25.0 * self.text_scale.max(1.0)).into()),
                    None,
                    TextWeight::Regular,
                );
                if let Some((_, thumbnail)) =
                    self.step_previews.iter_mut().find(|(step, _)| step == id)
                {
                    let thumb = Rect::new(r.x1 - 70.0, r.y0 + 5.0, r.x1 - 8.0, r.y1 - 5.0);
                    thumbnail.paint_at(s, a, thumb);
                    s.stroke(
                        &Stroke::new(1.0),
                        a,
                        c.border_standard,
                        None,
                        &RoundedRect::from_rect(thumb, 4.0),
                    );
                }
            }
            s.pop_layer();
            let track = Rect::new(
                viewport.x1 - 5.0,
                viewport.y0 + 4.0,
                viewport.x1 - 2.0,
                viewport.y1 - 4.0,
            );
            let (row_step, row_height) = self.stack_row_geometry();
            let content = steps.len().saturating_sub(1) as f64 * row_step + row_height;
            let metrics = ScrollMetrics::new(content, viewport.height(), self.stack_scroll);
            if let Some(thumb) = Scrollbar::vertical().thumb_rect(track, metrics) {
                s.fill(
                    Fill::NonZero,
                    a,
                    c.border_standard,
                    None,
                    &RoundedRect::from_rect(thumb, 2.0),
                );
            }
            s.fill(
                Fill::NonZero,
                a,
                c.lift(0.35),
                None,
                &RoundedRect::from_rect(self.inspector_rect, 10.0),
            );
            let selected = self
                .editor
                .selection
                .iter()
                .next()
                .and_then(|id| self.project.graph.nodes.get(id))
                .map(|node| node.title.as_str());
            if let Some(selected) = selected {
                tr.draw_text_weighted(
                    s,
                    selected,
                    13.0,
                    c.text_primary,
                    a.pre_translate(
                        (
                            self.inspector_rect.x0 + 12.0,
                            self.inspector_rect.y0
                                + if self.rect.width() < 820.0 {
                                    28.0
                                } else {
                                    23.0
                                },
                        )
                            .into(),
                    ),
                    Some((self.inspector_rect.width() - 80.0) as f32),
                    TextWeight::Semibold,
                );
            }
            if let Some((value, label)) = self.selected_slider() {
                let slider = self.inspector_slider_rect();
                Slider::paint(
                    s,
                    a,
                    theme,
                    &SliderFace::new(slider, value)
                        .hovered(slider.contains(self.cursor))
                        .dragging(self.inspector_dragging)
                        .focused(self.inspector_focused),
                );
                tr.draw_text_weighted(
                    s,
                    &label,
                    12.0,
                    c.text_primary,
                    a.pre_translate(
                        (
                            self.inspector_rect.x1 - 118.0,
                            self.inspector_rect.y0 + 22.0,
                        )
                            .into(),
                    ),
                    Some(54.0),
                    TextWeight::Medium,
                );
            } else if self
                .editor
                .selection
                .iter()
                .next()
                .and_then(|id| self.project.operations.get(id))
                .is_some_and(|operation| matches!(operation, Operation::Rotate { .. }))
            {
                let (minus, plus) = self.inspector_controls();
                ui::widget::Button::paint(
                    s,
                    a,
                    tr,
                    theme,
                    &ui::widget::ButtonFace::new(minus, "")
                        .icon(ui::icons::Icon::RotateCcw)
                        .ghost(),
                );
                ui::widget::Button::paint(
                    s,
                    a,
                    tr,
                    theme,
                    &ui::widget::ButtonFace::new(plus, "")
                        .icon(ui::icons::Icon::RotateCw)
                        .ghost(),
                );
            } else {
                let (hint, x) = if selected.is_none() {
                    (
                        "Select an adjustment to edit",
                        self.inspector_rect.x0 + 12.0,
                    )
                } else if self.crop_editor.is_some() {
                    ("Drag a corner on the image", self.inspector_rect.x1 - 250.0)
                } else {
                    ("No settings for this step", self.inspector_rect.x1 - 250.0)
                };
                tr.draw_text_weighted(
                    s,
                    hint,
                    12.0,
                    c.text_dim,
                    a.pre_translate((x, self.inspector_rect.y0 + 22.0).into()),
                    Some(186.0),
                    TextWeight::Regular,
                );
            }
            if self.selected_is_removable() {
                ui::widget::Button::paint(
                    s,
                    a,
                    tr,
                    theme,
                    &ui::widget::ButtonFace::new(self.inspector_remove_rect(), "")
                        .icon(ui::icons::Icon::Trash2)
                        .style(ui::widget::ButtonStyle::Danger),
                );
            }
        } else {
            s.fill(Fill::NonZero, a, c.neutral[1], None, &self.graph_rect);
            let graph_hint = if self.rect.width() < 820.0 {
                "Drag empty space to pan. Drag ports to reconnect. Tap Graph to return."
            } else {
                "Drag ports to reconnect. Keyboard: select a step, Ctrl+Enter, Tab, Enter. Press G to return."
            };
            tr.draw_text_weighted(
                s,
                graph_hint,
                12.0,
                c.text_dim,
                a.pre_translate((self.graph_rect.x0 + 18.0, self.graph_rect.y0 + 22.0).into()),
                None,
                TextWeight::Regular,
            );
            let contributing = self.project.graph.contributing_nodes("output");
            for e in &self.project.graph.edges {
                if let (Some(from), Some(to)) = (
                    self.output_port_center(&e.from_node),
                    self.input_port_center(&e.to_node),
                ) {
                    let mut p = BezPath::new();
                    p.move_to(from);
                    p.curve_to((from.x + 50.0, from.y), (to.x - 50.0, to.y), (to.x, to.y));
                    let active =
                        contributing.contains(&e.from_node) && contributing.contains(&e.to_node);
                    s.stroke(
                        &Stroke::new(if active { 2.0 } else { 1.0 }),
                        a,
                        if active {
                            c.accent[5]
                        } else {
                            c.text_dim.with_alpha(0.45)
                        },
                        None,
                        &p,
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
                let mut p = BezPath::new();
                p.move_to(from);
                p.curve_to((from.x + 50.0, from.y), (to.x - 50.0, to.y), (to.x, to.y));
                s.stroke(&Stroke::new(2.0), a, c.accent[6], None, &p);
            }
            for (id, n) in &self.project.graph.nodes {
                if let Some(r) = self.node_rect(id) {
                    let selected = self.editor.selection.contains(id);
                    let active = contributing.contains(id);
                    s.fill(
                        Fill::NonZero,
                        a,
                        if selected {
                            c.lift(0.22)
                        } else if !active {
                            c.surfaces.raised.with_alpha(0.62)
                        } else {
                            c.surfaces.raised
                        },
                        None,
                        &RoundedRect::from_rect(r, 10.0),
                    );
                    s.stroke(
                        &Stroke::new(if selected { 2.0 } else { 1.0 }),
                        a,
                        if selected {
                            c.accent[5]
                        } else {
                            c.border_standard
                        },
                        None,
                        &RoundedRect::from_rect(r, 10.0),
                    );
                    tr.draw_text_weighted(
                        s,
                        &n.title,
                        14.0,
                        if active { c.text_primary } else { c.text_dim },
                        a.pre_translate((r.x0 + 14.0, r.y0 + 23.0).into()),
                        Some(120.0),
                        TextWeight::Semibold,
                    );
                    let node_state = match id.as_str() {
                        "source" => "Original image",
                        "output" => "Final result",
                        _ if !active => "Not in final image",
                        _ if n.enabled => "Enabled",
                        _ => "Bypassed",
                    };
                    tr.draw_text_weighted(
                        s,
                        node_state,
                        12.0,
                        c.text_dim,
                        a.pre_translate((r.x0 + 14.0, r.y0 + 50.0).into()),
                        Some(120.0),
                        TextWeight::Regular,
                    );
                    if let Some((_, preview)) =
                        self.step_previews.iter_mut().find(|(step, _)| step == id)
                    {
                        let thumb = Rect::new(r.x0 + 10.0, r.y0 + 68.0, r.x1 - 10.0, r.y1 - 10.0);
                        preview.paint_at(s, a, thumb);
                        s.stroke(
                            &Stroke::new(1.0),
                            a,
                            c.border_standard,
                            None,
                            &RoundedRect::from_rect(thumb, 5.0),
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
                        if focused {
                            s.fill(
                                Fill::NonZero,
                                a,
                                c.accent[5].with_alpha(0.22),
                                None,
                                &Circle::new(center, 14.0),
                            );
                        }
                        s.fill(
                            Fill::NonZero,
                            a,
                            c.surfaces.base,
                            None,
                            &Circle::new(center, 7.0),
                        );
                        s.stroke(
                            &Stroke::new(if focused { 3.0 } else { 2.0 }),
                            a,
                            c.accent[5],
                            None,
                            &Circle::new(center, 7.0),
                        );
                    }
                }
            }
        }
        tr.draw_text_weighted(
            s,
            if self.text_scale > 1.25 {
                "Every step stays editable."
            } else {
                &self.notice
            },
            12.0,
            c.text_dim,
            a.pre_translate(
                (
                    self.rect.x0 + 14.0,
                    self.rect.y1 - 48.0 * self.text_scale.max(1.0)
                        + 20.0 * self.text_scale.max(1.0),
                )
                    .into(),
            ),
            Some((self.rect.width() - 28.0).max(1.0) as f32),
            TextWeight::Regular,
        );
        if self.more_rect.area() > 0.0 {
            s.fill(
                Fill::NonZero,
                a,
                c.overlay_medium,
                None,
                &RoundedRect::from_rect(self.more_rect.inflate(4.0, 4.0), 16.0),
            );
            s.fill(
                Fill::NonZero,
                a,
                c.surfaces.raised,
                None,
                &RoundedRect::from_rect(self.more_rect, 14.0),
            );
            s.stroke(
                &Stroke::new(1.0),
                a,
                c.border_standard,
                None,
                &RoundedRect::from_rect(self.more_rect, 14.0),
            );
            tr.draw_text_weighted(
                s,
                "Tools",
                14.0,
                c.text_primary,
                a.pre_translate((self.more_rect.x0 + 14.0, self.more_rect.y0 + 22.0).into()),
                None,
                TextWeight::Semibold,
            );
            for (_, button) in &mut self.toolbar {
                if button.get_rect().area() > 0.0
                    && self.more_rect.contains(button.get_rect().center())
                {
                    button.draw(s, a, tr, theme, &PresenceCtx::empty());
                }
            }
        }
    }
    fn handle_event(&mut self, e: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        for (_, button) in &mut self.toolbar {
            if button.get_rect().area() > 0.0 {
                button.handle_event(
                    e,
                    &mut ui::widget::EventCtx {
                        modifiers: &ui::winit::event::Modifiers::default(),
                        ..ctx.reborrow(&automerge::ROOT, ctx.node_id)
                    },
                );
            }
        }
        if !self.advanced_graph && self.preview.is_none() {
            self.empty_open.handle_event(
                e,
                &mut ui::widget::EventCtx {
                    modifiers: &ui::winit::event::Modifiers::default(),
                    ..ctx.reborrow(&automerge::ROOT, ctx.node_id)
                },
            );
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
            && !self.advanced_graph
            && !self.compare_original
            && self.crop_editor.is_some()
        {
            self.begin_gesture();
        }
        if !self.advanced_graph
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
                if self.advanced_graph && self.editor.is_connecting() {
                    self.editor.update_connection(ui::graph_editor::GraphPoint {
                        x: next.x - self.graph_rect.x0,
                        y: next.y - self.graph_rect.y0,
                    });
                    *ctx.handled = true;
                } else if self.advanced_graph && self.editor.is_dragging_node() {
                    let point = ui::graph_editor::GraphPoint {
                        x: next.x - self.graph_rect.x0,
                        y: next.y - self.graph_rect.y0,
                    };
                    if self.editor.update_node_drag(&mut self.project.graph, point) {
                        *ctx.handled = true;
                    }
                } else if self.advanced_graph && self.editor.is_panning() {
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
                if self.advanced_graph && self.graph_rect.contains(self.cursor) =>
            {
                let amount = match delta {
                    ui::winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    ui::winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 80.0,
                };
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
                if self.stack_viewport().contains(self.cursor) && self.max_stack_scroll() > 0.0 =>
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
                                self.notice = "Connection updated".into();
                            }
                            Err(_) => {
                                self.notice =
                                    "Those steps cannot be connected; the previous path is unchanged"
                                        .into();
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
                if self.more_open && !self.more_rect.contains(self.cursor) {
                    self.more_open = false;
                    *ctx.handled = true;
                } else if self.more_rect.contains(self.cursor) {
                    *ctx.handled = true;
                } else if self.cursor.y < self.rect.y0 + 94.0 {
                    *ctx.handled = true;
                } else if !self.advanced_graph {
                    let (minus, plus) = self.inspector_controls();
                    let remove = self.inspector_remove_rect();
                    let slider = self.inspector_slider_rect();
                    if self.selected_is_removable() && remove.contains(self.cursor) {
                        self.inspector_focused = false;
                        self.remove_selected(ctx.state);
                    } else if self.selected_slider().is_some() && slider.contains(self.cursor) {
                        self.begin_gesture();
                        self.inspector_dragging = true;
                        self.inspector_focused = true;
                        self.panning_from = None;
                        self.set_selected_slider(ctx.state, self.inspector_value_at(self.cursor.x));
                    } else if minus.contains(self.cursor) {
                        self.inspector_focused = false;
                        self.adjust_selected(ctx.state, -1.0);
                    } else if plus.contains(self.cursor) {
                        self.inspector_focused = false;
                        self.adjust_selected(ctx.state, 1.0);
                    } else if let Some((id, _)) = self
                        .step_rects
                        .iter()
                        .find(|(_, r)| r.contains(self.cursor))
                        .cloned()
                    {
                        self.inspector_focused = false;
                        self.editor.select_only(id);
                        self.rebuild_crop_editor();
                    } else if self.canvas_rect.contains(self.cursor) {
                        self.inspector_focused = false;
                        self.panning_from = Some(self.cursor);
                    }
                } else if self.advanced_graph
                    && self.graph_touch.is_none()
                    && self.graph_pan_touch.is_none()
                    && self.graph_connection_touch.is_none()
                    && let Some((node, port)) = self.graph_port_at(self.cursor, true)
                {
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
                    self.begin_gesture();
                    let point = ui::graph_editor::GraphPoint {
                        x: self.cursor.x - self.graph_rect.x0,
                        y: self.cursor.y - self.graph_rect.y0,
                    };
                    self.editor.begin_node_drag(&self.project.graph, &id, point);
                    self.rebuild_crop_editor();
                } else if self.advanced_graph && self.graph_rect.contains(self.cursor) {
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
                        if self.more_open {
                            if !self.more_rect.contains(point) {
                                self.more_open = false;
                            }
                            *ctx.handled = true;
                            return;
                        }
                        if self.advanced_graph {
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
                                        self.graph_pan_touch = Some(touch.id);
                                    }
                                }
                            }
                            *ctx.handled = true;
                            return;
                        }
                        self.touches.insert(touch.id, point);
                        if self.stack_viewport().contains(point) {
                            self.stack_touch = Some(touch.id);
                            self.stack_touch_y = point.y;
                        }
                        {
                            let (minus, plus) = self.inspector_controls();
                            let remove = self.inspector_remove_rect();
                            let slider = self.inspector_slider_rect();
                            if self.selected_is_removable() && remove.contains(point) {
                                self.inspector_focused = false;
                                self.remove_selected(ctx.state);
                            } else if self.selected_slider().is_some() && slider.contains(point) {
                                self.begin_gesture();
                                self.inspector_touch = Some(touch.id);
                                self.inspector_focused = true;
                                self.panning_from = None;
                                self.set_selected_slider(
                                    ctx.state,
                                    self.inspector_value_at(point.x),
                                );
                            } else if minus.contains(point) {
                                self.inspector_focused = false;
                                self.adjust_selected(ctx.state, -1.0);
                            } else if plus.contains(point) {
                                self.inspector_focused = false;
                                self.adjust_selected(ctx.state, 1.0);
                            } else if let Some((id, _)) = self
                                .step_rects
                                .iter()
                                .find(|(_, rect)| rect.contains(point))
                                .cloned()
                            {
                                self.inspector_focused = false;
                                self.editor.select_only(id);
                                self.rebuild_crop_editor();
                            } else if self.canvas_rect.contains(point) {
                                self.inspector_focused = false;
                                self.panning_from = Some(point);
                            }
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
                        if self.advanced_graph {
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
                        if self.advanced_graph {
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
                                            self.notice = "Connection updated".into();
                                        }
                                        Err(_) => {
                                            self.notice = "Those steps cannot be connected; the previous path is unchanged".into();
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
                } else if self.advanced_graph
                    && self.editor.is_connecting()
                    && self.graph_keyboard_target.is_some()
                    && matches!(event.logical_key, Key::Named(NamedKey::Tab))
                {
                    self.cycle_keyboard_connection(ctx.modifiers.state().shift_key());
                    *ctx.handled = true;
                } else if self.advanced_graph
                    && self.editor.is_connecting()
                    && self.graph_keyboard_target.is_some()
                    && matches!(event.logical_key, Key::Named(NamedKey::Enter))
                {
                    self.finish_keyboard_connection(ctx.state);
                    *ctx.handled = true;
                } else if self.advanced_graph
                    && self.editor.is_connecting()
                    && matches!(event.logical_key, Key::Named(NamedKey::Escape))
                {
                    self.editor.cancel_connection();
                    self.graph_keyboard_target = None;
                    self.notice = "Connection cancelled".into();
                    *ctx.handled = true;
                } else if self.advanced_graph
                    && ctx.modifiers.state().control_key()
                    && matches!(event.logical_key, Key::Named(NamedKey::Enter))
                {
                    self.begin_keyboard_connection();
                    *ctx.handled = true;
                } else if self.advanced_graph
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
                } else if matches!(event.logical_key, Key::Named(NamedKey::Escape))
                    && self.more_open
                {
                    self.more_open = false;
                    *ctx.handled = true;
                } else if matches!(&event.logical_key,Key::Character(s) if s.eq_ignore_ascii_case("g"))
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
                        self.notice = if removed == 1 {
                            "Adjustment removed. Later steps remain connected.".into()
                        } else {
                            format!("Removed {removed} adjustments. Later steps remain connected.")
                        };
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
        if self.advanced_graph && self.graph_rect.contains(point) {
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
    }
    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut out = vec![
            ("canvas".into(), self.canvas_rect),
            ("adjustments".into(), self.stack_rect),
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
            for button in self.retained_buttons_mut() {
                if let Some(started) = button.start_operation(op, doc, path)? {
                    return Ok(Some(started));
                }
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
            .toolbar
            .iter()
            .filter(|(_, button)| button.get_rect().area() > 0.0)
            .filter_map(|(_, button)| button.accessibility())
            .collect::<Vec<_>>();
        if !self.advanced_graph && self.preview.is_none() {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name("Open image")
                    .with_action_target("image-editor.empty.open")
                    .with_bounds(self.empty_open.get_rect())
                    .with_state(AccessState::new().focusable(true)),
            );
        }
        children.push(
            AccessNode::new(Role::Image).with_name(if self.compare_original {
                "Original image"
            } else {
                "Live edited image"
            }),
        );
        if let Some(crop) = self.crop_editor.as_ref()
            && let Some(access) = crop.accessibility()
        {
            children.push(access);
        }
        let contributing = self.project.graph.contributing_nodes("output");
        children.extend(
            self.project
                .graph
                .nodes
                .iter()
                .map(|(id, node)| {
                    let bounds = if self.advanced_graph {
                        self.node_rect(id)
                    } else {
                        self.step_rects
                            .iter()
                            .find_map(|(candidate, rect)| (candidate == id).then_some(*rect))
                    };
                    let mut access = AccessNode::new(Role::Button)
                        .with_name(node.title.clone())
                        .with_value(if !contributing.contains(id) {
                            "Not in final image"
                        } else if node.enabled {
                            "Enabled"
                        } else {
                            "Bypassed"
                        })
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
        if self.advanced_graph {
            for (id, node) in &self.project.graph.nodes {
                if let (Some(port), Some(center)) =
                    (node.outputs.first(), self.output_port_center(id))
                {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(format!("Connect from {} {}", node.title, port.label))
                            .with_action_target(format!("image-editor.output.{id}"))
                            .with_bounds(Rect::from_center_size(center, (44.0, 44.0)))
                            .with_state(AccessState::new().focusable(true)),
                    );
                }
                if let (Some(port), Some(center)) =
                    (node.inputs.first(), self.input_port_center(id))
                {
                    children.push(
                        AccessNode::new(Role::Button)
                            .with_name(format!("Connect to {} {}", node.title, port.label))
                            .with_action_target(format!("image-editor.input.{id}"))
                            .with_bounds(Rect::from_center_size(center, (44.0, 44.0)))
                            .with_state(AccessState::new().focusable(true)),
                    );
                }
            }
        }
        if !self.advanced_graph {
            if let Some((_, value)) = self.selected_slider() {
                let name = self
                    .editor
                    .selection
                    .iter()
                    .next()
                    .and_then(|id| self.project.graph.nodes.get(id))
                    .map(|node| node.title.as_str())
                    .unwrap_or("Image adjustment");
                children.push(
                    AccessNode::new(Role::Slider)
                        .with_name(name)
                        .with_value(value)
                        .with_action_target("image-editor.adjust.slider")
                        .with_bounds(self.inspector_slider_rect())
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .focused(self.inspector_focused),
                        ),
                );
            } else if self
                .editor
                .selection
                .iter()
                .next()
                .and_then(|id| self.project.operations.get(id))
                .is_some_and(|operation| matches!(operation, Operation::Rotate { .. }))
            {
                let (left, right) = self.inspector_controls();
                children.extend([
                    AccessNode::new(Role::Button)
                        .with_name("Rotate left")
                        .with_action_target("image-editor.adjust.decrease")
                        .with_bounds(left)
                        .with_state(AccessState::new().focusable(true)),
                    AccessNode::new(Role::Button)
                        .with_name("Rotate right")
                        .with_action_target("image-editor.adjust.increase")
                        .with_bounds(right)
                        .with_state(AccessState::new().focusable(true)),
                ]);
            }
            if self.selected_is_removable() {
                children.push(
                    AccessNode::new(Role::Button)
                        .with_name("Remove selected adjustment")
                        .with_description("Later steps remain connected; this can be undone")
                        .with_action_target("image-editor.adjust.remove")
                        .with_bounds(self.inspector_remove_rect())
                        .with_state(AccessState::new().focusable(true)),
                );
            }
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
                .with_name(if self.advanced_graph {
                    "Advanced image processing graph"
                } else {
                    "Image editor canvas and adjustment stack"
                })
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
        let toolbar_command = match target {
            "image-editor.open" => Some(ToolbarCommand::Open),
            "image-editor.undo" => Some(ToolbarCommand::Undo),
            "image-editor.redo" => Some(ToolbarCommand::Redo),
            "image-editor.exposure" => Some(ToolbarCommand::Exposure),
            "image-editor.contrast" => Some(ToolbarCommand::Contrast),
            "image-editor.colour" => Some(ToolbarCommand::Colour),
            "image-editor.b&w" => Some(ToolbarCommand::Mono),
            "image-editor.rotate" => Some(ToolbarCommand::Rotate),
            "image-editor.crop" => Some(ToolbarCommand::Crop),
            "image-editor.fit" => Some(ToolbarCommand::Fit),
            "image-editor.compare" => Some(ToolbarCommand::Compare),
            "image-editor.graph" => Some(ToolbarCommand::Graph),
            "image-editor.more" => Some(ToolbarCommand::More),
            "image-editor.export" => Some(ToolbarCommand::Export),
            _ => None,
        };
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
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.actions.is_hosted() {
            return Vec::new();
        }
        vec![self.project_resource(doc)]
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.actions.is_hosted() && id == crate::actions::PROJECT_RESOURCE_ID {
            return None;
        }
        fn find(resource: &tilo_actions::Resource, id: &str) -> Option<tilo_actions::Resource> {
            if resource.id.as_str() == id {
                return Some(resource.clone());
            }
            for child in &resource.children {
                if let Some(found) = find(child, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.project_resource(doc), id)
    }
}

impl ImageEditorApp {
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
        for (_, button) in &self.toolbar {
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
                self.editor.select_only(id.clone());
                self.rebuild_crop_editor();
                self.notice = if id == "source" {
                    "Original source selected".into()
                } else if id == self.project.output {
                    "Final export selected".into()
                } else {
                    "Adjustment selected".into()
                };
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
                    self.notice = notice;
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    }
                }
                Err(message) => {
                    self.notice = message.clone();
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

#[cfg(test)]
mod portal_tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use ui::headless::Framed;

    /// Responsive layout: the toolbar compacts before collisions, and every published
    /// target (empty-state open, phone toolbar, More menu, inspector, graph ports) is
    /// touch sized, disjoint and semantically reachable.
    #[test]
    fn responsive_layout_publishes_touch_sized_disjoint_targets() {
        toolbar_compacts_before_desktop_actions_can_collide();
        empty_state_open_is_a_real_touch_sized_semantic_button();
        phone_toolbar_targets_are_touch_sized_and_never_overlap();
        phone_more_exposes_every_tool_as_disjoint_touch_targets();
        compact_inspector_controls_remain_touch_sized_and_disjoint();
        advanced_graph_exposes_touch_sized_accessible_ports();
    }

    fn toolbar_compacts_before_desktop_actions_can_collide() {
        assert!(compact_toolbar_for(820.0));
        assert!(compact_toolbar_for(1_024.0));
        assert!(!compact_toolbar_for(1_280.0));
    }

    fn empty_state_open_is_a_real_touch_sized_semantic_button() {
        let mut app = framed(ImageEditorApp::new(), 390.0, 760.0);
        let rect = app.empty_open.get_rect();
        assert!(rect.width() >= 44.0 && rect.height() >= 44.0);
        assert!(app.canvas_rect.contains(rect.center()));

        let access = app.accessibility().unwrap();
        assert!(access.children.iter().any(|child| {
            child.action_target.as_deref() == Some("image-editor.empty.open")
                && child.role == Role::Button
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

    fn phone_toolbar_targets_are_touch_sized_and_never_overlap() {
        for width in [360.0, 390.0] {
            let app = framed(ImageEditorApp::new(), width, 640.0);
            let visible = app
                .toolbar
                .iter()
                .filter_map(|(_, button)| {
                    (button.get_rect().area() > 0.0).then_some(button.get_rect())
                })
                .collect::<Vec<_>>();
            assert_eq!(visible.len(), 5);
            assert!(visible.iter().all(|rect| rect.height() >= 44.0));
            assert!(visible.windows(2).all(|pair| pair[0].x1 < pair[1].x0));
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

        app.add(&mut state, Operation::Exposure { stops: 0.5 }, "Exposure");
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

        app.add(
            &mut document,
            Operation::Exposure { stops: 0.5 },
            "Exposure",
        );
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
        app.add(&mut state, Operation::Exposure { stops: 0.5 }, "Exposure");
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
        let mut app = framed(ImageEditorApp::new(), 1_280.0, 800.0);
        app.advanced_graph = true;
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
            assert!(
                port.name
                    .as_deref()
                    .is_some_and(|name| name.starts_with("Connect "))
            );
        }
    }

    fn phone_more_exposes_every_tool_as_disjoint_touch_targets() {
        for width in [360.0, 390.0] {
            let mut app = ImageEditorApp::new();
            app.more_open = true;
            let app = framed(app, width, 640.0);
            let menu_targets = app
                .toolbar
                .iter()
                .filter_map(|(_, button)| {
                    let rect = button.get_rect();
                    (rect.area() > 0.0 && app.more_rect.contains(rect.center())).then_some(rect)
                })
                .collect::<Vec<_>>();
            assert_eq!(menu_targets.len(), 8);
            assert!(menu_targets.iter().all(|rect| rect.height() >= 44.0));
            for (index, rect) in menu_targets.iter().enumerate() {
                assert!(app.more_rect.contains(rect.center()));
                assert!(
                    menu_targets[index + 1..]
                        .iter()
                        .all(|other| rect.intersect(*other).area() == 0.0)
                );
            }
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
        assert_eq!(app.notice, "Open cancelled");
        assert!(app.picker.is_none());
    }

    fn compact_inspector_controls_remain_touch_sized_and_disjoint() {
        let mut app = ImageEditorApp::new();
        app.inspector_rect = Rect::new(12.0, 680.0, 608.0, 740.0);
        let (minus, plus) = app.inspector_controls();
        let remove = app.inspector_remove_rect();
        assert!(minus.width() >= 44.0 && minus.height() >= 44.0);
        assert!(plus.width() >= 44.0 && plus.height() >= 44.0);
        assert!(remove.width() >= 44.0 && remove.height() >= 44.0);
        assert!(minus.x1 < plus.x0);
        assert!(plus.x1 < remove.x0);
        assert!(app.inspector_rect.contains(minus.center()));
        assert!(app.inspector_rect.contains(plus.center()));
        assert!(app.inspector_rect.contains(remove.center()));
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
        let mut app = ImageEditorApp::new();
        app.inspector_rect = Rect::new(12.0, 680.0, 608.0, 740.0);
        assert!(app.project.insert_before_output(
            "exposure",
            "Exposure",
            Operation::Exposure { stops: 0.0 },
        ));
        app.editor.select_only("exposure");
        let access = app.accessibility().unwrap();
        let slider = access
            .children
            .iter()
            .find(|node| node.action_target.as_deref() == Some("image-editor.adjust.slider"))
            .expect("numeric adjustment has one real slider");
        assert_eq!(slider.role, Role::Slider);
        assert!(slider.bounds.unwrap().to_rect().height() >= 44.0);

        assert!(
            app.project
                .insert_before_output("mono", "Black and white", Operation::Grayscale,)
        );
        app.editor.select_only("mono");
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
                && node.bounds.unwrap().to_rect().height() >= 44.0
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
        let mut app = ImageEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 390.0, 760.0);
        app.stack_rect = Rect::new(0.0, 410.0, 390.0, 712.0);
        app.inspector_rect = Rect::new(12.0, 644.0, 378.0, 704.0);
        for index in 0..12 {
            assert!(app.project.insert_before_output(
                &format!("exposure-{index}"),
                "Exposure",
                Operation::Exposure { stops: 0.0 },
            ));
        }

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
        app.add(
            &mut document,
            Operation::Exposure { stops: 1.0 },
            "Exposure",
        );

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
        app.add(
            &mut document,
            Operation::Contrast { amount: 0.2 },
            "Contrast",
        );
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
        assert_eq!(app.notice, "Crop updated. Later steps were recomputed.");
    }
}
