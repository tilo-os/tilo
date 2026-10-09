//! What a committed action did, as data.
//!
//! Every change an action commits carries a history entry
//! ([`crate::ActionContext`]): who acted, for whom, on which item and what
//! they did. "What they did" is a [`Did`]: a field edit with its path and,
//! for scalars, the value before and after; a verb with its [`ActionKey`]
//! and scalar arguments; a group of those; or a history step (undo, redo,
//! restore, checkpoint). No sentence is stored. Readers render one in their
//! own language (`tilo-history`), and agents, scripts and logs read the data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::key::ActionKey;

/// Longest text kept for one scalar value, in characters.
pub const MAX_SCALAR_CHARS: usize = 80;
/// Most arguments kept for one verb.
pub const MAX_VERB_ARGS: usize = 8;
/// Most members of a group kept by value; [`GroupOf::count`] keeps the total.
pub const MAX_GROUP_MEMBERS: usize = 8;

/// What an action did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "did", rename_all = "kebab-case")]
pub enum Did {
    Edit(FieldEdit),
    Verb(VerbCall),
    Group(GroupOf),
    History(HistoryStep),
}

impl Did {
    /// A verb with its arguments bounded to plain values.
    pub fn verb(action: ActionKey, args: impl IntoIterator<Item = (String, Scalar)>) -> Self {
        Self::Verb(VerbCall::new(action, args))
    }

    /// A group of `members`, keeping at most [`MAX_GROUP_MEMBERS`] by value.
    pub fn group(members: impl IntoIterator<Item = Did>) -> Self {
        let mut count = 0u32;
        let mut kept = Vec::new();
        for member in members {
            count = count.saturating_add(1);
            if kept.len() < MAX_GROUP_MEMBERS {
                kept.push(member);
            }
        }
        Self::Group(GroupOf {
            members: kept,
            count,
        })
    }

    /// Drop every verb argument and field value, keeping what was done and
    /// where: the form a public audit keeps.
    pub fn forget_args(&mut self) {
        match self {
            Self::Edit(edit) => {
                edit.old = None;
                edit.new = None;
            }
            Self::Verb(verb) => verb.args.clear(),
            Self::Group(group) => group.members.iter_mut().for_each(Self::forget_args),
            Self::History(step) => {
                if let Some(of) = &mut step.of {
                    of.what.forget_args();
                }
            }
        }
    }

    pub fn history(op: HistoryOp, steps: u32, of: Option<Described>) -> Self {
        Self::History(HistoryStep {
            op,
            steps,
            of: of.map(Box::new),
        })
    }
}

/// A built-in edit of one field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldEdit {
    /// The model type that owns the field, kebab-case (`snippet`).
    pub ty: String,
    /// The field's storage key (`favorite`).
    pub field: String,
    /// Where the field is, from the resource's root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path: Vec<PathStep>,
    pub op: EditOp,
    /// The value before, for a scalar field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<Scalar>,
    /// The value after, for a scalar field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<Scalar>,
}

/// One step of a path from a resource's root to a field.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathStep {
    Field(String),
    Item(String),
    Key(String),
    Cell(String, String),
}

/// The operation of a field edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EditOp {
    Set,
    Splice,
    Replace,
    Add,
    Insert,
    Remove,
    MoveTo,
    Mark,
    SetCell,
    InsertRow,
    InsertColumn,
    RemoveRow,
    RemoveColumn,
}

impl EditOp {
    /// The name readers pass to messages as `$op`.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Splice => "splice",
            Self::Replace => "replace",
            Self::Add => "add",
            Self::Insert => "insert",
            Self::Remove => "remove",
            Self::MoveTo => "move-to",
            Self::Mark => "mark",
            Self::SetCell => "set-cell",
            Self::InsertRow => "insert-row",
            Self::InsertColumn => "insert-column",
            Self::RemoveRow => "remove-row",
            Self::RemoveColumn => "remove-column",
        }
    }
}

/// A verb and its plain arguments by parameter name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerbCall {
    pub action: ActionKey,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, Scalar>,
}

impl VerbCall {
    /// Keeps at most [`MAX_VERB_ARGS`] arguments.
    pub fn new(action: ActionKey, args: impl IntoIterator<Item = (String, Scalar)>) -> Self {
        Self {
            action,
            args: args.into_iter().take(MAX_VERB_ARGS).collect(),
        }
    }
}

/// Several actions committed as one change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupOf {
    /// The first [`MAX_GROUP_MEMBERS`] members.
    pub members: Vec<Did>,
    /// How many members the group had.
    pub count: u32,
}

impl GroupOf {
    /// The one thing every member did, when they all did the same.
    pub fn shared(&self) -> Option<&Did> {
        let first = self.members.first()?;
        (self.count as usize == self.members.len() && self.members.iter().all(|m| m == first))
            .then_some(first)
    }
}

/// A step through history itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryStep {
    pub op: HistoryOp,
    /// How many steps it took back or brought back.
    #[serde(default)]
    pub steps: u32,
    /// What it took back or brought back, when it was one step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub of: Option<Box<Described>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistoryOp {
    Undo,
    Redo,
    Restore,
    Checkpoint {
        name: String,
    },
    /// The document was brought to the storage version `to`.
    Migrate {
        to: u32,
    },
}

/// The item an action addressed, with its label at the time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    pub id: String,
    pub label: String,
}

/// What an action says about itself before it applies: what it does and to
/// which item ([`crate::Action::describe`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Described {
    pub what: Did,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ActionItem>,
}

impl Described {
    pub fn new(what: Did) -> Self {
        Self { what, item: None }
    }

    #[must_use]
    pub fn on(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.item = Some(ActionItem {
            id: id.into(),
            label: label.into(),
        });
        self
    }

    /// The published action `key` invoked with `encoded`: the verb with the
    /// plain arguments the encoded value carries. An empty `key.app` is
    /// filled by the executor from the entry's app.
    pub fn of_published(encoded: &crate::EncodedAction, key: ActionKey) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(&encoded.payload).ok()?;
        let args = variant_of(&value).and_then(|(_, fields)| fields);
        Some(Self::new(Did::verb(key, plain_args(args))))
    }

    /// What an encoded action says about itself when its type describes
    /// nothing: the verb its serde variant names, with its plain arguments.
    /// `app` names the publishing app; without one the action type's stem
    /// (`notes` of `notes.action`) stands in.
    pub fn from_encoded(encoded: &crate::EncodedAction, app: Option<&str>) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_slice(&encoded.payload).ok()?;
        Self::from_value(&encoded.type_id, &value, app)
    }

    /// [`Self::from_encoded`] for an action value already in JSON.
    pub fn from_value(type_id: &str, value: &serde_json::Value, app: Option<&str>) -> Option<Self> {
        let (variant, fields) = variant_of(value)?;
        let app = app
            .filter(|app| !app.trim().is_empty())
            .map_or_else(|| type_stem(type_id), str::to_owned);
        Some(Self::new(Did::verb(
            ActionKey::new(app, type_id, kebab(variant)),
            plain_args(fields),
        )))
    }
}

/// The plain arguments of a variant's fields (an adjacently tagged value's
/// `args`, else the fields beside an internal `action` tag): its plain
/// values by name, then the plain values of its nested records as
/// `record.field`.
fn plain_args(
    fields: Option<&serde_json::Map<String, serde_json::Value>>,
) -> impl Iterator<Item = (String, Scalar)> + '_ {
    let tagged = fields.is_some_and(|fields| fields.contains_key("action"));
    let fields = match fields.and_then(|fields| fields.get("args")) {
        Some(serde_json::Value::Object(args)) if tagged => Some(args),
        _ => fields,
    };
    let own = move || {
        fields
            .into_iter()
            .flat_map(serde_json::Map::iter)
            .filter(move |(name, _)| !(tagged && name.as_str() == "action"))
    };
    let plain = own().filter_map(|(name, value)| Some((name.clone(), Scalar::from_json(value)?)));
    let nested = own().flat_map(|(name, value)| {
        value
            .as_object()
            .into_iter()
            .flat_map(serde_json::Map::iter)
            .filter_map(move |(field, value)| {
                Some((format!("{name}.{field}"), Scalar::from_json(value)?))
            })
    });
    plain.chain(nested)
}

/// The variant name and fields of an externally, adjacently or internally
/// tagged action value, looking through a stamped `{op, at}` envelope.
fn variant_of(
    value: &serde_json::Value,
) -> Option<(&str, Option<&serde_json::Map<String, serde_json::Value>>)> {
    match value {
        serde_json::Value::String(variant) => Some((variant, None)),
        serde_json::Value::Object(fields) => {
            if let (Some(op), Some(_)) = (fields.get("op"), fields.get("at"))
                && fields.len() == 2
            {
                return variant_of(op);
            }
            if let Some(serde_json::Value::String(variant)) = fields.get("action") {
                return Some((variant, Some(fields)));
            }
            if fields.len() == 1 {
                let (variant, inner) = fields.iter().next()?;
                return Some((variant, inner.as_object()));
            }
            None
        }
        _ => None,
    }
}

fn type_stem(type_id: &str) -> String {
    type_id.split('.').next().unwrap_or(type_id).to_owned()
}

/// `RemoveMarks`, `remove_marks` and `remove-marks` all become `remove-marks`.
pub fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len().saturating_add(4));
    let mut previous_lower_or_digit = false;
    for ch in name.chars() {
        if ch == '_' || ch == '-' || ch == ' ' {
            if !out.is_empty() && !out.ends_with('-') {
                out.push('-');
            }
            previous_lower_or_digit = false;
            continue;
        }
        if ch.is_uppercase() && previous_lower_or_digit {
            out.push('-');
        }
        previous_lower_or_digit = ch.is_lowercase() || ch.is_ascii_digit();
        out.extend(ch.to_lowercase());
    }
    out
}

/// One plain value in an entry: a boolean, a number, a short text or an
/// enum variant. Nested values are never stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Scalar(serde_json::Value);

impl Scalar {
    /// A plain JSON value, text cut to [`MAX_SCALAR_CHARS`]. `None` for
    /// arrays and objects, and for null.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        match value {
            serde_json::Value::Bool(_) | serde_json::Value::Number(_) => Some(Self(value.clone())),
            serde_json::Value::String(text) => Some(Self::text(text)),
            _ => None,
        }
    }

    /// Text cut to [`MAX_SCALAR_CHARS`], without control characters.
    pub fn text(text: &str) -> Self {
        let mut chars = text.chars().filter(|ch| !ch.is_control());
        let cut: String = chars.by_ref().take(MAX_SCALAR_CHARS).collect();
        if chars.next().is_some() {
            Self(serde_json::Value::String(cut + "…"))
        } else {
            Self(serde_json::Value::String(cut))
        }
    }

    /// Any serializable plain value (a bool, a number, a unit enum, a
    /// string). `None` when it serializes to anything else.
    pub fn of<T: Serialize + ?Sized>(value: &T) -> Option<Self> {
        Self::from_json(&serde_json::to_value(value).ok()?)
    }

    pub fn as_json(&self) -> &serde_json::Value {
        &self.0
    }

    pub fn as_bool(&self) -> Option<bool> {
        self.0.as_bool()
    }

    pub fn as_str(&self) -> Option<&str> {
        self.0.as_str()
    }

    pub fn as_f64(&self) -> Option<f64> {
        self.0.as_f64()
    }
}

impl From<bool> for Scalar {
    fn from(value: bool) -> Self {
        Self(serde_json::Value::Bool(value))
    }
}

impl From<i64> for Scalar {
    fn from(value: i64) -> Self {
        Self(value.into())
    }
}

impl From<&str> for Scalar {
    fn from(value: &str) -> Self {
        Self::text(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_actions_describe_themselves_as_verbs() {
        let encoded = crate::EncodedAction {
            type_id: "notes.action".into(),
            payload: br#"{"RenameNote":{"id":"n1","title":"Groceries","tags":["a"]}}"#.to_vec(),
        };
        let described = Described::from_encoded(&encoded, None).unwrap();
        let Did::Verb(verb) = &described.what else {
            panic!("a verb: {described:?}");
        };
        assert_eq!(
            verb.action,
            ActionKey::new("notes", "notes.action", "rename-note")
        );
        assert_eq!(verb.args.get("title"), Some(&Scalar::from("Groceries")));
        assert!(
            !verb.args.contains_key("tags"),
            "nested values are left out"
        );

        let tagged = crate::EncodedAction {
            type_id: "canvas.action".into(),
            payload: br#"{"op":{"action":"draw","args":{"size":3}},"at":17}"#.to_vec(),
        };
        let Did::Verb(verb) = Described::from_encoded(&tagged, Some("canvas"))
            .unwrap()
            .what
        else {
            panic!("a verb");
        };
        assert_eq!(verb.action.action, "draw");
        assert_eq!(verb.args.get("size"), Some(&Scalar::from(3)));
        assert!(
            Described::from_encoded(
                &crate::EncodedAction {
                    type_id: "x".into(),
                    payload: b"7".to_vec()
                },
                None
            )
            .is_none()
        );
    }

    #[test]
    fn values_are_bounded() {
        let long = "x".repeat(MAX_SCALAR_CHARS * 2);
        assert_eq!(
            Scalar::text(&long).as_str().unwrap().chars().count(),
            MAX_SCALAR_CHARS + 1
        );
        assert!(Scalar::from_json(&serde_json::json!({"a": 1})).is_none());
        assert!(Scalar::from_json(&serde_json::Value::Null).is_none());
        let args = (0..20).map(|n| (format!("a{n}"), Scalar::from(i64::from(n))));
        let Did::Verb(verb) = Did::verb(ActionKey::default(), args) else {
            unreachable!()
        };
        assert_eq!(verb.args.len(), MAX_VERB_ARGS);
        let Did::Group(group) = Did::group((0..20).map(|_| Did::verb(ActionKey::default(), [])))
        else {
            unreachable!()
        };
        assert_eq!((group.members.len(), group.count), (MAX_GROUP_MEMBERS, 20));
        assert!(
            group.shared().is_none(),
            "a cut group does not claim one shared step"
        );
    }

    #[test]
    fn kebab_names() {
        assert_eq!(kebab("RemoveMarks"), "remove-marks");
        assert_eq!(kebab("remove_marks"), "remove-marks");
        assert_eq!(kebab("Set2Up"), "set2-up");
    }
}
