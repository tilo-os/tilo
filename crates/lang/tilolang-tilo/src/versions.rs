//! The versions of a published record: every version of a published resource and of the
//! resources published in it, each as the app's history lists it.

use serde_json::Value as Json;
use tilo_actions::ResourceSpec;
/// The words [`say_versions`] reads: an app's catalogue in the reader's language over its source.
pub use tilo_history::{Catalogue, Messages};
use tilolang_runtime::store::{Store as _, Version};
use tilolang_runtime::value::ResourceRef;

use crate::host::Host;
use crate::naming::resource_id;
use crate::publish::split_published;

/// `version`, of a resource of `app` that `host` reads, as its history lists it: the version,
/// its resource by the id it is published under, the label the app's history calls it by, and
/// what it did to the resource, and to each of its fields, with the values `reader` may see.
#[must_use]
pub fn version_entry(
    host: &Host,
    app: &str,
    version: &Version,
    reader: Option<&tilolang_runtime::Actor>,
) -> Json {
    let published = resource_id(app, &version.resource);
    let label = history_label(host, &version.resource);
    let mut entry = serde_json::to_value(version).unwrap_or_default();
    if let Some(entry) = entry.as_object_mut() {
        entry.insert("resource".to_owned(), Json::String(published));
        entry.insert("label".to_owned(), Json::String(label));
        // What the version did to the resource it is a version of, read from the store's own
        // kind, beside the change's id.
        entry.insert(
            "happened".to_owned(),
            Json::String(version.kind.what().word().to_owned()),
        );
        // The arguments the words its action declares for what it did read, by name: what
        // history says the change with. The others stay out of the entry, so a value only the
        // action took (a password, a message) is never listed beside the change.
        let said = host.runtime().did_args(version);
        if said.is_empty() {
            entry.remove("args");
        } else {
            entry.insert("args".to_owned(), Json::Object(said.into_iter().collect()));
        }
        // What the change did to each field, with the values `reader` may see of it: none for a
        // feed read by several, whose entry says each change to all of them alike.
        let written = reader
            .map(|reader| host.runtime().field_changes(reader, version))
            .unwrap_or_default();
        // The message each changed field is labelled by, so history names it in the reader's
        // language.
        let labels: serde_json::Map<String, Json> = written
            .iter()
            .map(|change| {
                let key = tilolang::messages::key_of(&change.id);
                (change.field.clone(), key.into())
            })
            .collect();
        let changes: Vec<Json> = written
            .into_iter()
            .map(|change| {
                let mut said = serde_json::Map::new();
                said.insert("field".to_owned(), change.field.into());
                if let Some(before) = change.before {
                    said.insert("before".to_owned(), before);
                }
                if let Some(after) = change.after {
                    said.insert("after".to_owned(), after);
                }
                if change.secret {
                    said.insert("secret".to_owned(), true.into());
                }
                Json::Object(said)
            })
            .collect();
        if !changes.is_empty() {
            entry.insert("changes".to_owned(), Json::Array(changes));
            entry.insert("field_labels".to_owned(), Json::Object(labels));
        }
        if host.runtime().declares_did(&version.action) {
            entry.insert("declared".to_owned(), true.into());
        }
    }
    entry
}

/// The words of the actions a window publishes, as history reads a step of one: each action's
/// label, and what the app says it did, read in the reader's language with the step's own
/// arguments (`did: if on { "turned a rule on" } else { "turned a rule off" }`, the action's
/// `d-<id>.did` message), so a translation picks its own words by them too.
struct Declared<'a> {
    labels: &'a tilo_history::ActionLabels,
    words: &'a dyn tilo_history::Messages,
}

impl tilo_history::Labels for Declared<'_> {
    fn action(&self, key: &tilo_actions::ActionKey) -> Option<String> {
        // The action's own message is its label in the reader's language, whether or not the
        // window publishes it.
        self.words
            .format(
                &tilolang::messages::key_of(&key.action),
                &tilo_history::FluentArgs::new(),
            )
            .or_else(|| tilo_history::Labels::action(self.labels, key))
    }

    fn did(
        &self,
        key: &tilo_actions::ActionKey,
        args: &tilo_history::FluentArgs<'_>,
    ) -> Option<String> {
        // A change names its action by the id it declares it under, the key of its words.
        let words = format!("{}.did", tilolang::messages::key_of(&key.action));
        self.words
            .format(&words, args)
            .or_else(|| tilo_history::Labels::did(self.labels, key, args))
    }
}

/// Give each version `history` lists (as [`version_entry`] writes each) the sentence it reads as
/// in `locale`, with the app's words from `words`: what was done, in the words its action declares
/// for it and with the arguments they read, else by the label its action is published under in
/// `resources`, to the resource by the name history calls it ("Turned a rule off “https”", "Ran
/// “New note” on Untitled", "Created Notebook"), never by an id.
pub fn say_versions(
    history: &mut Json,
    resources: &[ResourceSpec],
    words: &dyn tilo_history::Messages,
    locale: &str,
) {
    let mut labels = tilo_history::ActionLabels::default();
    labels.learn(resources);
    let declared = Declared {
        labels: &labels,
        words,
    };
    let reader = tilo_history::Reader::new(locale).with_labels(&declared);
    let Some(versions) = history.get_mut("versions").and_then(Json::as_array_mut) else {
        return;
    };
    for version in versions {
        let text = |key: &str| version[key].as_str().unwrap_or_default().to_owned();
        let (resource, action, label) = (text("resource"), text("action"), text("label"));
        let named = version["named"].as_str().map(str::to_owned);
        let app = crate::publish::app_of(&resource)
            .unwrap_or_default()
            .to_owned();
        // A checkpoint or a restore is a step through history, said as history says it.
        let step = match (action.as_str(), named) {
            ("checkpoint", Some(name)) => Some(tilo_actions::HistoryOp::Checkpoint { name }),
            ("restore", _) => Some(tilo_actions::HistoryOp::Restore),
            _ => None,
        };
        // The arguments the action's words read, as the entry names them.
        let mut args: std::collections::BTreeMap<String, tilo_actions::Scalar> = version["args"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| {
                Some((name.clone(), tilo_actions::Scalar::from_json(value)?))
            })
            .collect();
        // Each field as the change found and left it, where the reader may see it: what
        // `{limit.before}` and `{limit.after}` read.
        let changes = version["changes"].as_array().cloned().unwrap_or_default();
        for change in &changes {
            let field = change["field"].as_str().unwrap_or_default();
            for when in ["before", "after"] {
                if let Some(value) = tilo_actions::Scalar::from_json(&change[when]) {
                    args.insert(format!("{field}_{when}"), value);
                }
            }
        }
        // An action that declares no words for what it did, which changed one field, says that
        // field's change ("Changed Weekly of Policy to 5"; a secret one only that it changed).
        let declared = version["declared"].as_bool().unwrap_or(false);
        let edit = match changes.as_slice() {
            [change] if !declared && step.is_none() && action != "$create" => {
                let value = |when: &str| tilo_actions::Scalar::from_json(&change[when]);
                let field = change["field"].as_str().unwrap_or_default();
                // The field by its label in the reader's language, else by its name.
                let label = version
                    .get("field_labels")
                    .and_then(|labels| labels.get(field))
                    .and_then(Json::as_str)
                    .and_then(|key| words.format(key, &tilo_history::FluentArgs::new()))
                    .unwrap_or_else(|| field.to_owned());
                Some(tilo_actions::Did::Edit(tilo_actions::FieldEdit {
                    ty: String::new(),
                    field: label,
                    path: Vec::new(),
                    op: tilo_actions::EditOp::Set,
                    old: value("before"),
                    new: value("after"),
                    args: std::collections::BTreeMap::new(),
                }))
            }
            _ => None,
        };
        let entry = tilo_actions::ActionContext {
            app: Some(tilo_actions::ActionApp {
                id: app.clone(),
                label: String::new(),
            }),
            item: Some(tilo_actions::ActionItem {
                id: resource,
                label,
            }),
            what: Some(step.map_or_else(
                || {
                    if let Some(edit) = edit {
                        return edit;
                    }
                    tilo_actions::Did::Verb(tilo_actions::VerbCall {
                        action: tilo_actions::ActionKey {
                            app,
                            kind: String::new(),
                            action,
                        },
                        args,
                    })
                },
                |op| tilo_actions::Did::history(op, 0, None),
            )),
            ..tilo_actions::ActionContext::default()
        };
        let sentence = reader.sentence(&entry);
        if let Some(fields) = version.as_object_mut() {
            fields.insert("sentence".to_owned(), sentence.into());
        }
    }
}

/// What the app's history calls the resource `id` (as [`ResourceRef::id_string`] writes it): an
/// item by its label as published, a package's root by its type ("Notebook"), as the app's name
/// it is published under says nothing in the app's own history.
fn history_label(host: &Host, id: &str) -> String {
    let Some(at) = host
        .store()
        .type_of_id(id)
        .and_then(|ty| ResourceRef::parse_id(id, ty))
    else {
        return String::new();
    };
    match (&at.item, crate::schema::lookup(&at.ty)) {
        (None, Some(schema)) => schema.spoken_name(),
        _ => host.label_at(&at),
    }
}

/// The versions of the published resource `tree`, which `host` reads, and of every resource
/// published in it, oldest first.
///
/// # Errors
/// The store did not read a resource's versions.
pub fn versions(host: &Host, tree: &ResourceSpec) -> Result<Vec<Version>, String> {
    let store = host.store();
    let mut versions = Vec::new();
    for at in published_refs(host, tree) {
        versions.extend(store.versions(&at).map_err(|error| error.to_string())?);
    }
    versions.sort_by(|a, b| a.order().cmp(&b.order()));
    Ok(versions)
}

/// The published resource `tree`, which `host` reads, and every resource published in it, as
/// the store names them; those of no type loaded here left out.
#[must_use]
pub fn published_refs(host: &Host, tree: &ResourceSpec) -> Vec<ResourceRef> {
    let mut refs = Vec::new();
    each_published(tree, &mut |spec| refs.extend(store_ref(host, spec)));
    refs
}

/// The published resource `spec`, as the store names it; `None` for one of no type loaded here.
#[must_use]
pub fn store_ref(host: &Host, spec: &ResourceSpec) -> Option<ResourceRef> {
    let (_, path) = split_published(spec.id.as_str())?;
    let ty = host
        .store()
        .kinds()
        .of_kind(&spec.kind)
        .map(|entry| entry.id.clone())?;
    ResourceRef::parse_id(path, ty)
}

/// Call `visit` with `spec` and every resource published in it, depth first.
pub fn each_published(spec: &ResourceSpec, visit: &mut dyn FnMut(&ResourceSpec)) {
    visit(spec);
    for child in &spec.children {
        each_published(child, visit);
    }
}
