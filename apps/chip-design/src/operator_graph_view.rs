//! Generic structural graph, rendered by the shared retained canvas inside
//! the toolkit's zoom/pan viewport.
//!
//! The graph is a place to move around in, so it fills the window and every
//! way of seeing more of it is here: the viewport's own zoom and pan (Ctrl+
//! wheel, a pinch, Ctrl+`+`/`-`, arrows, drag), the toolbar's Zoom out, level
//! and Zoom in, and Fit — the whole design on opening. The operator list is
//! the other way in: every operator by name, so one is reachable without
//! hunting the canvas for it.
use std::cell::Cell;
use std::rc::Rc;

use crate::operator_graph::{OperatorKind, OperatorNode, OperatorState};
use crate::resource::{Project as ChipProject, project};
use crate::view::ChipEffect;
use ui::framework::{EffectCx, Effected, ViewCx};
use ui::graph_editor::{Edge, connection_targets};
use ui::i18n::I18nManager;
use ui::icons::Icon;
use ui::tilo_resource::{Id, Invoke};
use ui::tokens::TextRole;
use ui::widget::{
    Button, Column, Container, DocumentViewport, Dropdown, Element, Field, GraphCanvas,
    GraphCanvasConnection, GraphCanvasNode, ListRow, Property, PropertyList, Row, Scrollable, Text,
    TextInput, ViewportZoom,
};

/// A node's own box in the graph's coordinates: its width, and the height it
/// is given before the canvas measures the real one. Not a breakpoint: a node
/// is this size in every window.
const NODE_BOX: (f32, f32) = (248.0, 320.0);

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

/// The graph, as the window's content: the ways to change what is drawn,
/// then the canvas, filling the room the window has for it.
pub fn body(
    cx: &ViewCx<ChipProject>,
    state: &OperatorState,
    selection: &Selection,
    zoom: &ui::LocalState<ViewportZoom>,
    scroll: &Rc<Cell<(f32, f32)>>,
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

    let mut tools = Row::new().push(add_menu(cx));
    tools = tools.push(zoom_controls(cx, zoom));
    let mut page = Column::new().push(Box::new(tools)).push(Box::new(
        Text::new(&tr("chip-graph-scope")).with_role(TextRole::Caption),
    ));
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
    for rejected in &state.rejected {
        let edge = &rejected.edge;
        page = page.push(Box::new(Text::new(&I18nManager::format(
            "chip-graph-conflict-line",
            &[
                ("from_node", edge.from_node.clone().into()),
                (
                    "from_port",
                    tr(&format!("chip-port-{}", edge.from_port)).into(),
                ),
                ("to_node", edge.to_node.clone().into()),
                ("to_port", tr(&format!("chip-port-{}", edge.to_port)).into()),
                (
                    "reason",
                    tr(&format!("chip-graph-{}", rejected.reason)).into(),
                ),
            ],
        ))));
    }
    let (width, height) = extent(state);
    let mut canvas = DocumentViewport::new(Box::new(GraphCanvas::new(nodes, connections)))
        .with_zoom(zoom)
        .with_scroll(scroll)
        .with_alignment(
            ui::widget::SmallContentAlign::Start,
            ui::widget::SmallContentAlign::Start,
        );
    canvas.set_content_size(width, height);
    // The canvas takes the room the window has after the choices above it,
    // so a wider window is a wider graph rather than a wider margin.
    page = page.push(Box::new(
        Row::new()
            .with_fill_remaining_height()
            .push(Box::new(canvas)),
    ));
    Box::new(page)
}

/// The graph's extent in its own coordinates: how far its nodes reach.
fn extent(state: &OperatorState) -> (f64, f64) {
    let mut right: f32 = NODE_BOX.0;
    let mut bottom: f32 = NODE_BOX.1;
    for node in state.nodes.values() {
        right = right.max(f32::from(node.x) + NODE_BOX.0);
        bottom = bottom.max(f32::from(node.y) + NODE_BOX.1);
    }
    (f64::from(right), f64::from(bottom))
}

/// Zoom out, the level the canvas shows, zoom in, and fit the whole graph to
/// the room it has. The level is a word while the whole graph is visible and
/// a percentage once it is not, and it follows a wheel or a pinch, because it
/// reads the same state the canvas answers.
fn zoom_controls(cx: &ViewCx<ChipProject>, zoom: &ui::LocalState<ViewportZoom>) -> Element {
    let step = |id: &str, icon: Icon, label: &str, name: &str| {
        cx.actions().bind_effect_invocation(
            Button::icon_only(id, icon, label),
            &ChipEffect::ZoomGraph {
                step: name.to_owned(),
            },
        )
    };
    let level = Text::from_state(zoom, |shown| {
        shown.percent().map_or_else(
            || tr("chip-graph-zoom-fitted"),
            |percent| {
                I18nManager::format("chip-graph-zoom-percent", &[("percent", percent.into())])
            },
        )
    });
    Box::new(
        Row::new()
            .push(Box::new(step(
                "chip-graph-zoom-out",
                Icon::Minus,
                &tr("chip-graph-zoom-out"),
                "out",
            )))
            .push(Box::new(level))
            .push(Box::new(step(
                "chip-graph-zoom-in",
                Icon::Plus,
                &tr("chip-graph-zoom-in"),
                "in",
            )))
            .push(Box::new(cx.effect_button(
                tr("chip-graph-zoom-fit"),
                &ChipEffect::ZoomGraph {
                    step: "fit".to_owned(),
                },
            ))),
    )
}

/// Every operator, by name, as the list beside the canvas: one is reachable
/// from here without hunting for it on the canvas, and choosing one shows it
/// in the inspector.
pub fn operators(
    cx: &ViewCx<ChipProject>,
    state: &OperatorState,
    selection: &Selection,
) -> Element {
    let mut list = Column::new().with_intrinsic_height();
    if state.nodes.is_empty() {
        list = list.push(Box::new(
            Text::new(&tr("chip-graph-no-operators"))
                .with_role(TextRole::Caption)
                .dimmed(),
        ));
    }
    for node in state.nodes.values() {
        let row = ListRow::new(format!("chip-operator-{}", node.id), tr(node.kind.key()))
            .secondary(I18nManager::format(
                "chip-graph-node-at",
                &[("x", node.x.into()), ("y", node.y.into())],
            ))
            .selected(selection.node.as_deref() == Some(node.id.as_str()));
        list = list.push(Box::new(cx.actions().bind_effect_invocation(
            row,
            &ChipEffect::InspectOperator {
                node: Some(node.id.clone()),
            },
        )));
    }
    Box::new(Scrollable::new_vertical().with_child(Box::new(list)))
}

/// One operator's own facts and edits, beside the canvas when the window has
/// the room and over it when it does not.
pub fn details(
    cx: &ViewCx<ChipProject>,
    state: &OperatorState,
    selection: &Selection,
) -> Option<Element> {
    let node = selection.node.as_ref().and_then(|id| state.nodes.get(id))?;
    Some(Box::new(
        Scrollable::new_vertical().with_child(inspector(cx, node, state)),
    ))
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
    zoom: &ui::LocalState<ViewportZoom>,
    details: &ui::LocalState<bool>,
) -> Result<Effected, String> {
    match effect {
        ChipEffect::ZoomGraph { step } => {
            let level = zoom.with(|shown| *shown);
            let level = match step.as_str() {
                "fit" => ViewportZoom::Fit,
                "in" | "out" => {
                    // Step from the factor on screen, which a wheel or a
                    // pinch may have moved since the buttons were drawn.
                    let factor = match level {
                        ViewportZoom::Factor(factor) => factor,
                        ViewportZoom::Fit => 1.0,
                    };
                    let step = if step == "in" {
                        ui::content_zoom::ZoomStep::In
                    } else {
                        ui::content_zoom::ZoomStep::Out
                    };
                    ViewportZoom::Factor(
                        step.apply(
                            #[allow(
                                clippy::cast_possible_truncation,
                                reason = "a zoom factor is a small positive float"
                            )]
                            {
                                factor as f32
                            },
                        )
                        .into(),
                    )
                }
                _ => return Err(tr("chip-graph-zoom-missing")),
            };
            zoom.set(level);
            return Ok(Effected::done());
        }
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
            // Choosing an operator is asking to see it: the pane holding its
            // facts comes with the choice, and closing the pane keeps it.
            details.set(node.is_some());
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
