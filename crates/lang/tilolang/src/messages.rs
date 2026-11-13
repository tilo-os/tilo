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
///
/// A key is a Fluent message id, which takes letters, digits, `_` and `-` only; a written view's
/// id carries the `~` of the loop it was made in, so anything else becomes `-`. A catalogue with
/// one invalid key parses as nothing at all, so no app's words would read.
#[must_use]
pub fn key_of(id: &str) -> String {
    let mut key = String::from("d-");
    key.extend(id.chars().map(|c| {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        }
    }));
    key
}

/// The key of the message of the filter `key` a type with the stable id `ty` declares: a
/// filter's own words, which a translation replaces (`d-<ty>-filter-<key>`). It is the filter's
/// own even where it shows a field's label, since `filter watch_later "Watch later"` says
/// something else than the field is called.
#[must_use]
pub fn filter_key(ty: &str, key: &str) -> String {
    key_of(&format!("{ty}-filter-{key}"))
}

/// The key of the message of the parameter `param` of the action with the stable id `action`:
/// what the parameter is called where a person gives it (`d-<action>-<param>`). It is keyed by
/// its name, which an invocation names it by.
#[must_use]
pub fn param_key(action: &str, param: &str) -> String {
    key_of(&format!("{action}-{param}"))
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
/// `Note order`). It is the words a declaration is called by when nothing names it otherwise,
/// and the message catalogue's source for it, which a translation replaces.
#[must_use]
pub fn label(name: &str) -> String {
    let words = in_words(name);
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The keys a catalogue defines: each message (`d-x`) and each attribute (`d-x.doc`).
#[must_use]
pub fn keys(catalogue: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut message: Option<String> = None;
    for line in catalogue.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        let Some((name, _)) = trimmed.split_once(" =") else {
            continue;
        };
        if line.starts_with(char::is_whitespace) {
            if let (Some(attr), Some(message)) = (name.strip_prefix('.'), &message) {
                out.push(format!("{message}.{attr}"));
            }
        } else {
            message = Some(name.to_owned());
            out.push(name.to_owned());
        }
    }
    out
}

/// The one-line word `catalogue` gives `key`, when it has one (`d-so-tilo-lang-today = Heute`): a
/// name a package gives itself, which a manifest carries so every surface reads it.
#[must_use]
pub fn word(catalogue: &str, key: &str) -> Option<String> {
    for line in catalogue.lines() {
        if line.starts_with(char::is_whitespace) || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once(" = ") else {
            continue;
        };
        if name != key {
            continue;
        }
        let value = value.trim();
        // A message spanning lines or placeables is no name.
        if value.is_empty() || value.starts_with('{') || value.ends_with('{') {
            return None;
        }
        return Some(value.to_owned());
    }
    None
}

/// The keys `translation` has that `source` does not: words whose source changed or went, so
/// their translation is never shown.
#[must_use]
pub fn stale_keys(source: &str, translation: &str) -> Vec<String> {
    let known = keys(source);
    keys(translation)
        .into_iter()
        .filter(|key| !known.contains(key))
        .collect()
}

/// Why `words` read as code rather than as the words a person says, if they do: a name keeps
/// what it was written with (`save_draft`, `notes.save()`, `store::notes`), and a word of one
/// letter inside a phrase is the binding a name was built from (`Note editor for t`). A name of
/// one word may be a letter of its own (an axis `x`).
#[must_use]
pub fn reads_as_code(words: &str) -> Option<&'static str> {
    if words.contains('_') && !words.contains(' ') {
        return Some("code identifier");
    }
    if words.contains("::") {
        return Some("code path");
    }
    let words: Vec<&str> = words
        .split(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .filter(|word| !word.is_empty())
        .collect();
    if words.iter().any(|word| word.ends_with("()")) {
        return Some("function call");
    }
    if words.len() > 1 && words.iter().any(|word| one_letter(word)) {
        return Some("code identifier");
    }
    None
}

/// Whether `word` is one letter: a binding a name was built from, or the words English writes
/// with one letter (`a`, `I`).
fn one_letter(word: &str) -> bool {
    let mut chars = word.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_alphabetic())
        && !matches!(word, "a" | "A" | "I")
}

/// The views of `meta` whose name reads as code ([`reads_as_code`]), each with the words it is
/// called by: a view's name is one a person hears (its window's, its bar's), and a part the
/// compiler makes of a loop body is named after the view it stands in, never after its binding.
/// A field's name in words is data (`canvas_x` is "Canvas x", a place), not a name to say.
#[must_use]
pub fn views_named_as_code(meta: &PackageMeta) -> Vec<(String, String)> {
    meta.views
        .iter()
        .map(|view| (view.name.clone(), label(&view.name)))
        .filter(|(_, words)| reads_as_code(words).is_some())
        .collect()
}

/// The package's source catalogue: one message per declaration, in declaration order.
#[must_use]
pub fn source_catalogue(meta: &PackageMeta) -> String {
    let mut out = String::from(
        "# tilolang messages: generated from the package's source, commit it. Translate in\n\
         # locales/<language>.ftl beside it; edit the words in the source, not here.\n",
    );
    if !meta.title.is_empty() {
        message(&mut out, &meta.app, &meta.title, "");
    }
    for ty in &meta.types {
        message(&mut out, &ty.id, &label(&ty.name), &ty.doc);
        attribute(&mut out, "noun", &noun(ty));
        // What a record of it is called while its title is empty, which every surface that
        // names one shows: a type's own words, translated with the rest.
        if let Some(untitled) = &ty.untitled {
            attribute(&mut out, "untitled", &pattern(untitled));
        }
        for field in &ty.fields {
            message(&mut out, &field.id, &label(&field.name), &field.doc);
        }
        for filter in &ty.search.filters {
            // What a filter is, for agents, is its field's doc: the field's message says it.
            named(
                &mut out,
                &filter_key(&ty.id, &filter.key),
                &filter.label,
                "",
            );
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
        // What the action did, as a history entry says it after who did it.
        if let Some(did) = &action.did {
            attribute(&mut out, "did", &pattern(did));
        }
        // What it tells a person, as a notification shows it (`notify`'s title and body).
        for word in meta.words.iter().filter(|word| word.view == action.id) {
            attribute(&mut out, &word.key, &pattern(&word.text));
        }
        // What each argument is called where a person gives it; the subject is the record it
        // runs on, which nobody gives.
        for param in meta.arguments(&action.params) {
            named(
                &mut out,
                &param_key(&action.id, &param.name),
                &param.words(),
                "",
            );
        }
    }
    // The words a `fn` says (`std.say("...")`) are its attributes; a fn that says none is no word
    // a person reads.
    for function in &meta.fns {
        let mut said = meta
            .words
            .iter()
            .filter(|word| word.view == function.id)
            .peekable();
        if said.peek().is_none() {
            continue;
        }
        message(&mut out, &function.id, &label(&function.name), "");
        for word in said {
            attribute(&mut out, &word.key, &pattern(&word.text));
        }
    }
    // A view's doc comment says how it draws, for its author; a person sees its label.
    // The words it shows as written are its attributes (i18n.md §2).
    // A view the compiler made of a loop body is a part of the view it stands in, not a
    // declaration: it takes that view's name and shows its item's own words, so it has no
    // message of its own.
    for view in meta.views.iter().filter(|view| view.row_of.is_none()) {
        message(&mut out, &view.id, &label(&view.name), "");
        for word in meta.words.iter().filter(|word| word.view == view.id) {
            attribute(&mut out, &word.key, &pattern(&word.text));
        }
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
    named(out, &key_of(id), words, doc);
}

/// One message under the key `key`, complete as a catalogue writes it.
fn named(out: &mut String, key: &str, words: &str, doc: &str) {
    let _ = writeln!(out, "\n{key} = {}", pattern(words));
    let description = summary(doc);
    if !description.is_empty() && description != words.trim() {
        attribute(out, "doc", &pattern(&description));
    }
}

/// A doc comment's first sentence as one line: its lines are wrapped source, not sentences.
fn summary(doc: &str) -> String {
    let paragraph = doc
        .lines()
        .map(str::trim)
        .skip_while(|line| line.is_empty())
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut pieces = paragraph.split(". ").peekable();
    let mut sentence = String::new();
    while let Some(piece) = pieces.next() {
        sentence.push_str(piece);
        match pieces.peek() {
            Some(next) if next.starts_with(char::is_lowercase) => sentence.push_str(". "),
            Some(_) => {
                sentence.push('.');
                break;
            }
            None => break,
        }
    }
    sentence
}

fn attribute(out: &mut String, name: &str, value: &str) {
    let _ = writeln!(out, "    .{name} = {value}");
}

/// Words as a Fluent pattern: `{expr}` interpolation becomes a variable named after what it
/// reads (`{t.count}` is `{ $count }`), every other brace a literal.
fn pattern(words: &str) -> String {
    let mut out = String::new();
    let mut rest = words;
    while let Some((before, after)) = rest.split_once('{') {
        out.push_str(&escape(before));
        match after.split_once('}') {
            Some((expr, next)) if interpolation(expr) => {
                let _ = write!(out, "{{ ${} }}", variable(expr));
                rest = next;
            }
            // Braces that do not hold a value are text: a description quotes code as its author
            // wrote it (`{ ... }`), and Fluent would fail the catalogue's whole message on a
            // variable the code sample was read as.
            _ => {
                out.push_str(&escape("{"));
                rest = after;
            }
        }
    }
    out.push_str(&escape(rest));
    if out.starts_with(['[', '*', '.']) {
        out.insert_str(0, "{\"\"}");
    }
    out
}

/// Whether what is between braces reads a value: a path, a call, a name, written without any
/// space (`{t.count}`, `{count()}`, `{n}`).
fn interpolation(expr: &str) -> bool {
    !expr.is_empty()
        && expr
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '(' | ')'))
}

/// What a variable is called: the last name the interpolated expression reads.
fn variable(expr: &str) -> String {
    expr.rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|part| part.chars().next().is_some_and(char::is_alphabetic))
        .unwrap_or("value")
        .to_owned()
}

/// Text Fluent takes literally: braces as quoted literals, one line. One pass over the text:
/// replacing in two passes would escape the `}` of the `{` it just wrote.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '{' => out.push_str("{\"{\"}"),
            '}' => out.push_str("{\"}\"}"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        pattern, reads_as_code, refined_blocks, source_catalogue, stale_keys, summary, variable,
        views_named_as_code,
    };
    use crate::meta::{PackageMeta, ViewMeta};

    /// A word a screen reader announces is words; a name that keeps an identifier is not, and a
    /// binding a name was built from is one letter read on its own.
    #[test]
    fn words_that_read_as_code_are_not_names() {
        for words in [
            "Note editor for t",
            "save_draft",
            "notes.save()",
            "store::notes",
        ] {
            assert!(reads_as_code(words).is_some(), "{words}");
        }
        for words in ["Note editor", "Save draft", "X", "A note", "I pinned this"] {
            assert!(reads_as_code(words).is_none(), "{words}");
            assert!(views_named_as_code(&alone(words)).is_empty(), "{words}");
        }
    }

    /// What an action tells a person is words a person reads: `notify`'s title and body are
    /// messages of the action, so a catalogue translates them as it translates a view's.
    #[test]
    fn what_an_action_notifies_is_a_message_of_the_action() {
        let mut meta = PackageMeta::default();
        let action = serde_json::from_value(serde_json::json!({
            "id": "7vjyuy7c",
            "name": "remind",
            "doc": "",
            "params": [],
            "class": "undoable",
            "reaches": [],
            "label": "Remind",
        }))
        .expect("an action");
        meta.actions.push(action);
        meta.words.push(crate::meta::WordMeta {
            view: "7vjyuy7c".to_owned(),
            key: "w-due".to_owned(),
            text: "Due {d}".to_owned(),
        });
        let catalogue = source_catalogue(&meta);
        assert!(catalogue.contains("d-7vjyuy7c = Remind"), "{catalogue}");
        assert!(catalogue.contains(".w-due = Due { $d }"), "{catalogue}");
    }

    /// A view the compiler makes of a loop body is a part of the view it stands in: it has no
    /// words of its own, so the catalogue never names its binding.
    #[test]
    fn a_part_a_view_composes_has_no_words_of_its_own() {
        let mut meta = alone("Note editor");
        meta.views.push(ViewMeta {
            id: "editor~row0".to_owned(),
            row_of: Some("editor".to_owned()),
            ..alone("Note editor").views[0].clone()
        });
        let catalogue = source_catalogue(&meta);
        assert!(catalogue.contains("= Note editor"), "{catalogue}");
        assert!(!catalogue.contains("editor-row0"), "{catalogue}");
        assert!(!catalogue.contains(" for "), "{catalogue}");
        // A part named after its binding is a name the compiler must never write.
        meta.views[1].name = "Note editor for t".to_owned();
        assert_eq!(
            views_named_as_code(&meta).len(),
            1,
            "{:?}",
            views_named_as_code(&meta)
        );
    }

    /// A package whose one view is called `name`.
    fn alone(name: &str) -> PackageMeta {
        let mut meta = PackageMeta::default();
        meta.views.push(ViewMeta {
            id: "editor".to_owned(),
            name: name.to_owned(),
            doc: String::new(),
            params: Vec::new(),
            anchors: Vec::new(),
            reaches: Vec::new(),
            state: Vec::new(),
            presents: None,
            row_of: None,
            show: None,
        });
        meta
    }

    /// A translation of words the source no longer has is reported by its key; a current one
    /// is not.
    #[test]
    fn a_translation_of_words_gone_from_the_source_is_stale() {
        let source = "d-a = Inbox\n    .w-new = { $n } new\n\nd-b = Notes\n";
        let translation =
            "d-a = Eingang\n    .w-new = { $n } neu\n    .w-unread = ungelesen\n\nd-gone = Weg\n";
        assert_eq!(stale_keys(source, translation), ["d-a.w-unread", "d-gone"]);
        assert!(stale_keys(source, "# a comment\nd-b = Notizen\n").is_empty());
    }

    #[test]
    fn a_description_is_the_first_sentence_unwrapped() {
        assert_eq!(
            summary("The top bar: the parts\nin their order.\n\nMore."),
            "The top bar: the parts in their order."
        );
        assert_eq!(
            summary("One dock item, e.g. a tile. Its badge sits on it."),
            "One dock item, e.g. a tile."
        );
        assert_eq!(summary(""), "");
    }

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

    #[test]
    fn a_code_sample_in_a_description_is_not_a_variable() {
        assert_eq!(
            pattern("Declared one (`view open_threads(m: mail.Mail) { ... }`)"),
            "Declared one (`view open_threads(m: mail.Mail) {\"{\"} ... {\"}\"}`)"
        );
        assert_eq!(
            pattern("Then `{a.b}` is a value and `{ }` is not."),
            "Then `{ $b }` is a value and `{\"{\"} {\"}\"}` is not."
        );
    }
}
