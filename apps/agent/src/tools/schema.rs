//! An operation's arguments in full, written compactly: what a planner's
//! reference of the apps' operations says of each parameter.

use std::fmt::Write as _;

use serde_json::Value;

use crate::surface::Offer;

/// An operation's arguments, a line each after `indent`, with each parameter's JSON Schema written as a
/// compact type (`{region: "Body"|"Header", at: int≥0, ins?: string}`,
/// variants one per line, a description said once) rather than as JSON:
/// the same constraints in a fraction of the tokens.
pub fn argument_lines_compact(offer: &Offer, indent: &str) -> String {
    let mut s = String::new();
    let mut said = Vec::new();
    for p in &offer.descriptor.params {
        let _ = write!(
            s,
            "{indent}{}: {}{} — {}",
            p.name,
            p.ty.label(),
            if p.required { "" } else { " (optional)" },
            p.description
        );
        if !p.choices.is_empty() {
            let choices = p
                .choices
                .iter()
                .map(|choice| format!("{:?}", choice.as_ref()))
                .collect::<Vec<_>>()
                .join("|");
            let _ = write!(s, "; one of {choices}");
        }
        if let Some(schema) = &p.schema {
            let _ = write!(
                s,
                "; shape: {}",
                compact_schema(schema, indent, &mut said, 0)
            );
        }
        s.push('\n');
    }
    s
}

/// The longest description a [`compact_schema`] repeats inline.
const MOST_SCHEMA_NOTE_CHARS: usize = 200;

/// `schema` as a compact type. Variants of a top-level `oneOf` go one per
/// line with their description; a property's description is said the
/// first time only (`said` holds those already said).
fn compact_schema(schema: &Value, indent: &str, said: &mut Vec<String>, depth: usize) -> String {
    if let Some(variants) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        return compact_variants(variants, indent, said, depth);
    }
    if let Some(constant) = schema.get("const") {
        return constant.to_string();
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("|");
    }
    let kind = match schema.get("type") {
        Some(Value::String(kind)) => kind.clone(),
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|"),
        _ => String::new(),
    };
    match kind.as_str() {
        "object" => compact_object(schema, indent, said, depth),
        "array" => compact_array(schema, indent, said, depth),
        "integer" => format!("int{}", schema_bounds(schema)),
        "number" => format!("number{}", schema_bounds(schema)),
        "boolean" => "bool".into(),
        "string" => {
            let mut text = "string".to_owned();
            if let Some(format) = schema.get("format").and_then(Value::as_str) {
                let _ = write!(text, "({format})");
            }
            if let Some(most) = schema.get("maxLength") {
                let _ = write!(text, " ≤{most} chars");
            }
            text
        }
        "" => "any".into(),
        other => other.into(),
    }
}

/// A `oneOf`'s variants: one per line with their description at the top
/// level, joined by `|` inside.
fn compact_variants(
    variants: &[Value],
    indent: &str,
    said: &mut Vec<String>,
    depth: usize,
) -> String {
    let rendered: Vec<String> = variants
        .iter()
        .map(|variant| {
            let body = compact_schema(variant, indent, said, depth.saturating_add(1));
            if depth > 0 {
                return body;
            }
            let described = variant
                .get("description")
                .and_then(Value::as_str)
                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                .map_or_else(String::new, |text| format!(" — {text}"));
            format!("\n{indent}  | {body}{described}")
        })
        .collect();
    if depth == 0 {
        rendered.concat()
    } else {
        rendered.join(" | ")
    }
}

/// An object's fields, `name?` when optional, each description said once;
/// a variant tagged by its one key is `{Tag: {…}}`, as its JSON is.
fn compact_object(schema: &Value, indent: &str, said: &mut Vec<String>, depth: usize) -> String {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return "object".into();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let [(tag, inner)] = properties.iter().collect::<Vec<_>>().as_slice()
        && required.contains(&tag.as_str())
        && tag.starts_with(|c: char| c.is_ascii_uppercase())
    {
        return format!(
            "{{{tag}: {}}}",
            compact_schema(inner, indent, said, depth.saturating_add(1))
        );
    }
    let fields: Vec<String> = properties
        .iter()
        .map(|(name, value)| {
            let optional = if required.contains(&name.as_str()) {
                ""
            } else {
                "?"
            };
            let shape = compact_schema(value, indent, said, depth.saturating_add(1));
            let described = schema_note(value, said);
            format!("{name}{optional}: {shape}{described}")
        })
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// An array as `T[]`, with its length bounds.
fn compact_array(schema: &Value, indent: &str, said: &mut Vec<String>, depth: usize) -> String {
    let items = schema.get("items").map_or_else(
        || "any".to_owned(),
        |items| compact_schema(items, indent, said, depth.saturating_add(1)),
    );
    let mut text = if items.contains(' ') {
        format!("({items})[]")
    } else {
        format!("{items}[]")
    };
    match (schema.get("minItems"), schema.get("maxItems")) {
        (Some(least), Some(most)) if least == most => {
            let _ = write!(text, " of {least}");
        }
        (least, most) => {
            if let Some(least) = least {
                let _ = write!(text, " ≥{least}");
            }
            if let Some(most) = most {
                let _ = write!(text, " ≤{most}");
            }
        }
    }
    text
}

/// A number's bounds, `≥min≤max`.
fn schema_bounds(schema: &Value) -> String {
    let mut text = String::new();
    if let Some(least) = schema.get("minimum") {
        let _ = write!(text, "≥{least}");
    }
    if let Some(most) = schema.get("maximum") {
        let _ = write!(text, "≤{most}");
    }
    text
}

/// A field's description as a comment, the first time it is said and when
/// short enough to repeat.
fn schema_note(value: &Value, said: &mut Vec<String>) -> String {
    let Some(text) = value.get("description").and_then(Value::as_str) else {
        return String::new();
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > MOST_SCHEMA_NOTE_CHARS || said.contains(&text) {
        return String::new();
    }
    said.push(text.clone());
    format!(" /* {text} */")
}
