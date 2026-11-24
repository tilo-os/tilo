//! A package's words as a Fluent catalogue (docs/designs/app-language/i18n.md).
//!
//! Every word a package shows about what it declares is a message keyed by the declaration's stable
//! id, so renaming a declaration keeps its translations as it keeps its data. The package's source
//! catalogue is written from its sources at build time and travels in its module; a translation is
//! a file, `locales/<language>.ftl`.

use std::fmt::Write as _;

use crate::meta::{PackageMeta, TypeMeta, in_words};

pub const SOURCE_LOCALE: &str = "en-US";

pub const LOCALES_DIR: &str = "locales";

/// The key of the message of the declaration with the stable id `id`. A key is a Fluent message id:
/// letters, digits, `_` and `-` only, so the `~` of a written view's id becomes `-`. A catalogue
/// with one invalid key parses as nothing at all, so no app's words would read.
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

/// The key of the message naming the other words people know `app` by.
#[must_use]
pub fn keywords_key(app: &str) -> String {
    key_of(&format!("{app}-keywords"))
}

/// The words people know `app` by in `catalogue`, split from its `d-<app>-keywords` message:
/// none when the catalogue does not translate them.
#[must_use]
pub fn keywords_word(catalogue: &str, app: &str) -> Option<Vec<String>> {
    let words = word(catalogue, &keywords_key(app))?;
    let words: Vec<String> = words
        .split(',')
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    (!words.is_empty()).then_some(words)
}

/// The key of the message naming what `app` does in one phrase, its tagline (`d-<app>-tagline`):
/// the phrase its header gives in the source language, and a language's own in a translation.
#[must_use]
pub fn tagline_key(app: &str) -> String {
    key_of(&format!("{app}-tagline"))
}

/// The one-line phrase `catalogue` says `app` does, from its `d-<app>-tagline` message: none when
/// the catalogue does not translate it, and the package's header tagline stands.
#[must_use]
pub fn tagline_word(catalogue: &str, app: &str) -> Option<String> {
    word(catalogue, &tagline_key(app))
}

/// The key of the message of the filter `key` a type with the stable id `ty` declares
/// (`d-<ty>-filter-<key>`), which a translation replaces.
#[must_use]
pub fn filter_key(ty: &str, key: &str) -> String {
    key_of(&format!("{ty}-filter-{key}"))
}

/// The key of the message of the parameter `param` of the action `action` (`d-<action>-<param>`).
#[must_use]
pub fn param_key(action: &str, param: &str) -> String {
    key_of(&format!("{action}-{param}"))
}

/// A declared name as a label: in words, its first letter upper case (`NoteOrder` is `Note order`).
/// The resource type in an action's lock name is not part of its words: the view presenting
/// `join@Wifi` is called as `join`'s is.
#[must_use]
pub fn label(name: &str) -> String {
    let words = match name.split_once('@') {
        Some((verb, on)) => match on.split_once('~') {
            Some((_, rest)) => in_words(&format!("{verb}~{rest}")),
            None => in_words(verb),
        },
        None => in_words(name),
    };
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

/// The one-line word `catalogue` gives `key`, when it has one (`d-so-tilo-lang-today = Heute`): a name
/// a package gives itself.
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

fn keys_only_in(one: &str, two: &str) -> Vec<String> {
    let other = keys(two);
    keys(one)
        .into_iter()
        .filter(|key| !other.contains(key))
        .collect()
}

/// The keys `translation` has that `source` does not, so their translation is never shown.
#[must_use]
pub fn stale_keys(source: &str, translation: &str) -> Vec<String> {
    keys_only_in(translation, source)
}

/// The keys `translation` has that `source` does not and that are not a language's own word for
/// `app`: its tagline has no source original, because the header says it in the source language.
#[must_use]
pub fn stale_keys_for(app: &str, source: &str, translation: &str) -> Vec<String> {
    let tagline = tagline_key(app);
    stale_keys(source, translation)
        .into_iter()
        .filter(|key| *key != tagline)
        .collect()
}

/// Why `words` read as code rather than as words a person says, if they do: a name keeps what it was
/// written with (`save_draft`, `notes.save()`, `store::notes`), and a one-letter word inside a phrase
/// is the binding a name was built from.
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

fn one_letter(word: &str) -> bool {
    let mut chars = word.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_alphabetic())
        && !matches!(word, "a" | "A" | "I")
}

/// The views of `meta` whose name reads as code, each with the words it is called by. A part the
/// compiler makes of a loop body is named after the view it stands in, never after its binding.
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
        "# tilolang messages: generated from the package's source. Translate in\n\
         # locales/<language>.ftl beside it; edit the words in the source, not here.\n",
    );
    if !meta.title.is_empty() {
        message(&mut out, &meta.app, &meta.title, "");
    }
    // The other words people know the app by, which search matches: one message a translation
    // replaces, so each language names the app in its own words.
    if !meta.keywords.is_empty() {
        named(
            &mut out,
            &keywords_key(&meta.app),
            &meta.keywords.join(", "),
            "",
        );
    }
    for ty in &meta.types {
        message(&mut out, &ty.id, &ty.label(), &ty.doc);
        attribute(&mut out, "noun", &noun(ty));
        // What a record of it is called while its title is empty, which every surface that names one
        // shows.
        if let Some(untitled) = &ty.untitled {
            attribute(&mut out, "untitled", &pattern(untitled));
        }
        for field in &ty.fields {
            message(&mut out, &field.id, &label(&field.name), &field.doc);
        }
        for filter in &ty.search.filters {
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
            let words = match &action.did_many {
                Some(many) => counted(&pattern(did), &pattern(many)),
                None => pattern(did),
            };
            attribute(&mut out, "did", &words);
        }
        // What the person types where they answer its ask, each labelled.
        for (n, take) in action.takes.iter().enumerate() {
            attribute(
                &mut out,
                &crate::meta::take_key(n + 1),
                &pattern(&take.label),
            );
        }
        // What it tells a person, as a notification shows it.
        for word in meta.words.iter().filter(|word| word.view == action.id) {
            attribute(&mut out, &word.key, &pattern(&word.text));
        }
        // What each argument is called where a person gives it; the target is the record it runs on.
        for param in meta.arguments(&action.params) {
            named(
                &mut out,
                &param_key(&action.id, &param.name),
                &param.words(),
                "",
            );
        }
    }
    // The words a `fn` says (`std.say("...")`) are its attributes.
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
    // A view's doc comment says how it draws, for its author; a person sees its label. The words it
    // shows as written are its attributes (i18n.md §2). A view the compiler made of a loop body takes
    // that view's name, so it has no message of its own.
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
    counted(&escape(&ty.noun()), &escape(&ty.plural()))
}

/// `one` and `several` as one Fluent message that picks by `$count`.
fn counted(one: &str, several: &str) -> String {
    format!("{{ $count ->\n        [one] {one}\n       *[other] {several}\n    }}")
}

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

/// Words as a Fluent pattern: `{expr}` becomes a variable named after what it reads (`{t.count}` is
/// `{ $count }`), every other brace a literal.
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
            // Braces that do not hold a value are text: a description quotes code as its author wrote
            // it.
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

/// Whether what is between braces reads a value: a path, a call, a name, written without any space.
fn interpolation(expr: &str) -> bool {
    !expr.is_empty()
        && expr
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '(' | ')'))
}

fn variable(expr: &str) -> String {
    expr.rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|part| part.chars().next().is_some_and(char::is_alphabetic))
        .unwrap_or("value")
        .to_owned()
}

/// Text Fluent takes literally: braces as quoted literals, one line. One pass over the text: replacing
/// in two passes would escape the `}` of the `{` it just wrote.
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
        pattern, reads_as_code, source_catalogue, stale_keys, stale_keys_for, summary, tagline_key,
        tagline_word, variable, views_named_as_code,
    };
    use crate::meta::{PackageMeta, ViewMeta};

    tilo_test_env::scenario! {
        /// The catalogue of a package's messages: which words are messages, and what a translation of them keeps.
        fn a_packages_messages_are_its_words_and_their_translations;

        phase words_that_read_as_code_are_not_names;
        {
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

        phase what_an_action_notifies_is_a_message_of_the_action;
        {
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

        phase a_types_noun_is_one_of_it_and_several;
        {
            let mut meta = PackageMeta::default();
            for (id, name) in [("t1", "Tasks"), ("t2", "Person"), ("t3", "Today")] {
                meta.types.push(
                    serde_json::from_value(serde_json::json!({
                        "id": id,
                        "name": name,
                        "doc": "",
                        "fields": [],
                    }))
                    .expect("a type"),
                );
            }
            let catalogue = source_catalogue(&meta);
            for noun in [
                "[one] task\n       *[other] tasks",
                "[one] person\n       *[other] people",
                "[one] today\n       *[other] today",
            ] {
                assert!(catalogue.contains(noun), "{noun} of\n{catalogue}");
            }
        }

        phase a_part_a_view_composes_has_no_words_of_its_own;
        {
            let mut meta = alone("Note editor");
            meta.views.push(ViewMeta {
                id: "editor~row0".to_owned(),
                window: None,
                row_of: Some("editor".to_owned()),
                ..alone("Note editor").views[0].clone()
            });
            let catalogue = source_catalogue(&meta);
            assert!(catalogue.contains("= Note editor"), "{catalogue}");
            assert!(!catalogue.contains("editor-row0"), "{catalogue}");
            assert!(!catalogue.contains(" for "), "{catalogue}");
            meta.views[1].name = "Note editor for t".to_owned();
            assert_eq!(
                views_named_as_code(&meta).len(),
                1,
                "{:?}",
                views_named_as_code(&meta)
            );
        }

        phase a_translation_of_words_gone_from_the_source_is_stale;
        {
            let source = "d-a = Inbox\n    .w-new = { $n } new\n\nd-b = Notes\n";
            let translation =
                "d-a = Eingang\n    .w-new = { $n } neu\n    .w-unread = ungelesen\n\nd-gone = Weg\n";
            assert_eq!(stale_keys(source, translation), ["d-a.w-unread", "d-gone"]);
            assert!(stale_keys(source, "# a comment\nd-b = Notizen\n").is_empty());
        }

        phase a_language_names_the_tagline_it_has_no_source_for;
        {
            let app = "so.tilo.notes";
            assert_eq!(tagline_key(app), "d-so-tilo-notes-tagline");
            let source = "d-so-tilo-notes = Notes\n\nd-n1 = Note\n";
            let translation = concat!(
                "d-so-tilo-notes = Notizen\n",
                "d-so-tilo-notes-tagline = Notizen, allein oder zusammen\n",
                "d-gone = Weg\n",
            );
            assert_eq!(
                tagline_word(translation, app).as_deref(),
                Some("Notizen, allein oder zusammen")
            );
            assert_eq!(tagline_word(source, app), None);
            // The tagline is a word of the language, not a translation of a word gone from the
            // source; a key the source really lost is still stale.
            assert_eq!(stale_keys_for(app, source, translation), ["d-gone"]);
        }

        phase a_description_is_the_first_sentence_unwrapped;
        {
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

        phase interpolation_becomes_a_named_variable_and_braces_stay_literal;
        {
            assert_eq!(pattern("Open {item.title}"), "Open { $title }");
            assert_eq!(pattern("{n} new"), "{ $n } new");
            assert_eq!(variable("count()"), "count");
            assert_eq!(pattern("a } b"), "a {\"}\"} b");
            assert_eq!(pattern("[draft]"), "{\"\"}[draft]");
        }

        phase a_code_sample_in_a_description_is_not_a_variable;
        {
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

    fn alone(name: &str) -> PackageMeta {
        let mut meta = PackageMeta::default();
        meta.views.push(ViewMeta {
            id: "editor".to_owned(),
            name: name.to_owned(),
            window: None,
            doc: String::new(),
            params: Vec::new(),
            anchors: Vec::new(),
            calls: Vec::new(),
            reaches: Vec::new(),
            state: Vec::new(),
            presents: None,
            row_of: None,
            shows: Vec::new(),
            capture_protected: false,
        });
        meta
    }
}
