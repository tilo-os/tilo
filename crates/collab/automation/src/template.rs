//! `{{...}}` interpolation in [`Binding::Template`](crate::Binding::Template).
//!
//! A placeholder is one of the other bindings spelled as text, so a template
//! is checked and evaluated by the same rules as the bindings it contains.

use crate::Binding;

/// One run of literal text or one placeholder.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Piece<'a> {
    Text(&'a str),
    Value(Binding),
}

/// Split `template` into literal text and placeholder bindings.
pub(crate) fn parse(template: &str) -> Result<Vec<Piece<'_>>, String> {
    let mut pieces = Vec::new();
    let mut rest = template;
    while let Some((before, after)) = rest.split_once("{{") {
        if !before.is_empty() {
            pieces.push(Piece::Text(before));
        }
        let (inner, tail) = after
            .split_once("}}")
            .ok_or_else(|| "a `{{` placeholder is never closed".to_string())?;
        pieces.push(Piece::Value(placeholder(inner.trim())?));
        rest = tail;
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest));
    }
    Ok(pieces)
}

fn placeholder(inner: &str) -> Result<Binding, String> {
    let unknown =
        || format!("`{{{{{inner}}}}}` is not `input.NAME`, `var.NAME` or `step.ID[/pointer]`");
    let (source, name) = inner.split_once('.').ok_or_else(unknown)?;
    let binding = match source {
        "input" => Binding::Input(name.to_string()),
        "var" => Binding::Var(name.to_string()),
        "step" => match name.split_once('/') {
            Some((step, pointer)) => Binding::step(step, format!("/{pointer}")),
            None => Binding::step(name, ""),
        },
        _ => return Err(unknown()),
    };
    let named = match &binding {
        Binding::Input(name) | Binding::Var(name) => Binding::named_part(name).0,
        Binding::StepOutput { step: name, .. } => name.as_str(),
        Binding::Literal(_) | Binding::Template(_) => return Err(unknown()),
    };
    if is_identifier(named) {
        Ok(binding)
    } else {
        Err(unknown())
    }
}

/// Names of steps, inputs and variables: ASCII letters, digits, `-`, `_`.
/// Keeping them this small keeps placeholders and step paths unambiguous.
pub(crate) fn is_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// How a value appears inside rendered text.
pub(crate) fn render_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_bindings_between_literal_text() {
        let pieces = parse("Hi {{ input.name }}, {{step.fetch/items/0}}!").unwrap();
        assert_eq!(
            pieces,
            vec![
                Piece::Text("Hi "),
                Piece::Value(Binding::Input("name".into())),
                Piece::Text(", "),
                Piece::Value(Binding::step("fetch", "/items/0")),
                Piece::Text("!"),
            ]
        );
        assert!(parse("{{input.name").is_err());
        assert!(parse("{{secret.x}}").is_err());
        assert!(parse("{{var.a b}}").is_err());
    }
}
