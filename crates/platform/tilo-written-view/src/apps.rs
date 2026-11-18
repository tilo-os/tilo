//! The workspace's apps as a written view names them.
//!
//! Every app ships its published surface (`app_fleet::surface`). Read as a package for readings
//! ([`catalogue::published_document`]) it gives a view the app's types, its actions, and the record
//! the app keeps for the workspace as a root (`tasks.tracker`). A view names an app by the last
//! part of its id, as every written view does, with `_` where the id has `-`, which no name in a
//! view holds (`word_processor` for `so.tilo.word-processor`): the package the view reads the app
//! as is named so throughout, and the app's own id ([`App::id`]) is kept for what is asked of the
//! workspace.
//!
//! An app answers a record read with its record as it is stored: a collection as its items by id,
//! each under its place in the order. A view reads a list of records. [`App::reading`] turns the
//! one into the other by what the package declares, and by nothing it guesses.

use std::rc::Rc;

use serde_json::{Map, Value as Json};
use tilolang_tilo::catalogue;
use tilolang_tilo::runtime::ModuleHash;
use tilolang_tilo::tilolang::PackageMeta;

/// One app a view may name.
pub(crate) struct App {
    /// The app's own id: what the app keeps is asked of it, and its actions are sent to it.
    pub(crate) id: String,
    /// The package a view reads the app as, named as a view can write it ([`written`]).
    pub(crate) meta: PackageMeta,
    /// What people call it.
    pub(crate) title: String,
    /// What a view names it by: the last part of its id, as a view can write it.
    pub(crate) alias: String,
    /// What stands for the app's code where the policy decides one of its actions.
    pub(crate) module: ModuleHash,
    /// The record it keeps for the workspace, when it keeps one a view reads.
    pub(crate) root: Option<Root>,
    verbs: Vec<Verb>,
}

/// An app's record of the workspace, as a view names and reads it.
pub(crate) struct Root {
    /// Its name in the app's package (`tracker`).
    pub(crate) name: String,
    /// Its type (`so.tilo.tasks#Tracker`).
    pub(crate) ty: String,
    /// The resource its reading is published as, and its actions are addressed to (`tasks.tracker`).
    pub(crate) published: String,
}

/// A verb as the app's surface declares it: what the app's own API takes.
struct Verb {
    id: String,
    label: String,
    params: Vec<String>,
}

impl App {
    /// The app `id` that people call `title`, from its surface file.
    pub(crate) fn of(id: &str, title: &str, surface: &str) -> Result<Self, String> {
        let named = written(id);
        let meta = catalogue::published_document(&named, title, surface)?;
        let verbs = catalogue::parse_surfaces(surface)?
            .into_iter()
            .flat_map(|surface| surface.verbs)
            .map(|verb| Verb {
                id: verb.id,
                label: verb.label,
                params: verb.params.into_iter().map(|param| param.name).collect(),
            })
            .collect();
        let root = meta.named_roots.iter().find_map(|root| {
            Some(Root {
                name: root.name.clone(),
                ty: root.ty.clone(),
                published: root.published.clone()?,
            })
        });
        Ok(Self {
            id: id.to_owned(),
            title: title.to_owned(),
            alias: named.rsplit('.').next().unwrap_or(&named).to_owned(),
            module: ModuleHash::of_bytes(&meta.to_json_bytes()),
            root,
            verbs,
            meta,
        })
    }

    /// The verb the app's own API takes for the catalogue action `action`.
    pub(crate) fn verb<'a>(&self, action: &'a str) -> &'a str {
        catalogue::published_verb(&self.meta.app, action)
    }

    /// What `action` does, in the app's words: its verb's label, or its id read as words.
    pub(crate) fn label(&self, action: &str) -> String {
        let verb = self.verb(action);
        self.verbs
            .iter()
            .find(|declared| declared.id == verb)
            .map(|declared| declared.label.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| words(verb))
    }

    /// `arguments` under the names the app's verb declares: a view writes a parameter with
    /// underscores where the verb's name has hyphens.
    pub(crate) fn arguments(
        &self,
        action: &str,
        arguments: Map<String, Json>,
    ) -> Map<String, Json> {
        let verb = self.verb(action);
        let Some(declared) = self.verbs.iter().find(|declared| declared.id == verb) else {
            return arguments;
        };
        arguments
            .into_iter()
            .map(|(name, value)| {
                let declared = declared
                    .params
                    .iter()
                    .find(|param| param.replace('-', "_") == name)
                    .cloned()
                    .unwrap_or(name);
                (declared, value)
            })
            .collect()
    }

    /// The app's record `record`, as the app answers a read of it, as the reading a view reads.
    pub(crate) fn reading(&self, record: &Json) -> Json {
        match &self.root {
            Some(root) => normal(&self.meta, &root.ty, record),
            None => record.clone(),
        }
    }

    /// What the app answered a read of one of its documents with, `held`, as the reading a view
    /// reads of the type `ty`.
    pub(crate) fn reading_as(&self, ty: &str, held: &Json) -> Json {
        normal(&self.meta, ty, held)
    }
}

/// The app id `id` as a view can write its parts: a name in a view holds letters, digits and `_`,
/// so an id's `-` is written `_`, as a verb's is where a view calls it.
fn written(id: &str) -> String {
    id.replace('-', "_")
}

/// A verb's id read as words: `delete-area` is "Delete area".
fn words(verb: &str) -> String {
    let spaced = verb.replace(['-', '_'], " ");
    let mut letters = spaced.chars();
    match letters.next() {
        Some(first) => first.to_uppercase().chain(letters).collect(),
        None => spaced,
    }
}

/// `json`, a stored value of the type `ty` of `meta`'s package, as a reading holds it: a
/// collection as the list of its items in their order, each with its id; an enum as its case's
/// name, with the values the case carries where it carries any ([`case`]); a map as its
/// entries by key ([`entries`]); a record as its declared fields under their names.
fn normal(meta: &PackageMeta, ty: &str, json: &Json) -> Json {
    let ty = ty.trim();
    if ty == "RichText" {
        return rich_text(json);
    }
    if let Some(inner) = wrapped(ty, "Option") {
        return if json.is_null() {
            Json::Null
        } else {
            normal(meta, inner, json)
        };
    }
    for list in ["Collection", "List", "Set"] {
        if let Some(inner) = wrapped(ty, list) {
            let items = items(json).into_iter().map(|(id, item)| {
                let mut item = normal(meta, inner, &item);
                if let (Some(id), Json::Object(fields)) = (id, &mut item) {
                    fields.entry("id").or_insert(Json::String(id));
                }
                item
            });
            return Json::Array(items.collect());
        }
    }
    if let Some(inner) = wrapped(ty, "Map") {
        return entries(meta, inner, json);
    }
    if let Some(name) = ty.strip_prefix("enum ") {
        return case(meta, name, json);
    }
    let (Some(declared), Json::Object(held)) = (
        meta.types
            .iter()
            .find(|declared| declared.id == ty || declared.name == ty),
        json,
    ) else {
        return json.clone();
    };
    let mut fields = Map::new();
    for field in &declared.fields {
        let key = field.id.rsplit('.').next().unwrap_or(&field.name);
        if let Some(value) = held.get(key).or_else(|| held.get(&field.name)) {
            fields.insert(field.name.clone(), normal(meta, &field.ty, value));
        }
    }
    Json::Object(fields)
}

/// `json`, a stored value of `meta`'s enum `name`, as a reading holds it: a plain case as its
/// name. A case that carries values is stored as a map, its name under `$variant` beside its
/// values (under `0`, `1` where the case numbers them), and is handed on the same way, each
/// value under the name the package declares for it and as its own type reads.
fn case(meta: &PackageMeta, name: &str, json: &Json) -> Json {
    let Some(variant) = json.get("$variant") else {
        return json.clone();
    };
    let carried = meta
        .enums
        .iter()
        .find(|declared| declared.name == name)
        .and_then(|declared| {
            declared
                .variants
                .iter()
                .find(|held| Some(held.name.as_str()) == variant.as_str())
        })
        .map(|held| held.fields.as_slice())
        .unwrap_or_default();
    if carried.is_empty() {
        return variant.clone();
    }
    let mut spelled = Map::new();
    spelled.insert("$variant".to_owned(), variant.clone());
    for (index, field) in carried.iter().enumerate() {
        let key = field.id.rsplit('.').next().unwrap_or(&field.name);
        let stored = json
            .get(key)
            .or_else(|| json.get(&field.name))
            .or_else(|| json.get(index.to_string()));
        if let Some(value) = stored {
            spelled.insert(key.to_owned(), normal(meta, &field.ty, value));
        }
    }
    Json::Object(spelled)
}

/// `json`, a stored map of `inner`, as a reading holds it: the object of its entries, each
/// value as its own type reads. What is no object is handed on as it is.
fn entries(meta: &PackageMeta, inner: &str, json: &Json) -> Json {
    let Json::Object(held) = json else {
        return json.clone();
    };
    let read = held
        .iter()
        .map(|(key, value)| (key.clone(), normal(meta, inner, value)));
    Json::Object(read.collect())
}

/// The type `wrap<...>` wraps.
fn wrapped<'a>(ty: &'a str, wrap: &str) -> Option<&'a str> {
    ty.strip_prefix(wrap)?.strip_prefix('<')?.strip_suffix('>')
}

/// The items a record keeps under a collection, in their order, each with the id it is kept
/// under: a stored collection is its items by id, each `{k: place, v: item}`, a keyed one its
/// items by key, and a list is a list.
fn items(json: &Json) -> Vec<(Option<String>, Json)> {
    match json {
        Json::Array(items) => items.iter().map(|item| (None, item.clone())).collect(),
        Json::Object(held) => {
            let mut placed: Vec<(String, String, Json)> = held
                .iter()
                .map(|(id, entry)| match (entry.get("k"), entry.get("v")) {
                    (Some(place), Some(item)) => {
                        let place = place
                            .as_str()
                            .map_or_else(|| place.to_string(), str::to_owned);
                        (place, id.clone(), item.clone())
                    }
                    _ => (id.clone(), id.clone(), entry.clone()),
                })
                .collect();
            placed.sort_by(|a, b| a.0.cmp(&b.0));
            placed
                .into_iter()
                .map(|(_, id, item)| (Some(id), item))
                .collect()
        }
        _ => Vec::new(),
    }
}

thread_local! {
    /// Read once: every view on this thread names the same apps.
    static APPS: Rc<[Rc<App>]> = fleet();
}

/// Every app whose surface this build carries.
fn fleet() -> Rc<[Rc<App>]> {
    app_fleet::apps()
        .iter()
        .filter_map(|app| {
            let surface = app_fleet::surface(app.id)?;
            match App::of(app.id, app.name, surface) {
                Ok(app) => Some(Rc::new(app)),
                Err(error) => {
                    tracing::warn!(
                        app = app.id,
                        error,
                        "an app's surface does not read as a package"
                    );
                    None
                }
            }
        })
        .collect::<Vec<_>>()
        .into()
}

/// The apps the view `source` names and its writer listed in `listed`, each once: an app is named
/// by its short name before a dot (`tasks.`), wherever in the definition that stands. Naming more
/// than the view reads only loads a package it does not use; naming less is not possible, since a
/// view reaches an app in no other way. An app the definition names that is not listed is not
/// given to the view: the definition then names something unknown, and says so where it stands.
pub(crate) fn named(source: &str, listed: &[String]) -> Vec<Rc<App>> {
    let named = dotted(source);
    APPS.with(|apps| {
        let mut out: Vec<Rc<App>> = Vec::new();
        for (name, _) in &named {
            if !listed.iter().any(|listed| listed == name) {
                continue;
            }
            let found = apps.iter().find(|app| app.alias == *name);
            if let Some(app) = found
                && !out.iter().any(|held| Rc::ptr_eq(held, app))
            {
                out.push(Rc::clone(app));
            }
        }
        out
    })
}

/// Whether `source` names the record `app` keeps for the workspace (`tasks.tracker`).
pub(crate) fn reads_record(app: &App, source: &str) -> bool {
    app.root.as_ref().is_some_and(|root| {
        dotted(source)
            .iter()
            .any(|(name, member)| *name == app.alias && *member == root.name)
    })
}

/// Every `name.member` in `source`, as written.
fn dotted(source: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let mut before: Option<String> = None;
    let mut after_dot = false;
    let mut word = String::new();
    let mut close = |word: &mut String, before: &mut Option<String>, after_dot: &mut bool| {
        if word.is_empty() {
            return;
        }
        let word = std::mem::take(word);
        if *after_dot && let Some(name) = before.take() {
            pairs.push((name, word.clone()));
        }
        *before = Some(word);
        *after_dot = false;
    };
    for c in source.chars() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            close(&mut word, &mut before, &mut after_dot);
            if c == '.' && before.is_some() {
                after_dot = true;
            } else {
                before = None;
                after_dot = false;
            }
        }
    }
    close(&mut word, &mut before, &mut after_dot);
    pairs
}

/// `json`, a rich text as a read answers it, as a reading holds one: `{"rich": [paragraphs]}`.
///
/// A read says a rich text's body as the list of its paragraphs in their order under `blocks`,
/// each with its id, its kind, its words, its attributes and the paragraphs it holds. The
/// order is the read's: nothing is ordered here. What is no such answer (a text kept as a
/// string, a value that is a rich text to a reading already) is handed on as it is.
fn rich_text(json: &Json) -> Json {
    match json.get("blocks") {
        Some(Json::Array(paragraphs)) if json.get("rich").is_none() => {
            let paragraphs: Vec<Json> = paragraphs.iter().map(paragraph).collect();
            serde_json::json!({ "rich": paragraphs })
        }
        _ => json.clone(),
    }
}

/// One paragraph of a read as the language's rich text keeps one. Its inline marks are no part
/// of a read, so it has none here.
fn paragraph(read: &Json) -> Json {
    let said = |key: &str| read.get(key).and_then(Json::as_str).unwrap_or_default();
    let attrs: Map<String, Json> = read
        .get("attrs")
        .and_then(Json::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| Some((name.clone(), attribute(value)?)))
        .collect();
    let children: Vec<Json> = read
        .get("children")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .map(paragraph)
        .collect();
    serde_json::json!({
        "id": said("id"),
        "kind": said("kind"),
        "text": said("text"),
        "marks": [],
        "attrs": attrs,
        "children": children,
    })
}

/// A paragraph's attribute as the language's rich text keeps one: under the kind of value it
/// is. `None` for what is no attribute's value.
fn attribute(value: &Json) -> Option<Json> {
    match value {
        Json::Bool(flag) => Some(serde_json::json!({ "Bool": flag })),
        Json::String(text) => Some(serde_json::json!({ "Str": text })),
        Json::Number(number) if number.is_i64() => Some(serde_json::json!({ "Int": number })),
        Json::Number(number) => Some(serde_json::json!({ "Float": number.as_f64()? })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilolang_tilo::runtime::value::{Value, from_json};

    const WRITER: &str = "so.tilo.word-processor";

    /// The word processor as a view names it, from the surface this build carries.
    fn writer() -> App {
        let surface = app_fleet::surface(WRITER).expect("the word processor ships its surface");
        App::of(WRITER, "Word Processor", surface).expect("its surface reads as a package")
    }

    #[test]
    fn a_documents_body_reaches_a_view_as_its_paragraphs_in_the_order_the_read_says() {
        // A document of the word processor as a read answers it: the body's paragraphs a list
        // in their order, each with the id it is kept under (here not in the order of the ids).
        let read = serde_json::json!({
            "body": {
                "schema": "tilo.richtext",
                "blocks": [
                    {"id": "p2", "kind": "heading", "text": "A poem", "attrs": {"level": 1}},
                    {"id": "p3", "kind": "paragraph", "text": "Roses are red", "attrs": {}},
                    {"id": "p1", "kind": "list_item", "text": "and so are you",
                     "attrs": {"ordered": false, "indent": 1.5, "style": "body"},
                     "children": [{"id": "c1", "kind": "paragraph", "text": "(held)"}]},
                ],
                "styles": {},
            },
        });
        let reading = writer().reading(&read);
        let body = match from_json(&reading["body"]) {
            Ok(Value::Rich(body)) => body,
            other => panic!("the body is a rich text to a view, not {other:?}: {reading}"),
        };
        assert_eq!(
            body.written_text(),
            "A poem\nRoses are red\nand so are you",
            "the words, a line a paragraph, in the read's order"
        );
        let said: Vec<(&str, &str)> = body
            .blocks
            .iter()
            .map(|paragraph| (paragraph.id.as_str(), paragraph.kind.as_str()))
            .collect();
        assert_eq!(
            said,
            [("p2", "heading"), ("p3", "paragraph"), ("p1", "list_item")]
        );
        // A paragraph keeps its attributes and what it holds.
        assert_eq!(
            reading["body"]["rich"][0]["attrs"],
            serde_json::json!({"level": {"Int": 1}})
        );
        assert_eq!(body.blocks[2].children[0].text, "(held)");
        assert_eq!(body.blocks[2].attrs.len(), 3);
    }

    #[test]
    fn a_body_that_is_no_list_of_paragraphs_is_handed_on_as_it_is() {
        // A text kept as a string, and a value that is a rich text to a reading already.
        for body in [
            serde_json::json!("plain words"),
            serde_json::json!({"rich": [{"id": "", "kind": "paragraph", "text": "as it is",
                "marks": [], "attrs": {}, "children": []}]}),
        ] {
            let reading = writer().reading(&serde_json::json!({ "body": body }));
            assert_eq!(reading["body"], body);
        }
    }
}
