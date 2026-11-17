//! The workspace's apps as a written view names them.
//!
//! Every app ships its published surface (`app_fleet::surface`). Read as a package for readings
//! ([`catalogue::published_record`]) it gives a view the app's types, its actions, and the record
//! the app keeps for the workspace as a root (`tasks.tracker`). A view names an app by the last
//! part of its id, as every written view does.
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
    pub(crate) meta: PackageMeta,
    /// What people call it.
    pub(crate) title: String,
    /// What a view names it by: the last part of its id.
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
        let meta = catalogue::published_record(id, title, surface)?;
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
            title: title.to_owned(),
            alias: id.rsplit('.').next().unwrap_or(id).to_owned(),
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
/// name; a record as its declared fields under their names.
fn normal(meta: &PackageMeta, ty: &str, json: &Json) -> Json {
    let ty = ty.trim();
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
    if ty.starts_with("enum ") {
        return json.get("$variant").unwrap_or(json).clone();
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

/// The apps the view `source` names, each once: an app is named by its short name before a dot
/// (`tasks.`), wherever in the definition that stands. Naming more than the view reads only loads
/// a package it does not use; naming less is not possible, since a view reaches an app in no
/// other way.
pub(crate) fn named(source: &str) -> Vec<Rc<App>> {
    let named = dotted(source);
    APPS.with(|apps| {
        let mut out: Vec<Rc<App>> = Vec::new();
        for (name, _) in &named {
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
