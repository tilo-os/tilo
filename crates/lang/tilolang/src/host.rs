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

fn field(name: &str, ty: &str, doc: &str) -> FieldMeta {
    FieldMeta {
        id: format!("inv-{name}"),
        name: name.to_string(),
        ty: ty.to_string(),
        merge: MergeKind::Register,
        doc: doc.to_string(),
        default: None,
    }
}

/// The metadata of the host's package.
pub fn host_meta() -> PackageMeta {
    PackageMeta {
        app: HOST_APP.to_string(),
        title: "Host".to_string(),
        types: vec![TypeMeta {
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
        }],
        ..PackageMeta::default()
    }
}
