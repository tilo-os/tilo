//! The package the host itself provides: the types that exist in every module.
//!
//! `Invocation` records what happened to an action a block applied: the host creates and
//! changes these resources, and code only reads them. Every module sees the package as `host`.

use crate::meta::{FieldMeta, PackageMeta, TypeMeta};
use crate::types::MergeKind;

/// The app id of the host's package.
pub const HOST_APP: &str = "tilo.host";
/// The id of the `Invocation` type.
pub const INVOCATION_TYPE: &str = "host-invocation";

/// The ids of the built-in enum `Result<T, E>`: its type, its variants and their payload
/// fields. `try act(args)` builds one from an invocation's outcome.
pub const RESULT_TYPE: &str = "result";
pub const RESULT_OK: &str = "ok";
pub const RESULT_ERR: &str = "err";
pub const RESULT_VALUE: &str = "value";
pub const RESULT_ERROR: &str = "error";

/// A type of the host whose values are made by the host and read by code, field by field:
/// what the platform says about a resource, an action or a lens. Such a value is not a resource
/// and is never stored; it is held as a tuple of its fields, in this order.
pub struct ValueType {
    pub name: &'static str,
    pub id: &'static str,
    pub doc: &'static str,
    /// Its fields: name, type as metadata writes it, doc.
    pub fields: &'static [(&'static str, &'static str, &'static str)],
}

/// The value types of the host, each held as a tuple of its fields in their order.
pub const VALUE_TYPES: &[ValueType] = &[
    ValueType {
        name: "Action",
        id: "host-action",
        doc: "An action offered on a resource to the person looking, as the platform presents it.",
        fields: &[
            (
                "label",
                "Text",
                "What it does, as a button or a menu names it.",
            ),
            (
                "icon",
                "Option<Text>",
                "The name of its icon, when it declares one.",
            ),
            ("role", "Text", "`primary`, `destructive`, or empty."),
            (
                "primary",
                "Bool",
                "Whether it is the resource's primary action.",
            ),
            (
                "destructive",
                "Bool",
                "Whether it takes something away: it is undone, not confirmed.",
            ),
            (
                "on",
                "Option<Bool>",
                "Whether the state it toggles is on for the resource, when it says.",
            ),
            (
                "params",
                "List<Param>",
                "What it takes besides the resource, each with its default.",
            ),
            (
                "run",
                "Action",
                "The action applied to the resource: what a control runs.",
            ),
        ],
    },
    ValueType {
        name: "Param",
        id: "host-param",
        doc: "A parameter of an action.",
        fields: &[
            ("name", "Text", "Its name."),
            ("type_name", "Text", "Its type, as source writes it."),
            (
                "default",
                "Option<Text>",
                "Its default as written, when it has one.",
            ),
        ],
    },
    ValueType {
        name: "Lens",
        id: "host-lens",
        doc: "Someone's lens on a resource: a person or an agent whose window shows it now.",
        fields: &[
            ("who", "Principal", "Whose lens it is."),
            (
                "mine",
                "Bool",
                "Whether it is the lens of the person looking.",
            ),
        ],
    },
    ValueType {
        name: "Meta",
        id: "host-meta",
        doc: "What the platform shows of a resource or of its type.",
        fields: &[
            (
                "title",
                "Text",
                "Its title: its field marked `title`; else its app's title for a root, or its type's name.",
            ),
            (
                "type_name",
                "Text",
                "Its type's name, in words: `New note` for `NewNote`.",
            ),
            ("noun", "Text", "What one is called in a sentence: `note`."),
            ("icon", "Option<Text>", "The name of its app's icon."),
            (
                "opens",
                "Bool",
                "Whether it opens in a window of its own: its type has a default view.",
            ),
        ],
    },
];

/// The value type `name` of the host.
#[must_use]
pub fn value_type(name: &str) -> Option<&'static ValueType> {
    VALUE_TYPES.iter().find(|t| t.name == name)
}

/// Whether the record type `key` is a value type of the host: held as a tuple, never a
/// resource, so it has no id.
#[must_use]
pub fn is_value(key: &crate::types::TypeKey) -> bool {
    key.app == HOST_APP && value_type(&key.name).is_some()
}

/// The place of the field `field` in the tuple of the value type `ty`.
#[must_use]
pub fn value_field(ty: &str, field: &str) -> Option<usize> {
    value_type(ty)?
        .fields
        .iter()
        .position(|(name, ..)| *name == field)
}

fn field(name: &str, ty: &str, doc: &str) -> FieldMeta {
    FieldMeta {
        id: format!("inv-{name}"),
        name: name.to_string(),
        ty: ty.to_string(),
        merge: MergeKind::Register,
        doc: doc.to_string(),
        default: None,
        role: None,
    }
}

/// The metadata of the host's package.
pub fn host_meta() -> PackageMeta {
    PackageMeta {
        app: HOST_APP.to_string(),
        title: "Host".to_string(),
        types: std::iter::once(TypeMeta {
            id: INVOCATION_TYPE.to_string(),
            name: "Invocation".to_string(),
            doc: "An action a block applied, and what became of it.".to_string(),
            fields: vec![
                field("action", "Text", "The id of the action or flow."),
                field("name", "Text", "Its name."),
                field("app", "Text", "The app that declares it."),
                field("args", "Text", "The arguments, as JSON."),
                field("author", "Principal", "Who wrote the block."),
                field(
                    "state",
                    "Text",
                    "done, suggested, declined, failed or denied.",
                ),
                field("result", "Text", "The result, or why it failed."),
                field(
                    "run",
                    "Text",
                    "The suggestion's run on the device that made it.",
                ),
            ],
        })
        .chain(VALUE_TYPES.iter().map(|value| {
            TypeMeta {
                id: value.id.to_string(),
                name: value.name.to_string(),
                doc: value.doc.to_string(),
                fields: value
                    .fields
                    .iter()
                    .map(|(name, ty, doc)| FieldMeta {
                        id: format!("{}-{name}", value.id),
                        name: (*name).to_string(),
                        ty: (*ty).to_string(),
                        merge: MergeKind::Register,
                        doc: (*doc).to_string(),
                        default: None,
                        role: None,
                    })
                    .collect(),
            }
        }))
        .collect(),
        ..PackageMeta::default()
    }
}
