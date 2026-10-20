//! The `tilolang.meta` JSON (implementation design section 5), as the runtime reads it.
//!
//! This is the only module that knows the metadata's shape. The compiler crate emits the
//! same JSON; swapping these types for the compiler's own is a change to this file alone.

use serde::{Deserialize, Serialize};

/// Effect class of an action (spec 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectClass {
    Read,
    Undoable,
    Outside,
}

impl EffectClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Undoable => "undoable",
            Self::Outside => "outside",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub app: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub module_hash: String,
    #[serde(default)]
    pub types: Vec<TypeMeta>,
    #[serde(default)]
    pub actions: Vec<ActionMeta>,
    #[serde(default)]
    pub views: Vec<ViewMeta>,
    #[serde(default)]
    pub flows: Vec<FlowMeta>,
    #[serde(default)]
    pub fns: Vec<FnMeta>,
}

impl Meta {
    /// Parse the metadata JSON.
    ///
    /// # Errors
    ///
    /// Returns the parse error.
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TypeMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub fields: Vec<FieldMeta>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldMeta {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
    #[serde(default)]
    pub merge: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    /// The declared default, as plain JSON (`3`, `"x"`, `true`, `null` for `none`).
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ParamMeta {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reach {
    pub action: String,
    pub class: EffectClass,
    #[serde(default)]
    pub own: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActionMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub params: Vec<ParamMeta>,
    #[serde(default)]
    pub result: Option<String>,
    pub class: EffectClass,
    #[serde(default)]
    pub proposal: Option<String>,
    #[serde(default)]
    pub reaches: Vec<Reach>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AnchorMeta {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub params: Vec<ParamMeta>,
    #[serde(default)]
    pub anchors: Vec<AnchorMeta>,
    #[serde(default = "Vec::new")]
    pub reaches: Vec<Reach>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FlowMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub params: Vec<ParamMeta>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub reaches: Vec<Reach>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FnMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub params: Vec<ParamMeta>,
    #[serde(default)]
    pub result: Option<String>,
}

/// A parsed type expression (implementation design 3.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Text,
    /// A declared record type, by name or id, possibly qualified (`todo.Item`).
    Named(String),
    List(Box<Ty>),
    Set(Box<Ty>),
    Map(Box<Ty>),
    Collection(Box<Ty>),
    Option(Box<Ty>),
    Tuple(Vec<Ty>),
    Action,
    View,
}

/// How a value of a type is passed to and from wasm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repr {
    I64,
    F64,
    Bool,
    Handle,
}

impl Ty {
    /// Parse a type expression such as `Collection<Track>` or `(Int, Text)`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the malformed expression.
    pub fn parse(text: &str) -> Result<Self, String> {
        let t = text.trim();
        if t.is_empty() {
            return Err("empty type".into());
        }
        if let Some(inner) = t.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
            let parts = split_top_level(inner)
                .into_iter()
                .map(Self::parse)
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Self::Tuple(parts));
        }
        if let Some((head, rest)) = t.split_once('<') {
            let inner = rest
                .strip_suffix('>')
                .ok_or_else(|| format!("unclosed `<` in type `{t}`"))?;
            let arg = Box::new(Self::parse(inner)?);
            return match head.trim() {
                "List" => Ok(Self::List(arg)),
                "Set" => Ok(Self::Set(arg)),
                "Map" => Ok(Self::Map(arg)),
                "Collection" => Ok(Self::Collection(arg)),
                "Option" => Ok(Self::Option(arg)),
                other => Err(format!("`{other}` takes no type argument")),
            };
        }
        Ok(match t {
            "Int" => Self::Int,
            "Float" => Self::Float,
            "Bool" => Self::Bool,
            "Text" => Self::Text,
            "Action" => Self::Action,
            "View" => Self::View,
            name => Self::Named(name.to_owned()),
        })
    }

    pub fn repr(&self) -> Repr {
        match self {
            Self::Int => Repr::I64,
            Self::Float => Repr::F64,
            Self::Bool => Repr::Bool,
            _ => Repr::Handle,
        }
    }
}

fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '(' => depth = depth.saturating_add(1),
            '>' | ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(s.get(start..i).unwrap_or_default());
                start = i.saturating_add(1);
            }
            _ => {}
        }
    }
    parts.push(s.get(start..).unwrap_or_default());
    parts
}

/// How a field merges concurrent edits (implementation design 3.2 and 4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldKind {
    Register,
    Counter,
    Text,
    Collection,
    Set,
    Map,
}

impl FieldMeta {
    /// The parsed type and merge kind of the field.
    ///
    /// # Errors
    ///
    /// Returns a message when the type expression or the merge name is not understood.
    pub fn resolve(&self) -> Result<(Ty, FieldKind), String> {
        let ty = Ty::parse(&self.ty)?;
        let kind = match (&ty, self.merge.as_deref()) {
            (Ty::Collection(_), _) => FieldKind::Collection,
            (Ty::Set(_), _) => FieldKind::Set,
            (Ty::Map(_), _) => FieldKind::Map,
            (_, Some("counter")) => FieldKind::Counter,
            (_, Some("text")) | (Ty::Text, None) => FieldKind::Text,
            (_, Some("register") | None) => FieldKind::Register,
            (_, Some(other)) => return Err(format!("unknown merge kind `{other}`")),
        };
        Ok((ty, kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_parse() {
        assert_eq!(Ty::parse("Int"), Ok(Ty::Int));
        assert_eq!(
            Ty::parse("Collection<Track>"),
            Ok(Ty::Collection(Box::new(Ty::Named("Track".into()))))
        );
        assert_eq!(
            Ty::parse("(Int, Option<Text>)"),
            Ok(Ty::Tuple(vec![Ty::Int, Ty::Option(Box::new(Ty::Text))]))
        );
        assert!(Ty::parse("List<Int").is_err());
    }

    #[test]
    fn meta_example_parses() {
        let json = r#"{
          "app": "so.tilo.counter", "title": "Counter", "module_hash": "x",
          "types": [{"id": "k3f9", "name": "Counter", "doc": "d",
                     "fields": [{"id": "x1", "name": "value", "type": "Int", "merge": "counter"}]}],
          "actions": [{"id": "a1", "name": "increment", "params": [{"name": "c", "type": "Counter"}],
                       "result": null, "class": "undoable", "proposal": null,
                       "reaches": [{"action": "a1", "class": "undoable", "own": true}]}],
          "views": [{"id": "v1", "name": "counter", "params": [], "anchors": [{"id": "p", "name": "plus"}],
                     "reaches": []}],
          "flows": [], "fns": []
        }"#;
        let m = Meta::from_json(json).unwrap();
        assert_eq!(
            m.types[0].fields[0].resolve().unwrap().1,
            FieldKind::Counter
        );
        assert_eq!(m.actions[0].class, EffectClass::Undoable);
    }
}
