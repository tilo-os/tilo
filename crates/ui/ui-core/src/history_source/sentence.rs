//! History entries as sentences for the person reading this app's history:
//! in their locale, from the app's catalogue on this thread and the labels
//! of the actions it publishes.

use tilo_actions::ActionContext;
use tilo_actions::remote::workspace_activity as wire;
use tilo_history::{FluentArgs, Messages, Reader};

use tilo_ui_base::i18n::I18nManager;
use crate::widget::history::{AuthorRelation, HistoryAuthor};

/// The catalogues installed on this thread, which include the app's own.
///
/// A message is the app's only where this step's words complete it: a
/// message only a value completes ("Named the workbook { $new }") is one a
/// step that carried no value leaves to the framework's own sentence for the
/// action, rather than showing the app's with a placeable left empty.
struct ThreadCatalogue;

impl Messages for ThreadCatalogue {
    fn format(&self, key: &str, args: &FluentArgs<'_>) -> Option<String> {
        I18nManager::lookup_complete(key, Some(args))
    }

    fn has(&self, key: &str) -> bool {
        I18nManager::has_message(key)
    }
}

/// Run `read` with a reader in this thread's locale and catalogues.
pub fn with_reader<T>(read: impl FnOnce(&Reader<'_>) -> T) -> T {
    let reader = Reader::new(&I18nManager::window_locale())
        .with_app(&ThreadCatalogue)
        .with_labels(&ThreadLabels);
    read(&reader)
}

pub fn sentence(entry: &ActionContext) -> String {
    with_reader(|reader| reader.sentence(entry))
}

/// An author as one line names them, in this thread's words: "You", "Ada", "Claude", "Ada via
/// task ‘Prepare Q3 financials’".
///
/// The one place a history row, a pill or an app's list says who made a change: by the name the
/// author goes by, as every chip, card and caret tag names them. Whom software acted for is a
/// detail the hover says ([`attribution_hover`]), never part of the name.
pub fn author_line(author: &HistoryAuthor) -> String {
    actor_line(
        &author.name,
        // A task software worked names the person it was taken for; a person's own row is theirs.
        author
            .attribution
            .as_ref()
            .filter(|_| author.relation == AuthorRelation::Software)
            .and_then(|attribution| attribution.on_behalf_of_name.as_deref()),
        author.relation == AuthorRelation::Reader,
        author.task.as_deref(),
    )
}

/// [`author_line`] for an author as an app reads it over the wire.
pub fn wire_author_line(author: &wire::Author) -> String {
    actor_line(
        &author.name,
        author.on_behalf_of.as_deref(),
        author.relation == wire::Relation::Reader,
        author.task.as_deref(),
    )
}

/// The badge that names what wrote an attributed change beside the person's
/// own avatar: the app's name or the program's name. `None` for the person's
/// own change, for an author whose change carries no recorded writer, and for
/// an agent, which is the author itself and goes by its member name
/// ([`author_parts`]).
pub fn writer_badge(author: &HistoryAuthor) -> Option<String> {
    let attribution = author.attribution.as_ref()?;
    match attribution.writer.as_ref()? {
        collab_types::history::authors::AttributedWriter::Agent { .. } => None,
        collab_types::history::authors::AttributedWriter::App(_)
        | collab_types::history::authors::AttributedWriter::Program(_) => {
            attribution.writer_label.clone()
        }
    }
    .filter(|badge| !badge.trim().is_empty())
}

/// An author as the separate fields a surface renders with its own elements:
/// the person the change is in the name of, the key presence knows them by, the
/// software that wrote for them when one did, and the computer whose session
/// vouched for it. A row renders each with the widget it has for people and
/// agents — never one line joined from names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthorParts {
    /// The person the change is in the name of.
    pub person: String,
    /// That person's actor, as presence knows them.
    pub person_identity: Option<String>,
    /// The agent (or app, or program) that wrote for the person, when one did.
    pub writer: Option<String>,
    /// The computer whose session vouched for the writer, when one is known.
    pub device: Option<String>,
}

/// [`AuthorParts`] for `author`: what a row shows, field by field.
#[must_use]
pub fn author_parts(author: &HistoryAuthor) -> AuthorParts {
    let attribution = author.attribution.as_ref();
    let person = attribution
        .and_then(|attribution| attribution.on_behalf_of_name.clone())
        .unwrap_or_else(|| author.name.clone());
    let person_identity = attribution
        .and_then(|attribution| attribution.acting.acted_for_person().map(str::to_owned));
    let writer = writer_badge(author)
        .or_else(|| (author.relation == AuthorRelation::Software).then(|| author.name.clone()))
        .filter(|writer| writer.trim() != person.trim());
    AuthorParts {
        person,
        person_identity,
        writer,
        device: attribution.and_then(|attribution| attribution.device.clone()),
    }
}

/// [`AuthorParts`] as a band's lead shows them, with whom software acted for: software that
/// acted for someone leads by its own name ("Planner", then "for Ben"), as its face already
/// says what it is; anyone else as [`author_parts`] names them.
#[must_use]
pub(crate) fn lead_parts(author: &HistoryAuthor) -> (AuthorParts, Option<String>) {
    let mut parts = author_parts(author);
    let acted_for = acted_for(author);
    if acted_for.is_some() {
        parts.person.clone_from(&author.name);
        parts.writer = None;
    }
    (parts, acted_for)
}

/// What a reader hears of an author cell, as one description built from its fields:
/// the person, then what wrote for them, then the computer whose session vouched for
/// it. Never a label joined from names.
#[must_use]
pub fn author_spoken(parts: &AuthorParts) -> String {
    let mut spoken = parts.person.clone();
    if let Some(writer) = parts
        .writer
        .as_deref()
        .filter(|writer| !writer.trim().is_empty())
    {
        spoken = tilo_ui_base::i18n::widget_format(
            "author-spoken-writer",
            &[
                ("person", spoken.into()),
                ("writer", writer.to_owned().into()),
            ],
        );
    }
    if let Some(device) = parts
        .device
        .as_deref()
        .filter(|device| !device.trim().is_empty())
    {
        spoken = tilo_ui_base::i18n::widget_format(
            "author-spoken-device",
            &[("who", spoken.into()), ("device", device.to_owned().into())],
        );
    }
    spoken
}

/// The hover that names what wrote an attributed change, a field to a line: "External
/// agent", "Scout", "‘Prepare the report’", "Kaiserslautern", "/Reports".
/// `None` for a change with no recorded writer, whose row names a person.
pub fn attribution_hover(author: &HistoryAuthor) -> Option<String> {
    // Each fact on a line of its own: the tooltip lays them out apart, never joined by a glyph.
    let attribution = author.attribution.as_ref()?;
    let Some(writer) = attribution.writer.as_ref() else {
        // An agent's own device signed the change: the agent, the computer and
        // the folder, as its own fields.
        if author.relation != AuthorRelation::Software {
            return None;
        }
        let mut parts = vec![
            tilo_ui_base::i18n::widget_format("history-writer-agent", &[]),
            author.name.clone(),
        ];
        parts.extend(acted_for(author));
        if let Some(device) = attribution.device.as_deref()
            && !parts.iter().any(|part| part == device)
        {
            parts.push(device.to_owned());
        }
        parts.extend(attribution.folder.clone());
        return Some(parts.join("\n"));
    };
    let label = attribution.writer_label.as_deref().unwrap_or_default();
    let mut parts = Vec::with_capacity(6);
    match writer {
        collab_types::history::authors::AttributedWriter::Agent { .. } => {
            parts.push(tilo_ui_base::i18n::widget_format("history-writer-agent", &[]));
            // The agent itself when it is the author, then the session's own title.
            if author.relation == AuthorRelation::Software {
                parts.push(author.name.clone());
            }
            if !label.trim().is_empty() {
                parts.push(format!("‘{label}’"));
            }
        }
        collab_types::history::authors::AttributedWriter::App(_) => {
            parts.push(tilo_ui_base::i18n::widget_format("history-writer-app", &[]));
            if !label.trim().is_empty() {
                parts.push(label.to_owned());
            }
        }
        collab_types::history::authors::AttributedWriter::Program(_) => {
            parts.push(tilo_ui_base::i18n::widget_format("history-writer-program", &[]));
            if !label.trim().is_empty() {
                parts.push(label.to_owned());
            }
        }
    }
    parts.extend(acted_for(author));
    // The device whose session vouched for the writer, then the folder the
    // change is in: where a reader goes to look at it.
    // The computer, unless it is what the writer was already called.
    if let Some(device) = attribution.device.as_deref()
        && !parts.iter().any(|part| part == device)
    {
        parts.push(device.to_owned());
    }
    parts.extend(attribution.folder.clone());
    Some(parts.join("\n"))
}

/// Whom software `author` acted for, as the hover and a band's lead say it: "for Ada", "for
/// you". `None` for a person's own change and for software that acted for nobody.
pub(crate) fn acted_for(author: &HistoryAuthor) -> Option<String> {
    if author.relation != AuthorRelation::Software {
        return None;
    }
    if author.for_reader {
        return Some(tilo_ui_base::i18n::widget_format("history-acted-for-you", &[]));
    }
    let person = author.attribution.as_ref()?.on_behalf_of_name.as_deref()?;
    (!person.trim().is_empty() && person.trim() != author.name.trim()).then(|| {
        tilo_ui_base::i18n::widget_format("history-acted-for", &[("person", person.to_owned().into())])
    })
}

fn actor_line(name: &str, for_person: Option<&str>, is_reader: bool, task: Option<&str>) -> String {
    // A task names who did it: the person it was taken for, else the actor. A task that goes by
    // its agent's own name (an agent's run) is that agent's: it says who it acts for.
    let task = task.filter(|label| label.trim() != name.trim());
    let actor = match (task, for_person) {
        (Some(_), Some(person)) => person,
        _ => name,
    };
    let task = task.map(|label| tilo_history::Task { id: "", label });
    with_reader(|reader| {
        reader.actor(&tilo_history::Who {
            actor,
            is_reader,
            task: task.as_ref(),
        })
    })
}

/// History in this thread's words: a step as [`sentence`] says it, and a
/// person as a directory that knows nobody yet names them. What the history
/// verbs below the toolkit (`steps::undo`) speak in for an app.
pub struct ThreadWords;

impl collab_types::history::authors::ResolveAuthor for ThreadWords {
    fn resolve(&self, actor_hex: &str) -> crate::widget::history::HistoryAuthor {
        super::AuthorDirectory::default().resolve(actor_hex)
    }
}

impl collab_types::history::words::HistoryWords for ThreadWords {
    fn sentence(&self, entry: &ActionContext) -> String {
        sentence(entry)
    }
}

/// What a history row says about one change: what it did, else what its
/// actor said they were doing. `None` when the entry says neither.
pub fn step_summary(entry: &ActionContext) -> Option<String> {
    collab_types::history::verbs::step_summary(entry, &ThreadWords)
}

/// The one thing a step acted on, apart from what it did, when its sentence says the two as
/// "what it did" then "the thing" ("Deleted" and "Supplier terms"): a row shows them as separate
/// elements, the thing as a link that opens it. `None` for a step on no named item, or whose
/// sentence says more after the thing ("Renamed Draft to Plan"), which stays one sentence.
#[must_use]
pub fn step_target(entry: &ActionContext, summary: &str) -> Option<crate::widget::EntryTarget> {
    let item = entry.item.as_ref()?;
    let label = with_reader(|reader| reader.item_name(item))?;
    // The item is the sentence's last words, as they are or quoted ("Added to the note “Plan”").
    let summary = summary.trim_end();
    let action = summary.strip_suffix(label.as_str()).or_else(|| {
        [("“", "”"), ("„", "“")]
            .into_iter()
            .find_map(|(open, close)| {
                summary
                    .strip_suffix(close)?
                    .strip_suffix(label.as_str())?
                    .strip_suffix(open)
            })
    })?;
    // The framework's own words for an action it has no sentence for put a colon between the
    // two ("Write note: Supplier terms"): the row lays them out apart instead.
    let action = action.trim_end().trim_end_matches(':').trim();
    (!action.is_empty()).then(|| crate::widget::EntryTarget {
        action: action.to_owned(),
        label,
        id: item.id.clone(),
    })
}

/// What one step did, said without anything it carried: an action by its
/// published label ("Post a message"), never through an app's message,
/// which may quote an argument (the words of the message posted). What
/// someone is doing is told to everyone here, and their words are not.
/// `None` when the entry says neither what it did nor why.
pub fn step_title(entry: &ActionContext) -> Option<String> {
    let Some(tilo_actions::Did::Verb(verb)) = &entry.what else {
        return step_summary(entry);
    };
    let mut bare = entry.clone();
    let mut verb = verb.clone();
    verb.args.clear();
    bare.what = Some(tilo_actions::Did::Verb(verb));
    bare.resource = None;
    bare.item = None;
    // The app's words only where they stand without the value the step
    // carried: a title that needs one is left to the action's own label.
    let reader = Reader::new(&I18nManager::window_locale())
        .with_app(&ThreadCatalogue)
        .with_labels(&ThreadLabels);
    Some(reader.sentence(&bare))
}

thread_local! {
    /// The titles of the fields this thread's app stores, by owner type and
    /// key.
    static FIELD_TITLES: std::cell::RefCell<std::collections::HashMap<(String, String), String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    /// The labels of the actions this thread's app publishes.
    static ACTION_LABELS: std::cell::RefCell<tilo_history::ActionLabels> =
        std::cell::RefCell::new(tilo_history::ActionLabels::default());
    /// The names this thread's app gives its items now, by id.
    static ITEM_NAMES: std::cell::RefCell<std::collections::HashMap<String, String>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Remember what this thread's app calls each item it holds now, by id, in place of what it
/// called them before: a step on one of them reads with its name as it is.
pub fn learn_item_names(names: impl IntoIterator<Item = (String, String)>) {
    ITEM_NAMES.with(|known| *known.borrow_mut() = names.into_iter().collect());
}

/// Remember the labels of the actions `resources` publish, as the app words
/// them in the reader's language, so entries naming them read the same.
pub fn learn_labels(resources: &[tilo_actions::ResourceSpec]) {
    ACTION_LABELS.with(|labels| labels.borrow_mut().learn(resources));
}

/// Remember the words of the actions `app` publishes, as a reader that words other apps' changes
/// (the session's History of a workspace) hears them from the apps: each action's label and what
/// the app says it did.
pub fn learn_published(app: &str, actions: &[tilo_actions::published::PublishedAction]) {
    ACTION_LABELS.with(|labels| labels.borrow_mut().learn_published(app, actions));
}

/// Remember the words of the actions `app` offers, as the device's catalogue describes them.
pub fn learn_described(app: &str, actions: &[tilo_actions::ActionDescriptor]) {
    ACTION_LABELS.with(|labels| labels.borrow_mut().learn_described(app, actions));
}

/// Remember the words of the actions the record type `R` declares, for a reader that holds such
/// records itself (the session's workspace agents): each action's label and what it did.
pub fn learn_declared<R: tilo_resource::Resource>() {
    let declared = R::declarations();
    ACTION_LABELS.with(|labels| {
        labels.borrow_mut().learn_declared(
            R::KIND,
            declared
                .iter()
                .map(|decl| (decl.id, decl.label.as_str(), decl.did.as_deref())),
        );
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
pub struct ThreadLabels;

/// The label this thread's app publishes for `action` of a resource of
/// `kind` (or of any kind, when that one has none), in the reader's language.
#[must_use]
pub fn action_label(kind: &str, action: &str) -> Option<String> {
    tilo_history::Labels::action(
        &ThreadLabels,
        &tilo_actions::ActionKey::new("", kind, action),
    )
}

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
        ACTION_LABELS.with(|labels| tilo_history::Labels::action(&*labels.borrow(), key))
    }

    fn did(&self, key: &tilo_actions::ActionKey) -> Option<String> {
        ACTION_LABELS.with(|labels| tilo_history::Labels::did(&*labels.borrow(), key))
    }

    fn item(&self, id: &str) -> Option<String> {
        ITEM_NAMES.with(|names| names.borrow().get(id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use tilo_ui_base::I18nManager;
    use tilo_actions::{
        ActionContext, ActionKey, ActionResource, Did, EditOp, FieldEdit, PathStep, Scalar,
    };

    /// A step that edits `title` of a workbook, with the value it set (`new`)
    /// when the step carried one.
    fn title_step(new: Option<Scalar>) -> ActionContext {
        ActionContext::for_app("so.tilo.sheets", "Sheets").with_described(
            tilo_actions::Described::new(Did::Edit(FieldEdit {
                ty: "workbook".into(),
                field: "title".into(),
                path: vec![PathStep::Field("title".into())],
                op: EditOp::Set,
                old: None,
                new,
                args: Default::default(),
            })),
        )
    }

    #[test]
    fn an_app_message_a_step_cannot_complete_is_left_to_the_frameworks_own_words() {
        I18nManager::install(
            "en-US",
            "en-US",
            &[(
                "en-US",
                "sheets-workbook-title = Named the workbook { $new }",
            )],
        )
        .unwrap();
        // The step named the workbook: the app's sentence says which name it gave it.
        let named = super::sentence(&title_step(Some(Scalar::text("Q3"))));
        assert_eq!(named, "Named the workbook Q3");
        // The step carried no name, so the app's message is not one it can say:
        // the framework's own words for the edit stand in, never the app's
        // with a placeable left empty.
        let bare = super::sentence(&title_step(None));
        assert!(!bare.contains("workbook"), "{bare}");
        assert!(!bare.is_empty());
    }

    /// A row says what was done apart from the thing it was done to, which it links, whether its
    /// sentence names the thing as it is or quoted.
    #[test]
    fn a_step_target_is_found_quoted_or_not() {
        let entry = ActionContext::for_resource("notes.pages", "notes.pages", "Notes")
            .with_app("so.tilo.notes", "Notes")
            .with_described(
                tilo_actions::Described::new(Did::verb(
                    ActionKey::new("so.tilo.notes", "notes.pages", "append-to-note"),
                    [],
                ))
                .on("n1", "Spring plans"),
            );
        for (summary, action) in [
            ("Added to the note “Spring plans”", "Added to the note"),
            ("Added to the note: „Spring plans“", "Added to the note"),
            ("Ran “Pin” on Spring plans", "Ran “Pin” on"),
        ] {
            let target = super::step_target(&entry, summary).expect(summary);
            assert_eq!(
                (
                    target.action.as_str(),
                    target.label.as_str(),
                    target.id.as_str()
                ),
                (action, "Spring plans", "n1")
            );
        }
        assert!(super::step_target(&entry, "Renamed Spring plans to Summer").is_none());
    }

    #[test]
    fn a_step_title_names_the_action_and_never_what_it_carried() {
        let posted = ActionContext {
            resource: Some(ActionResource {
                id: "chat.messages".into(),
                kind: "chat.messages".into(),
                label: "Messages".into(),
            }),
            what: Some(Did::verb(
                ActionKey::new("org.tilo.Chat", "chat.messages", "post"),
                Scalar::from_json(&serde_json::json!("the words they wrote"))
                    .map(|text| ("text".to_owned(), text)),
            )),
            ..ActionContext::default()
        };
        let title = super::step_title(&posted).unwrap_or_default();
        assert!(title.contains("Post"), "{title}");
        assert!(!title.contains("words they wrote"), "{title}");
        assert!(!title.contains("Messages"), "{title}");
    }
}
