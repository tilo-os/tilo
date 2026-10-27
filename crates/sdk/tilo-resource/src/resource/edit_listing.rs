//! The field-edit listing an agent builds an edit from.
//!
//! A resource with editable fields publishes one `field-edit` action
//! ([`FIELD_EDIT`](crate::FIELD_EDIT)) whose parameter's schema names every
//! editable path once: its template, the operations its type takes, the value
//! it holds, its title and flags, and one example edit per shape of path — a
//! field, an item's field, an entry. An agent reads that listing instead of
//! being told each field's shape again, so what the app publishes is what a
//! caller holds the edit to.

use serde_json::Value;

use crate::{Edit, Vocabulary};

/// The schema of a field edit of `R`: one alternative per editable path of
/// its stored shape — the path's steps, the operations its type takes, the
/// value it holds, its title and flags — so an agent builds a valid edit
/// from the published descriptor alone. A resource without a shape gets
/// the generic one, with its fields' own operations named for `rich`.
pub(crate) fn edit_schema(vocabulary: &Vocabulary) -> Value {
    let ops = (vocabulary.op_schemas)();
    if let Some(shape) = (vocabulary.shape)()
        && let Some(described) = described_shape(shape, &ops)
    {
        return described;
    }
    generic_edit_schema(&ops)
}

/// The listing of `shape`'s editable paths, checked against `ops`: `None`
/// when the shape publishes none (a field edit of it is refused instead).
pub(crate) fn described_shape(shape: crate::Shape, ops: &[(String, Value)]) -> Option<Value> {
    let mut alternatives = Vec::new();
    edit_paths(shape, &[], "", ops, &mut alternatives);
    (!alternatives.is_empty()).then(|| described_edit(&alternatives, ops))
}

/// The schema a field edit is checked against, with every editable path
/// described: its template, the operations it takes, the value's schema and
/// its title and flags (the description), and one example edit per shape of
/// path (a field, an item's field, an entry) — what an agent builds a field
/// edit from. Each thing is said once: a path's line does not restate the
/// field the path names or a description already stated for it, the step
/// forms are the action's own description, and the operations are what the
/// schema's `op` offers and each line says.
fn described_edit(
    alternatives: &[serde_json::Value],
    ops: &[(String, serde_json::Value)],
) -> serde_json::Value {
    let mut schema = generic_edit_schema(ops);
    let mut lines = vec!["Editable paths:".to_owned()];
    let mut examples = Vec::new();
    let mut shapes: Vec<Vec<String>> = Vec::new();
    let mut said: Vec<String> = Vec::new();
    for alternative in alternatives {
        let at = |pointer: &str| alternative.pointer(pointer).cloned().unwrap_or_default();
        let template = at("/properties/path/examples/0");
        let ops: Vec<String> = at("/properties/op/enum")
            .as_array()
            .map(|ops| {
                ops.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let value = at("/properties/value");
        let text = |key: &str| at(key).as_str().unwrap_or_default().to_owned();
        let kind = value_kind(&value);
        lines.push(format!(
            "- `{}`: {}{}{}",
            text("/title"),
            ops.join(", "),
            beyond_repeated_description(
                &text("/title"),
                &text("/field"),
                &text("/flags"),
                &mut said,
            ),
            kind.map_or_else(String::new, |kind| format!(" ({kind})")),
        ));
        // One example per shape of path: the listing names every path of
        // each shape, and the example shows how a path of it is written.
        let shape: Vec<String> = template
            .as_array()
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|step| step.as_object()?.keys().next().cloned())
                    .collect()
            })
            .unwrap_or_default();
        if shapes.contains(&shape) {
            continue;
        }
        shapes.push(shape);
        let mut example = serde_json::Map::new();
        example.insert("path".into(), template);
        if let Some(op) = ops.first() {
            example.insert("op".into(), op.clone().into());
        }
        examples.push(serde_json::Value::Object(example));
    }
    if let Some(object) = schema.as_object_mut() {
        object.insert("description".into(), lines.join("\n").into());
        object.insert("examples".into(), examples.into());
    }
    schema
}

/// What a path's line says beyond the path and its operations: the field's
/// own words and its flags, less what the listing already tells beside it.
///
/// Which field the path names is in the path, so a title that only restates
/// the path's last step says nothing: `layers.*.name: set — Name` names the
/// field twice. An entry's or cell's line names a field its parent's line
/// describes, so it carries the flags alone; and a description already stated
/// for an earlier path (a record type the record holds twice, as a layer's
/// adjustments and a mark's) is said once.
fn beyond_repeated_description(
    path: &str,
    field: &str,
    flags: &str,
    said: &mut Vec<String>,
) -> String {
    let step = path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .replace('_', " ");
    let entry = path.ends_with(".*");
    let restated = field.trim().eq_ignore_ascii_case(&step);
    let repeated = !entry && said.iter().any(|said| said == field);
    if !entry && !restated && !repeated && !field.is_empty() {
        said.push(field.to_owned());
    }
    let title = if entry || restated || repeated {
        ""
    } else {
        field
    };
    match (title.is_empty(), flags.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!(" — {flags}"),
        (false, true) => format!(" — {title}"),
        (false, false) => format!(" — {title}; {flags}"),
    }
}

/// The kind of value a schema describes, in a word or two, when it says one
/// (`string`, `number`, `one of a, b`): what a path's line in the field-edit
/// listing tells beside its operations. Objects are described by their own
/// paths.
fn value_kind(schema: &serde_json::Value) -> Option<String> {
    if let Some(options) = schema.get("enum").and_then(serde_json::Value::as_array) {
        let options: Vec<String> = options
            .iter()
            .map(|option| {
                option
                    .as_str()
                    .map_or_else(|| option.to_string(), str::to_owned)
            })
            .collect();
        return Some(format!("one of {}", options.join(", ")));
    }
    match schema.get("type")? {
        serde_json::Value::String(kind) if kind != "object" => Some(kind.clone()),
        serde_json::Value::Array(kinds) => Some(
            kinds
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(" or "),
        ),
        _ => None,
    }
}

/// The generic field-edit schema, with the vocabularies of the record's
/// fields' own operations named for `rich`.
fn generic_edit_schema(ops: &[(String, serde_json::Value)]) -> serde_json::Value {
    let ops = ops.to_vec();
    let mut schema = <Edit<()> as crate::JsonSchema>::json_schema();
    if !ops.is_empty()
        && let Some(properties) = schema
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
    {
        // A field type's own vocabulary (rich text's) can outweigh the
        // whole listing: it is summarised by its operations and their
        // arguments, and checked when the edit applies.
        let alternatives = ops
            .into_iter()
            .map(|(path, schema)| {
                serde_json::json!({
                    "description": format!(
                        "An edit of `{path}` in its own vocabulary, one of: {}",
                        crate::schema::summary(&schema)
                    ),
                })
            })
            .collect();
        properties.insert("rich".into(), crate::schema::one_of(alternatives));
    }
    schema
}

fn step_schema(kind: &str, value: serde_json::Value) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert(kind.to_owned(), value);
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": [kind],
    })
}

/// The operations a field of `field`'s type takes, by its stored shape and
/// type.
fn field_ops(field: &crate::FieldShape, rich: bool) -> Vec<&'static str> {
    let ty = field.ty;
    match field.shape {
        crate::Shape::Collection(_) | crate::Shape::Keyed(_) => {
            vec![
                "upsert-item",
                "insert-item",
                "remove-item",
                "move-to",
                "set",
            ]
        }
        _ if rich => vec!["rich"],
        _ if ty.starts_with("Counter") => vec!["add", "set"],
        _ if ty.starts_with("Set<") => vec!["insert", "remove", "set"],
        _ if ty.starts_with("Grid<") => vec![
            "set-cell",
            "insert-row",
            "insert-column",
            "remove-row",
            "remove-column",
        ],
        _ if ty.starts_with("Outbox") => vec!["settle", "forget"],
        crate::Shape::Map(_) => vec!["set", "remove-key"],
        _ if ty == "String" => vec!["set", "splice", "replace"],
        _ => vec!["set"],
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one walk over the stored shape, an alternative per kind of path"
)]
fn edit_paths(
    shape: crate::Shape,
    steps: &[serde_json::Value],
    dotted: &str,
    rich: &[(String, serde_json::Value)],
    out: &mut Vec<serde_json::Value>,
) {
    let alternative = |steps: &[serde_json::Value],
                       open: bool,
                       ops: &[&str],
                       value: serde_json::Value,
                       rich: Option<&serde_json::Value>,
                       title: &str,
                       description: String| {
        // The path as an agent writes it, items by a placeholder id.
        let template: Vec<serde_json::Value> = steps
            .iter()
            .map(|step| {
                let properties = step.get("properties").and_then(serde_json::Value::as_object);
                match properties.and_then(|properties| properties.iter().next()) {
                    Some((kind, value)) if kind == "field" => {
                        serde_json::json!({ "field": value.get("const").cloned().unwrap_or_default() })
                    }
                    Some((kind, _)) => serde_json::json!({ kind.clone(): format!("<{kind}>") }),
                    None => serde_json::Value::Null,
                }
            })
            .collect();
        let dotted: Vec<String> = template
            .iter()
            .map(
                |step| match step.get("field").and_then(serde_json::Value::as_str) {
                    Some(field) => field.to_owned(),
                    None => "*".to_owned(),
                },
            )
            .collect();
        let mut path = serde_json::json!({
            "type": "array",
            "prefixItems": steps,
            "minItems": steps.len(),
            "examples": [template],
        });
        if let Some(path) = path.as_object_mut() {
            if open {
                // What is inside the entry or cell, by any steps.
                path.insert("items".into(), serde_json::json!({ "type": "object" }));
            } else {
                path.insert("maxItems".into(), steps.len().into());
            }
        }
        let mut properties = serde_json::Map::new();
        properties.insert("path".into(), path);
        properties.insert("op".into(), serde_json::json!({ "enum": ops }));
        properties.insert("value".into(), value);
        if ops.contains(&"upsert-item") {
            properties.insert(
                "match".into(),
                serde_json::json!({
                    "type": "object",
                    "description": "upsert-item: the fields that identify the item, e.g. {\"name\": \"Ana\"}; it updates the one item holding them with `value`'s fields, else appends `value` as a new item"
                }),
            );
            properties.insert(
                "at".into(),
                serde_json::json!({
                    "description": "insert-item, move-to: \"end\" (the default), {\"after\": id} or {\"before\": id}",
                    "anyOf": [
                        { "const": "end" },
                        { "type": "object", "properties": { "after": { "type": "string" } }, "required": ["after"] },
                        { "type": "object", "properties": { "before": { "type": "string" } }, "required": ["before"] },
                        { "type": "integer", "minimum": 0 }
                    ]
                }),
            );
        }
        if let Some(rich) = rich {
            properties.insert("rich".into(), rich.clone());
        }
        serde_json::json!({
            "title": dotted.join("."),
            // The field's own words and its flags, apart: the listing decides
            // which of them the line beside the path still has to say.
            "field": title,
            "flags": description,
            "type": "object",
            "properties": properties,
            "required": ["path", "op"],
        })
    };
    if let crate::Shape::Variant(variants) = shape {
        // A variant is a branch an edit names before its fields
        // (`shape.rect.x`), so fields of different variants never share a
        // path even when they share a storage key.
        for variant in variants {
            let mut branch = steps.to_vec();
            branch.push(step_schema(
                "field",
                serde_json::json!({ "const": variant.key }),
            ));
            edit_paths(
                crate::Shape::Record(variant.fields),
                &branch,
                &format!("{dotted}{}.", variant.key),
                rich,
                out,
            );
        }
        return;
    }
    let item = step_schema("item", serde_json::json!({ "type": "string" }));
    // A read-only field is changed only by the record's verbs: no path an
    // edit could take.
    for field in shape.fields().iter().filter(|field| !field.readonly) {
        let mut here = steps.to_vec();
        here.push(step_schema(
            "field",
            serde_json::json!({ "const": field.key }),
        ));
        let path = format!("{dotted}{}", field.key);
        let rich_schema = rich
            .iter()
            .find(|(at, _)| *at == path)
            .map(|(_, schema)| schema);
        let ops = field_ops(field, rich_schema.is_some());
        let collection = matches!(
            field.shape,
            crate::Shape::Collection(_) | crate::Shape::Keyed(_)
        );
        // A value field's `set` takes its value; containers' operations
        // carry items, elements or keys of their own.
        let simple = ops.first() == Some(&"set") && !collection;
        let value = if simple {
            (field.schema)()
        } else {
            serde_json::json!({})
        };
        let mut flags = Vec::new();
        if field.unit {
            flags.push("its own storage unit".to_owned());
        }
        match field.trust {
            crate::Trust::Ordinary => {}
            crate::Trust::Sensitive => flags.push("changing it asks for confirmation".to_owned()),
            crate::Trust::Weakens(_) => flags.push(
                "making it weaker can weaken security: such a change asks for confirmation and needs the person's own grant; making it stricter does not"
                    .to_owned(),
            ),
            crate::Trust::Weakening => flags.push(
                "changing it can weaken security: it asks for confirmation and needs the person's own grant"
                    .to_owned(),
            ),
        }
        if collection {
            // A field edit changes only the record, so Undo takes a removed
            // item back and nobody is asked first.
            flags.push(if field.quiet_remove {
                "removing an item asks nothing".to_owned()
            } else {
                "removing an item asks nothing; Undo takes it back".to_owned()
            });
        }
        let description = flags.join("; ");
        out.push(alternative(
            &here,
            false,
            &ops,
            value,
            rich_schema,
            field.title,
            description,
        ));
        match field.shape {
            crate::Shape::Record(_) | crate::Shape::Variant(_) => {
                edit_paths(field.shape, &here, &format!("{path}."), rich, out);
            }
            crate::Shape::Collection(inner) | crate::Shape::Keyed(inner) => {
                let mut items = here.clone();
                items.push(item.clone());
                let item_value = (field.schema)().get("items").map_or_else(
                    || serde_json::json!({}),
                    |items| {
                        // A collection's JSON lists `{id, value}` entries.
                        items
                            .get("properties")
                            .and_then(|properties| properties.get("value"))
                            .cloned()
                            .unwrap_or_else(|| items.clone())
                    },
                );
                out.push(alternative(
                    &items,
                    false,
                    &["set"],
                    item_value,
                    None,
                    field.title,
                    "one item, by its id".to_owned(),
                ));
                edit_paths(*inner, &items, &format!("{path}.*."), rich, out);
            }
            crate::Shape::Value if field.ty.starts_with("Grid<") => {
                let mut cells = here.clone();
                cells.push(step_schema(
                    "cell",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "row": { "type": "string" },
                            "column": { "type": "string" },
                        },
                        "required": ["row", "column"],
                    }),
                ));
                out.push(alternative(
                    &cells,
                    true,
                    &[
                        "set", "splice", "replace", "add", "insert", "remove", "rich",
                    ],
                    serde_json::json!({}),
                    None,
                    field.title,
                    "a cell, by its row and column ids, and what is inside it".to_owned(),
                ));
            }
            crate::Shape::Map(_) => {
                let mut entries = here.clone();
                entries.push(step_schema("key", serde_json::json!({ "type": "string" })));
                out.push(alternative(
                    &entries,
                    true,
                    &["set", "splice", "replace", "add", "insert", "remove"],
                    serde_json::json!({}),
                    None,
                    field.title,
                    "an entry, by its key, and what is inside it".to_owned(),
                ));
            }
            crate::Shape::Value => {}
        }
    }
}
