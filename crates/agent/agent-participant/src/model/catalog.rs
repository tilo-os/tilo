//! How much of what the agent's apps can do a plan is told
//! ([`Catalog`]), and where ([`system_messages`]).
//!
//! In [`Catalog::All`] the reference describes every app the context
//! offers it ([`super::briefing`]). In [`Catalog::Relevant`] the first turn
//! opens with one line per granted app ([`index`]), the same for every plan
//! of the agent, and describes in full only the apps the request is about
//! ([`relevant_apps`]); the others' operations come on demand, at the cost
//! of a turn: `list_operations` for one of them answers with its section
//! ([`lookup`]), and a plan naming an operation of one is described its app
//! with the refusal, if it is refused. Each plan counts what was described,
//! looked up and missed ([`CatalogUse`]), for the plan log.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use tilo_actions::door::ActionDescriptor;
use tilo_agent::surface::{self, Catalogue, Offer};
use tilo_model_gateway::host_backend::protocol::{Message, Role, ToolDefinition};

use super::{
    APPLY_ACTION, Briefing, GROUP_ACTION, MOST_OPERATION_CHARS, MOST_REFERENCE_CHARS, noted,
    planned,
};
use crate::planner::PlanContext;
pub use crate::profile::Catalog;

/// One app's operations in full, as the reference shows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Section {
    /// Each operation's id, label, description and parameters, a few lines
    /// each.
    pub text: String,
    /// The ids it shows that a plan may name now: those offered.
    pub shown: Vec<String>,
    /// The operations it describes, by resource kind and name, with the
    /// signature of their parameters.
    pub described: Vec<(String, String, String)>,
    /// How many operations it left out, over the budget.
    pub left_out: usize,
    /// The resources and resource kinds its operations act on, by which an
    /// operation id is told to be the app's.
    pub resources: BTreeSet<String>,
}

/// The [`Section`] of `app`, whose new record publishes `descriptors`, with
/// `offered` the ids the plan may name now: its operations (the generic
/// ones its typed edits cover aside), document operations first, within
/// [`MOST_REFERENCE_CHARS`], each within [`MOST_OPERATION_CHARS`].
pub(crate) fn app_section(
    app: &str,
    descriptors: &[ActionDescriptor],
    offered: &BTreeSet<String>,
) -> Section {
    let mut ordered: Vec<&ActionDescriptor> = descriptors
        .iter()
        .filter(|descriptor| planned(descriptor, descriptors))
        .collect();
    ordered.sort_by_key(|descriptor| descriptor.kind != tilo_actions::OperationKind::Document);
    let mut section = Section::default();
    for descriptor in ordered {
        section
            .resources
            .insert(descriptor.address.resource.clone());
        section.resources.insert(descriptor.resource_kind.clone());
        let offer = Offer {
            app: app.to_owned(),
            broker_peer_id: None,
            broker_provider: None,
            descriptor: noted(descriptor),
        };
        let id = offer.id();
        let mut detail = format!("{id} — {}", offer.descriptor.action_label);
        if let Some(description) = &offer.descriptor.description {
            let _ = write!(detail, ". {description}");
        }
        detail.push('\n');
        if offer.wants_content() {
            detail.push_str("    takes what the previous step produced (it is given it)\n");
        }
        // The framework's `group` takes a list of what `apply` takes: its
        // schema is `apply`'s again, so it is said in a line.
        let grouped = offer.descriptor.address.action == GROUP_ACTION
            && descriptors.iter().any(|other| {
                other.address.action == APPLY_ACTION
                    && other.resource_kind == descriptor.resource_kind
            });
        if grouped {
            let _ = writeln!(
                detail,
                "    actions: array (required) — the edits, each {{\"action\": \"apply\", \"args\": \
                 <the argument object apply takes>}}"
            );
        } else {
            detail.push_str(&tilo_agent::tools::argument_lines_compact(&offer, "    "));
        }
        let size = detail.chars().count();
        if size > MOST_OPERATION_CHARS
            || section.text.chars().count().saturating_add(size) > MOST_REFERENCE_CHARS
        {
            section.left_out = section.left_out.saturating_add(1);
            continue;
        }
        section.text.push_str(&detail);
        section.described.push((
            offer.descriptor.resource_kind.clone(),
            offer.descriptor.address.action.clone(),
            tilo_agent::tools::parameter_signature(&offer),
        ));
        if offered.contains(&id) {
            section.shown.push(id);
        }
    }
    section
}

/// `section`, `app`'s, added to what `briefing` describes in full: its
/// reference, opened with `intro`, the ids it shows and its operations. An
/// app none of whose operations fit adds nothing.
pub(crate) fn describe(briefing: &mut Briefing, app: &str, section: &Section, intro: &str) {
    if section.text.is_empty() {
        return;
    }
    let reference = &mut briefing.reference;
    if reference.is_empty() {
        reference.push_str(intro);
        reference.push('\n');
    }
    let _ = writeln!(reference, "## {app}");
    reference.push_str(&section.text);
    if section.left_out > 0 {
        briefing.left_out = true;
        let _ = writeln!(
            reference,
            "({} more operations of {app}: their signatures are listed with the ids; ask \
             `describe_operation` for one only if that is not enough.)",
            section.left_out
        );
    }
    briefing.shown.extend(section.shown.iter().cloned());
    briefing
        .described
        .extend(section.described.iter().map(|(kind, action, signature)| {
            (
                (app.to_owned(), kind.clone(), action.clone()),
                signature.clone(),
            )
        }));
    briefing.described_apps.push(app.to_owned());
}

/// What opens a [`Catalog::Relevant`] reference.
const RELEVANT_INTRO: &str = "What the apps this request is about can do, with every \
    parameter in full (types, required, choices, constraints). The ids to name are the ones \
    listed under \"Operations published right now\" below: a document's own resources (a \
    workbook's sheets) carry their own ids, with these same operations.";

/// What opens the [`index`].
const INDEX_INTRO: &str = "The apps you may use, one line each: what each works on and some \
    of what it can do. Only the apps the request is about have their operations described in \
    full next. Before planning an operation of another, call `list_operations` with \
    `application` set to its id to get them in full, or plan its open and put the rest in \
    `then`.";

/// What the planner's instructions add in [`Catalog::Relevant`].
pub(crate) const INSTRUCTIONS_NOTE: &str = " Here these instructions end with one line per \
    app you may use and then describe in full only the apps the request is about, before the \
    request; for another app, call `list_operations` with its `application` id before you plan \
    its operations.";

/// The system messages a plan opens with: `instructions`, the same for
/// every plan (in [`Catalog::Relevant`] ending with the [`index`], the same
/// for every plan of the agent), then the apps' reference. A provider
/// caches a prompt up to the end of a system message, never within the
/// request after them: every plan reads the instructions and the tools from
/// its cache whatever the reference describes, and the reference too when
/// it describes the same apps, rather than writing them to it again. The
/// instructions stay a message of their own, so a provider that takes them
/// apart (`OpenAI`'s `instructions`, Anthropic's first system block) keeps
/// what varies out of them.
pub(crate) fn system_messages(instructions: &str, briefing: &Briefing) -> Vec<Message> {
    let mut opening = instructions.to_owned();
    if briefing.catalog == Catalog::Relevant {
        opening.push_str(INSTRUCTIONS_NOTE);
        if !briefing.index.is_empty() {
            opening.push_str("\n\n");
            opening.push_str(&briefing.index);
        }
    }
    let mut messages = vec![Message::text(Role::System, opening)];
    if !briefing.reference.is_empty() {
        messages.push(Message::text(Role::System, briefing.reference.clone()));
    }
    messages
}

/// The most characters of one app's line in the [`index`] given to the
/// operations it names.
const MOST_INDEX_OPERATION_CHARS: usize = 160;

/// The most resource kinds one app's line in the [`index`] names.
const MOST_INDEX_KINDS: usize = 4;

/// One line per app of `context`'s reference, by app id: its name, the
/// kinds of resource it works on and the first of its operations. It
/// depends on the granted apps alone, so every plan of the agent opens
/// with the same bytes.
pub(crate) fn index(context: &PlanContext) -> String {
    let mut index = String::new();
    for (app, descriptors) in &context.reference {
        if index.is_empty() {
            index.push_str(INDEX_INTRO);
            index.push('\n');
        }
        let mut ordered: Vec<&ActionDescriptor> = descriptors
            .iter()
            .filter(|descriptor| planned(descriptor, descriptors))
            .collect();
        ordered.sort_by_key(|descriptor| descriptor.kind != tilo_actions::OperationKind::Document);
        let mut kinds: Vec<&str> = Vec::new();
        let mut actions: Vec<&str> = Vec::new();
        for descriptor in &ordered {
            let kind = descriptor.resource_kind.as_str();
            if kind != surface::APPLICATION_KIND && !kinds.contains(&kind) {
                kinds.push(kind);
            }
            let action = descriptor.address.action.as_str();
            if !actions.contains(&action) {
                actions.push(action);
            }
        }
        let _ = write!(index, "- {app}");
        if let Some(name) = name_of(context, app) {
            let _ = write!(index, " ({name})");
        }
        let more = kinds.len() > MOST_INDEX_KINDS;
        kinds.truncate(MOST_INDEX_KINDS);
        let _ = write!(index, ": {}", kinds.join(", "));
        if more {
            index.push_str(", …");
        }
        let mut named = String::new();
        let mut left = actions.len();
        for action in &actions {
            if named.chars().count().saturating_add(action.len()) > MOST_INDEX_OPERATION_CHARS {
                break;
            }
            if !named.is_empty() {
                named.push_str(", ");
            }
            named.push_str(action);
            left = left.saturating_sub(1);
        }
        let _ = write!(index, " — {named}");
        if left > 0 {
            let _ = write!(index, " and {left} more");
        }
        index.push('\n');
    }
    index
}

/// What `app` is called, when the context says.
fn name_of<'c>(context: &'c PlanContext, app: &str) -> Option<&'c str> {
    context
        .app_names
        .iter()
        .find(|(named, _)| named == app)
        .map(|(_, name)| name.as_str())
}

/// The fewest characters of a record's title that name it.
const MIN_TITLE_CHARS: usize = 3;

/// The apps of `context`'s reference the plan of `task` is about:
///
/// - those the request names by their name (Sheets), what they call
///   themselves (PDF Viewer) or a word for their records (the spreadsheet);
/// - those of a record the request names by its title, or a tag
///   (`#logins`) the record holds;
/// - those of the records the task made, touched, read or has open (the
///   participant offers only those of the open ones) and those a step
///   would start a new record of;
/// - every one of an item's agent's, which are the item's own.
///
/// `task` is what is planned (a re-plan's rest says what arrived and where
/// news came from); the request as given counts too.
pub(crate) fn relevant_apps(task: &str, context: &PlanContext) -> BTreeSet<String> {
    let mut text = task.to_owned();
    if let Some(request) = context.request.as_deref() {
        text.push('\n');
        text.push_str(request);
    }
    let lower = text.to_lowercase();
    let words = super::scripted::words(&text);
    let tags: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ')' | '(' | '"' | '\''))
        .map(|word| word.trim_end_matches(['.', ':', '!', '?']))
        .filter(|word| word.starts_with('#') && word.chars().count() > MIN_TITLE_CHARS)
        .collect();
    let titled = |title: &str| {
        let title = title.trim().to_lowercase();
        title.chars().count() >= MIN_TITLE_CHARS && lower.contains(&title)
    };
    let item = matches!(context.role, crate::planner::Role::Item { .. });
    context
        .reference
        .iter()
        .map(|(app, _)| app)
        .filter(|app| {
            let app = app.as_str();
            item || context.open.iter().any(|(route, _)| route.app_id == app)
                || context
                    .previews
                    .iter()
                    .any(|(previewed, _)| previewed == app)
                || context.new_documents.iter().any(|new| new == app)
                || super::task_names_app(&text, app)
                || name_of(context, app).is_some_and(|name| {
                    let name = super::scripted::words(name);
                    !name.is_empty() && name.iter().all(|word| words.contains(word))
                })
                || context.known.iter().any(|(route, title)| {
                    route.app_id == app && title.as_deref().is_some_and(titled)
                })
                || context.searchable.iter().any(|doc| {
                    doc.route.app_id == app
                        && (doc.title.as_deref().is_some_and(titled)
                            || tags.iter().any(|tag| doc.text.to_lowercase().contains(tag)))
                })
        })
        .cloned()
        .collect()
}

/// An app the [`Catalog::Relevant`] reference leaves out, for
/// `list_operations` to describe.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookup {
    /// What the app is called, when the context says.
    pub name: Option<String>,
    /// Its operations in full.
    pub section: Section,
}

/// `briefing`, made over `context` for `task` in [`Catalog::Relevant`]:
/// its reference describes only the [`relevant_apps`], after the
/// [`index`]; the others are kept to be looked up. It says an operation
/// is left out where any granted app's section leaves one out.
pub(crate) fn narrow(
    briefing: &mut Briefing,
    task: &str,
    context: &PlanContext,
    catalogue: &Catalogue,
) {
    let offered: BTreeSet<String> = catalogue.offers.iter().map(Offer::id).collect();
    let relevant = relevant_apps(task, context);
    briefing.catalog = Catalog::Relevant;
    briefing.index = index(context);
    briefing.reference.clear();
    briefing.shown.clear();
    briefing.left_out = false;
    briefing.described.clear();
    briefing.described_apps.clear();
    briefing.lookups.clear();
    let mut left_out = false;
    for (app, descriptors) in &context.reference {
        let section = app_section(app, descriptors, &offered);
        left_out |= !section.text.is_empty() && section.left_out > 0;
        if relevant.contains(app) {
            describe(briefing, app, &section, RELEVANT_INTRO);
        } else {
            briefing.lookups.insert(
                app.clone(),
                Lookup {
                    name: name_of(context, app).map(str::to_owned),
                    section,
                },
            );
        }
    }
    // Whether `describe_operation` is offered depends on every granted
    // app, never on which are described, so every plan has the same tools.
    briefing.left_out = left_out;
}

/// `tools`, with `list_operations` first, as every turn of a
/// [`Catalog::Relevant`] plan has it.
pub(crate) fn with_lookup(tools: &mut Vec<ToolDefinition>, most_steps: usize) {
    tools.retain(|tool| tool.name != tilo_agent::tools::LIST_OPERATIONS);
    if let Some(list) = tilo_agent::tools::typed_definitions(most_steps)
        .into_iter()
        .find(|tool| tool.name == tilo_agent::tools::LIST_OPERATIONS)
    {
        tools.insert(0, list);
    }
}

/// The app of `briefing`'s lookups `asked` names: its id, the last part of
/// it (`mail`), or what it is called, in any case.
fn looked_up<'b>(briefing: &'b Briefing, asked: &str) -> Option<(&'b String, &'b Lookup)> {
    let asked = asked.trim().to_lowercase();
    briefing.lookups.iter().find(|(app, lookup)| {
        let app = app.to_lowercase();
        app == asked
            || app.rsplit('.').next() == Some(asked.as_str())
            || lookup
                .name
                .as_deref()
                .is_some_and(|name| name.to_lowercase() == asked)
    })
}

/// The answer to a `list_operations` call naming an app the reference left
/// out (`input`'s `application`): its operations in full, with what of it
/// is published now and how to use them when none is. `None` for any other
/// call, answered as before.
pub(crate) fn lookup(
    input: &serde_json::Value,
    briefing: &Briefing,
    catalogue: &Catalogue,
    revealed: &mut tilo_agent::tools::Revealed,
) -> Option<String> {
    let asked = input
        .get("application")
        .and_then(serde_json::Value::as_str)?;
    let (app, found) = looked_up(briefing, asked)?;
    let mut answer = app_description(app, found, catalogue, revealed);
    if catalogue.offers.iter().any(|offer| offer.app == *app) {
        let published = tilo_agent::tools::answer(
            tilo_agent::tools::LIST_OPERATIONS,
            &serde_json::json!({"application": app}),
            catalogue,
            revealed,
        );
        let _ = write!(answer, "\nPublished now:{published}");
    }
    note(|used| used.looked_up(app));
    Some(answer)
}

/// `app`'s operations in full (`found`), revealing the ids of them a plan
/// may name now, and how to reach the rest when its record is not open.
fn app_description(
    app: &str,
    found: &Lookup,
    catalogue: &Catalogue,
    revealed: &mut tilo_agent::tools::Revealed,
) -> String {
    for id in &found.section.shown {
        revealed.reveal(id.clone());
    }
    let usable = catalogue
        .offers
        .iter()
        .any(|offer| offer.app == app && tilo_agent::tools::opened_app(offer).is_none());
    let mut text = format!("What {app} can do, with every parameter in full:\n");
    text.push_str(&found.section.text);
    if found.section.left_out > 0 {
        let _ = writeln!(
            text,
            "({} more operations of {app}, too large to show here.)",
            found.section.left_out
        );
    }
    if !usable {
        let _ = writeln!(
            text,
            "None of {app}'s documents is open, so these operations are not published yet: plan \
             the open of the document you need (record.{app}.<record>#open, as listed) or \
             app.{app}#open for a new one, and put what uses them in `then`."
        );
    }
    text
}

/// The app whose operation `id` names, when it is not an open: the app of
/// the offer of that id, else the app of `briefing`'s lookups whose
/// operations act on its resource.
fn app_of(id: &str, briefing: &Briefing, catalogue: &Catalogue) -> Option<String> {
    if let Some(offer) = catalogue.offers.iter().find(|offer| offer.id() == id) {
        return tilo_agent::tools::opened_app(offer)
            .is_none()
            .then(|| offer.app.clone());
    }
    let address = surface::decode_address(id)?;
    let kind = address
        .resource
        .split_once(':')
        .map_or(address.resource.as_str(), |(kind, _)| kind);
    briefing
        .lookups
        .iter()
        .find(|(_, lookup)| {
            lookup.section.resources.contains(&address.resource)
                || lookup.section.resources.contains(kind)
        })
        .map(|(app, _)| app.clone())
}

/// The apps a `plan` call's `input` names an operation of that the plan was
/// not told (neither described in full nor looked up yet), once each, and
/// how many of its steps name one: misses of what is relevant.
pub(crate) fn missed(
    input: &serde_json::Value,
    briefing: &Briefing,
    catalogue: &Catalogue,
) -> (Vec<String>, usize) {
    let told = |app: &str| {
        briefing
            .described_apps
            .iter()
            .any(|described| described == app)
            || used(|used| used.looked_up.iter().any(|looked| looked == app))
    };
    let mut apps = Vec::new();
    let mut steps = 0_usize;
    for step in input
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = step.get("operation").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(app) = app_of(id.trim(), briefing, catalogue) else {
            continue;
        };
        if told(&app) {
            continue;
        }
        steps = steps.saturating_add(1);
        if !apps.contains(&app) {
            apps.push(app);
        }
    }
    (apps, steps)
}

/// What a plan refused for `refusal` is told when it named operations of
/// `apps`, which it was not told: each one's operations in full, so its
/// next turn plans with them. Counted as looked up.
pub(crate) fn described_on_refusal(
    refusal: String,
    apps: &[String],
    briefing: &Briefing,
    catalogue: &Catalogue,
    revealed: &mut tilo_agent::tools::Revealed,
) -> String {
    let mut text = refusal;
    for app in apps {
        let Some(found) = briefing.lookups.get(app) else {
            continue;
        };
        let _ = write!(
            text,
            "\n\nThe plan names operations of {app}, which were not described yet. {}",
            app_description(app, found, catalogue, revealed)
        );
        note(|used| used.looked_up(app));
    }
    text
}

/// What one plan was told of the apps and how it used it, for the plan
/// log: in which mode, the apps described in full, those listed in a line,
/// those looked up (asked for, or described after a plan named them) and
/// how many steps named an operation of an app it was not told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogUse {
    pub mode: Catalog,
    pub described: Vec<String>,
    pub listed: Vec<String>,
    pub looked_up: Vec<String>,
    pub misses: usize,
}

impl CatalogUse {
    /// What `briefing` tells a plan.
    pub(crate) fn of(briefing: &Briefing) -> Self {
        Self {
            mode: briefing.catalog,
            described: briefing.described_apps.clone(),
            listed: briefing.lookups.keys().cloned().collect(),
            ..Self::default()
        }
    }

    fn looked_up(&mut self, app: &str) {
        if !self.looked_up.iter().any(|looked| looked == app) {
            self.looked_up.push(app.to_owned());
        }
    }

    /// The plan log's `catalog`.
    #[must_use]
    pub fn logged(&self) -> serde_json::Value {
        serde_json::json!({
            "mode": self.mode.name(),
            "described": self.described,
            "listed": self.listed,
            "looked_up": self.looked_up,
            "misses": self.misses,
        })
    }
}

std::thread_local! {
    /// What the plan being made on this thread was told and how it used it.
    static CATALOG_USE: std::cell::RefCell<Option<CatalogUse>> =
        const { std::cell::RefCell::new(None) };
}

/// Start counting a plan on this thread, told `used`.
pub(crate) fn begin(used: CatalogUse) {
    CATALOG_USE.with(|current| *current.borrow_mut() = Some(used));
}

/// What the plan made on this thread since [`begin`] was told and used.
pub(crate) fn take() -> Option<CatalogUse> {
    CATALOG_USE.with(|current| current.borrow_mut().take())
}

/// Count `misses` steps that named an operation of an app the plan was not
/// told.
pub(crate) fn note_misses(misses: usize) {
    note(|used| used.misses = used.misses.saturating_add(misses));
}

fn note(change: impl FnOnce(&mut CatalogUse)) {
    CATALOG_USE.with(|current| {
        if let Some(used) = current.borrow_mut().as_mut() {
            change(used);
        }
    });
}

fn used(read: impl FnOnce(&CatalogUse) -> bool) -> bool {
    CATALOG_USE.with(|current| current.borrow().as_ref().is_some_and(read))
}
