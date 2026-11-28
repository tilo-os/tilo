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
//! The same entry reads in German for a German member and in English
//! for an English one, whoever wrote it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use fluent::{FluentBundle, FluentResource};
use tilo_actions::{
    ActionContext, ActionKey, Described, Did, EditOp, FieldEdit, GroupOf, HistoryOp, HistoryStep,
    Scalar, VerbCall,
};
use unic_langid::LanguageIdentifier;

pub use fluent::{FluentArgs, FluentValue};

const SOURCE_LOCALE: &str = "en-US";
const SOURCE: (&str, &str) = (SOURCE_LOCALE, include_str!("../locales/en-US.ftl"));
const FRAMEWORK: &[(&str, &str)] = &[SOURCE, ("de-DE", include_str!("../locales/de-DE.ftl"))];

/// Format the message `key` of `bundle`, or the attribute it names (`message.attribute`:
/// Fluent keys hold no `.`), with whether it formatted without errors (a missing variable, an
/// unknown reference).
pub fn format_message<R: std::borrow::Borrow<FluentResource>>(
    bundle: &FluentBundle<R>,
    key: &str,
    args: Option<&FluentArgs<'_>>,
) -> Option<(String, bool)> {
    let pattern = if let Some(message) = bundle.get_message(key) {
        message.value()?
    } else {
        let (name, attribute) = key.rsplit_once('.')?;
        bundle.get_message(name)?.get_attribute(attribute)?.value()
    };
    let mut errors = Vec::new();
    let text = bundle
        .format_pattern(pattern, args, &mut errors)
        .into_owned();
    Some((text, errors.is_empty()))
}

/// A catalogue of messages in one locale: the app's, for its own entries.
pub trait Messages {
    /// The message `key` formatted with `args`; `None` when the catalogue has
    /// no such message.
    fn format(&self, key: &str, args: &FluentArgs<'_>) -> Option<String>;

    /// Whether the catalogue has the message `key`.
    fn has(&self, key: &str) -> bool;
}

/// The labels an app publishes for its fields, actions and items, in the reader's language: the
/// field's doc label, the action's label, the name an item goes by now.
pub trait Labels {
    fn field(&self, ty: &str, field: &str) -> Option<String> {
        let _ = (ty, field);
        None
    }

    fn action(&self, key: &ActionKey) -> Option<String> {
        let _ = key;
        None
    }

    /// What the action did, in the words its app declares beside its label, said after who did
    /// it ("added to the note"): how a step of it reads where the app has no message of its own.
    /// `args` are the step's arguments: words that name one ("scanning at { $dpi } dpi") or pick
    /// by one (`{ $on -> [true] turned it on *[other] turned it off }`) read the step's own.
    fn did(&self, key: &ActionKey, args: &FluentArgs<'_>) -> Option<String> {
        let _ = (key, args);
        None
    }

    /// The name the item `id` goes by now, where the reader's app holds it: a step that recorded
    /// no name for it reads with its name as it is.
    fn item(&self, id: &str) -> Option<String> {
        let _ = id;
        None
    }
}

/// The labels of the actions apps publish, by the id a change of each says it by, each with the
/// app and the kind of the resource that publishes it, and what the app says the action did: what
/// a reader names an action by, in the words the app publishes it in.
#[derive(Clone, Debug, Default)]
pub struct ActionLabels(HashMap<String, Vec<Known>>);

/// One published action's words, as [`ActionLabels`] keeps them.
#[derive(Clone, Debug)]
struct Known {
    /// The app that publishes it: empty for the reader's own app.
    app: String,
    kind: String,
    label: String,
    did: Option<String>,
}

impl ActionLabels {
    /// Remember the labels of the actions `resources` publish, under each the id a change of it
    /// says it by: the one the app declares it under.
    pub fn learn(&mut self, resources: &[tilo_actions::ResourceSpec]) {
        for resource in resources {
            for action in &resource.actions {
                // A change says the id the app declares the action by, where it declares one
                // apart from the name it publishes the action under.
                let by = if action.declared.is_empty() {
                    &action.id
                } else {
                    &action.declared
                };
                self.know("", &resource.kind, by, &action.label, action.did.as_deref());
            }
            self.learn(&resource.children);
        }
    }

    /// Remember the words of the actions `app` publishes as `actions`: what a reader that is not
    /// that app (the session's History of a workspace) names a change of one by.
    pub fn learn_published(
        &mut self,
        app: &str,
        actions: &[tilo_actions::published::PublishedAction],
    ) {
        for action in actions {
            self.know(
                app,
                &action.resource_kind,
                &action.action,
                &action.action_label,
                action.did.as_deref(),
            );
        }
    }

    /// Remember the words of the actions `app` offers on the device's dispatch, as its catalogue
    /// describes them: what [`Self::learn_published`] learns of a surface the app published.
    pub fn learn_described(&mut self, app: &str, actions: &[tilo_actions::ActionDescriptor]) {
        for action in actions {
            self.know(
                app,
                &action.resource_kind,
                &action.address.action,
                &action.action_label,
                action.did.as_deref(),
            );
        }
    }

    /// Remember the words of the actions a record of `kind` declares (id, label, what it did)
    /// that the reader holds itself rather than hears from an app: the session's own records.
    pub fn learn_declared<'a>(
        &mut self,
        kind: &str,
        declared: impl IntoIterator<Item = (&'a str, &'a str, Option<&'a str>)>,
    ) {
        for (id, label, did) in declared {
            self.know("", kind, id, label, did);
        }
    }

    fn know(&mut self, app: &str, kind: &str, by: &str, label: &str, did: Option<&str>) {
        // A change says a native verb by its declared name (`append_to_note`), and a surface may
        // publish it spelled for callers (`append-to-note`): one action either way.
        let known = self.0.entry(tilo_actions::entry::kebab(by)).or_default();
        let did = did
            .map(str::trim)
            .filter(|did| !did.is_empty())
            .map(str::to_owned);
        match known
            .iter_mut()
            .find(|entry| entry.app == app && entry.kind == kind)
        {
            Some(entry) => {
                label.clone_into(&mut entry.label);
                entry.did = did;
            }
            None => known.push(Known {
                app: app.to_owned(),
                kind: kind.to_owned(),
                label: label.to_owned(),
                did,
            }),
        }
    }

    /// The words for `key`: those of the action on a resource of its kind, else of the action
    /// its app (or the reader's own app) publishes on any resource. Another app's action of the
    /// same name is never this one.
    fn find(&self, key: &ActionKey) -> Option<&Known> {
        let known = self.0.get(&tilo_actions::entry::kebab(&key.action))?;
        let app = app_prefix(&key.app);
        known
            .iter()
            .find(|entry| entry.kind == key.kind)
            .or_else(|| {
                known
                    .iter()
                    .find(|entry| entry.app.is_empty() || app_prefix(&entry.app) == app)
            })
    }
}

impl Labels for ActionLabels {
    fn action(&self, key: &ActionKey) -> Option<String> {
        self.find(key)
            .map(|known| known.label.clone())
            .filter(|label| !label.trim().is_empty())
    }

    fn did(&self, key: &ActionKey, _args: &FluentArgs<'_>) -> Option<String> {
        self.find(key)?.did.clone()
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

    /// Adds `source`'s messages. A message an earlier source held is replaced, so a translation
    /// added after its source language wins over it, and a later build's words over the
    /// earlier build's.
    pub fn add(&mut self, source: &str) {
        let resource =
            FluentResource::try_new(source.to_owned()).unwrap_or_else(|(resource, _)| resource);
        self.bundle.add_resource_overriding(resource);
    }
}

impl Messages for Catalogue {
    fn has(&self, key: &str) -> bool {
        self.bundle.has_message(key)
    }

    fn format(&self, key: &str, args: &FluentArgs<'_>) -> Option<String> {
        let (text, clean) = format_message(&self.bundle, key, Some(args))?;
        clean.then_some(text)
    }
}

fn langid(locale: &str) -> LanguageIdentifier {
    tilo_paths::locale_tag(locale)
        .parse()
        .or_else(|_| SOURCE_LOCALE.parse())
        .unwrap_or_default()
}

/// The language a window speaks, chosen once from the person's preference and the languages
/// what it shows ships: the language they prefer when it ships it (`de-DE` for a `de-DE`
/// preference), the language of theirs it ships in another region (`de-DE` for `de-AT`), the
/// source language when it ships that, else the first language it ships. A window speaks one
/// language: an app that ships a translation leaves nothing in it in another. No preference (or
/// a malformed one) is the source language when it ships, the first one otherwise.
#[must_use]
pub fn window_language(preference: Option<&str>, shipped: &[&str]) -> String {
    if shipped.is_empty() {
        return preference.map_or_else(|| SOURCE_LOCALE.to_owned(), |p| langid(p).to_string());
    }
    if let Some(preference) = preference
        && let Some(position) = tilo_paths::matching(preference, shipped.iter().copied())
        && let Some(tag) = shipped.get(position)
    {
        return (*tag).to_owned();
    }
    shipped
        .iter()
        .find(|tag| tilo_paths::language_of(tag) == tilo_paths::language_of(SOURCE_LOCALE))
        .or_else(|| shipped.first())
        .map_or_else(|| SOURCE_LOCALE.to_owned(), |tag| (*tag).to_owned())
}

thread_local! {
    static FRAMEWORK_CATALOGUES: RefCell<HashMap<String, Rc<Catalogue>>> = RefCell::new(HashMap::new());
}

/// The framework's messages in `locale`, or in the closest locale it has:
/// the same language, else the source locale.
fn framework(locale: &str) -> Rc<Catalogue> {
    let chosen = tilo_paths::matching(locale, FRAMEWORK.iter().map(|(tag, _)| *tag))
        .and_then(|position| FRAMEWORK.get(position))
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
/// message the app means to have and does not (`tilolang check`).
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

/// What an item is called, when it is called anything: a label that only repeats the item's
/// id (what a record with no title yet is labelled by) names nothing a person reads, and the
/// sentence says the item unnamed instead of printing the id.
pub fn item_label(item: &tilo_actions::ActionItem) -> Option<&str> {
    let label = item.label.trim();
    let id = item.id.trim();
    // A name that happens to sit inside the id ("chat" in "c_chat") is still a name: only the
    // id itself, its last segment, or a UUID is what a record with no title falls back to.
    let repeats_id = id == label || id.rsplit(['/', ':']).next() == Some(label) || is_uuid(label);
    (!label.is_empty() && !repeats_id).then_some(label)
}

/// Whether `text` is a UUID in its usual hyphenated form: an id, never a name.
fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(at, c)| match at {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// `new-snippet` → `New snippet`: a readable name for an id nothing labels.
fn humanize(id: &str) -> String {
    capitalised(&tilo_actions::entry::kebab(id).replace('-', " "))
}

/// `text` with its first letter a capital: words said after who did them ("added to the note")
/// starting a row of their own.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
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

    /// What `item` is called: the label the step recorded, when that names it ("Renamed Plan to
    /// Roadmap" keeps the name it had then), else the name the reader's app gives it now (a
    /// note typed into after it was made reads as what it says).
    #[must_use]
    pub fn item_name(&self, item: &tilo_actions::ActionItem) -> Option<String> {
        item_label(item).map(str::to_owned).or_else(|| {
            self.labels
                .and_then(|labels| labels.item(&item.id))
                .filter(|name| !name.trim().is_empty())
        })
    }

    /// The name `item` goes by now, as the reader's labels call it, else as the change
    /// recorded it.
    fn item_now(&self, item: &tilo_actions::ActionItem) -> Option<String> {
        self.labels
            .and_then(|labels| labels.item(&item.id))
            .filter(|name| !name.trim().is_empty())
            .or_else(|| self.item_name(item))
    }

    /// The sentence for `entry`, without naming who did it.
    pub fn sentence(&self, entry: &ActionContext) -> String {
        let app = entry.app.as_ref().map_or("", |app| app.id.as_str());
        let item = entry.item.as_ref().and_then(|item| self.item_name(item));
        let now = entry.item.as_ref().and_then(|item| self.item_now(item));
        // A step on an item of the resource that has no name yet is about that item: it says
        // the item unnamed rather than borrowing the resource's name ("Renamed Notebook to …"
        // for a note that had no title).
        let resource = entry
            .resource
            .as_ref()
            .filter(|_| entry.item.is_none())
            .map(|resource| resource.label.as_str())
            .filter(|label| !label.trim().is_empty());
        let scope = Scope {
            app,
            item: item.as_deref().or(resource),
            now: now.as_deref(),
            within: item.is_none() && resource.is_some(),
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
            .and_then(|item| self.item_name(item));
        let now = described.item.as_ref().and_then(|item| self.item_now(item));
        self.did(
            &described.what,
            &Scope {
                app,
                item: item.as_deref(),
                now: now.as_deref(),
                within: false,
            },
            &mut false,
        )
    }

    /// Who did it, as one line: "You", "Ada", "Ada via task 'Prepare Q3 financials'".
    ///
    /// Anyone but the reader acted as a task, so the task names them: the person under whose
    /// authority it was done. Without a named task, the actor by the name it goes by: whom
    /// software acted for is a fact a detail shows, and what the actor is — an agent, a
    /// Shortcut, an app, a bot — is the face beside the line, never words in it.
    pub fn actor(&self, who: &Who<'_>) -> String {
        let mut args = FluentArgs::new();
        args.set("actor", who.actor.to_owned());
        if who.is_reader {
            return self.framework_message("history-actor-you", &args);
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
        who.actor.to_owned()
    }

    /// The sentence for `what` as `app` renders it, with `item` the label of
    /// the item it acted on, and whether the app's own catalogue rendered it:
    /// `false` when the app has no such message and the framework fell back
    /// to naming the field or the action.
    pub fn did_by_app(&self, what: &Did, app: &str, item: Option<&str>) -> (String, bool) {
        let item = item.filter(|label| !label.trim().is_empty());
        let scope = Scope {
            app,
            item,
            now: item,
            within: false,
        };
        let mut by_app = false;
        let sentence = self.did(what, &scope, &mut by_app);
        (sentence, by_app)
    }

    fn did(&self, what: &Did, scope: &Scope<'_>, by_app: &mut bool) -> String {
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
    fn has_app_message(&self, what: &Did, scope: &Scope<'_>) -> bool {
        message_key(scope.app, what).is_some_and(|key| self.app.is_some_and(|app| app.has(&key)))
    }

    fn app_message(&self, what: &Did, scope: &Scope<'_>, args: &FluentArgs<'_>) -> Option<String> {
        let app = self.app?;
        let key = message_key(scope.app, what)?;
        app.format(&key, args)
    }

    fn base_args(&self, scope: &Scope<'_>) -> FluentArgs<'static> {
        let mut args = FluentArgs::new();
        args.set(
            "item",
            scope
                .item
                .map_or_else(|| self.item_unnamed(), str::to_owned),
        );
        args.set(
            "now",
            scope
                .now
                .or_else(|| scope.item.filter(|_| !scope.within))
                .map_or_else(|| self.item_unnamed(), str::to_owned),
        );
        args
    }

    fn edit(&self, edit: &FieldEdit, what: &Did, scope: &Scope<'_>) -> String {
        let mut args = self.base_args(scope);
        for (name, value) in &edit.args {
            if !matches!(
                name.as_str(),
                "item" | "now" | "actor" | "for" | "old" | "new"
            ) {
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

    fn verb(&self, verb: &VerbCall, what: &Did, scope: &Scope<'_>) -> String {
        let mut args = self.base_args(scope);
        // A verb that names no app is the entry's app's.
        let key = if verb.action.app.is_empty() {
            ActionKey::new(
                scope.app,
                verb.action.kind.as_str(),
                verb.action.action.as_str(),
            )
        } else {
            verb.action.clone()
        };
        // An empty value says nothing: a message that needs it falls back to the action's label.
        // An argument naming an item reads as the item's name now ("New note" says the note it
        // made); an id the reader cannot name says nothing either.
        for (name, value) in &verb.args {
            if matches!(name.as_str(), "item" | "now" | "actor" | "for") {
                continue;
            }
            match value.as_str() {
                Some("") => {}
                Some(id) => {
                    if let Some(named) = self.labels.and_then(|labels| labels.item(id)) {
                        args.set(name.clone(), named);
                    } else if !is_uuid(id) {
                        args.set(name.clone(), fluent_value(value));
                    }
                }
                None => args.set(name.clone(), fluent_value(value)),
            }
        }
        if let Some(sentence) = self.app_message(what, scope, &args) {
            return sentence;
        }
        // What the app says the action did ("added to the note"), said with the item it did it
        // to, starts the row: a sentence in the app's own words without a message of its own.
        if let Some(did) = self.labels.and_then(|labels| labels.did(&key, &args)) {
            args.set("did", did);
            let sentence = if scope.within {
                self.framework_message("history-verb-did-in", &args)
            } else {
                self.named_or_not("history-verb-did", scope, &args)
            };
            return capitalised(&sentence);
        }
        if verb.action.action == tilo_actions::CREATE_ACTION {
            return self.named_or_not("history-verb-create", scope, &args);
        }
        // A record a branch took in, with its history: the step that founded it there.
        if verb.action.action == tilo_actions::stamp::Stamp::BRANCH_VERB {
            return self.named_or_not("history-verb-branch", scope, &args);
        }
        let action = self
            .labels
            .and_then(|labels| labels.action(&key))
            .unwrap_or_else(|| humanize(&verb.action.action));
        args.set("action", action);
        if scope.within {
            return self.framework_message("history-verb-in", &args);
        }
        self.named_or_not("history-verb", scope, &args)
    }

    fn group(&self, group: &GroupOf, scope: &Scope<'_>, by_app: &mut bool) -> String {
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

    fn history(&self, step: &HistoryStep, scope: &Scope<'_>, by_app: &mut bool) -> String {
        let mut args = self.base_args(scope);
        args.set("count", step.steps);
        // What it took back is named as the step itself was: on its item,
        // else in the resource the undo ran in.
        let one = step.of.as_deref().map(|described| {
            let item = described
                .item
                .as_ref()
                .and_then(|item| self.item_name(item));
            let now = described.item.as_ref().and_then(|item| self.item_now(item));
            self.did(
                &described.what,
                &Scope {
                    app: scope.app,
                    item: item.as_deref().or(scope.item),
                    now: now.as_deref().or(scope.now),
                    within: item.is_none() && scope.within,
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
            (HistoryOp::Migrate { to, .. }, _) => {
                args.set("to", *to);
                self.framework_message("history-migrate", &args)
            }
            (HistoryOp::Checkpoint { name }, _) => {
                args.set("name", name.clone());
                self.framework_message("history-checkpoint", &args)
            }
        }
    }

    fn changed(&self, scope: &Scope<'_>) -> String {
        let args = self.base_args(scope);
        self.named_or_not("history-changed", scope, &args)
    }

    /// `key` when the entry names an item, else `key-unnamed`.
    fn named_or_not(&self, key: &str, scope: &Scope<'_>, args: &FluentArgs<'_>) -> String {
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

struct Scope<'s> {
    app: &'s str,
    /// The item's label, else the resource's.
    item: Option<&'s str>,
    /// The item's name as the reader calls it now: what a message says an item by when it
    /// names the item it made, which may have been renamed since (`$now`).
    now: Option<&'s str>,
    /// Whether `item` names the resource the action ran in rather than an
    /// item it acted on.
    within: bool,
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
