//! History entries as sentences, in the reader's language.
//!
//! An entry ([`ActionContext`]) records what happened as data. A reader turns
//! it into a sentence here, in its own locale:
//!
//! - from the app's Fluent catalogue, by convention: a field edit is
//!   `<app>-<type>-<field>`, a verb `<app>-<action>`, with the arguments
//!   `$item` (the item's label at the time), `$old`, `$new`, `$count`,
//!   `$actor`, `$for`, and each verb parameter by name;
//! - else from the framework's own messages ("Changed *Folder* of *Deploy
//!   script*"), naming the field by its published label ([`Labels`]) and never
//!   by a raw id or path.
//!
//! The same entry reads in German for a German collaborator and in English
//! for an English one, whoever wrote it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fluent::{FluentBundle, FluentResource};
use tilo_actions::{
    ActionContext, ActionKey, ActorKind, Described, Did, EditOp, FieldEdit, GroupOf, HistoryOp,
    HistoryStep, Scalar, VerbCall,
};
use unic_langid::LanguageIdentifier;

pub use fluent::{FluentArgs, FluentValue};

const SOURCE_LOCALE: &str = "en-US";
const SOURCE: (&str, &str) = (SOURCE_LOCALE, include_str!("../locales/en-US.ftl"));
const FRAMEWORK: &[(&str, &str)] = &[SOURCE, ("de-DE", include_str!("../locales/de-DE.ftl"))];

/// A catalogue of messages in one locale: the app's, for its own entries.
pub trait Messages {
    /// The message `key` formatted with `args`; `None` when the catalogue has
    /// no such message.
    fn format(&self, key: &str, args: &FluentArgs<'_>) -> Option<String>;

    /// Whether the catalogue has the message `key`.
    fn has(&self, key: &str) -> bool;
}

/// The labels an app publishes for its fields and actions, in the reader's
/// language: the field's doc label, the action's label.
pub trait Labels {
    fn field(&self, ty: &str, field: &str) -> Option<String> {
        let _ = (ty, field);
        None
    }

    fn action(&self, key: &ActionKey) -> Option<String> {
        let _ = key;
        None
    }
}

/// Fluent sources loaded into one bundle for one locale.
pub struct Catalogue {
    bundle: FluentBundle<FluentResource>,
}

impl Catalogue {
    /// A catalogue of `sources` in `locale`. A source that does not parse is
    /// left out, never fatal: its messages fall back like missing ones.
    pub fn new<S: AsRef<str>>(locale: &str, sources: &[S]) -> Self {
        let langid = langid(locale);
        let mut bundle = FluentBundle::new(vec![langid]);
        bundle.set_use_isolating(false);
        for source in sources {
            let resource = FluentResource::try_new(source.as_ref().to_owned())
                .unwrap_or_else(|(resource, _)| resource);
            let _ = bundle.add_resource(resource);
        }
        Self { bundle }
    }

    pub fn has(&self, key: &str) -> bool {
        self.bundle.has_message(key)
    }
}

impl Messages for Catalogue {
    fn has(&self, key: &str) -> bool {
        self.bundle.has_message(key)
    }

    fn format(&self, key: &str, args: &FluentArgs<'_>) -> Option<String> {
        let pattern = self.bundle.get_message(key)?.value()?;
        let mut errors = Vec::new();
        let text = self.bundle.format_pattern(pattern, Some(args), &mut errors);
        errors.is_empty().then(|| text.into_owned())
    }
}

fn langid(locale: &str) -> LanguageIdentifier {
    locale
        .replace('_', "-")
        .split('.')
        .next()
        .unwrap_or(SOURCE_LOCALE)
        .parse()
        .or_else(|_| SOURCE_LOCALE.parse())
        .unwrap_or_default()
}

thread_local! {
    static FRAMEWORK_CATALOGUES: RefCell<HashMap<String, Rc<Catalogue>>> = RefCell::new(HashMap::new());
}

/// The framework's messages in `locale`, or in the closest locale it has:
/// the same language, else the source locale.
fn framework(locale: &str) -> Rc<Catalogue> {
    let wanted = langid(locale);
    let chosen = FRAMEWORK
        .iter()
        .find(|(candidate, _)| langid(candidate) == wanted)
        .or_else(|| {
            FRAMEWORK
                .iter()
                .find(|(candidate, _)| langid(candidate).language == wanted.language)
        })
        .unwrap_or(&SOURCE);
    FRAMEWORK_CATALOGUES.with(|cache| {
        let mut cache = cache.borrow_mut();
        Rc::clone(cache.entry(chosen.0.to_owned()).or_insert_with(|| {
            // The source catalogue is loaded after the locale's own, so a
            // message a translation lacks still reads in the source language.
            let mut sources = vec![chosen.1];
            if chosen.0 != SOURCE_LOCALE {
                sources.push(SOURCE.1);
            }
            Rc::new(Catalogue::new(chosen.0, &sources))
        }))
    })
}

/// Who did something, as the reader names them.
#[derive(Clone, Copy, Debug)]
pub struct Who<'a> {
    pub actor: &'a str,
    pub kind: ActorKind,
    /// The display name of the person the actor acted for.
    pub for_person: Option<&'a str>,
    /// Whether the person the actor acted for is the reader.
    pub for_reader: bool,
    pub is_reader: bool,
    /// The task the action was taken in, as its id and the label it carried
    /// at the time. There are no separate agents: everything an agent does
    /// it does as a task, so an actor that is not the reader is named by the
    /// task it worked for its person.
    pub task: Option<&'a Task<'a>>,
}

/// The task an action is attributed to ([`Who::task`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Task<'a> {
    pub id: &'a str,
    /// The task's name at the time of the change; empty when nothing named it.
    pub label: &'a str,
}

/// The Fluent message prefix of an app id: its last segment in kebab case
/// (`org.tilo.WordProcessor` → `word-processor`, `so.tilo.mail` → `mail`).
pub fn app_prefix(app: &str) -> String {
    tilo_actions::entry::kebab(app.rsplit('.').next().unwrap_or(app))
}

/// The app message an entry's step renders through, by convention:
/// `<app>-<type>-<field>` for a field edit, `<app>-<action>` for a verb.
pub fn message_key(app: &str, what: &Did) -> Option<String> {
    match what {
        Did::Edit(edit) => Some(format!(
            "{}-{}-{}",
            app_prefix(app),
            tilo_actions::entry::kebab(&edit.ty),
            tilo_actions::entry::kebab(&edit.field)
        )),
        Did::Verb(verb) => {
            let app = if verb.action.app.is_empty() {
                app
            } else {
                &verb.action.app
            };
            Some(format!(
                "{}-{}",
                app_prefix(app),
                tilo_actions::entry::kebab(&verb.action.action)
            ))
        }
        Did::Group(_) | Did::History(_) => None,
    }
}

/// The app message keys the steps of `what` render through, by the same
/// convention as [`message_key`]: none for a history step.
///
/// A reader falls back to the framework's sentences for a key its app
/// catalogue lacks, so a key listed here that the catalogue lacks is a
/// message the app means to have and does not (`tilo-app check`).
pub fn message_keys(app: &str, what: &Did) -> Vec<String> {
    let single = |did: &Did| message_key(app, did).into_iter().collect::<Vec<_>>();
    match what {
        Did::Edit(_) | Did::Verb(_) => single(what),
        Did::Group(group) => {
            if let Some(shared) = group.shared() {
                return single(shared);
            }
            let mut keys: Vec<String> = Vec::new();
            for member in &group.distinct {
                for key in single(member) {
                    if !keys.contains(&key) {
                        keys.push(key);
                    }
                }
            }
            keys
        }
        Did::History(_) => Vec::new(),
    }
}

/// `new-snippet` → `New snippet`: a readable name for an id nothing labels.
fn humanize(id: &str) -> String {
    let words = tilo_actions::entry::kebab(id).replace('-', " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// Renders entries for one reader: its locale, the app's catalogue and
/// published labels.
pub struct Reader<'a> {
    framework: Rc<Catalogue>,
    app: Option<&'a dyn Messages>,
    labels: Option<&'a dyn Labels>,
}

impl<'a> Reader<'a> {
    /// A reader in `locale` (`de-DE`, `en_US.UTF-8`) with no app catalogue:
    /// every entry renders through the framework's messages.
    pub fn new(locale: &str) -> Self {
        Self {
            framework: framework(locale),
            app: None,
            labels: None,
        }
    }

    #[must_use]
    pub fn with_app(mut self, app: &'a dyn Messages) -> Self {
        self.app = Some(app);
        self
    }

    /// The app's published field and action labels, in the reader's locale.
    #[must_use]
    pub fn with_labels(mut self, labels: &'a dyn Labels) -> Self {
        self.labels = Some(labels);
        self
    }

    /// The sentence for `entry`, without naming who did it.
    pub fn sentence(&self, entry: &ActionContext) -> String {
        self.sentence_by(entry, None)
    }

    /// The sentence for `entry`, with `$actor` and `$for` for messages that
    /// name who did it.
    pub fn sentence_by(&self, entry: &ActionContext, who: Option<&Who<'_>>) -> String {
        let app = entry.app.as_ref().map_or("", |app| app.id.as_str());
        let item = entry
            .item
            .as_ref()
            .map(|item| item.label.as_str())
            .filter(|label| !label.trim().is_empty());
        let resource = entry
            .resource
            .as_ref()
            .map(|resource| resource.label.as_str())
            .filter(|label| !label.trim().is_empty());
        let scope = Scope {
            app,
            item: item.or(resource),
            within: item.is_none() && resource.is_some(),
            who,
        };
        match &entry.what {
            Some(what) => self.did(what, &scope, &mut false),
            None => self.changed(&scope),
        }
    }

    /// The sentence for one described step, as an undo names what it took
    /// back.
    pub fn described(&self, described: &Described, app: &str) -> String {
        let item = described
            .item
            .as_ref()
            .map(|item| item.label.as_str())
            .filter(|label| !label.trim().is_empty());
        self.did(
            &described.what,
            &Scope {
                app,
                item,
                within: false,
                who: None,
            },
            &mut false,
        )
    }

    /// Who did it, as one line and as presence names them: "You",
    /// "ada via task 'Prepare Q3 financials'", "Morning · Shortcut".
    ///
    /// Anyone but the reader acted as a task, so the task names them: the
    /// person under whose authority it was done, or the Shortcut that ran.
    /// Without a named task, the actor as it is recorded.
    pub fn actor(&self, who: &Who<'_>) -> String {
        let mut args = FluentArgs::new();
        args.set("actor", who.actor.to_owned());
        if who.is_reader {
            return match who.for_person {
                Some(person) => {
                    args.set("for", person.to_owned());
                    self.framework_message("history-actor-you-for", &args)
                }
                None => self.framework_message("history-actor-you", &args),
            };
        }
        if who.actor.trim().is_empty() {
            return self.framework_message("history-actor-unknown", &args);
        }
        if let Some(task) = who.task
            && !task.label.trim().is_empty()
        {
            let mut args = FluentArgs::new();
            args.set("person", who.actor.to_owned());
            args.set("task", task.label.to_owned());
            return self.framework_message("history-actor-via-task", &args);
        }
        let named = match who.kind {
            ActorKind::Agent => self.framework_message("history-actor-agent", &args),
            ActorKind::Shortcut => self.framework_message("history-actor-shortcut", &args),
            ActorKind::App => self.framework_message("history-actor-app", &args),
            ActorKind::Bot => self.framework_message("history-actor-bot", &args),
            ActorKind::Person => who.actor.to_owned(),
        };
        let person = if who.for_reader {
            Some(self.framework_message("history-actor-for-you", &FluentArgs::new()))
        } else {
            who.for_person.map(str::to_owned)
        };
        match person {
            Some(person) => {
                let mut args = FluentArgs::new();
                args.set("actor", named);
                args.set("for", person);
                self.framework_message("history-actor-for", &args)
            }
            None => named,
        }
    }

    /// The sentence for `what` as `app` renders it, with `item` the label of
    /// the item it acted on, and whether the app's own catalogue rendered it:
    /// `false` when the app has no such message and the framework fell back
    /// to naming the field or the action.
    pub fn did_by_app(&self, what: &Did, app: &str, item: Option<&str>) -> (String, bool) {
        let scope = Scope {
            app,
            item: item.filter(|label| !label.trim().is_empty()),
            within: false,
            who: None,
        };
        let mut by_app = false;
        let sentence = self.did(what, &scope, &mut by_app);
        (sentence, by_app)
    }

    fn did(&self, what: &Did, scope: &Scope<'_, '_>, by_app: &mut bool) -> String {
        match what {
            Did::Edit(edit) => {
                *by_app |= self.has_app_message(what, scope);
                self.edit(edit, what, scope)
            }
            Did::Verb(verb) => {
                *by_app |= self.has_app_message(what, scope);
                self.verb(verb, what, scope)
            }
            Did::Group(group) => self.group(group, scope, by_app),
            Did::History(step) => self.history(step, scope, by_app),
        }
    }

    /// Whether the app's catalogue has the message `what` renders through.
    fn has_app_message(&self, what: &Did, scope: &Scope<'_, '_>) -> bool {
        message_key(scope.app, what).is_some_and(|key| self.app.is_some_and(|app| app.has(&key)))
    }

    fn app_message(
        &self,
        what: &Did,
        scope: &Scope<'_, '_>,
        args: &FluentArgs<'_>,
    ) -> Option<String> {
        let app = self.app?;
        let key = message_key(scope.app, what)?;
        app.format(&key, args)
    }

    fn base_args(&self, scope: &Scope<'_, '_>) -> FluentArgs<'static> {
        let mut args = FluentArgs::new();
        args.set(
            "item",
            scope
                .item
                .map_or_else(|| self.item_unnamed(), str::to_owned),
        );
        if let Some(who) = scope.who {
            args.set("actor", who.actor.to_owned());
            if let Some(person) = who.for_person {
                args.set("for", person.to_owned());
            }
        }
        args
    }

    fn edit(&self, edit: &FieldEdit, what: &Did, scope: &Scope<'_, '_>) -> String {
        let mut args = self.base_args(scope);
        for (name, value) in &edit.args {
            if !matches!(name.as_str(), "item" | "actor" | "for" | "old" | "new") {
                args.set(name.clone(), fluent_value(value));
            }
        }
        args.set("op", edit.op.as_str());
        if let Some(old) = &edit.old {
            args.set("old", fluent_value(old));
        }
        if let Some(new) = &edit.new {
            args.set("new", fluent_value(new));
        }
        if let Some(sentence) = self.app_message(what, scope, &args) {
            return sentence;
        }
        let field = self
            .labels
            .and_then(|labels| labels.field(&edit.ty, &edit.field))
            .unwrap_or_else(|| humanize(&edit.field));
        args.set("field", field);
        let new = edit.new.as_ref();
        let key = match edit.op {
            EditOp::Set | EditOp::SetCell => match new {
                Some(value) if value.as_bool() == Some(true) => "history-edit-on",
                Some(value) if value.as_bool() == Some(false) => "history-edit-off",
                Some(value) if value.as_str().is_some_and(str::is_empty) => "history-edit-cleared",
                Some(value) => {
                    args.set("new", self.shown(value));
                    "history-edit-set"
                }
                None if edit.old.is_some() => "history-edit-cleared",
                None => "history-edit",
            },
            EditOp::Splice | EditOp::Replace => "history-edit-text",
            EditOp::Add => match new {
                Some(value) => {
                    args.set("new", self.shown(value));
                    "history-edit-add"
                }
                None => "history-edit",
            },
            // The element a set gained or lost, when the entry names it.
            EditOp::Insert if new.is_some() => {
                if let Some(value) = new {
                    args.set("new", self.shown(value));
                }
                "history-edit-insert-element"
            }
            EditOp::Remove if new.is_some() => {
                if let Some(value) = new {
                    args.set("new", self.shown(value));
                }
                "history-edit-remove-element"
            }
            EditOp::Insert | EditOp::InsertRow | EditOp::InsertColumn => "history-edit-insert",
            EditOp::Remove | EditOp::RemoveRow | EditOp::RemoveColumn => "history-edit-remove",
            EditOp::MoveTo => "history-edit-move",
            EditOp::Mark => "history-edit-format",
            EditOp::Settle => {
                if let Some(reason) = edit.args.get("reason") {
                    args.set("reason", self.shown(reason));
                    "history-edit-settle-failed"
                } else {
                    "history-edit-settle"
                }
            }
            EditOp::Forget => "history-edit-forget",
            EditOp::React => "history-edit-react",
            EditOp::Unreact => "history-edit-unreact",
        };
        self.named_or_not(key, scope, &args)
    }

    fn verb(&self, verb: &VerbCall, what: &Did, scope: &Scope<'_, '_>) -> String {
        let mut args = self.base_args(scope);
        for (name, value) in &verb.args {
            if !matches!(name.as_str(), "item" | "actor" | "for") {
                args.set(name.clone(), fluent_value(value));
            }
        }
        if let Some(sentence) = self.app_message(what, scope, &args) {
            return sentence;
        }
        let action = self
            .labels
            .and_then(|labels| labels.action(&verb.action))
            .unwrap_or_else(|| humanize(&verb.action.action));
        args.set("action", action);
        if scope.within {
            return self.framework_message("history-verb-in", &args);
        }
        self.named_or_not("history-verb", scope, &args)
    }

    fn group(&self, group: &GroupOf, scope: &Scope<'_, '_>, by_app: &mut bool) -> String {
        let mut args = self.base_args(scope);
        args.set("count", group.count);
        if let Some(shared) = group.shared() {
            let one = self.did(shared, scope, by_app);
            if group.count <= 1 {
                return one;
            }
            args.set("what", one);
            return self.framework_message("history-group-same", &args);
        }
        if scope.item.is_some() {
            self.framework_message("history-group-to", &args)
        } else {
            self.framework_message("history-group", &args)
        }
    }

    fn history(&self, step: &HistoryStep, scope: &Scope<'_, '_>, by_app: &mut bool) -> String {
        let mut args = self.base_args(scope);
        args.set("count", step.steps);
        // What it took back is named as the step itself was: on its item,
        // else in the resource the undo ran in.
        let one = step.of.as_deref().map(|described| {
            let item = described
                .item
                .as_ref()
                .map(|item| item.label.as_str())
                .filter(|label| !label.trim().is_empty());
            self.did(
                &described.what,
                &Scope {
                    app: scope.app,
                    item: item.or(scope.item),
                    within: item.is_none() && scope.within,
                    who: None,
                },
                by_app,
            )
        });
        match (&step.op, one) {
            (HistoryOp::Undo, Some(what)) => {
                args.set("what", what);
                self.framework_message("history-undo", &args)
            }
            (HistoryOp::Undo, None) if step.steps == 0 => {
                self.framework_message("history-undo-some", &args)
            }
            (HistoryOp::Undo, None) => self.framework_message("history-undo-steps", &args),
            (HistoryOp::Redo, Some(what)) => {
                args.set("what", what);
                self.framework_message("history-redo", &args)
            }
            (HistoryOp::Redo, None) if step.steps == 0 => {
                self.framework_message("history-redo-some", &args)
            }
            (HistoryOp::Redo, None) => self.framework_message("history-redo-steps", &args),
            (HistoryOp::Restore, _) => self.framework_message("history-restore", &args),
            (HistoryOp::Migrate { to }, _) => {
                args.set("to", *to);
                self.framework_message("history-migrate", &args)
            }
            (HistoryOp::Checkpoint { name }, _) => {
                args.set("name", name.clone());
                self.framework_message("history-checkpoint", &args)
            }
        }
    }

    fn changed(&self, scope: &Scope<'_, '_>) -> String {
        let args = self.base_args(scope);
        self.named_or_not("history-changed", scope, &args)
    }

    /// `key` when the entry names an item, else `key-unnamed`.
    fn named_or_not(&self, key: &str, scope: &Scope<'_, '_>, args: &FluentArgs<'_>) -> String {
        if scope.item.is_some() {
            self.framework_message(key, args)
        } else {
            self.framework_message(&format!("{key}-unnamed"), args)
        }
    }

    /// The framework's own message `key`. A catalogue that lacks it is a
    /// broken build, not a message to show: an entry never renders as the
    /// id of the sentence it should have been.
    fn framework_message(&self, key: &str, args: &FluentArgs<'_>) -> String {
        self.framework.format(key, args).unwrap_or_else(|| {
            // A translation the framework lacks falls back to the source
            // catalogue; a message it never had falls back to its words.
            debug_assert!(
                self.framework.has(key),
                "the framework catalogue has no `{key}`"
            );
            framework(SOURCE_LOCALE)
                .format(key, args)
                .unwrap_or_else(|| humanize(key))
        })
    }

    fn item_unnamed(&self) -> String {
        self.framework_message("history-item-unnamed", &FluentArgs::new())
    }

    /// A value as the framework's sentences show it.
    fn shown(&self, value: &Scalar) -> String {
        match value.as_json() {
            serde_json::Value::Bool(true) => {
                self.framework_message("history-value-true", &FluentArgs::new())
            }
            serde_json::Value::Bool(false) => {
                self.framework_message("history-value-false", &FluentArgs::new())
            }
            serde_json::Value::String(text) if text.is_empty() => {
                self.framework_message("history-value-empty", &FluentArgs::new())
            }
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        }
    }
}

struct Scope<'s, 'w> {
    app: &'s str,
    /// The item's label, else the resource's.
    item: Option<&'s str>,
    /// Whether `item` names the resource the action ran in rather than an
    /// item it acted on.
    within: bool,
    who: Option<&'s Who<'w>>,
}

/// A value as an app message receives it: numbers as numbers (for plurals),
/// booleans as `true` / `false` (for selectors), text as text.
fn fluent_value(value: &Scalar) -> FluentValue<'static> {
    match value.as_json() {
        serde_json::Value::Bool(flag) => FluentValue::from(if *flag { "true" } else { "false" }),
        serde_json::Value::Number(number) => number.as_i64().map_or_else(
            || FluentValue::from(number.as_f64().unwrap_or(0.0)),
            FluentValue::from,
        ),
        serde_json::Value::String(text) => FluentValue::from(text.clone()),
        other => FluentValue::from(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
