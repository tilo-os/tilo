//! History entries as sentences for the person reading this app's history:
//! in their locale, from the app's catalogue on this thread and the labels
//! of the actions it publishes.

use tilo_actions::ActionContext;
use tilo_history::{FluentArgs, Messages, Reader};

use crate::i18n::I18nManager;

/// The catalogues installed on this thread, which include the app's own.
struct ThreadCatalogue;

impl Messages for ThreadCatalogue {
    fn format(&self, key: &str, args: &FluentArgs<'_>) -> Option<String> {
        I18nManager::has_message(key).then(|| I18nManager::lookup(key, Some(args)))
    }

    fn has(&self, key: &str) -> bool {
        I18nManager::has_message(key)
    }
}

/// Run `read` with a reader in this thread's locale and catalogues.
pub fn with_reader<T>(read: impl FnOnce(&Reader<'_>) -> T) -> T {
    let reader = Reader::new(&I18nManager::current_locale())
        .with_app(&ThreadCatalogue)
        .with_labels(&ThreadLabels);
    read(&reader)
}

pub fn sentence(entry: &ActionContext) -> String {
    with_reader(|reader| reader.sentence(entry))
}

/// What a history row says about one change: what it did, else what its
/// actor said they were doing. `None` when the entry says neither.
pub fn step_summary(entry: &ActionContext) -> Option<String> {
    if entry.what.is_some() {
        return Some(sentence(entry));
    }
    entry
        .purpose
        .as_deref()
        .map(str::trim)
        .filter(|purpose| !purpose.is_empty())
        .map(str::to_owned)
}

thread_local! {
    /// The labels of the actions this thread's app publishes, by action id:
    /// each with the kind of the resource that publishes it.
    /// The titles of the fields this thread's app stores, by owner type and
    /// key.
    static FIELD_TITLES: std::cell::RefCell<std::collections::HashMap<(String, String), String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static ACTION_LABELS: std::cell::RefCell<std::collections::HashMap<String, Vec<(String, String)>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Remember the labels of the actions `resources` publish, as the app words
/// them in the reader's language, so entries naming them read the same.
pub fn learn_labels(resources: &[tilo_actions::Resource]) {
    fn walk(
        resource: &tilo_actions::Resource,
        labels: &mut std::collections::HashMap<String, Vec<(String, String)>>,
    ) {
        for action in &resource.actions {
            let known = labels.entry(action.id.clone()).or_default();
            match known.iter_mut().find(|(kind, _)| *kind == resource.kind) {
                Some(entry) => entry.1.clone_from(&action.label),
                None => known.push((resource.kind.clone(), action.label.clone())),
            }
        }
        for child in &resource.children {
            walk(child, labels);
        }
    }
    ACTION_LABELS.with(|labels| {
        let mut labels = labels.borrow_mut();
        for resource in resources {
            walk(resource, &mut labels);
        }
    });
}

/// Remember the titles of the fields `shape` lays out (and those of its
/// nested records and items), so entries name a field by its title.
pub fn learn_field_titles(shape: &tilo_resource::Shape) {
    fn walk(
        shape: &tilo_resource::Shape,
        titles: &mut std::collections::HashMap<(String, String), String>,
        depth: usize,
    ) {
        if depth > 16 {
            return;
        }
        match shape {
            tilo_resource::Shape::Record(fields) => {
                for field in *fields {
                    titles.insert(
                        (field.owner.to_owned(), field.key.to_owned()),
                        field.title.to_owned(),
                    );
                    walk(&field.shape, titles, depth.saturating_add(1));
                }
            }
            tilo_resource::Shape::Collection(item)
            | tilo_resource::Shape::Keyed(item)
            | tilo_resource::Shape::Map(item) => {
                walk(item, titles, depth.saturating_add(1));
            }
            tilo_resource::Shape::Value => {}
        }
    }
    FIELD_TITLES.with(|titles| walk(shape, &mut titles.borrow_mut(), 0));
}

/// The published labels learned on this thread.
struct ThreadLabels;

impl tilo_history::Labels for ThreadLabels {
    fn field(&self, ty: &str, field: &str) -> Option<String> {
        FIELD_TITLES.with(|titles| {
            titles
                .borrow()
                .get(&(ty.to_owned(), field.to_owned()))
                .filter(|title| !title.trim().is_empty())
                .cloned()
        })
    }

    fn action(&self, key: &tilo_actions::ActionKey) -> Option<String> {
        ACTION_LABELS.with(|labels| {
            let labels = labels.borrow();
            let known = labels.get(&key.action)?;
            known
                .iter()
                .find(|(kind, _)| *kind == key.kind)
                .or_else(|| known.first())
                .map(|(_, label)| label.clone())
                .filter(|label| !label.trim().is_empty())
        })
    }
}
