//! A package's words as a Fluent catalogue (docs/designs/app-language/i18n.md).
//!
//! Every word a package shows about what it declares (a type's name and noun, a field's, an
//! action's label, an enum case's, each description) is a message keyed by the declaration's
//! stable id, so renaming a declaration keeps its translations as it keeps its data. The
//! compiler writes the package's source catalogue from its source, `locales/en-US.ftl`, the way
//! it writes `app.ids`; translations sit beside it and may refine any message with Fluent's
//! selectors (an irregular plural, a gender, another word order). The source catalogue may be
//! refined the same way: a message under a `# refined` comment is the author's, and writing the
//! catalogue again keeps it as written.

use std::fmt::Write as _;

use crate::meta::{PackageMeta, TypeMeta, in_words, plural};

/// The language a package's source is written in, and the name of its source catalogue.
pub const SOURCE_LOCALE: &str = "en-US";

/// The directory a package keeps its catalogues in.
pub const LOCALES_DIR: &str = "locales";

/// The key of the message of the declaration with the stable id `id`.
#[must_use]
pub fn key_of(id: &str) -> String {
    format!("d-{id}")
}

/// The comment that marks a message of the source catalogue as the author's own.
pub const REFINED: &str = "# refined";

/// The package's source catalogue as its source says it reads, keeping every message of
/// `committed` (the catalogue on disk) that its author marked refined.
#[must_use]
pub fn source_catalogue_keeping(meta: &PackageMeta, committed: &str) -> String {
    let refined = refined_blocks(committed);
    let generated = source_catalogue(meta);
    if refined.is_empty() {
        return generated;
    }
    let mut out = String::new();
    for block in generated.split("\n\n") {
        let key = block
            .lines()
            .find(|line| line.starts_with("d-"))
            .and_then(|line| line.split(" = ").next());
        match key.and_then(|key| refined.iter().find(|(k, _)| k == key)) {
            Some((_, kept)) => out.push_str(kept),
            None => out.push_str(block),
        }
        out.push_str("\n\n");
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// The messages of `catalogue` under a [`REFINED`] comment, by key, each with its comment.
fn refined_blocks(catalogue: &str) -> Vec<(String, String)> {
    catalogue
        .split("\n\n")
        .filter(|block| block.lines().any(|line| line.trim() == REFINED))
        .filter_map(|block| {
            let key = block
                .lines()
                .find(|line| line.starts_with("d-"))?
                .split(" = ")
                .next()?
                .to_owned();
            Some((key, block.trim_matches('\n').to_owned()))
        })
        .collect()
}

/// A declared name as a label: in words, its first letter upper case (`NoteOrder` is
/// `Note order`).
fn label(name: &str) -> String {
    let words = in_words(name);
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The package's source catalogue: one message per declaration, in declaration order.
#[must_use]
pub fn source_catalogue(meta: &PackageMeta) -> String {
    let mut out = String::from(
        "# tilolang messages: generated from the package's source, commit it. Translate in\n\
         # locales/<language>.ftl beside it; edit the words in the source, not here.\n",
    );
    for ty in &meta.types {
        message(&mut out, &ty.id, &label(&ty.name), &ty.doc);
        attribute(&mut out, "noun", &noun(ty));
        for field in &ty.fields {
            message(&mut out, &field.id, &label(&field.name), &field.doc);
        }
    }
    for en in &meta.enums {
        message(&mut out, &en.id, &label(&en.name), &en.doc);
        for case in &en.variants {
            message(&mut out, &case.id, &case.label, &case.doc);
        }
    }
    for action in meta.actions.iter().filter(|action| !action.scenario) {
        message(&mut out, &action.id, &action.label, &action.doc);
    }
    for view in &meta.views {
        message(&mut out, &view.id, &label(&view.name), &view.doc);
    }
    out
}

/// `{ $count -> [one] note *[other] notes }`: what one of the type is called, and several.
fn noun(ty: &TypeMeta) -> String {
    let one = ty.noun();
    let many = plural(&one);
    format!(
        "{{ $count ->\n        [one] {}\n       *[other] {}\n    }}",
        escape(&one),
        escape(&many)
    )
}

/// One message: its words, and its description as an attribute when it has one.
fn message(out: &mut String, id: &str, words: &str, doc: &str) {
    let _ = writeln!(out, "\n{} = {}", key_of(id), pattern(words));
    let description = doc.lines().next().unwrap_or_default().trim();
    if !description.is_empty() && description != words.trim() {
        attribute(out, "doc", &pattern(description));
    }
}

fn attribute(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "    .{name} = {value}");
}

/// Words as a Fluent pattern: `{expr}` interpolation becomes a variable named after what it
/// reads (`{t.count}` is `{ $count }`), every other brace a literal.
fn pattern(words: &str) -> String {
    let mut out = String::new();
    let mut rest = words;
    while let Some(open) = rest.find('{') {
        out.push_str(&escape(&rest[..open]));
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let _ = write!(out, "{{ ${} }}", variable(&after[..close]));
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&escape(&rest[open..]));
                rest = "";
            }
        }
    }
    out.push_str(&escape(rest));
    if out.starts_with(['[', '*', '.']) {
        out.insert_str(0, "{\"\"}");
    }
    out
}

/// What a variable is called: the last name the interpolated expression reads.
fn variable(expr: &str) -> String {
    let name: String = expr
        .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|part| part.chars().next().is_some_and(char::is_alphabetic))
        .unwrap_or("value")
        .to_owned();
    name
}

/// Text Fluent takes literally: braces as quoted literals, one line.
fn escape(text: &str) -> String {
    text.replace('{', "{\"{\"}")
        .replace('}', "{\"}\"}")
        .replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::{pattern, refined_blocks, variable};

    #[test]
    fn a_refined_message_is_the_authors() {
        let committed = "# header\n\n# refined\nd-abc = Notes\n    .noun = { $count ->\n        [one] notebook\n       *[other] notebooks\n    }\n\nd-def = Title\n";
        let kept = refined_blocks(committed);
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].0, "d-abc");
        assert!(kept[0].1.contains("notebooks"));
    }

    #[test]
    fn interpolation_becomes_a_named_variable_and_braces_stay_literal() {
        assert_eq!(pattern("Open {item.title}"), "Open { $title }");
        assert_eq!(pattern("{n} new"), "{ $n } new");
        assert_eq!(variable("count()"), "count");
        assert_eq!(pattern("a } b"), "a {\"}\"} b");
        assert_eq!(pattern("[draft]"), "{\"\"}[draft]");
    }
}
