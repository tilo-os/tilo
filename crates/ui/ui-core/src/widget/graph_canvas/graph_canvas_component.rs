//! [`GraphCanvas`] as a described component: nodes that are views of their own, and the connections
//! drawn between them as [`Connection`] values.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use tilo_ui_base::{component::Component, component::Described, component::PropType, component::Type, component::Value, component::case_field, component::case_field_description};
use tilo_ui_base::{widget::Element, widget::Spacer};

use super::{GraphCanvas, GraphCanvasConnection, GraphCanvasNode};

/// One connection of a graph: from the node or control named `from` to the one named `to`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Connection {
    pub from: String,
    pub to: String,
}

impl PropType for Connection {
    fn ty() -> Type {
        Type::Cases {
            name: "Connection".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
        };
        if case != "connection" {
            return Err(format!(
                "a graph's connection is a `connection`, not `{case}`"
            ));
        }
        let end = |name: &str| -> Result<String, String> {
            case_field(fields, name)?.ok_or_else(|| format!("`connection` needs `{name}`, a node"))
        };
        Ok(Self {
            from: end("from")?,
            to: end("to")?,
        })
    }
}

/// The description of [`Connection`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn connection_description() -> EnumDescription {
    EnumDescription {
        name: "Connection".to_owned(),
        doc: "One connection of a graph, between two of its nodes.".to_owned(),
        cases: vec![CaseDescription {
            name: "connection".to_owned(),
            doc: "From the node (or a control in one) named `from` to the one named `to`."
                .to_owned(),
            fields: vec![
                case_field_description("from", Type::Text, None, "Where it starts."),
                case_field_description("to", Type::Text, None, "Where it ends."),
            ],
        }],
    }
}

/// Nodes that are views of their own, placed where each says (or in rows when none says), and
/// the connections between them drawn under them: a pipeline, a flow, the parts of a system.
#[derive(Component, Default)]
#[component(name = "GraphCanvas", role = "Group", label = "title")]
pub struct GraphCanvasProps {
    /// What the graph is called: shown over it, as every chart's title is, and its name for
    /// assistive technology.
    pub title: Option<String>,
    /// The nodes, in order.
    #[component(part(default))]
    pub nodes: Vec<GraphNodeProps>,
    /// The connections drawn between nodes: `.connection(from: "fetch", to: "build")`, each end a
    /// node's id or the test id of a control inside one.
    pub connections: Vec<Connection>,
    /// How many nodes stand side by side when the nodes give no places: 3 unless said.
    pub columns: Option<i64>,
    /// How wide each node is when the nodes give no places, in logical pixels: 200 unless said.
    pub column_width: Option<f64>,
}

/// One node of a graph: its id, which connections name, where it stands (in logical pixels from
/// the graph's corner), and the view it is.
#[derive(Component, Default)]
#[component(name = "GraphNode", role = "Group")]
pub struct GraphNodeProps {
    /// What connections call it: a name for the view to refer to, never shown or translated.
    #[component(required, positional, data)]
    pub id: String,
    /// How far from the graph's leading edge it stands; with `y`, a place of its own.
    pub x: Option<f64>,
    /// How far from the graph's top it stands.
    pub y: Option<f64>,
    /// How wide it is: 200 unless said.
    pub width: Option<f64>,
    /// What the node shows.
    #[component(part(default))]
    pub content: Option<Element>,
}

tilo_ui_base::component::part_type!(GraphNodeProps);

impl Described for GraphCanvas {
    type Props = GraphCanvasProps;

    #[expect(
        clippy::cast_possible_truncation,
        reason = "places and widths in logical pixels, well inside f32"
    )]
    fn from_props(_key: &str, props: GraphCanvasProps) -> Self {
        let layout = layout_of(&props);
        let connections = props
            .connections
            .into_iter()
            .map(|c| GraphCanvasConnection {
                from: c.from,
                to: c.to,
            })
            .collect();
        let placed = props
            .nodes
            .iter()
            .all(|node| node.x.is_some() && node.y.is_some());
        let ids: Vec<String> = props.nodes.iter().map(|node| node.id.clone()).collect();
        let content = |node: &mut GraphNodeProps| -> Element {
            node.content
                .take()
                .unwrap_or_else(|| Box::new(Spacer::token(tilo_ui_base::tokens::SpacingStep::S0)))
        };
        let canvas = if placed && !props.nodes.is_empty() {
            GraphCanvas::new(
                props
                    .nodes
                    .into_iter()
                    .map(|mut node| GraphCanvasNode {
                        x: node.x.unwrap_or_default() as f32,
                        y: node.y.unwrap_or_default() as f32,
                        width: node.width.unwrap_or(200.0) as f32,
                        content: content(&mut node),
                    })
                    .collect(),
                connections,
            )
        } else {
            GraphCanvas::grid(
                props
                    .nodes
                    .into_iter()
                    .map(|mut node| content(&mut node))
                    .collect(),
                connections,
                props.column_width.unwrap_or(200.0) as f32,
                u16::try_from(props.columns.unwrap_or(3).clamp(1, 64)).unwrap_or(3),
            )
        };
        let mut canvas = canvas.with_title(props.title);
        canvas.node_ids = ids;
        canvas.layout = layout;
        canvas
    }

    /// New connections are taken where the graph stands; nodes that are not the ones it holds,
    /// that stand elsewhere now, or a new title build it again.
    fn set_props(&mut self, props: GraphCanvasProps) -> bool {
        let ids: Vec<String> = props.nodes.iter().map(|node| node.id.clone()).collect();
        if ids != self.node_ids || layout_of(&props) != self.layout || props.title != self.title {
            return false;
        }
        self.connections = props
            .connections
            .into_iter()
            .map(|c| GraphCanvasConnection {
                from: c.from,
                to: c.to,
            })
            .collect();
        true
    }
}

/// Where a graph's nodes stand, as its props say: each node's place and width, and the grid's
/// columns, in a form a change of them can be told by.
fn layout_of(props: &GraphCanvasProps) -> String {
    let nodes: Vec<String> = props
        .nodes
        .iter()
        .map(|node| format!("{:?},{:?},{:?}", node.x, node.y, node.width))
        .collect();
    format!(
        "{};{:?};{:?}",
        nodes.join("|"),
        props.columns,
        props.column_width
    )
}
