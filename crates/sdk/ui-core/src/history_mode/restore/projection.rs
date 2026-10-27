//! Object-id-independent, conflict-aware semantic values used for exact checks.
use super::{RestoreError, RestoreValueType, ScalarType};
use automerge::{AutoCommit, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value, marks::Mark};
use std::collections::BTreeMap;
use tilo_sort::CompactSort as _;

pub(super) type NodeId = usize;
pub(super) type Alternatives = Vec<NodeId>;
pub(super) type Properties = BTreeMap<String, Alternatives>;

/// Immutable semantic content backed by a flat private arena. Child indices are
/// meaningful only inside this projection and are never CRDT object identities.
#[derive(Clone, Debug)]
pub struct ContentProjection(pub(super) Properties, pub(super) Vec<Node>);

impl PartialEq for ContentProjection {
    fn eq(&self, other: &Self) -> bool {
        properties_equal(&self.0, &other.0, &self.1, &other.1)
    }
}
impl Eq for ContentProjection {}

#[derive(Clone, Debug)]
pub(super) struct ExactScalar(pub ScalarValue);
impl PartialEq for ExactScalar {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (ScalarValue::F64(a), ScalarValue::F64(b)) => a.to_bits() == b.to_bits(),
            (a, b) => a == b,
        }
    }
}
impl Eq for ExactScalar {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SemanticMark {
    pub name: String,
    pub start: usize,
    pub end: usize,
    pub value: ExactScalar,
}
impl SemanticMark {
    pub fn to_mark(&self) -> Mark {
        Mark::new(
            self.name.clone(),
            self.value.0.clone(),
            self.start,
            self.end,
        )
    }
}

#[derive(Clone, Debug)]
pub(super) enum Node {
    Protected(ObjId, NodeId),
    Scalar(ExactScalar),
    Map(ObjType, Properties),
    List(Vec<Alternatives>, Vec<SemanticMark>),
    Text {
        text: String,
        marks: Vec<SemanticMark>,
        protected_elements: Option<Vec<Alternatives>>,
    },
}

impl ContentProjection {
    pub(super) fn node(&self, id: NodeId) -> Result<&Node, RestoreError> {
        self.1
            .get(id)
            .ok_or(RestoreError::ContentPostconditionMismatch)
    }

    pub(super) fn value_type(&self, mut id: NodeId) -> Result<RestoreValueType, RestoreError> {
        loop {
            match self.node(id)? {
                Node::Protected(_, child) => id = *child,
                Node::Map(kind, _) => return Ok(RestoreValueType::Object(*kind)),
                Node::List(..) => return Ok(RestoreValueType::Object(ObjType::List)),
                Node::Text { .. } => return Ok(RestoreValueType::Object(ObjType::Text)),
                Node::Scalar(value) => {
                    return Ok(RestoreValueType::Scalar(match value.0 {
                        ScalarValue::Bytes(_) => ScalarType::Bytes,
                        ScalarValue::Str(_) => ScalarType::String,
                        ScalarValue::Int(_) => ScalarType::Int,
                        ScalarValue::Uint(_) => ScalarType::Uint,
                        ScalarValue::F64(_) => ScalarType::Float,
                        ScalarValue::Counter(_) => ScalarType::Counter,
                        ScalarValue::Timestamp(_) => ScalarType::Timestamp,
                        ScalarValue::Boolean(_) => ScalarType::Boolean,
                        ScalarValue::Null => ScalarType::Null,
                        ScalarValue::Unknown { .. } => ScalarType::Unknown,
                    }));
                }
            }
        }
    }

    pub(super) fn alternatives_equal(
        &self,
        left: &[NodeId],
        other: &Self,
        right: &[NodeId],
    ) -> bool {
        alternatives_equal(left, right, &self.1, &other.1)
    }

    #[cfg(test)]
    pub(super) fn push_test_node(&mut self, node: Node) -> NodeId {
        let id = self.1.len();
        self.1.push(node);
        id
    }
}

fn alternatives_equal(
    left: &[NodeId],
    right: &[NodeId],
    left_nodes: &[Node],
    right_nodes: &[Node],
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut pending: Vec<_> = left.iter().copied().zip(right.iter().copied()).collect();
    nodes_equal(&mut pending, left_nodes, right_nodes)
}

fn properties_equal(
    left: &Properties,
    right: &Properties,
    left_nodes: &[Node],
    right_nodes: &[Node],
) -> bool {
    let mut pending = Vec::new();
    properties_match(left, right, &mut pending)
        && nodes_equal(&mut pending, left_nodes, right_nodes)
}

fn properties_match(
    left: &Properties,
    right: &Properties,
    pending: &mut Vec<(NodeId, NodeId)>,
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    for ((left_key, left_values), (right_key, right_values)) in left.iter().zip(right) {
        if left_key != right_key || left_values.len() != right_values.len() {
            return false;
        }
        pending.extend(
            left_values
                .iter()
                .copied()
                .zip(right_values.iter().copied()),
        );
    }
    true
}

fn elements_match(
    left: &[Alternatives],
    right: &[Alternatives],
    pending: &mut Vec<(NodeId, NodeId)>,
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    for (left_values, right_values) in left.iter().zip(right) {
        if left_values.len() != right_values.len() {
            return false;
        }
        pending.extend(
            left_values
                .iter()
                .copied()
                .zip(right_values.iter().copied()),
        );
    }
    true
}

fn nodes_equal(
    pending: &mut Vec<(NodeId, NodeId)>,
    left_nodes: &[Node],
    right_nodes: &[Node],
) -> bool {
    while let Some((left, right)) = pending.pop() {
        let (Some(left), Some(right)) = (left_nodes.get(left), right_nodes.get(right)) else {
            return false;
        };
        match (left, right) {
            (Node::Protected(left_id, left), Node::Protected(right_id, right)) => {
                if left_id != right_id {
                    return false;
                }
                pending.push((*left, *right));
            }
            (Node::Scalar(left), Node::Scalar(right)) if left == right => {}
            (Node::Map(left_kind, left), Node::Map(right_kind, right)) => {
                if left_kind != right_kind || !properties_match(left, right, pending) {
                    return false;
                }
            }
            (Node::List(left, left_marks), Node::List(right, right_marks)) => {
                if left_marks != right_marks || !elements_match(left, right, pending) {
                    return false;
                }
            }
            (
                Node::Text {
                    text: left_text,
                    marks: left_marks,
                    protected_elements: left_elements,
                },
                Node::Text {
                    text: right_text,
                    marks: right_marks,
                    protected_elements: right_elements,
                },
            ) => {
                if left_text != right_text || left_marks != right_marks {
                    return false;
                }
                match (left_elements, right_elements) {
                    (None, None) => {}
                    (Some(left), Some(right)) if elements_match(left, right, pending) => {}
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

pub(super) struct ProjectionBuilder {
    nodes: Vec<Option<Node>>,
}

impl ProjectionBuilder {
    pub(super) fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub(super) fn push(&mut self, node: Node) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Some(node));
        id
    }

    fn reserve(&mut self) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(None);
        id
    }

    fn finish_node(&mut self, id: NodeId, node: Node) -> Result<(), RestoreError> {
        let slot = self
            .nodes
            .get_mut(id)
            .ok_or(RestoreError::ContentPostconditionMismatch)?;
        if slot.replace(node).is_some() {
            return Err(RestoreError::ContentPostconditionMismatch);
        }
        Ok(())
    }

    pub(super) fn node(&self, id: NodeId) -> Result<&Node, RestoreError> {
        self.nodes
            .get(id)
            .and_then(Option::as_ref)
            .ok_or(RestoreError::ContentPostconditionMismatch)
    }

    pub(super) fn value_type(&self, id: NodeId) -> Result<RestoreValueType, RestoreError> {
        match self.node(id)? {
            Node::Map(kind, _) => Ok(RestoreValueType::Object(*kind)),
            Node::List(..) => Ok(RestoreValueType::Object(ObjType::List)),
            Node::Text { .. } => Ok(RestoreValueType::Object(ObjType::Text)),
            Node::Scalar(value) => Ok(RestoreValueType::Scalar(match value.0 {
                ScalarValue::Bytes(_) => ScalarType::Bytes,
                ScalarValue::Str(_) => ScalarType::String,
                ScalarValue::Int(_) => ScalarType::Int,
                ScalarValue::Uint(_) => ScalarType::Uint,
                ScalarValue::F64(_) => ScalarType::Float,
                ScalarValue::Counter(_) => ScalarType::Counter,
                ScalarValue::Timestamp(_) => ScalarType::Timestamp,
                ScalarValue::Boolean(_) => ScalarType::Boolean,
                ScalarValue::Null => ScalarType::Null,
                ScalarValue::Unknown { .. } => ScalarType::Unknown,
            })),
            Node::Protected(..) => Err(RestoreError::ContentPostconditionMismatch),
        }
    }

    pub(super) fn build(self, roots: Properties) -> Result<ContentProjection, RestoreError> {
        let nodes = self
            .nodes
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or(RestoreError::ContentPostconditionMismatch)?;
        Ok(ContentProjection(roots, nodes))
    }
}

struct ObjectTask {
    slot: NodeId,
    object: ObjId,
    kind: ObjType,
    path: String,
    is_content: bool,
    depth: usize,
}

pub(super) fn property(
    builder: &mut ProjectionBuilder,
    doc: &AutoCommit,
    object: &ObjId,
    prop: Prop,
    path: &str,
    is_content: bool,
    depth: usize,
) -> Result<Alternatives, RestoreError> {
    let mut tasks = Vec::new();
    let result = schedule_property(
        builder,
        doc,
        object,
        prop,
        path.into(),
        is_content,
        depth,
        &mut tasks,
    )?;
    while let Some(task) = tasks.pop() {
        build_object(builder, doc, task, &mut tasks)?;
    }
    Ok(result)
}

fn schedule_property(
    builder: &mut ProjectionBuilder,
    doc: &AutoCommit,
    object: &ObjId,
    prop: Prop,
    path: String,
    is_content: bool,
    depth: usize,
    tasks: &mut Vec<ObjectTask>,
) -> Result<Alternatives, RestoreError> {
    if depth > 256 {
        return Err(RestoreError::UnsupportedContent(format!(
            "{path}: nesting exceeds 256"
        )));
    }
    let values = doc
        .get_all(object, prop)
        .map_err(|err| RestoreError::read(&path, &err))?;
    if is_content && values.len() > 1 {
        return Err(RestoreError::UnsupportedConflict(path));
    }
    let mut result = Vec::with_capacity(values.len());
    for (value, object_id) in values {
        let inner = match value {
            Value::Scalar(value) => builder.push(Node::Scalar(ExactScalar(value.into_owned()))),
            Value::Object(kind) => {
                let slot = builder.reserve();
                tasks.push(ObjectTask {
                    slot,
                    object: object_id.clone(),
                    kind,
                    path: path.clone(),
                    is_content,
                    depth,
                });
                slot
            }
        };
        result.push(if is_content {
            inner
        } else {
            builder.push(Node::Protected(object_id, inner))
        });
    }
    Ok(result)
}

fn build_object(
    builder: &mut ProjectionBuilder,
    doc: &AutoCommit,
    task: ObjectTask,
    tasks: &mut Vec<ObjectTask>,
) -> Result<(), RestoreError> {
    let ObjectTask {
        slot,
        object,
        kind,
        path,
        is_content,
        depth,
    } = task;
    match kind {
        ObjType::Map | ObjType::Table => {
            let mut properties = BTreeMap::new();
            for key in doc.keys(&object) {
                let child_path = format!("{path}/{key}");
                let values = schedule_property(
                    builder,
                    doc,
                    &object,
                    key.clone().into(),
                    child_path,
                    is_content,
                    depth.saturating_add(1),
                    tasks,
                )?;
                properties.insert(key, values);
            }
            builder.finish_node(slot, Node::Map(kind, properties))
        }
        ObjType::List | ObjType::Text => {
            let mut elements = Vec::new();
            for index in 0..doc.length(&object) {
                let child_path = format!("{path}/{index}");
                let values = schedule_property(
                    builder,
                    doc,
                    &object,
                    index.into(),
                    child_path.clone(),
                    is_content,
                    depth.saturating_add(1),
                    tasks,
                )?;
                if is_content && kind == ObjType::Text {
                    let source_values = doc
                        .get_all(&object, index)
                        .map_err(|err| RestoreError::read(&child_path, &err))?;
                    if !matches!(
                        source_values.as_slice(),
                        [(Value::Scalar(value), _)] if matches!(value.as_ref(), ScalarValue::Str(_))
                    ) {
                        return Err(RestoreError::UnsupportedContent(format!(
                            "{child_path}: rich-text blocks and non-string text elements are unsupported"
                        )));
                    }
                }
                elements.push(values);
            }
            let marks = marks(doc, &object, &path)?;
            if kind == ObjType::List {
                builder.finish_node(slot, Node::List(elements, marks))
            } else {
                builder.finish_node(
                    slot,
                    Node::Text {
                        text: doc
                            .text(&object)
                            .map_err(|err| RestoreError::read(&path, &err))?,
                        marks,
                        protected_elements: (!is_content).then_some(elements),
                    },
                )
            }
        }
    }
}

fn marks(doc: &AutoCommit, id: &ObjId, path: &str) -> Result<Vec<SemanticMark>, RestoreError> {
    let mut marks: Vec<_> = doc
        .marks(id)
        .map_err(|err| RestoreError::read(path, &err))?
        .into_iter()
        .map(|mark| SemanticMark {
            name: mark.name().into(),
            start: mark.start,
            end: mark.end,
            value: ExactScalar(mark.value().clone()),
        })
        .collect();
    marks.compact_sort_by(|a, b| (&a.name, a.start, a.end).cmp(&(&b.name, b.start, b.end)));
    Ok(marks)
}
