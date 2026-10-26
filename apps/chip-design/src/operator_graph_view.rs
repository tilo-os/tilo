//! Generic structural graph, rendered by the shared retained canvas.
use std::sync::{Arc, Mutex};

use crate::operator_graph::{OperatorKind, OperatorNode, OperatorState};
use crate::resource::{Project as ChipProject, project};
use crate::view::ChipEffect;
use ui::LocalState;
use ui::content_zoom::ZoomStep;
use ui::framework::{EffectCx, Effected, ViewCx};
use ui::graph_editor::{Edge, connection_targets};
use ui::i18n::I18nManager;
use ui::icons::Icon;
use ui::lock::MutexRecover as _;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::{Id, Invoke};
use ui::tokens::TextRole;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use ui::widget::{
    AppModel, Button, Column, Container, Dropdown, Element, Field, GraphCanvasConnection,
    GraphCanvasNode, ModelPath, NavItem, NavList, NavSection, Property, PropertyList, Row,
    ScrollAxis, Scrollable, Text, TextInput, ViewportState, Widget,
};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub node: Option<String>,
    pub output: Option<(String, String)>,
}
fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}
fn connect(edge: Edge) -> Invoke<ChipProject> {
    ChipProject::CONNECT.with(project::Connect {
        from_node: edge.from_node,
        from_port: edge.from_port,
        to_node: edge.to_node,
        to_port: edge.to_port,
    })
}

/// Moving the operator `node` to `x`, `y`: two field edits, one step.
fn move_to(node: &str, x: u16, y: u16) -> Invoke<ChipProject> {
    let at = ChipProject::paths().nodes.item(&Id::from_raw(node));
    Invoke::group(
        [at.x.set(x), at.y.set(y)]
            .iter()
            .filter_map(|edit| Invoke::edit(edit).ok()),
    )
}
pub fn port_id(node: &str, port: &str, input: bool) -> String {
    format!(
        "chip-port-{}-{node}-{port}",
        if input { "in" } else { "out" }
    )
}

fn effect_button(cx: &ViewCx<ChipProject>, id: &str, label: &str, effect: &ChipEffect) -> Button {
    cx.actions()
        .bind_effect_invocation(Button::new(id, label), effect)
}
fn action_button(
    cx: &ViewCx<ChipProject>,
    id: &str,
    label: &str,
    invoke: Invoke<ChipProject>,
) -> Button {
    let button = Button::new(id, label);
    match cx.actions().invocation(invoke) {
        Some(binding) => button.on_action(binding),
        None => button.with_enabled(false),
    }
}

/// A fresh operator of `kind`, placed after the `count` already in the
/// graph on a four-wide grid.
fn new_node(kind: OperatorKind, count: usize) -> OperatorNode {
    let n = u16::try_from(count).unwrap_or(64);
    OperatorNode {
        id: ui::tilo_actions::ActionId::new().0.to_string(),
        kind,
        shape: if matches!(
            kind,
            OperatorKind::NormWeightInput | OperatorKind::MatrixWeightInput
        ) {
            vec![8]
        } else {
            vec![4, 8]
        },
        normalization_width: 8,
        output_width: matches!(
            kind,
            OperatorKind::MatrixWeightInput | OperatorKind::LinearProjection
        )
        .then_some(8),
        x: (n % 4).saturating_mul(280),
        y: (n / 4).saturating_mul(320),
    }
}

/// The operator the add menu lists as `label`.
fn kind_named(label: &str) -> Option<OperatorKind> {
    OperatorKind::ALL
        .into_iter()
        .find(|kind| tr(kind.key()) == label)
}

/// One "Add operator" menu, not a button per operator kind: the kinds are a
/// choice, and a row of a dozen buttons buried the graph under them.
fn add_menu(cx: &ViewCx<ChipProject>) -> Element {
    let options = OperatorKind::ALL
        .into_iter()
        .map(|kind| tr(kind.key()))
        .collect();
    let menu = Dropdown::new(options, "")
        .with_id("chip-graph-add")
        .with_placeholder(&tr("chip-graph-add-operator"))
        .with_accessible_name(tr("chip-graph-add-operator"));
    Box::new(cx.actions().bind_effect(menu, ChipEffect::ADD_OPERATOR))
}

#[allow(
    clippy::too_many_lines,
    reason = "one table of the graph's controls, list and inspector keeps the view comparable"
)]
pub fn body(
    cx: &ViewCx<ChipProject>,
    state: &OperatorState,
    selection: &Selection,
    graph_selection: &LocalState<Selection>,
    controls: &GraphControls,
) -> Element {
    let graph = state.graph();
    let targets = selection
        .output
        .as_ref()
        .map_or_else(Vec::new, |(node, port)| {
            connection_targets(&graph, node, port)
        });
    let nodes = state
        .nodes
        .values()
        .map(|node| node_view(cx, node, &graph, selection, &targets))
        .collect();
    let connections = state
        .active
        .iter()
        .map(|edge| GraphCanvasConnection {
            from: port_id(&edge.from_node, &edge.from_port, false),
            to: port_id(&edge.to_node, &edge.to_port, true),
        })
        .collect();
    let mut page = Column::new()
        .push(Box::new(
            Text::new(&tr("chip-graph-scope")).with_role(TextRole::Caption),
        ))
        .push(add_menu(cx));
    if let Some((node, port)) = &selection.output {
        let from = state
            .nodes
            .get(node)
            .map_or_else(|| node.clone(), |n| tr(n.kind.key()));
        page = page.push(Box::new(
            Row::new()
                .push(Box::new(Text::new(&I18nManager::format(
                    "chip-graph-connect-from",
                    &[
                        ("port", tr(&format!("chip-port-{port}")).into()),
                        ("operator", from.into()),
                    ],
                ))))
                .push(Box::new(cx.effect_button(
                    tr("chip-graph-clear-output"),
                    &ChipEffect::InspectOperator { node: None },
                ))),
        ));
    } else if !state.nodes.is_empty() {
        page = page.push(Box::new(
            Text::new(&tr("chip-graph-connect-hint")).with_role(TextRole::Caption),
        ));
    }
    let zoom_out = {
        let controls = controls.clone();
        Button::icon_only(
            "chip-graph-zoom-out",
            Icon::Minus,
            &tr("chip-graph-zoom-out"),
        )
        .on_view(move || controls.request(GraphViewRequest::Step(ZoomStep::Out)))
    };
    let zoom_in = {
        let controls = controls.clone();
        Button::icon_only("chip-graph-zoom-in", Icon::Plus, &tr("chip-graph-zoom-in"))
            .on_view(move || controls.request(GraphViewRequest::Step(ZoomStep::In)))
    };
    let fit = {
        let controls = controls.clone();
        Button::new("chip-graph-fit", &tr("chip-graph-fit"))
            .on_view(move || controls.request(GraphViewRequest::Fit))
    };
    let zoom_label = {
        let controls = controls.clone();
        Text::dynamic(move || controls.zoom_percent()).with_role(TextRole::Caption)
    };
    page = page.push(Box::new(
        Row::new()
            .push(Box::new(zoom_out))
            .push(Box::new(zoom_label))
            .push(Box::new(zoom_in))
            .push(Box::new(fit)),
    ));
    page = page.push(Box::new(Container::new().with_height(240.0).with_child(
        Box::new(operator_list(state, selection, graph_selection, controls)),
    )));
    let positions = state
        .nodes
        .values()
        .map(|node| (node.id.clone(), f64::from(node.x), f64::from(node.y)))
        .collect();
    page = page.push(Box::new(Container::new().with_height(540.0).with_child(
        Box::new(GraphView::new(
            nodes,
            connections,
            positions,
            controls.clone(),
        )),
    )));
    if let Some(node) = selection.node.as_ref().and_then(|id| state.nodes.get(id)) {
        page = page.push(inspector(cx, node, state));
    }
    for rejected in &state.rejected {
        page = page.push(Box::new(Text::new(&format!(
            "{} {}.{} to {}.{}: {}",
            tr("chip-graph-conflict"),
            rejected.edge.from_node,
            rejected.edge.from_port,
            rejected.edge.to_node,
            rejected.edge.to_port,
            tr(&format!("chip-graph-{}", rejected.reason))
        ))));
    }
    Box::new(Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(page)))
}

/// The operator list: every node by the name the add menu gave it, in the
/// shared navigation list. Choosing one selects it and brings it into view.
fn operator_list(
    state: &OperatorState,
    selection: &Selection,
    graph_selection: &LocalState<Selection>,
    controls: &GraphControls,
) -> NavList {
    let items: Vec<NavItem> = state
        .nodes
        .values()
        .map(|node| {
            NavItem::new(node.id.clone(), tr(node.kind.key()))
                .secondary(node.id.clone())
                .icon(Icon::Square)
        })
        .collect();
    let chosen = graph_selection.clone();
    let reveal = controls.clone();
    NavList::new(vec![NavSection::new(
        "chip.operators",
        tr("chip-graph-operators"),
        items,
    )])
    .with_id("chip-graph-operators")
    .with_accessible_name(tr("chip-graph-operators"))
    .selected(selection.node.as_deref().unwrap_or(""))
    .on_select(move |id| {
        reveal.reveal(&id);
        chosen.set(Selection {
            node: Some(id),
            output: None,
        });
    })
}

/// What a graph window keeps for itself: where its viewport is, what a
/// button asked for, and which operator the list last brought into view.
#[derive(Clone, Debug, Default, PartialEq)]
struct GraphViewportState {
    zoom: f64,
    scroll_x: f64,
    scroll_y: f64,
    /// The first layout fits the whole graph; later layouts restore this.
    fitted: bool,
    /// Whether the zoom is the fit the content has. Adding an operator to a
    /// graph the person has not zoomed away from re-fits it; adding one to a
    /// graph they have keeps their zoom and place.
    at_fit: bool,
    /// The content size last fitted or restored, so new operators are seen.
    content: (f64, f64),
    request: Option<GraphViewRequest>,
    reveal: Option<String>,
    revealed: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphViewRequest {
    Step(ZoomStep),
    Fit,
}

/// The graph's shared controls, held in the window's local state so a view
/// rebuild (a new operator, a selection) keeps the viewport and its place.
#[derive(Clone, Debug, Default)]
pub struct GraphControls {
    state: Arc<Mutex<GraphViewportState>>,
}

impl GraphControls {
    fn request(&self, request: GraphViewRequest) {
        self.state.lock_recover().request = Some(request);
    }

    fn reveal(&self, id: &str) {
        let mut state = self.state.lock_recover();
        state.reveal = Some(id.to_owned());
        state.revealed = None;
    }

    fn zoom_percent(&self) -> String {
        let zoom = self.state.lock_recover().zoom;
        format!("{:.0}%", zoom * 100.0)
    }
}

/// Operators at their authored places: real controls laid out at absolute
/// positions, with the edges painted between them. The shared
/// [`GraphCanvas`] places its children through a manual policy that the
/// viewport's own layout pass does not run; this keeps the same node
/// elements in an ordinary absolute layout so the shared viewport can zoom
/// and pan them.
struct PositionedGraph {
    children: Vec<Box<dyn Widget>>,
    child_nodes: Vec<ui::taffy::NodeId>,
    placements: Vec<(f32, f32, f32)>,
    connections: Vec<GraphCanvasConnection>,
    content: (f64, f64),
    scale: f32,
    rect: Rect,
}

impl PositionedGraph {
    fn new(
        nodes: Vec<GraphCanvasNode>,
        connections: Vec<GraphCanvasConnection>,
        content: (f64, f64),
    ) -> Self {
        let placements = nodes
            .iter()
            .map(|node| (node.x, node.y, node.width))
            .collect();
        Self {
            children: nodes.into_iter().map(|node| node.content).collect(),
            child_nodes: Vec::new(),
            placements,
            connections,
            content,
            scale: 1.0,
            rect: Rect::ZERO,
        }
    }

    fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.01);
    }

    fn connection_rects(&self) -> Vec<(Rect, Rect)> {
        self.connections
            .iter()
            .filter_map(|edge| {
                Some((
                    find(&self.children, &edge.from)?,
                    find(&self.children, &edge.to)?,
                ))
            })
            .collect()
    }
}

fn find(widgets: &[Box<dyn Widget>], id: &str) -> Option<Rect> {
    widgets
        .iter()
        .find_map(|child| ui::widget::find_test_id(child.as_ref(), id))
        .map(Widget::get_rect)
}

impl Widget for PositionedGraph {
    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.child_nodes.clear();
        let mut children = Vec::with_capacity(self.children.len());
        for (child, &(x, y, width)) in self.children.iter_mut().zip(&self.placements) {
            let (x, y, width) = (x * self.scale, y * self.scale, width * self.scale);
            let node = child.build_layout(taffy, theme, node_map);
            if let Ok(existing) = taffy.style(node).cloned() {
                let _ = taffy.set_style(
                    node,
                    ui::taffy::Style {
                        position: ui::taffy::Position::Absolute,
                        inset: ui::taffy::Rect {
                            left: ui::taffy::style_helpers::length(x),
                            top: ui::taffy::style_helpers::length(y),
                            right: ui::taffy::LengthPercentageAuto::auto(),
                            bottom: ui::taffy::LengthPercentageAuto::auto(),
                        },
                        size: ui::taffy::Size {
                            width: ui::taffy::Dimension::length(width),
                            height: ui::taffy::Dimension::auto(),
                        },
                        ..existing
                    },
                );
            }
            self.child_nodes.push(node);
            children.push(node);
        }
        #[allow(
            clippy::unwrap_used,
            reason = "the graph's own absolute children are inserted into the app-owned layout tree; only a corrupted arena can fail"
        )]
        let node = taffy
            .new_with_children(
                ui::taffy::Style {
                    size: ui::taffy::Size {
                        width: ui::taffy::Dimension::length(
                            ui::num::f32_of_f64(self.content.0) * self.scale,
                        ),
                        height: ui::taffy::Dimension::length(
                            ui::num::f32_of_f64(self.content.1) * self.scale,
                        ),
                    },
                    ..Default::default()
                },
                &children,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: Point,
    ) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        for (child, &child_node) in self.children.iter_mut().zip(&self.child_nodes) {
            child.apply_layout(taffy, child_node, origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        for (from, to) in self.connection_rects() {
            let start = from.center();
            let end = to.center();
            let bend = (end.x - start.x).abs() / 2.0;
            let mut path = BezPath::new();
            path.move_to(start);
            path.curve_to(
                Point::new(start.x + bend, start.y),
                Point::new(end.x - bend, end.y),
                end,
            );
            scene.stroke(
                &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
                transform,
                theme.text_dim,
                None,
                &path,
            );
        }
        for child in &mut self.children {
            child.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        ui::widget::route_to_children(&mut self.children, &self.child_nodes, event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        self.children.iter().any(|child| child.needs_redraw())
    }

    fn mark_drawn(&mut self) {
        for child in &mut self.children {
            child.mark_drawn();
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.children
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        let mut children: Vec<&mut dyn Widget> = Vec::with_capacity(self.children.len());
        for child in &mut self.children {
            children.push(child.as_mut());
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.children {
            visitor(child.as_ref());
        }
    }

    fn name(&self) -> &'static str {
        "ChipPositionedGraph"
    }
}

/// The graph canvas in the shared zoom/pan viewport. Ctrl+Plus, Ctrl+Minus,
/// Ctrl+0, Ctrl+scroll and a pinch work as in every document; the buttons
/// above say the same steps and a Fit control shows the whole graph.
struct GraphView {
    /// The operator controls at their authored places, in content space.
    graph: PositionedGraph,
    /// The shared zoom/pan kernel: the same levels, bounds and gestures as
    /// every document, with the graph as its content.
    viewport: ViewportState,
    controls: GraphControls,
    content: (f64, f64),
    positions: Vec<(String, f64, f64)>,
    rect: Rect,
    inner_node: ui::taffy::NodeId,
    last_cursor: Point,
    dirty: bool,
    needs_relayout: bool,
}

impl GraphView {
    fn new(
        nodes: Vec<GraphCanvasNode>,
        connections: Vec<GraphCanvasConnection>,
        positions: Vec<(String, f64, f64)>,
        controls: GraphControls,
    ) -> Self {
        let width = positions
            .iter()
            .map(|(_, x, _)| x + 248.0 + 64.0)
            .fold(0.0_f64, f64::max)
            .max(320.0);
        let height = positions
            .iter()
            .map(|(_, _, y)| y + 320.0)
            .fold(0.0_f64, f64::max)
            .max(320.0);
        Self {
            graph: PositionedGraph::new(nodes, connections, (width, height)),
            viewport: ViewportState::new()
                .with_zoom_bounds(0.25, 8.0)
                .with_padding(24.0),
            controls,
            content: (width, height),
            positions,
            rect: Rect::ZERO,
            inner_node: ui::taffy::NodeId::from(0_u64),
            last_cursor: Point::ZERO,
            dirty: true,
            needs_relayout: true,
        }
    }

    /// A new view widget has a fresh viewport: put back the zoom and scroll
    /// the window kept, or fit the graph the first time it is shown.
    fn restore(&mut self) {
        if self.rect.width() <= 0.0 {
            return;
        }
        self.viewport.set_rect(self.rect);
        self.viewport
            .set_content_size(self.content.0, self.content.1);
        let mut state = self.controls.state.lock_recover();
        if !state.fitted || (state.at_fit && state.content != self.content) {
            self.viewport.reset_to_fit();
            state.fitted = true;
            state.at_fit = true;
            state.content = self.content;
        } else {
            let (zoom, x, y) = (state.zoom, state.scroll_x, state.scroll_y);
            self.viewport.set_zoom(zoom);
            self.viewport.set_scroll(x, y);
        }
        self.persist_locked(&mut state);
        drop(state);
        self.dirty = true;
    }

    fn persist_locked(&self, state: &mut GraphViewportState) {
        state.zoom = self.viewport.zoom();
        state.scroll_x = self.viewport.scroll_x();
        state.scroll_y = self.viewport.scroll_y();
        state.at_fit = state.at_fit && (state.zoom - self.viewport.fit_zoom()).abs() < 0.001;
    }

    fn persist(&self) {
        let mut state = self.controls.state.lock_recover();
        self.persist_locked(&mut state);
    }

    /// Take up a button's step or fit, and a list row's reveal. Answers
    /// whether the frame changed.
    fn apply_requests(&mut self) -> bool {
        if self.rect.width() <= 0.0 {
            return false;
        }
        let (request, reveal) = {
            let mut state = self.controls.state.lock_recover();
            (state.request.take(), state.reveal.take())
        };
        let mut changed = false;
        if let Some(request) = request {
            changed = true;
            match request {
                GraphViewRequest::Step(step) => {
                    let next = step.apply(ui::num::f32_of_f64(self.viewport.zoom()));
                    self.viewport.set_zoom(f64::from(next));
                    self.controls.state.lock_recover().at_fit = false;
                }
                GraphViewRequest::Fit => {
                    self.viewport.reset_to_fit();
                    let mut state = self.controls.state.lock_recover();
                    state.at_fit = true;
                    state.content = self.content;
                }
            }
        }
        if let Some(id) = reveal {
            if let Some((_, x, y)) = self.positions.iter().find(|(node, ..)| node == &id) {
                let zoom = self.viewport.zoom();
                let rect = self.rect;
                self.viewport.set_scroll(
                    x * zoom - rect.width() / 3.0,
                    y * zoom - rect.height() / 3.0,
                );
                let mut state = self.controls.state.lock_recover();
                state.revealed = Some(id);
                state.at_fit = false;
            }
            changed = true;
        }
        if changed {
            self.persist();
            self.dirty = true;
            self.needs_relayout = true;
        }
        changed
    }
}

impl Widget for GraphView {
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.graph.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let mut changed = self.graph.update(state, path);
        changed |= self.apply_requests();
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        let zoom = self.viewport.zoom();
        self.graph.set_scale(ui::num::f32_of_f64(zoom));
        let graph_theme = theme
            .clone()
            .with_ui_scale(theme.ui_scale * ui::num::f32_of_f64(zoom));
        self.inner_node = self.graph.build_layout(taffy, &graph_theme, node_map);
        self.needs_relayout = false;
        #[allow(
            clippy::unwrap_used,
            reason = "a fresh viewport node with one freshly built child is infallible unless the taffy arena is already corrupted"
        )]
        let outer = taffy
            .new_with_children(
                ui::taffy::Style {
                    size: ui::taffy::Size {
                        width: ui::taffy::Dimension::percent(1.0),
                        height: ui::taffy::Dimension::length(540.0),
                    },
                    ..Default::default()
                },
                &[self.inner_node],
            )
            .unwrap();
        node_map.insert(outer, std::ptr::from_mut::<dyn Widget>(self));
        outer
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: Point,
    ) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        self.restore();
        let _ = self.apply_requests();
        self.graph
            .apply_layout(taffy, self.inner_node, self.viewport.content_origin());
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let graph_theme = theme
            .clone()
            .with_ui_scale(theme.ui_scale * ui::num::f32_of_f64(self.viewport.zoom()));
        scene.push_layer(
            ui::vello::peniko::Fill::NonZero,
            ui::vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        self.graph
            .draw(scene, transform, text_renderer, &graph_theme, presence);
        scene.pop_layer();
        self.viewport.draw_scrollbars(scene, transform, theme);
        self.dirty = false;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = ctx.logical(position);
                if self.viewport.drag_scrollbar(self.last_cursor)
                    || self.viewport.update_pan(self.last_cursor)
                {
                    self.dirty = true;
                    self.needs_relayout = true;
                    *ctx.handled = true;
                    return;
                }
                if self.viewport.update_scrollbar_hover(self.last_cursor) {
                    self.dirty = true;
                }
                self.graph.handle_event(event, ctx);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.last_cursor) => {
                if self.viewport.press_scrollbar(self.last_cursor) {
                    self.dirty = true;
                    self.needs_relayout = true;
                    *ctx.handled = true;
                    return;
                }
                self.graph.handle_event(event, ctx);
                if !*ctx.handled {
                    self.viewport.begin_pan(self.last_cursor);
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.viewport.release_scrollbar() {
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
                self.graph.handle_event(event, ctx);
                if self.viewport.is_panning() {
                    self.viewport.end_pan();
                    self.dirty = true;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.rect.contains(self.last_cursor) => {
                self.viewport.on_wheel(
                    delta,
                    &ctx.modifiers.state(),
                    self.last_cursor,
                    ctx.scale_factor,
                );
                self.dirty = true;
                self.needs_relayout = true;
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed
                    && self
                        .viewport
                        .on_key(&key.logical_key, &ctx.modifiers.state()) =>
            {
                self.dirty = true;
                self.needs_relayout = true;
                *ctx.handled = true;
            }
            _ => self.graph.handle_event(event, ctx),
        }
        self.persist();
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.viewport.is_panning()
            || self.viewport.scrollbar_dragging()
            || self.graph.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.graph.mark_drawn();
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.graph]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.graph]
    }

    fn wants_pointer_capture(&self) -> bool {
        self.viewport.is_panning()
            || self.viewport.scrollbar_dragging()
            || self.graph.wants_pointer_capture()
    }

    fn cursor_shape(&self, point: Point) -> Option<ui::winit::window::CursorIcon> {
        self.graph.cursor_shape(point)
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() {
            regions.push(self.rect);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.needs_relayout)
    }

    fn name(&self) -> &'static str {
        "ChipGraphView"
    }
}

fn node_view(
    cx: &ViewCx<ChipProject>,
    node: &OperatorNode,
    graph: &ui::graph_editor::Graph,
    selection: &Selection,
    targets: &[ui::graph_editor::ConnectionTarget],
) -> GraphCanvasNode {
    let visual = node.visual();
    let displayed_shape = if node.kind == OperatorKind::MatrixWeightInput {
        format!(
            "[{}, {}]",
            node.output_width.unwrap_or(0),
            node.normalization_width
        )
    } else {
        format!("{:?}", node.shape)
    };
    let mut content = Column::new()
        .push(Box::new(effect_button(
            cx,
            &format!("chip-node-{}", node.id),
            &tr(node.kind.key()),
            &ChipEffect::InspectOperator {
                node: Some(node.id.clone()),
            },
        )))
        .push(Box::new(
            Text::new(&I18nManager::format(
                "chip-tensor-shape",
                &[("shape", displayed_shape.clone().into())],
            ))
            .with_role(TextRole::Caption),
        ));
    for port in &visual.inputs {
        // Each port says whether it feeds the operator or comes out of it:
        // what a port is for, before anyone knows the graph.
        let label = I18nManager::format(
            "chip-graph-port-input",
            &[("port", tr(&format!("chip-port-{}", port.id)).into())],
        );
        if let Some((source, out)) = &selection.output {
            let edge = Edge {
                from_node: source.clone(),
                from_port: out.clone(),
                to_node: node.id.clone(),
                to_port: port.id.clone(),
            };
            let enabled = targets
                .iter()
                .any(|target| target.node == node.id && target.port == port.id)
                && graph.clone().connect(edge.clone()).is_ok();
            content = content.push(Box::new(
                action_button(
                    cx,
                    &port_id(&node.id, &port.id, true),
                    &label,
                    connect(edge),
                )
                .with_enabled(enabled),
            ));
        } else {
            content = content.push(Box::new(effect_button(
                cx,
                &port_id(&node.id, &port.id, true),
                &label,
                &ChipEffect::InspectOperator {
                    node: Some(node.id.clone()),
                },
            )));
        }
        content = content.push(Box::new(
            Text::new(&port.data_type).with_role(TextRole::Caption),
        ));
    }
    for port in &visual.outputs {
        content = content.push(Box::new(effect_button(
            cx,
            &port_id(&node.id, &port.id, false),
            &I18nManager::format(
                "chip-graph-port-output",
                &[("port", tr(&format!("chip-port-{}", port.id)).into())],
            ),
            &ChipEffect::SelectOutput {
                node: node.id.clone(),
                port: port.id.clone(),
            },
        )));
        // An output carrying the operator's own tensor is named by the shape
        // line above; only one that differs (a projection's) says its own.
        if port.data_type != format!("f32{displayed_shape}") {
            content = content.push(Box::new(
                Text::new(&port.data_type).with_role(TextRole::Caption),
            ));
        }
    }
    GraphCanvasNode {
        x: f32::from(node.x),
        y: f32::from(node.y),
        width: 248.0,
        content: Box::new(
            Container::new()
                .with_child(Box::new(content))
                .with_background(),
        ),
    }
}

fn inspector(cx: &ViewCx<ChipProject>, node: &OperatorNode, state: &OperatorState) -> Element {
    let properties = vec![
        Property::new(tr("chip-graph-node"), node.id.clone()),
        Property::new(
            tr(if node.output_width.is_some() {
                "chip-graph-input-width"
            } else {
                "chip-graph-norm-width"
            }),
            node.normalization_width.to_string(),
        ),
        Property::new(tr("chip-graph-position"), format!("{}, {}", node.x, node.y)),
    ];
    let mut column = Column::new().push(Box::new(PropertyList::new(properties)));
    column = column.push(shape_field(cx, node));
    if let Some(field) = output_width_field(cx, node) {
        column = column.push(field);
    }
    let mut moves = Row::new();
    for (key, x, y) in [
        ("chip-graph-left", node.x.saturating_sub(32), node.y),
        (
            "chip-graph-right",
            node.x.saturating_add(32).min(8192),
            node.y,
        ),
        ("chip-graph-up", node.x, node.y.saturating_sub(32)),
        (
            "chip-graph-down",
            node.x,
            node.y.saturating_add(32).min(8192),
        ),
    ] {
        moves = moves.push(Box::new(cx.button(tr(key), move_to(&node.id, x, y))));
    }
    column = column.push(Box::new(moves));
    for edge in state
        .candidates
        .values()
        .filter(|edge| edge.to_node == node.id)
    {
        column = column.push(Box::new(cx.button(
            format!(
                "{} {}",
                tr("chip-graph-disconnect"),
                tr(&format!("chip-port-{}", edge.to_port))
            ),
            ChipProject::DISCONNECT.with(project::Disconnect {
                node: node.id.clone(),
                port: edge.to_port.clone(),
            }),
        )));
    }
    Box::new(column.push(Box::new(cx.button(
        tr("chip-graph-remove"),
        ChipProject::REMOVE_NODE.with(node.id.as_str()),
    ))))
}

fn shape_field(cx: &ViewCx<ChipProject>, node: &OperatorNode) -> Element {
    let read_node = node.id.clone();
    let edit_node = node.id.clone();
    let input = cx
        .control_with(
            TextInput::from_state,
            move |state| {
                state
                    .node(&read_node)
                    .map_or_else(String::new, |node| format!("{:?}", node.shape))
            },
            ChipProject::SET_SHAPE.map(move |value: String| project::SetShape {
                node: edit_node.clone(),
                shape: serde_json::from_str(&value).unwrap_or_default(),
            }),
        )
        .with_id(&format!("chip-operator-shape-{}", node.id));
    Box::new(Field::new(tr("chip-graph-shape"), input))
}

fn output_width_field(cx: &ViewCx<ChipProject>, node: &OperatorNode) -> Option<Element> {
    node.output_width?;
    let path = ChipProject::paths()
        .nodes
        .item(&Id::from_raw(node.id.clone()))
        .output_width;
    let input = cx
        .control_map(
            TextInput::from_state,
            path,
            |width: &Option<u32>| width.map_or_else(String::new, |width| width.to_string()),
            |value: String| value.trim().parse().ok(),
        )
        .with_id(&format!("chip-matrix-output-width-{}", node.id));
    Some(Box::new(Field::new(tr("chip-graph-output-width"), input)) as Element)
}

pub fn scenario() -> Vec<Invoke<ChipProject>> {
    let kinds = [
        ("input", OperatorKind::Input, 0, 0),
        ("rms", OperatorKind::RmsNorm, 280, 0),
        ("weight", OperatorKind::NormWeightInput, 280, 320),
        ("weighted", OperatorKind::ApplyNormWeight, 560, 0),
        ("gate", OperatorKind::GateInput, 0, 320),
        ("silu", OperatorKind::Silu, 560, 320),
        ("mul", OperatorKind::Multiply, 840, 0),
        ("output", OperatorKind::ProjectionInput, 1120, 0),
    ];
    let mut actions: Vec<_> = kinds
        .into_iter()
        .map(|(id, kind, x, y)| {
            ChipProject::ADD_NODE.with(OperatorNode {
                id: id.into(),
                kind,
                shape: if matches!(
                    kind,
                    OperatorKind::NormWeightInput | OperatorKind::MatrixWeightInput
                ) {
                    vec![8]
                } else {
                    vec![4, 8]
                },
                normalization_width: 8,
                output_width: None,
                x,
                y,
            })
        })
        .collect();
    for (from_node, from_port, to_node, to_port) in [
        ("input", "value", "rms", "value"),
        ("rms", "result", "weighted", "value"),
        ("weight", "weights", "weighted", "weights"),
        ("gate", "value", "silu", "value"),
        ("weighted", "result", "mul", "left"),
        ("silu", "result", "mul", "right"),
        ("mul", "result", "output", "value"),
    ] {
        actions.push(connect(Edge {
            from_node: from_node.into(),
            from_port: from_port.into(),
            to_node: to_node.into(),
            to_port: to_port.into(),
        }));
    }
    actions
}

pub fn present(
    effect: ChipEffect,
    cx: &EffectCx<ChipProject>,
    opened: &ui::LocalState<Option<String>>,
    selection: &ui::LocalState<Selection>,
) -> Result<Effected, String> {
    match effect {
        ChipEffect::OperatorGraph => {}
        ChipEffect::AddOperator { kind } => {
            let kind = kind_named(&kind).ok_or_else(|| tr("chip-graph-unknown-operator"))?;
            let count = cx.live().with(|state| state.nodes.len());
            cx.submit(ChipProject::ADD_NODE.with(new_node(kind, count)));
        }
        ChipEffect::InspectOperator { node } => {
            if node
                .as_ref()
                .is_some_and(|id| !cx.live().with(|state| state.node(id).is_some()))
            {
                return Err(tr("chip-graph-node-missing"));
            }
            selection.set(Selection { node, output: None });
        }
        ChipEffect::SelectOutput { node, port } => {
            let valid = cx.live().with(|state| {
                state
                    .node(&node)
                    .is_some_and(|n| n.visual().outputs.iter().any(|p| p.id == port))
            });
            if !valid {
                return Err(tr("chip-graph-port-missing"));
            }
            selection.set(Selection {
                node: Some(node.clone()),
                output: Some((node, port)),
            });
        }
        _ => return Err(tr("chip-graph-port-missing")),
    }
    opened.set(Some(crate::operator_graph::RESOURCE_ID.into()));
    Ok(Effected::done())
}
