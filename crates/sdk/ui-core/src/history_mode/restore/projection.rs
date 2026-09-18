//! Object-id-independent, conflict-aware semantic values used for exact checks.
use super::{RestoreError, RestoreValueType, ScalarType};
use automerge::{AutoCommit, ObjId, ObjType, Prop, ReadDoc, ScalarValue, Value, marks::Mark};
use std::collections::BTreeMap;

pub(super) type Properties = BTreeMap<String, Vec<Node>>;

/// Immutable semantic content. Equality includes scalar types, floating-point
/// bits, list order, text and current mark ranges/values, but not internal `ObjIds`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentProjection(pub(super) Properties);

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Node {
    Protected(ObjId, Box<Node>),
    Scalar(ExactScalar),
    Map(ObjType, Properties),
    List(Vec<Vec<Node>>, Vec<SemanticMark>),
    Text {
        text: String,
        marks: Vec<SemanticMark>,
        // Non-content projection retains every alternative, including blocks.
        // Content text is defined by its string and marks after unsupported
        // elements and conflicts have been ruled out.
        protected_elements: Option<Vec<Vec<Node>>>,
    },
}
impl Node {
    pub fn value_type(&self) -> RestoreValueType {
        match self {
            Self::Protected(_, node) => node.value_type(),
            Self::Map(kind, _) => RestoreValueType::Object(*kind),
            Self::List(..) => RestoreValueType::Object(ObjType::List),
            Self::Text { .. } => RestoreValueType::Object(ObjType::Text),
            Self::Scalar(value) => RestoreValueType::Scalar(match value.0 {
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
            }),
        }
    }
}

pub(super) fn property(
    doc: &AutoCommit,
    object: &ObjId,
    prop: Prop,
    path: &str,
    is_content: bool,
    depth: usize,
) -> Result<Vec<Node>, RestoreError> {
    if depth > 256 {
        return Err(RestoreError::UnsupportedContent(format!(
            "{path}: nesting exceeds 256"
        )));
    }
    let values = doc
        .get_all(object, prop)
        .map_err(|err| RestoreError::read(path, &err))?;
    if is_content && values.len() > 1 {
        return Err(RestoreError::UnsupportedConflict(path.into()));
    }
    values
        .into_iter()
        .map(|(value, id)| {
            let node = node(doc, &id, value, path, is_content, depth)?;
            Ok(if is_content {
                node
            } else {
                Node::Protected(id, Box::new(node))
            })
        })
        .collect()
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
    marks.sort_by(|a, b| (&a.name, a.start, a.end).cmp(&(&b.name, b.start, b.end)));
    Ok(marks)
}

fn node(
    doc: &AutoCommit,
    id: &ObjId,
    value: Value<'_>,
    path: &str,
    is_content: bool,
    depth: usize,
) -> Result<Node, RestoreError> {
    match value {
        Value::Scalar(value) => Ok(Node::Scalar(ExactScalar(value.into_owned()))),
        Value::Object(kind @ (ObjType::Map | ObjType::Table)) => {
            let mut properties = BTreeMap::new();
            for key in doc.keys(id) {
                let child_path = format!("{path}/{key}");
                properties.insert(
                    key.clone(),
                    property(
                        doc,
                        id,
                        key.into(),
                        &child_path,
                        is_content,
                        depth.saturating_add(1),
                    )?,
                );
            }
            Ok(Node::Map(kind, properties))
        }
        Value::Object(kind @ (ObjType::List | ObjType::Text)) => {
            let mut elements = Vec::new();
            // list_range exposes zero-width mark operations and list indices even
            // for Text. Read by the document's text-encoding positions instead.
            for index in 0..doc.length(id) {
                let child_path = format!("{path}/{index}");
                let values = property(
                    doc,
                    id,
                    index.into(),
                    &child_path,
                    is_content,
                    depth.saturating_add(1),
                )?;
                if is_content
                    && kind == ObjType::Text
                    && !matches!(
                        values.as_slice(),
                        [Node::Scalar(ExactScalar(ScalarValue::Str(_)))]
                    )
                {
                    return Err(RestoreError::UnsupportedContent(format!(
                        "{child_path}: rich-text blocks and non-string text elements are unsupported"
                    )));
                }
                elements.push(values);
            }
            let marks = marks(doc, id, path)?;
            if kind == ObjType::List {
                Ok(Node::List(elements, marks))
            } else {
                Ok(Node::Text {
                    text: doc.text(id).map_err(|err| RestoreError::read(path, &err))?,
                    marks,
                    protected_elements: (!is_content).then_some(elements),
                })
            }
        }
    }
}
