//! Data shared by the chart widgets: valued items, hierarchy parts, graph nodes and the weighted
//! links between them. `id` survives updates and is what activation reports.

use std::collections::HashMap;

use tilo_ui_graph::component::{CaseDescription, EnumDescription};
use vello::peniko::Color;

use tilo_ui_base::{
    component::PropType, component::Type, component::Value, component::case_field,
    component::case_field_description,
};

#[derive(Clone, Debug)]
pub struct ChartItem {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl ChartItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Debug)]
pub struct ChartNode {
    pub id: String,
    pub label: String,
    pub color: Option<Color>,
    pub detail: String,
}

impl ChartNode {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Debug)]
pub struct ChartLink {
    pub id: String,
    pub source: String,
    pub target: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl ChartLink {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        target: impl Into<String>,
        value: f64,
    ) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            value,
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

impl ChartLink {
    pub(crate) fn flows(&self) -> bool {
        self.value.is_finite() && self.value > 0.0
    }
}

/// One part of a hierarchy a chart draws: what it is, which part it belongs to, and how much of
/// the whole it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartPart {
    /// What its parts name as their parent; its label unless it gives one.
    pub id: String,
    /// The id of the part it belongs to; empty at the top level.
    pub parent: String,
    pub label: String,
    /// How much of the whole it is; zero or less in a part that has parts of its own, which is
    /// then as large as everything under it.
    pub value: f64,
}

impl ChartPart {
    /// What the chart knows it by: its id, or its label when it names none.
    pub fn known_as(&self) -> &str {
        if self.id.is_empty() {
            &self.label
        } else {
            &self.id
        }
    }
}

/// A part of a hierarchy and the parts under it.
#[derive(Debug)]
pub struct ChartBranch<'a> {
    pub part: &'a ChartPart,
    pub children: Vec<ChartBranch<'a>>,
}

impl ChartBranch<'_> {
    /// How much the branch is worth: its own value, or everything under it when it gives none.
    #[must_use]
    pub fn value(&self) -> f64 {
        let children = self.children.iter().map(Self::value).sum::<f64>();
        if self.part.value.is_finite() && self.part.value > 0.0 {
            self.part.value.max(children)
        } else {
            children
        }
    }
}

/// The top-level parts of `parts`, each with the parts under it, in the order they are given; a
/// part standing at the top level when it names no parent, names one that is not there, or its
/// parents lead back to it.
#[must_use]
pub fn hierarchy(parts: &[ChartPart]) -> Vec<ChartBranch<'_>> {
    let mut lookup: HashMap<&str, usize> = HashMap::new();
    for (at, part) in parts.iter().enumerate() {
        if !part.id.is_empty() {
            // The first part of an id keeps it, so a repeated id adds no second parent.
            lookup.entry(part.id.as_str()).or_insert(at);
        }
    }
    let mut parent = parts
        .iter()
        .enumerate()
        .map(|(at, part)| {
            lookup
                .get(part.parent.as_str())
                .copied()
                .filter(|&up| up != at)
        })
        .collect::<Vec<_>>();
    // A loop has no top to hang from: the part that closes it stands at the top level.
    for at in 0..parts.len() {
        let mut seen = vec![at];
        let mut walk = parent.get(at).copied().flatten();
        while let Some(up) = walk {
            if seen.contains(&up) {
                if let Some(slot) = parent.get_mut(at) {
                    *slot = None;
                }
                break;
            }
            seen.push(up);
            walk = parent.get(up).copied().flatten();
        }
    }
    fn branch<'a>(
        parts: &'a [ChartPart],
        parent: &[Option<usize>],
        at: usize,
    ) -> Option<ChartBranch<'a>> {
        let part = parts.get(at)?;
        let children = (0..parts.len())
            .filter(|child| parent.get(*child).copied().flatten() == Some(at))
            .filter_map(|child| branch(parts, parent, child))
            .collect();
        Some(ChartBranch { part, children })
    }
    (0..parts.len())
        .filter(|at| parent.get(*at).copied().flatten().is_none())
        .filter_map(|at| branch(parts, &parent, at))
        .collect()
}

impl PropType for ChartPart {
    fn ty() -> Type {
        Type::Cases {
            name: "Part".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
        };
        if case != "part" {
            return Err(format!("a hierarchy's part is a `part`, not `{case}`"));
        }
        Ok(Self {
            id: case_field(fields, "id")?.unwrap_or_default(),
            parent: case_field(fields, "parent")?.unwrap_or_default(),
            label: case_field(fields, "label")?.ok_or("`part` needs `label`, what it is called")?,
            value: case_field(fields, "value")?
                .ok_or("`part` needs `value`, how much of the whole it is")?,
        })
    }
}

/// The description of [`ChartPart`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn part_description() -> EnumDescription {
    EnumDescription {
        name: "Part".to_owned(),
        doc: "One part of a hierarchy: what it is, the part it belongs to, and how much of the \
              whole it is."
            .to_owned(),
        cases: vec![CaseDescription {
            name: "part".to_owned(),
            doc: "A part called `label`, under the part called `parent`, worth `value`.".to_owned(),
            fields: vec![
                case_field_description(
                    "id",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "What its own parts call it as their parent; its label unless said.",
                ),
                case_field_description(
                    "parent",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "The id of the part it belongs to; nothing when it stands at the top level.",
                ),
                case_field_description(
                    "label",
                    Type::Text,
                    None,
                    "What it is called, on the chart and in its readout.",
                ),
                case_field_description(
                    "value",
                    Type::Float,
                    None,
                    "How much of the whole it is; a part with parts of its own is as large as \
                     everything under it.",
                ),
            ],
        }],
    }
}

impl PropType for ChartNode {
    fn ty() -> Type {
        Type::Cases {
            name: "FlowNode".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
        };
        if case != "node" {
            return Err(format!("a graph's node is a `node`, not `{case}`"));
        }
        let label = case_field::<String>(fields, "label")?
            .ok_or("`node` needs `label`, what it is called")?;
        let id = case_field::<String>(fields, "id")?.unwrap_or_else(|| label.clone());
        Ok(Self::new(id, label))
    }
}

/// The description of [`ChartNode`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn node_description() -> EnumDescription {
    EnumDescription {
        name: "FlowNode".to_owned(),
        doc: "One node of a flow or a web of relationships: a stage, a team, a source.".to_owned(),
        cases: vec![CaseDescription {
            name: "node".to_owned(),
            doc: "A node named `label`, called `id` by the links that start or end at it."
                .to_owned(),
            fields: vec![
                case_field_description(
                    "id",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "What the links call it; its label unless said.",
                ),
                case_field_description("label", Type::Text, None, "What it is called."),
            ],
        }],
    }
}

impl PropType for ChartLink {
    fn ty() -> Type {
        Type::Cases {
            name: "FlowLink".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
        };
        if case != "link" {
            return Err(format!("a graph's link is a `link`, not `{case}`"));
        }
        let end = |name: &str| -> Result<String, String> {
            case_field::<String>(fields, name)?
                .ok_or_else(|| format!("`link` needs `{name}`, the id of a node"))
        };
        let (from, to) = (end("from")?, end("to")?);
        let value = case_field(fields, "value")?
            .ok_or("`link` needs `value`, how much moves between them")?;
        Ok(Self::new(format!("{from}\u{2192}{to}"), from, to, value))
    }
}

/// The description of [`ChartLink`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn link_description() -> EnumDescription {
    EnumDescription {
        name: "FlowLink".to_owned(),
        doc: "One weighted link of a graph: how much moves or crosses between two nodes."
            .to_owned(),
        cases: vec![CaseDescription {
            name: "link".to_owned(),
            doc: "From the node called `from` to the node called `to`, worth `value`.".to_owned(),
            fields: vec![
                case_field_description(
                    "from",
                    Type::Text,
                    None,
                    "The id of the node it starts at.",
                ),
                case_field_description("to", Type::Text, None, "The id of the node it ends at."),
                case_field_description(
                    "value",
                    Type::Float,
                    None,
                    "How much moves between them; nothing at or below zero is drawn.",
                ),
            ],
        }],
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GraphSelection {
    Node(String),
    Link(String),
}

/// The accessible items of a graph chart: every node, then every flowing
/// link, described by `describe` unless it has a detail.
pub(crate) fn graph_items(
    nodes: &[ChartNode],
    links: &[ChartLink],
    describe: impl Fn(f64) -> String,
) -> Vec<crate::widget::PlotItem<GraphSelection>> {
    use crate::widget::PlotItem;
    nodes
        .iter()
        .map(|node| {
            PlotItem::new(
                format!("node:{}", node.id),
                &node.label,
                &node.detail,
                GraphSelection::Node(node.id.clone()),
            )
        })
        .chain(links.iter().filter(|link| link.flows()).map(|link| {
            PlotItem::new(
                format!("link:{}", link.id),
                format!("{} to {}", link.source, link.target),
                if link.detail.is_empty() {
                    describe(link.value)
                } else {
                    link.detail.clone()
                },
                GraphSelection::Link(link.id.clone()),
            )
        }))
        .collect()
}
