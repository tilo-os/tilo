//! A reference: a resource of one workspace naming a record of another, so the record is found
//! and opened from here while its bytes stay where they are.
//!
//! The kind is the system's, declared once for every workspace's store ([`schema`]), and every
//! app's records are referenced alike. A reference holds the record's address and nothing of
//! it: opening it opens the record in its own workspace, where reading it is decided as any
//! read is. It grants nothing, so a member of this workspace who may not read the record there
//! finds the reference and cannot open what it names.

use std::collections::BTreeMap;

use crate::schema::{Expected, Field, FieldKind, Schema, Type, Verb, Writes};
use collab_types::store::{Cell, Value};

/// The kind of a reference.
pub const KIND: &str = "tilo.reference";

/// The verb that puts a reference to a record of another workspace in this one.
pub const BRING: &str = "bring";

/// The verb that takes a reference out of this workspace, leaving the record it names as it is.
pub const REMOVE: &str = "remove-reference";

/// The fields of a reference: the workspace the record is in, the app whose record it is, its
/// id there, and its kind.
pub const WORKSPACE: &str = "workspace";
pub const APP: &str = "app";
pub const RECORD: &str = "record";
pub const RECORD_KIND: &str = "kind";

/// Ties a reference's id to what it names ([`id_of`]).
const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x6d1c_1f36_5265_4f66_8000_7265_6665_7265);

/// The record a reference names.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Named {
    pub workspace: String,
    pub app: String,
    pub record: String,
    pub kind: String,
}

impl Named {
    /// The id of the reference to this record: one per record in a workspace, so bringing it
    /// here again writes the same reference.
    #[must_use]
    pub fn id(&self) -> String {
        let named = [
            self.workspace.as_str(),
            self.app.as_str(),
            self.record.as_str(),
        ]
        .join("\0");
        format!(
            "ref-{}",
            uuid::Uuid::new_v5(&NAMESPACE, named.as_bytes()).simple()
        )
    }

    /// The fields a reference to this record holds.
    #[must_use]
    pub fn fields(&self) -> BTreeMap<String, Cell> {
        [
            (WORKSPACE, &self.workspace),
            (APP, &self.app),
            (RECORD, &self.record),
            (RECORD_KIND, &self.kind),
        ]
        .into_iter()
        .map(|(field, value)| (field.to_owned(), Cell::One(Value::Text(value.clone()))))
        .collect()
    }

    /// The record the reference whose indexed fields are `fields` names; `None` for one that
    /// names none whole.
    #[must_use]
    pub fn of(fields: &BTreeMap<String, Cell>) -> Option<Self> {
        let text = |field: &str| match fields.get(field)?.one()? {
            Value::Text(text) if !text.is_empty() => Some(text.clone()),
            _ => None,
        };
        Some(Self {
            workspace: text(WORKSPACE)?,
            app: text(APP)?,
            record: text(RECORD)?,
            kind: text(RECORD_KIND).unwrap_or_default(),
        })
    }
}

/// The reference kind and its verbs, as every workspace's store declares them.
#[must_use]
pub fn schema() -> Schema {
    let text = |name: &str, optional: bool| Field {
        name: name.to_owned(),
        kind: FieldKind::Scalar,
        value: Expected::Text,
        optional,
    };
    let fields = vec![
        text(WORKSPACE, false),
        text(APP, false),
        text(RECORD, false),
        text(RECORD_KIND, true),
    ];
    let writes = || {
        BTreeMap::from([(
            KIND.to_owned(),
            Writes {
                fields: fields.iter().map(|field| field.name.clone()).collect(),
                sets_only: false,
            },
        )])
    };
    Schema {
        kind: KIND.to_owned(),
        types: vec![Type {
            name: KIND.to_owned(),
            fields: fields.clone(),
            nested: Vec::new(),
            live: None,
            founding: Vec::new(),
            origin: None,
            version: 1,
        }],
        verbs: [BRING, REMOVE]
            .into_iter()
            .map(|name| Verb {
                name: name.to_owned(),
                writes: writes(),
                ..Verb::default()
            })
            .collect(),
        readers: BTreeMap::new(),
    }
}
