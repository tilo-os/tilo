//! Runtime values: what handles refer to, what actions take and return, what flows journal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value as Json};

use crate::ids::{ActionId, DocId, FieldId, ItemId, ModuleHash, TypeId};

/// One step into a nested record: the collection field and the item inside it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Step {
    pub field: FieldId,
    pub item: ItemId,
}

/// A reference to a resource: a document, or an item nested in one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceRef {
    pub doc: DocId,
    pub path: Vec<Step>,
    pub ty: TypeId,
}

impl ResourceRef {
    pub fn root(doc: impl Into<DocId>, ty: impl Into<TypeId>) -> Self {
        Self {
            doc: doc.into(),
            path: Vec::new(),
            ty: ty.into(),
        }
    }

    /// The item inside collection `field` of this resource.
    pub fn item(&self, field: &str, item: &str, ty: impl Into<TypeId>) -> Self {
        let mut path = self.path.clone();
        path.push(Step {
            field: field.to_owned(),
            item: item.to_owned(),
        });
        Self {
            doc: self.doc.clone(),
            path,
            ty: ty.into(),
        }
    }

    /// The reference as text: the document id, then `/field:item` per step.
    pub fn id_string(&self) -> String {
        let mut out = self.doc.clone();
        for step in &self.path {
            out.push('/');
            out.push_str(&step.field);
            out.push(':');
            out.push_str(&step.item);
        }
        out
    }

    /// Parse the text produced by [`ResourceRef::id_string`]. The caller supplies the type
    /// and is responsible for checking it against the stored one.
    pub fn parse_id(text: &str, ty: impl Into<TypeId>) -> Option<Self> {
        let mut parts = text.split('/');
        let doc = parts.next().filter(|d| !d.is_empty())?;
        let mut path = Vec::new();
        for part in parts {
            let (field, item) = part.split_once(':')?;
            path.push(Step {
                field: field.to_owned(),
                item: item.to_owned(),
            });
        }
        Some(Self {
            doc: doc.to_owned(),
            path,
            ty: ty.into(),
        })
    }
}

/// An action applied to arguments, with the module that wrote the application expression.
/// `author` is `None` for a call made straight by a principal, with no code in between.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionRef {
    pub action: ActionId,
    pub args: Vec<Value>,
    pub author: Option<ModuleHash>,
}

impl ActionRef {
    pub fn new(action: impl Into<ActionId>, args: Vec<Value>) -> Self {
        Self {
            action: action.into(),
            args,
            author: None,
        }
    }
}

impl From<(&str, Vec<Value>)> for ActionRef {
    fn from((action, args): (&str, Vec<Value>)) -> Self {
        Self::new(action, args)
    }
}

/// A record under construction, the init value of `coll_insert`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordInit {
    pub ty: TypeId,
    pub fields: BTreeMap<FieldId, Value>,
}

/// A resource reference together with the module that created the handle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefValue {
    pub target: ResourceRef,
    pub author: Option<ModuleHash>,
}

/// A value of the language.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
    List(Vec<Value>),
    Tuple(Vec<Value>),
    Opt(Option<Box<Value>>),
    Ref(RefValue),
    Action(ActionRef),
    /// A UI node under construction in the current view invocation (host-internal).
    Node(usize),
    /// A record under construction (host-internal).
    Rec(RecordInit),
    /// The error a denied or failed invocation returns to a flow.
    Error(String),
}

impl Value {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// A reference value with no authoring module, as handed in by the host.
    pub fn resource(target: &ResourceRef) -> Self {
        Self::Ref(RefValue {
            target: target.clone(),
            author: None,
        })
    }

    pub fn some(v: Self) -> Self {
        Self::Opt(Some(Box::new(v)))
    }

    pub fn none() -> Self {
        Self::Opt(None)
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(i) => Some(*i),
            Self::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    /// A short name of the variant, for trap messages.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Bool(_) => "bool",
            Self::Text(_) => "text",
            Self::List(_) => "list",
            Self::Tuple(_) => "tuple",
            Self::Opt(_) => "option",
            Self::Ref(_) => "resource reference",
            Self::Action(_) => "action reference",
            Self::Node(_) => "ui node",
            Self::Rec(_) => "record",
            Self::Error(_) => "error",
        }
    }
}

/// Encode a value as JSON, for registers, journals and change messages.
///
/// # Errors
///
/// Fails for host-internal values and non-finite floats.
pub fn to_json(v: &Value) -> Result<Json, String> {
    Ok(match v {
        Value::Unit => Json::Null,
        Value::Int(i) => Json::from(*i),
        Value::Bool(b) => Json::Bool(*b),
        Value::Float(f) => Json::Number(
            Number::from_f64(*f).ok_or_else(|| "a non-finite float cannot be stored".to_owned())?,
        ),
        Value::Text(s) => Json::String(s.clone()),
        Value::List(items) => tagged("list", items_to_json(items)?),
        Value::Tuple(items) => tagged("tuple", items_to_json(items)?),
        Value::Opt(Some(inner)) => tagged("some", to_json(inner)?),
        Value::Opt(None) => tagged("none", Json::Null),
        Value::Ref(r) => {
            let mut m = Map::new();
            m.insert("ref".into(), Json::String(r.target.id_string()));
            m.insert("ty".into(), Json::String(r.target.ty.clone()));
            if let Some(a) = &r.author {
                m.insert("author".into(), Json::String(a.0.clone()));
            }
            Json::Object(m)
        }
        Value::Action(a) => {
            let mut m = Map::new();
            m.insert("action".into(), Json::String(a.action.clone()));
            m.insert("args".into(), items_to_json(&a.args)?);
            if let Some(author) = &a.author {
                m.insert("author".into(), Json::String(author.0.clone()));
            }
            Json::Object(m)
        }
        Value::Error(msg) => tagged("error", Json::String(msg.clone())),
        Value::Node(_) | Value::Rec(_) => {
            return Err(format!("a {} cannot be stored", v.kind_name()));
        }
    })
}

fn tagged(tag: &str, inner: Json) -> Json {
    let mut m = Map::new();
    m.insert(tag.to_owned(), inner);
    Json::Object(m)
}

fn items_to_json(items: &[Value]) -> Result<Json, String> {
    items
        .iter()
        .map(to_json)
        .collect::<Result<Vec<_>, _>>()
        .map(Json::Array)
}

/// Decode what [`to_json`] produced.
///
/// # Errors
///
/// Fails on JSON that is not an encoded value.
pub fn from_json(j: &Json) -> Result<Value, String> {
    Ok(match j {
        Json::Null => Value::Unit,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) if !n.is_f64() => Value::Int(i),
            _ => Value::Float(n.as_f64().ok_or("number out of range")?),
        },
        Json::String(s) => Value::Text(s.clone()),
        Json::Array(_) => return Err("a bare array is not an encoded value".into()),
        Json::Object(m) => from_json_object(m)?,
    })
}

fn from_json_items(j: &Json) -> Result<Vec<Value>, String> {
    j.as_array()
        .ok_or("expected an array")?
        .iter()
        .map(from_json)
        .collect()
}

fn author_of(m: &Map<String, Json>) -> Option<ModuleHash> {
    m.get("author")
        .and_then(Json::as_str)
        .map(|s| ModuleHash(s.to_owned()))
}

fn from_json_object(m: &Map<String, Json>) -> Result<Value, String> {
    if let Some(items) = m.get("list") {
        return Ok(Value::List(from_json_items(items)?));
    }
    if let Some(items) = m.get("tuple") {
        return Ok(Value::Tuple(from_json_items(items)?));
    }
    if let Some(inner) = m.get("some") {
        return Ok(Value::some(from_json(inner)?));
    }
    if m.contains_key("none") {
        return Ok(Value::none());
    }
    if let Some(Json::String(msg)) = m.get("error") {
        return Ok(Value::Error(msg.clone()));
    }
    if let (Some(Json::String(id)), Some(Json::String(ty))) = (m.get("ref"), m.get("ty")) {
        let target = ResourceRef::parse_id(id, ty.clone()).ok_or("malformed reference")?;
        return Ok(Value::Ref(RefValue {
            target,
            author: author_of(m),
        }));
    }
    if let (Some(Json::String(action)), Some(args)) = (m.get("action"), m.get("args")) {
        return Ok(Value::Action(ActionRef {
            action: action.clone(),
            args: from_json_items(args)?,
            author: author_of(m),
        }));
    }
    Err("an object that is not an encoded value".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_through_json() {
        let target = ResourceRef::root("d1", "T").item("f", "i-1", "U");
        let v = Value::List(vec![
            Value::Int(3),
            Value::Float(2.0),
            Value::Tuple(vec![Value::text("x"), Value::none()]),
            Value::some(Value::Bool(true)),
            Value::Ref(RefValue {
                target,
                author: Some(ModuleHash("m".into())),
            }),
            Value::Action(ActionRef::new("a", vec![Value::Int(1)])),
        ]);
        let json = to_json(&v).unwrap();
        let text = serde_json::to_string(&json).unwrap();
        let back = from_json(&serde_json::from_str(&text).unwrap()).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn reference_ids_parse_back() {
        let r = ResourceRef::root("d1", "T").item("f", "i-1", "U");
        assert_eq!(r.id_string(), "d1/f:i-1");
        assert_eq!(ResourceRef::parse_id(&r.id_string(), "U"), Some(r));
    }
}
