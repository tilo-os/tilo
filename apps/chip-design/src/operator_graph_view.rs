//! Generic structural graph, rendered by the shared retained canvas.
use crate::operator_graph::{OperatorAction, OperatorKind, OperatorNode, OperatorState};
use crate::resource::{ChipProject, ProjectAction};
use crate::view::ChipEffect;
use ui::framework::{EffectCx, Effected, ViewCx};
use ui::graph_editor::{Edge, GraphEditor};
use ui::i18n::I18nManager;
use ui::tilo_resource::{ActionRef, Invoke};
use ui::tokens::TextRole;
use ui::widget::{
    Button, Column, Container, Element, Field, GraphCanvas, GraphCanvasConnection, GraphCanvasNode,
    Property, PropertyList, Row, ScrollAxis, Scrollable, Text,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub node: Option<String>,
    pub output: Option<(String, String)>,
}
fn tr(key: &str) -> String {
    I18nManager::lookup_plain(key)
}
fn invoke(edit: OperatorAction) -> Invoke<ChipProject> {
    ActionRef::<ChipProject, ProjectAction>::new(
        ui::tilo_resource::APPLY,
        &[ui::tilo_resource::APPLY_PARAM],
    )
    .with(ProjectAction::EditGraph { edit })
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

pub fn body(cx: &ViewCx<ChipProject>, state: &OperatorState, selection: &Selection) -> Element {
    let mut add = Row::new();
    for kind in OperatorKind::ALL {
        let id = ui::tilo_actions::ActionId::new().0.to_string();
        let n = u16::try_from(state.nodes.len()).unwrap_or(64);
        let node = OperatorNode {
            id,
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
        };
        add = add.push(Box::new(cx.button(
            format!("{} {}", tr("chip-graph-add"), tr(kind.key())),
            invoke(OperatorAction::AddNode { node }),
        )));
    }
    let graph = state.graph();
    let targets = selection
        .output
        .as_ref()
        .map_or_else(Vec::new, |(node, port)| {
            GraphEditor::connection_targets(&graph, node, port)
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
        .push(Box::new(add))
        .push(Box::new(cx.effect_button(
            tr("chip-graph-clear-output"),
            &ChipEffect::InspectOperator { node: None },
        )));
    if let Some((node, port)) = &selection.output {
        page = page.push(Box::new(Text::new(&format!(
            "{} {node}.{port}",
            tr("chip-graph-connect-from")
        ))));
    }
    page = page.push(Box::new(
        Container::new().with_height(540.0).with_child(Box::new(
            Scrollable::new(ScrollAxis::Both)
                .with_child(Box::new(GraphCanvas::new(nodes, connections)))
                .with_height(540.0),
        )),
    ));
    if let Some(node) = selection.node.as_ref().and_then(|id| state.nodes.get(id)) {
        page = page.push(inspector(cx, node, state));
    }
    for rejected in &state.rejected {
        page = page.push(Box::new(Text::new(&format!(
            "{} {}.{} → {}.{}: {}",
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
                &[("shape", displayed_shape.into())],
            ))
            .with_role(TextRole::Caption),
        ));
    for port in &visual.inputs {
        let label = tr(&format!("chip-port-{}", port.id));
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
                    invoke(OperatorAction::Connect { edge }),
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
            &tr(&format!("chip-port-{}", port.id)),
            &ChipEffect::SelectOutput {
                node: node.id.clone(),
                port: port.id.clone(),
            },
        )));
        content = content.push(Box::new(
            Text::new(&port.data_type).with_role(TextRole::Caption),
        ));
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
        moves = moves.push(Box::new(cx.button(
            tr(key),
            invoke(OperatorAction::MoveNode {
                node: node.id.clone(),
                x,
                y,
            }),
        )));
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
            invoke(OperatorAction::Disconnect {
                node: node.id.clone(),
                port: edge.to_port.clone(),
            }),
        )));
    }
    Box::new(column.push(Box::new(cx.button(
        tr("chip-graph-remove"),
        invoke(OperatorAction::RemoveNode {
            node: node.id.clone(),
        }),
    ))))
}

fn shape_field(cx: &ViewCx<ChipProject>, node: &OperatorNode) -> Element {
    let read_node = node.id.clone();
    let edit_node = node.id.clone();
    let action: ActionRef<ChipProject, ProjectAction> =
        ActionRef::new(ui::tilo_resource::APPLY, &[ui::tilo_resource::APPLY_PARAM]);
    let input = cx
        .input(
            move |state| {
                state
                    .operator_graph
                    .nodes
                    .get(&read_node)
                    .map_or_else(String::new, |node| format!("{:?}", node.shape))
            },
            action.map(move |value: String| ProjectAction::EditGraph {
                edit: OperatorAction::SetShape {
                    node: edit_node.clone(),
                    shape: serde_json::from_str(&value).unwrap_or_default(),
                },
            }),
        )
        .with_id(&format!("chip-operator-shape-{}", node.id));
    Box::new(Field::new(tr("chip-graph-shape"), input))
}

fn output_width_field(cx: &ViewCx<ChipProject>, node: &OperatorNode) -> Option<Element> {
    node.output_width?;
    {
        let read_node = node.id.clone();
        let edit_node = node.id.clone();
        let action: ActionRef<ChipProject, ProjectAction> =
            ActionRef::new(ui::tilo_resource::APPLY, &[ui::tilo_resource::APPLY_PARAM]);
        let input = cx
            .input(
                move |state| {
                    state
                        .operator_graph
                        .nodes
                        .get(&read_node)
                        .and_then(|node| node.output_width)
                        .map_or_else(String::new, |width| width.to_string())
                },
                action.map(move |value: String| ProjectAction::EditGraph {
                    edit: OperatorAction::SetOutputWidth {
                        node: edit_node.clone(),
                        width: value.parse().unwrap_or(0),
                    },
                }),
            )
            .with_id(&format!("chip-matrix-output-width-{}", node.id));
        Some(Box::new(Field::new(tr("chip-graph-output-width"), input)) as Element)
    }
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
            invoke(OperatorAction::AddNode {
                node: OperatorNode {
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
                },
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
        actions.push(invoke(OperatorAction::Connect {
            edge: Edge {
                from_node: from_node.into(),
                from_port: from_port.into(),
                to_node: to_node.into(),
                to_port: to_port.into(),
            },
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
        ChipEffect::InspectOperator { node } => {
            if node.as_ref().is_some_and(|id| {
                !cx.live()
                    .with(|state| state.operator_graph.nodes.contains_key(id))
            }) {
                return Err(tr("chip-graph-node-missing"));
            }
            selection.set(Selection { node, output: None });
        }
        ChipEffect::SelectOutput { node, port } => {
            let valid = cx.live().with(|state| {
                state
                    .operator_graph
                    .nodes
                    .get(&node)
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
