//! Natural-language tasks, planned by a model from the published surface.
//!
//! [`ModelPlanner`] sits behind the explicit-steps path: a task that is JSON
//! steps is read exactly as [`crate::DirectPlanner`] reads it; any other
//! text goes to a [`PlanModel`]. The model is `apps/agent`'s action
//! discovery (`tilo_agent::tools` over a `tilo_agent::surface::Catalogue`):
//! it is shown the published operations with the task, lists and describes
//! more through read-only tools when it needs to, and finishes with a `plan`
//! naming only operations it was shown. Its answer is
//! untrusted: every step is mapped back to an offered address here, and then
//! checked against the delegation and the app's door by the participant like
//! any other step.
//!
//! [`GatewayModel`] asks through `tilo-model-gateway`, which holds the
//! provider credential; the agent never sees a key. Tests use their own
//! [`PlanModel`] over the same tools.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_agent::surface::{self, Catalogue, Offer};
use tilo_agent::tools::Answer;
use tilo_browser_peer::DocumentRoute;
/// Trusted-host CLI credential source; sandboxed agents use the gateway capability.
pub use tilo_model_gateway::host_backend::ApiKey;

use crate::planner::{
    NoReads, Obligations, PlanContext, Planner, Reads, Step, StepAction, Wait, observation_within,
    readable, rendered_picture, written_steps,
};
use crate::vision::{Picture, Seen};

mod converse;
mod openai;
mod scripted;
#[cfg(test)]
pub(crate) mod tests;

pub use converse::{GatewayTurns, PlanTurns};
pub use openai::{CLOUD_MODEL, ModelChoice, OpenAiModel};
pub use scripted::ScriptedModel;
pub(crate) use scripted::{task_names_app, task_names_app_itself};

/// The most steps one plan may name. A plan carries the whole request
/// (every slide of a deck, every row of a sheet) rather than a few steps
/// and a re-plan for the rest, which costs a whole model call.
pub const MOST_STEPS: usize = 40;

/// What a model is told besides the operations list, built by
/// [`ModelPlanner`] for each plan: what the agent's apps can do, in full,
/// and what the documents the task is about hold now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Briefing {
    /// The operations of the apps the agent may use with their parameters
    /// in full. The same bytes for every agent and task granted the same
    /// apps, whatever the room holds, so it leads the prompt and a
    /// provider's prompt cache keeps it; empty when there is none.
    pub reference: String,
    /// What changes from plan to plan: what the documents the task is about
    /// hold now, and which apps a step would open first. Empty for none.
    pub state: String,
    /// The operation ids the reference shows that a plan may name.
    pub shown: Vec<String>,
    /// Whether the reference left any operation's parameters out, so the
    /// model is offered `describe_operation` for them.
    pub left_out: bool,
    /// The operations the reference describes in full, by app, resource
    /// kind and operation name: the list of what is published names them
    /// without repeating their parameters.
    pub described: std::collections::BTreeSet<(String, String, String)>,
    /// The prompt-cache routing key every planning request in the room
    /// shares, so its agents' requests reach the cache their common prefix
    /// is in.
    pub cache_key: Option<String>,
    /// The room's records [`crate::search::SEARCH_ROOM`] searches
    /// ([`PlanContext::searchable`]).
    pub searchable: Vec<crate::search::SearchDoc>,
    /// Whether a plan of no steps, with a reply saying so, is an answer
    /// ([`PlanContext::may_plan_nothing`]).
    pub may_plan_nothing: bool,
    /// The pictures the first turn shows after its text: on a re-plan, the
    /// newest the task's steps looked at ([`PlanContext::pictures`]).
    pub pictures: Vec<Picture>,
    /// The task's budget for asking a call lost to the network again
    /// ([`PlanContext::retries`]).
    pub retries: crate::planner::TurnRetries,
}

impl Briefing {
    /// Whether the model needs `describe_operation`: the reference left an
    /// operation out, or there is no reference.
    #[must_use]
    pub fn describes(&self) -> bool {
        self.left_out || self.reference.is_empty()
    }

    /// Whether `search_room` is offered: only when the room holds more
    /// records than the briefing names (`KNOWN_OFFERED`). Where every
    /// record is shown, a search only costs a turn.
    #[must_use]
    pub fn searches(&self) -> bool {
        self.searchable.len() > KNOWN_OFFERED
    }

    /// The reference, then the state: the briefing as one text.
    #[must_use]
    pub fn text(&self) -> String {
        [self.reference.as_str(), self.state.as_str()]
            .iter()
            .filter(|part| !part.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// What a planning conversation runs while the model plans: the reads it
/// asks for ([`READ_VIEW`]) and the pictures it looks at
/// ([`crate::vision::LOOK_CONTENT`]).
pub trait PlanReads {
    /// Run the read operation id `id` names with `arguments`, answering
    /// with what it found, summarised, and the picture it rendered.
    ///
    /// # Errors
    ///
    /// Why it was not run.
    fn read(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Seen, String>;

    /// Run the read `id` names twice, with `arguments` and with `against`,
    /// answering with what the first found and what differs from the second
    /// answer to it ([`crate::facts::changes`]).
    ///
    /// # Errors
    ///
    /// Why either was not run.
    fn compare(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        against: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Seen, String> {
        let _ = (id, arguments, against);
        Err("no document can be read here".into())
    }

    /// The picture kept as content at `address`, as the model is shown it.
    ///
    /// # Errors
    ///
    /// Why it is not shown.
    fn look(&mut self, address: &str) -> Result<Seen, String> {
        let _ = address;
        Err("no picture can be shown here".into())
    }
}

/// What runs nothing: a conversation where no document can be read.
pub struct NoPlanReads;

impl PlanReads for NoPlanReads {
    fn read(
        &mut self,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Seen, String> {
        Err("no document can be read here".into())
    }
}

/// The reads and looks of a planning conversation ([`PlanReads`]).
pub type ReadView<'a> = dyn PlanReads + 'a;

/// The discovery tool that runs a read of an open record in the planning
/// conversation itself.
pub const READ_VIEW: &str = "read_view";

/// Chooses operations for a task from a catalogue.
pub trait PlanModel: Send {
    /// The operations (by offer id) and their arguments that carry out
    /// `task`, using only `catalogue`.
    ///
    /// # Errors
    ///
    /// Returns why no plan was made: no model, a failed call, or an answer
    /// the discovery tools refused.
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String>;

    /// [`Self::choose`], told `briefing` about the task's records and able
    /// to run reads of the open ones through `read` while it decides. A
    /// model that takes neither chooses as [`Self::choose`] does.
    ///
    /// # Errors
    ///
    /// As [`Self::choose`].
    fn choose_briefed(
        &mut self,
        task: &str,
        briefing: &Briefing,
        catalogue: &Catalogue,
        most_steps: usize,
        read: &mut ReadView<'_>,
    ) -> Result<Answer, String> {
        let _ = (briefing, read);
        self.choose(task, catalogue, most_steps)
    }

    /// Whether the model is shown pictures: what a read rendered, a
    /// picture it looks at, the newest a re-plan's steps looked at. One
    /// that is not reads that they were not shown.
    fn sees(&self) -> bool {
        false
    }
}

/// The Settings-selected provider, through the per-user model gateway,
/// which holds the provider credential: the agent never sees a key. It
/// plans in the same conversation as [`OpenAiModel`] (reads, looks,
/// searches and waits included), its turns carried by the gateway.
pub struct GatewayModel {
    turns: GatewayTurns,
    sees: bool,
    timeout: std::time::Duration,
}

impl GatewayModel {
    /// A model call over `selection`, bounded by `timeout`.
    #[must_use]
    pub fn new(
        selection: tilo_agent::providers::ProviderSelection,
        timeout: std::time::Duration,
    ) -> Self {
        Self {
            sees: selection.model.vision,
            turns: GatewayTurns::new(selection),
            timeout,
        }
    }
}

impl PlanModel for GatewayModel {
    fn choose(
        &mut self,
        task: &str,
        catalogue: &Catalogue,
        most_steps: usize,
    ) -> Result<Answer, String> {
        self.choose_briefed(
            task,
            &Briefing::default(),
            catalogue,
            most_steps,
            &mut NoPlanReads,
        )
    }

    fn choose_briefed(
        &mut self,
        task: &str,
        briefing: &Briefing,
        catalogue: &Catalogue,
        most_steps: usize,
        read: &mut ReadView<'_>,
    ) -> Result<Answer, String> {
        let sees = self.sees;
        converse::plan(
            &mut self.turns,
            &converse::Asked {
                task,
                briefing,
                catalogue,
                most_steps,
                sees,
                timeout: self.timeout,
            },
            read,
        )
    }

    fn sees(&self) -> bool {
        self.sees
    }
}

/// Explicit steps as written; anything else planned by a model.
pub struct ModelPlanner {
    model: Box<dyn PlanModel>,
    /// Whether its tasks are checked against their requests once done.
    checks_work: bool,
    /// Whether the last plan it made was the model's.
    by_model: bool,
    /// The rest this planner last handed back as a
    /// [`StepAction::Continue`], and how many re-plans in a row led to it.
    continued: Option<(String, usize)>,
    /// The checklist the last plan declared, until it is taken.
    obligations: Obligations,
}

impl ModelPlanner {
    /// Plan natural-language tasks with `model`.
    #[must_use]
    pub fn new(model: Box<dyn PlanModel>) -> Self {
        Self {
            model,
            checks_work: false,
            by_model: false,
            continued: None,
            obligations: Obligations::default(),
        }
    }

    /// This planner, with its tasks checked against their requests once
    /// their steps ran when `on`.
    #[must_use]
    pub fn checking_work(mut self, on: bool) -> Self {
        self.checks_work = on;
        self
    }
}

/// The most times one task is planned again from a
/// [`StepAction::Continue`], so a model that keeps deferring cannot loop.
const MOST_REPLANS: usize = 8;

/// Where an offered operation runs.
enum Target {
    Record(DocumentRoute),
    /// On a new record of the app, opened by the step itself
    /// ([`PlanContext::previews`]).
    New(String),
    Open(String),
    OpenRecord(DocumentRoute),
}

/// Words that carry no meaning of their own in an operation's label.
const LABEL_FILLER: &[&str] = &[
    "the", "and", "with", "for", "into", "from", "this", "that", "its",
];

/// Every lowercase word of `text`, its quoted titles included.
fn all_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// `task` as the one operation of an open record it unmistakably asks for:
/// a single request that begins with the operation's verb, holds every
/// word of its label and names each required argument by one of its
/// choices. `None` when no operation, or more than one, fits that way, or
/// the task asks for more than one thing.
fn single_operation(
    task: &str,
    catalogue: &Catalogue,
    targets: &BTreeMap<String, Target>,
) -> Option<Answer> {
    let trimmed = task.trim().trim_end_matches('.');
    if trimmed.contains(['.', ';', '\n']) || trimmed.to_lowercase().contains(" then ") {
        return None;
    }
    let said = all_words(trimmed);
    let verb = said.first()?;
    // A quoted phrase is a value an operation's free text takes (a new
    // sheet's name): an operation with such a parameter is left to the model.
    let quotes = trimmed.contains(['"', '\u{201c}']);
    let fits =
        catalogue.offers.iter().filter_map(|offer| {
            if !matches!(targets.get(&offer.id()), Some(Target::Record(_))) {
                return None;
            }
            if quotes
                && offer.descriptor.params.iter().any(|param| {
                    param.ty == tilo_actions::ParamType::Text && param.choices.is_empty()
                })
            {
                return None;
            }
            let label: Vec<String> = all_words(&offer.descriptor.action_label)
                .into_iter()
                .filter(|word| word.chars().count() >= 3 && !LABEL_FILLER.contains(&word.as_str()))
                .collect();
            if label.first() != Some(verb) || !label.iter().all(|word| said.contains(word)) {
                return None;
            }
            let mut arguments = serde_json::Map::new();
            for param in offer
                .descriptor
                .params
                .iter()
                .filter(|param| param.required)
            {
                let mut named = param
                    .choices
                    .iter()
                    .filter(|choice| said.contains(&choice.to_lowercase()));
                let (Some(choice), None) = (named.next(), named.next()) else {
                    return None;
                };
                arguments.insert(
                    param.name.to_string(),
                    serde_json::Value::String(choice.to_string()),
                );
            }
            Some((
                offer.id(),
                arguments,
                offer.descriptor.resource_label.to_lowercase(),
            ))
        });
    let mut fits: Vec<_> = fits.collect();
    // The same operation on several resources (each note's export): the
    // one whose name the task gives.
    if fits.len() > 1 {
        let lower = trimmed.to_lowercase();
        fits.retain(|(_, _, label)| label.chars().count() >= 3 && lower.contains(label.as_str()));
    }
    let [(id, arguments, _)] = <[_; 1]>::try_from(fits).ok()?;
    Some(Answer {
        actions: vec![id],
        arguments: vec![arguments],
        ..Answer::default()
    })
}

/// Records of one app the room already has that are offered to open, those
/// people have open first, then the latest changed. A campaign room holds a
/// handful of documents per app (a brief, a launch page, notes from each
/// member), and a task names one of them by its title: four left the one
/// it named off the list as soon as a few more were made. Each costs one
/// line in the first turn, so the list stays well within it.
pub(crate) const KNOWN_OFFERED: usize = 12;

/// The catalogue a model sees for `context`, and where each offer runs.
fn catalogue(context: &PlanContext) -> (Catalogue, BTreeMap<String, Target>) {
    let mut offers = Vec::new();
    let mut targets = BTreeMap::new();
    for (route, descriptors) in &context.open {
        for descriptor in descriptors
            .iter()
            .filter(|descriptor| planned(descriptor, descriptors))
        {
            let offer = Offer {
                app: route.app_id.clone(),
                broker_peer_id: None,
                broker_provider: None,
                descriptor: noted(descriptor),
            };
            targets
                .entry(offer.id())
                .or_insert_with(|| Target::Record(route.clone()));
            offers.push(offer);
        }
    }
    // An app the task is about with no record open is offered with the
    // operations a new record of it publishes: a step naming one opens the
    // record first, so the task needs no plan to open it and another to
    // use it.
    let previewed = |app: &str| {
        !context.open.iter().any(|(route, _)| route.app_id == app)
            && context
                .previews
                .iter()
                .any(|(previewed, _)| previewed == app)
    };
    for (app, descriptors) in &context.previews {
        if !previewed(app) || !context.apps.contains(app) {
            continue;
        }
        for descriptor in descriptors
            .iter()
            .filter(|descriptor| planned(descriptor, descriptors))
        {
            let offer = Offer {
                app: app.clone(),
                broker_peer_id: None,
                broker_provider: None,
                descriptor: noted(descriptor),
            };
            targets
                .entry(offer.id())
                .or_insert_with(|| Target::New(app.clone()));
            offers.push(offer);
        }
    }
    for app in &context.apps {
        // The room's own documents of this app come before a new one, so
        // a task about "the notes" works on the notes people already have;
        // offered too while another record of the app is open, as a task
        // may be about a document other than the one the agent made.
        for (route, title) in context
            .known
            .iter()
            .filter(|(route, _)| route.app_id == *app)
            .take(KNOWN_OFFERED)
        {
            let offer = Offer {
                app: app.clone(),
                broker_peer_id: None,
                broker_provider: None,
                descriptor: open_record_descriptor(app, &route.record, title.as_deref()),
            };
            targets.insert(offer.id(), Target::OpenRecord(route.clone()));
            offers.push(offer);
        }
        let is_open = context.open.iter().any(|(route, _)| route.app_id == *app);
        let second = is_open && context.new_documents.contains(app);
        if (is_open || previewed(app)) && !second {
            continue;
        }
        let offer = app_open_offer(app, second);
        targets.insert(offer.id(), Target::Open(app.clone()));
        offers.push(offer);
    }
    found_targets(context, &mut targets);
    (
        Catalogue {
            offers,
            facts: Vec::new(),
            held: Vec::new(),
        },
        targets,
    )
}

/// The open of `app` on a new, empty document (`app.<app>#open`): offered
/// while the app is not open, or as a `second` document beside the open
/// one, which refused an import for the content it holds.
fn app_open_offer(app: &str, second: bool) -> Offer {
    let description = if second {
        format!(
            "A new, empty {app} document beside the one open, which holds content an import \
             would replace (so the import there was refused). Open this, then import into the \
             new document: its operations are listed once it opens."
        )
    } else {
        format!(
            "{app} is not open, so none of what it can do is listed yet. Opening it creates a \
             new, empty record and puts its own operations on the list. To change a document \
             the room already has, open that one with its record.{app}.<record>#open instead."
        )
    };
    Offer {
        app: app.to_owned(),
        broker_peer_id: None,
        broker_provider: None,
        descriptor: ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new(format!("app.{app}"), surface::OPEN_ACTION),
            resource_kind: surface::APPLICATION_KIND.to_owned(),
            resource_label: app.to_owned(),
            action_label: format!("open {app} on a new, empty document"),
            description: Some(description),
            locus: tilo_actions::ExecutionLocus::ServerPreferred,
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract::default(),
            params: Vec::new(),
            input: tilo_actions::InputSpec::None,
            view: None,
        },
    }
}

/// Where the open of each record `search_room` may find runs: it opens by
/// the id the answer shows, as an offered record does, without a line of
/// the list for each.
fn found_targets(context: &PlanContext, targets: &mut BTreeMap<String, Target>) {
    for doc in &context.searchable {
        if context.apps.contains(&doc.route.app_id)
            && !context.open.iter().any(|(route, _)| *route == doc.route)
        {
            targets
                .entry(crate::search::open_id(&doc.route))
                .or_insert_with(|| Target::OpenRecord(doc.route.clone()));
        }
    }
}

/// `descriptor` as offered to a model: a read says how its answer is used.
/// Whether a plan is offered `descriptor`, one of `all` a record publishes:
/// every operation except the framework's generic `apply` and `group`
/// where the record's own typed edits of that resource cover it. Those take
/// any edit as raw JSON (block ids, offsets, tagged variants), the shape a
/// model most often gets wrong, and their schema is the largest part of
/// what a plan is told.
fn planned(descriptor: &ActionDescriptor, all: &[ActionDescriptor]) -> bool {
    let generic = [APPLY_ACTION, GROUP_ACTION].contains(&descriptor.address.action.as_str());
    !generic
        || !all.iter().any(|other| {
            other.resource_kind == descriptor.resource_kind
                && other.kind == tilo_actions::OperationKind::Document
                && ![APPLY_ACTION, GROUP_ACTION].contains(&other.address.action.as_str())
        })
}

fn noted(descriptor: &ActionDescriptor) -> ActionDescriptor {
    let mut descriptor = descriptor.clone();
    if descriptor.kind == tilo_actions::OperationKind::View || readable(&descriptor) {
        descriptor.description = Some(match descriptor.description.take() {
            Some(own) => format!("{own} {VIEW_NOTE}"),
            None => VIEW_NOTE.to_owned(),
        });
    }
    descriptor
}

/// The most characters of one app's operations a [`Briefing`]'s reference
/// shows in full; the rest are left to their signatures and
/// `describe_operation`.
const MOST_REFERENCE_CHARS: usize = 64_000;

/// An operation whose parameters in full take more than this (a generic
/// `apply` whose schema covers every action of its app) is left to
/// `describe_operation`.
const MOST_OPERATION_CHARS: usize = 40_000;

/// The framework's generic edit, and its batch of several.
const APPLY_ACTION: &str = "apply";
const GROUP_ACTION: &str = "group";

/// The most operations the first turn lists as published now: open
/// documents publish one set per section or sheet, and the exports and
/// reads come after those.
const MOST_PUBLISHED: usize = 100;

/// The most reads one plan runs through [`READ_VIEW`].
const MOST_READS: usize = 8;

/// The most characters of one read's answer shown in the conversation.
const MOST_READ_CHARS: usize = 6_000;

/// The paragraphs a [`Briefing`] opens with.
const REFERENCE_INTRO: &str = "What the apps you may use can do, with every parameter in \
    full (types, required, choices, constraints). The ids to name are the ones listed under \
    \"Operations published right now\" below: a document's own resources (a workbook's \
    sheets) carry their own ids, with these same operations.";
const SNAPSHOT_INTRO: &str = "What the documents this request is about hold now (read just \
    before planning; use these values rather than asking for them):";

/// The [`Briefing`] for a plan over `context`, whose offers are `catalogue`.
///
/// Its reference shows the operations a new record of each app in
/// [`PlanContext::reference`] publishes (document operations first, each
/// app within [`MOST_REFERENCE_CHARS`]): it depends on the granted apps
/// alone, never on the task or the records open, so it is byte-identical
/// across agents and tasks. Its state says which apps a step opens first
/// and what the records hold ([`PlanContext::snapshots`]).
fn briefing(context: &PlanContext, catalogue: &Catalogue) -> Briefing {
    let offered: std::collections::BTreeSet<String> =
        catalogue.offers.iter().map(Offer::id).collect();
    let mut reference = String::new();
    let mut shown = Vec::new();
    let mut any_left_out = false;
    let mut described = std::collections::BTreeSet::new();
    for (app, descriptors) in &context.reference {
        let mut ordered: Vec<&ActionDescriptor> = descriptors
            .iter()
            .filter(|descriptor| planned(descriptor, descriptors))
            .collect();
        ordered.sort_by_key(|descriptor| descriptor.kind != tilo_actions::OperationKind::Document);
        let mut section = String::new();
        let mut left_out = 0_usize;
        for descriptor in ordered {
            let offer = Offer {
                app: app.clone(),
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
            // The framework's `group` takes a list of what `apply` takes:
            // its schema is `apply`'s again, so it is said in a line.
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
                || section.chars().count().saturating_add(size) > MOST_REFERENCE_CHARS
            {
                left_out = left_out.saturating_add(1);
                continue;
            }
            section.push_str(&detail);
            described.insert((
                app.clone(),
                offer.descriptor.resource_kind.clone(),
                offer.descriptor.address.action.clone(),
            ));
            if offered.contains(&id) {
                shown.push(id);
            }
        }
        if section.is_empty() {
            continue;
        }
        if reference.is_empty() {
            reference.push_str(REFERENCE_INTRO);
            reference.push('\n');
        }
        let _ = writeln!(reference, "## {app}");
        reference.push_str(&section);
        if left_out > 0 {
            any_left_out = true;
            let _ = writeln!(
                reference,
                "({left_out} more operations of {app}: their signatures are listed with the \
                 ids; ask `describe_operation` for one only if that is not enough.)"
            );
        }
    }
    let state = briefing_state(context, catalogue);
    Briefing {
        reference,
        state,
        shown,
        left_out: any_left_out,
        described,
        cache_key: Some(cache_key(&context.room)),
        searchable: context.searchable.clone(),
        may_plan_nothing: context.may_plan_nothing,
        pictures: Vec::new(),
        retries: context.retries.clone(),
    }
}

/// The prompt-cache routing key of every planning request in `room`: a
/// hash of it, so the provider is not told the room.
fn cache_key(room: &str) -> String {
    if room.is_empty() {
        return "tilo-agent-plan".into();
    }
    format!(
        "tilo-agent-plan-{}",
        blake3::hash(room.as_bytes())
            .to_hex()
            .get(..16)
            .unwrap_or_default()
    )
}

/// What changes from plan to plan: which apps a step opens first, and
/// what the records the task is about hold now.
fn briefing_state(context: &PlanContext, catalogue: &Catalogue) -> String {
    let mut state = String::new();
    for (app, _) in &context.previews {
        if catalogue
            .offers
            .iter()
            .any(|offer| offer.app == *app && opened_by_step(offer, context))
        {
            let _ = writeln!(
                state,
                "{app} is not open yet: planning any of its operations listed above opens a new, \
                 empty {app} document first and runs them there (no open step needed); to change \
                 a document the room already has, plan its record.{app}.<record>#open instead and \
                 put the rest in `then`."
            );
        }
    }
    if !context.snapshots.is_empty() {
        state.push_str(SNAPSHOT_INTRO);
        for snapshot in &context.snapshots {
            state.push_str("\n- ");
            state.push_str(snapshot);
        }
        state.push('\n');
    }
    state
}

/// Whether `offer` is one of a previewed app's, which its step opens a
/// new record for.
fn opened_by_step(offer: &Offer, context: &PlanContext) -> bool {
    !context
        .open
        .iter()
        .any(|(route, _)| route.app_id == offer.app)
        && tilo_agent::tools::opened_app(offer).is_none()
}

/// The offer that opens `record`, a document of `app` the room already has,
/// named `title` when its app names it: the title is in its labels, so a
/// task that names the document finds it.
fn open_record_descriptor(app: &str, record: &str, title: Option<&str>) -> ActionDescriptor {
    let (resource_label, action_label) = match title {
        Some(title) => (
            title.to_owned(),
            format!("open the room's {app} document \"{title}\" (then export, read or edit it)"),
        ),
        None => (
            format!("{app} document {record}"),
            format!("open the room's {app} document {record} (untitled)"),
        ),
    };
    ActionDescriptor {
        semantic_identity: None,
        address: surface::open_record_address(app, record),
        resource_kind: surface::APPLICATION_KIND.to_owned(),
        resource_label,
        action_label,
        description: Some(format!(
            "A {app} document this room already has, which people may be looking at. \
             Opening it puts its operations on the list, and they change this document: \
             to edit or add to it, open it here; app.{app}#open would create a new one."
        )),
        locus: tilo_actions::ExecutionLocus::ServerPreferred,
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract::default(),
        params: Vec::new(),
        input: tilo_actions::InputSpec::None,
        view: None,
    }
}

/// The reads and looks of one plan ([`PlanReads`]) over `targets`, the
/// offers' places, and `context`, what they were taken from.
struct PlanningReads<'a> {
    targets: &'a BTreeMap<String, Target>,
    context: &'a PlanContext,
    reads: &'a mut dyn Reads,
    /// How many more reads the plan may run ([`MOST_READS`]).
    left: usize,
    /// Whether the model is shown pictures.
    sees: bool,
    /// The content hashes of the pictures apps rendered for this task (its
    /// steps' looks and exports, its plans' reads): shown whatever the
    /// agent may do with the room's own pictures.
    rendered: std::collections::BTreeSet<String>,
}

/// Why a PDF kept as content is not looked at as a picture.
fn pdf_hint(address: &str) -> String {
    format!(
        "{address} is a PDF, not a picture: open it in pdf-viewer (import-pdf-content), then \
         look at a page with its look-page view"
    )
}

impl<'a> PlanningReads<'a> {
    /// The reads of a plan over `targets` and `context` through `reads`,
    /// for a model that `sees` pictures or not.
    fn new(
        targets: &'a BTreeMap<String, Target>,
        context: &'a PlanContext,
        reads: &'a mut dyn Reads,
        sees: bool,
    ) -> Self {
        Self {
            targets,
            context,
            reads,
            left: MOST_READS,
            sees,
            rendered: context
                .rendered
                .iter()
                .chain(&context.pictures)
                .filter_map(|address| crate::vision::content_hash(address))
                .collect(),
        }
    }

    /// What a re-plan's first turn shows: the newest picture the task's
    /// steps looked at, to a model that sees.
    fn newest_look(&mut self) -> Vec<Picture> {
        let context = self.context;
        let Some(newest) = context.pictures.last().filter(|_| self.sees) else {
            return Vec::new();
        };
        match self.picture(newest, true) {
            Ok(picture) => vec![picture],
            Err(why) => {
                tracing::debug!(%why, "agent: a looked-at picture is not shown");
                Vec::new()
            }
        }
    }

    /// The picture at `address` as the model is shown it; `rendered` when
    /// an app rendered it.
    fn picture(&mut self, address: &str, rendered: bool) -> Result<Picture, String> {
        let hash = crate::vision::content_hash(address)
            .ok_or_else(|| format!("`{address}` is not a content address"))?;
        let bytes = self.reads.look(&hash)?;
        if crate::vision::is_pdf(&bytes) {
            return Err(pdf_hint(address));
        }
        crate::vision::prepare(address, &bytes, rendered)
    }
}

impl PlanningReads<'_> {
    /// The record `id` names a read of, and the read's answer to
    /// `arguments`: only on an open record, only an operation that only
    /// reads ([`readable`]), and at most [`MOST_READS`] a plan.
    fn run(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(DocumentRoute, ActionAddress, tilo_actions::ActionOutput), String> {
        let Some(Target::Record(route)) = self.targets.get(id) else {
            return Err(format!(
                "{id} is not an operation of a document open now; plan it as a step"
            ));
        };
        let address =
            surface::decode_address(id).ok_or_else(|| format!("{id} is not an operation"))?;
        let descriptor = self
            .context
            .open
            .iter()
            .find(|(open, _)| open == route)
            .and_then(|(_, descriptors)| {
                descriptors
                    .iter()
                    .find(|descriptor| descriptor.address == address)
            })
            .ok_or_else(|| format!("{id} is not published now"))?;
        if !readable(descriptor) {
            return Err(format!(
                "{} changes or makes something, so it is not run while planning: plan it as a step",
                descriptor.action_label
            ));
        }
        if self.left == 0 {
            return Err(format!(
                "this plan has had its {MOST_READS} reads: plan with what they found"
            ));
        }
        self.left = self.left.saturating_sub(1);
        let output = self.reads.read(route, &address, arguments)?;
        Ok((route.clone(), address, output))
    }
}

impl PlanReads for PlanningReads<'_> {
    /// Run the read `id` names with `arguments` ([`PlanningReads::run`]).
    /// Answers with what it found, summarised, and the picture it rendered
    /// (a canvas look) when the model sees.
    fn read(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Seen, String> {
        let (route, address, output) = self.run(id, arguments)?;
        let mut text = observation_within(&route.app_id, &address, &output, MOST_READ_CHARS)
            .unwrap_or_else(|| format!("{id} answered nothing"));
        let mut pictures = Vec::new();
        if let Some(rendered) = rendered_picture(&output) {
            if let Some(hash) = crate::vision::content_hash(&rendered) {
                self.rendered.insert(hash);
            }
            let withheld = if self.sees {
                match self.picture(&rendered, true) {
                    Ok(picture) => {
                        pictures.push(picture);
                        None
                    }
                    Err(why) => Some(why),
                }
            } else {
                Some("this model is not shown pictures".to_owned())
            };
            if let Some(why) = withheld {
                let _ = write!(text, "\n({rendered} picture not shown: {why})");
            }
        }
        Ok(Seen { text, pictures })
    }

    /// Run the read `id` names with `arguments` and with `against`: what
    /// the first found, and what differs from the second's answer to it.
    fn compare(
        &mut self,
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        against: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Seen, String> {
        let (route, address, output) = self.run(id, arguments)?;
        let (_, _, other) = self.run(id, against)?;
        let null = serde_json::Value::Null;
        let changed = crate::facts::changes_text(
            other.value.as_ref().unwrap_or(&null),
            output.value.as_ref().unwrap_or(&null),
        );
        let text = observation_within(&route.app_id, &address, &output, MOST_READ_CHARS)
            .unwrap_or_else(|| format!("{id} answered nothing"));
        let against = serde_json::Value::Object(against.clone());
        Ok(Seen::from(format!(
            "{text}\nChanged from its answer to {against} to this one: {changed}"
        )))
    }

    /// The picture at `address`: one an app rendered for this task, or —
    /// only where the agent may show them ([`PlanContext::room_pictures`])
    /// — one of the room's own. A PDF is looked at page by page in
    /// pdf-viewer.
    fn look(&mut self, address: &str) -> Result<Seen, String> {
        let hash = crate::vision::content_hash(address).ok_or_else(|| {
            format!("`{address}` is not a content address: give tilo.content:<hash>")
        })?;
        let address = crate::vision::content_address(&hash);
        let bytes = self.reads.look(&hash)?;
        if crate::vision::is_pdf(&bytes) {
            return Err(pdf_hint(&address));
        }
        if let crate::vision::Admission::Refuse(why) = crate::vision::admit(&bytes) {
            return Err(format!("{address}: {why}"));
        }
        let rendered = self.rendered.contains(&hash);
        if !rendered && !self.context.room_pictures {
            return Err(format!(
                "{address} is a picture from the room, and this agent may not show the room's \
                 pictures to its model; plan with what the request and the reads say of it"
            ));
        }
        let picture = crate::vision::prepare(&address, &bytes, rendered)?;
        Ok(Seen {
            text: format!("{address} is a {} picture.", picture.size_note()),
            pictures: vec![picture],
        })
    }
}

impl Planner for ModelPlanner {
    fn plan(&mut self, task: &str, context: &PlanContext) -> Result<Vec<Step>, String> {
        self.plan_reading(task, context, &mut NoReads)
    }

    fn checks_work(&self) -> bool {
        self.checks_work
    }

    fn last_planned_by_model(&self) -> bool {
        self.by_model
    }

    fn take_obligations(&mut self) -> Obligations {
        std::mem::take(&mut self.obligations)
    }

    fn plan_reading(
        &mut self,
        task: &str,
        context: &PlanContext,
        reads: &mut dyn Reads,
    ) -> Result<Vec<Step>, String> {
        // A task that is exactly the rest this planner last deferred is that
        // task planned again; anything else is a new task.
        self.obligations = Obligations::default();
        let replans = match self.continued.take() {
            Some((rest, replans)) if rest == task => replans.saturating_add(1),
            _ => 0,
        };
        if let Some(steps) = written_steps(task, context) {
            return steps;
        }
        let (catalogue, targets) = catalogue(context);
        let mut briefing = briefing(context, &catalogue);
        let mut read = PlanningReads::new(&targets, context, reads, self.model.sees());
        briefing.pictures = read.newest_look();
        // A request that names exactly one operation of an open record, all
        // of whose arguments it states, is that operation: the model would
        // only repeat it.
        let direct = (replans == 0 && context.request.is_none())
            .then(|| single_operation(task, &catalogue, &targets))
            .flatten();
        self.by_model = direct.is_none();
        let answer = match direct {
            Some(answer) => answer,
            None => self.model.choose_briefed(
                &with_results(task, context),
                &briefing,
                &catalogue,
                MOST_STEPS,
                &mut read,
            )?,
        };
        if let Some(refused) = planned_nothing(&answer, context) {
            return Err(refused);
        }
        self.obligations = Obligations {
            items: answer.obligations.clone(),
            set_aside: answer.set_aside.clone(),
        };
        let (mut steps, rest, wait) = answer_steps(
            answer,
            task,
            (&catalogue, &targets),
            context,
            self.model.sees(),
        )?;
        if let Some(wait) = wait {
            // The wait ends this plan: what `then` left is planned with
            // what arrived, so it rides with the wait.
            steps.push(Step {
                app: String::new(),
                record: None,
                action: StepAction::Wait(Wait::planned(wait, rest)?),
                carries: None,
                serves: None,
            });
            return Ok(steps);
        }
        if let Some(rest) = rest {
            if replans >= MOST_REPLANS {
                return Err(format!(
                    "planned again {MOST_REPLANS} times without finishing; still to do: {rest}"
                ));
            }
            let app = steps
                .last()
                .map(|step| step.app.clone())
                .unwrap_or_default();
            self.continued = Some((rest.clone(), replans));
            steps.push(Step {
                app,
                record: None,
                action: StepAction::Continue(rest),
                carries: None,
                serves: None,
            });
        }
        Ok(steps)
    }
}

/// Why `answer`, a plan of no steps and no rest, is refused: no steps is an
/// answer only where nothing left to do may be (a check of finished work,
/// [`PlanContext::may_plan_nothing`]). A first plan that names nothing has
/// not planned the request, whatever its reply says. `None` otherwise.
fn planned_nothing(answer: &Answer, context: &PlanContext) -> Option<String> {
    if !answer.actions.is_empty()
        || answer.then.is_some()
        || answer.wait.is_some()
        || context.may_plan_nothing
    {
        return None;
    }
    let reply = answer
        .reply
        .as_deref()
        .map(|reply| format!("; {reply}"))
        .unwrap_or_default();
    Some(format!(
        "nothing offered fits: the plan named nothing to do{reply}"
    ))
}

/// The steps of an answer, the rest of the task to plan again after them,
/// and the wait that ends them ([`answer_steps`]).
type AnswerSteps = (Vec<Step>, Option<String>, Option<tilo_agent::tools::Wait>);

/// The steps `answer` names, over `offered` (the catalogue it was given
/// and where each offer runs), with the rest of the task to plan again
/// after them and the wait that ends them. "Nothing offered fits" first is
/// the plan's refusal; later, it ends the steps there.
///
/// A model `sees` that plans a picture (a frame, a page, the artwork) and
/// then writes in the same plan has written what the picture shows before
/// seeing it (a sign-off naming the wrong cut): its steps end after the
/// pictures, and the rest is planned again with them shown, told what it
/// had meant to write. A plan already shown the task's pictures is not cut
/// again, so a model that looks once more does not loop.
fn answer_steps(
    answer: tilo_agent::tools::Answer,
    task: &str,
    (catalogue, targets): (&Catalogue, &BTreeMap<String, Target>),
    context: &PlanContext,
    sees: bool,
) -> Result<AnswerSteps, String> {
    let mut steps = Vec::with_capacity(answer.actions.len().saturating_add(1));
    let mut rest = answer.then.clone();
    let mut wait = answer.wait;
    let serves = answer.serves;
    let mut pictured = false;
    let planned: Vec<(&String, serde_json::Map<String, serde_json::Value>)> =
        answer.actions.iter().zip(answer.arguments).collect();
    for (at, (id, arguments)) in planned.iter().enumerate() {
        // "Nothing offered fits" ends what can be done now: the steps
        // before it run, then the task is planned again over what they
        // published (an app opened, say).
        if *id == tilo_agent::plan::NOTHING_FITS {
            // A first answer of "nothing fits" that says what to do is a
            // plan the model did not map onto the operations: it is asked
            // once to, rather than ending the task.
            if steps.is_empty()
                && !task.starts_with(PLAN_WHAT_IT_SAID)
                && let Some(then) = answer.then.as_deref().filter(|then| concrete(then))
            {
                rest = Some(format!("{PLAN_WHAT_IT_SAID} {}", then.trim()));
                wait = None;
                break;
            }
            if steps.is_empty() {
                let because = [Some(answer.why.trim()), answer.then.as_deref()]
                    .into_iter()
                    .flatten()
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(format!("nothing offered fits: {because}"));
            }
            rest = Some(answer.then.clone().unwrap_or_else(|| task.to_owned()));
            break;
        }
        let address = surface::decode_address(id)
            .ok_or_else(|| format!("the model named {id:?}, which is not an operation"))?;
        let offer = catalogue.offers.iter().find(|offer| offer.id() == **id);
        if sees && pictured && context.pictures.is_empty() && offer.is_some_and(writes) {
            let unseen = planned.get(at..).unwrap_or_default();
            rest = Some(pictures_first(rest.as_deref().unwrap_or(task), unseen));
            wait = None;
            break;
        }
        pictured |= shows_picture(&address);
        let arguments = arguments.clone();
        let tag = serves.get(at).copied().flatten();
        match targets.get(*id) {
            Some(Target::Record(route)) => steps.push(Step {
                app: route.app_id.clone(),
                record: Some(route.record.clone()),
                carries: context.content_param(&route.app_id, Some(&route.record), &address),
                serves: tag,
                action: StepAction::Invoke { address, arguments },
            }),
            Some(Target::New(app)) => steps.push(Step {
                app: app.clone(),
                record: None,
                carries: context.preview_content_param(app, &address),
                serves: tag,
                action: StepAction::Invoke { address, arguments },
            }),
            Some(Target::Open(app)) => steps.push(Step {
                app: app.clone(),
                record: None,
                action: StepAction::Open,
                carries: None,
                serves: tag,
            }),
            Some(Target::OpenRecord(route)) => steps.push(Step {
                app: route.app_id.clone(),
                record: Some(route.record.clone()),
                action: StepAction::Open,
                carries: None,
                serves: tag,
            }),
            None => return Err(format!("the model named {id:?}, which was not offered")),
        }
    }
    Ok((steps, rest, wait))
}

/// What a plan that answered "nothing offered fits" while saying what to
/// do is planned again with, before that.
pub(crate) const PLAN_WHAT_IT_SAID: &str = "You answered that nothing offered fits, but said what to do. \
     Plan those steps now with the operations listed (an open first, by its listed open, when \
     they act in a document not open yet); answer that nothing fits only if no listed operation \
     can do them. What you said to do:";

/// The fewest words a "nothing fits" answer's `then` must have to be a
/// plan of its own rather than a remark.
const FEWEST_PLAN_WORDS: usize = 4;

/// Whether `then`, what a "nothing fits" answer says is left, is a plan
/// ([`FEWEST_PLAN_WORDS`] or more).
fn concrete(then: &str) -> bool {
    then.split_whitespace().count() >= FEWEST_PLAN_WORDS
}

/// Operations that answer with a picture of their record (a canvas look, a
/// PDF page, a video frame), by name: a model that sees is shown it on the
/// plan after them.
const PICTURE_ACTIONS: &[&str] = &["look", "look-page", "view-frame-content"];

/// Whether the operation at `address` answers with a picture
/// ([`PICTURE_ACTIONS`], or another `look-…`).
fn shows_picture(address: &ActionAddress) -> bool {
    PICTURE_ACTIONS.contains(&address.action.as_str()) || address.action.starts_with("look-")
}

/// Whether `offer` writes: it changes its record or makes something (an
/// edit, an export, a post), rather than only reading or showing it.
fn writes(offer: &Offer) -> bool {
    !shows_picture(offer.address())
        && tilo_agent::tools::opened_app(offer).is_none()
        && match offer.descriptor.kind {
            tilo_actions::OperationKind::Document => true,
            tilo_actions::OperationKind::View => false,
            tilo_actions::OperationKind::Effect => !readable(&offer.descriptor),
        }
}

/// The most characters of the steps a plan cut after its pictures that the
/// plan after them is told.
const MOST_UNSEEN_CHARS: usize = 1_200;

/// The rest of a task whose plan was cut after its pictures, before the
/// steps it had written ahead of seeing them (`unseen`, each an operation
/// id and its arguments): `then`, what the plan left after them.
fn pictures_first(
    then: &str,
    unseen: &[(&String, serde_json::Map<String, serde_json::Value>)],
) -> String {
    let mut planned = unseen
        .iter()
        .map(|(id, arguments)| format!("{id} {}", serde_json::Value::Object(arguments.clone())))
        .collect::<Vec<_>>()
        .join("; ");
    if planned.chars().count() > MOST_UNSEEN_CHARS {
        planned = planned.chars().take(MOST_UNSEEN_CHARS).collect::<String>() + "…";
    }
    format!(
        "Look at the pictures the steps so far rendered (the newest is shown; look_content shows \
         the others by the addresses they answered) before writing anything about them. The \
         plan went on with these steps, written before the pictures were seen: {planned}. Plan \
         them again only as the pictures show them to be right, correcting what they got wrong. \
         Then: {then}"
    )
}

/// What follows the task's own words in the text a model is given: the
/// content its steps produced, then what they answered, each a paragraph
/// opening with these words.
const PRODUCED_INTRO: &str = "Content produced so far:";
const OBSERVED_INTRO: &str = "What the steps so far answered";
/// What a re-plan's rest is followed by: the task as it was given.
const ORIGINAL_INTRO: &str = "The original request (for reference):";
/// What opens a re-plan's paragraph on the task's checklist.
pub(crate) const OBLIGATIONS_INTRO: &str = "The request's obligations";

/// What an open record's view is said to do, so the model plans a view
/// whose answer a later step needs rather than asking for the value.
const VIEW_NOTE: &str = "A view: it changes nothing and answers with what it finds (sizes, \
    ids, text). To use its answer, call `read_view` with it when that tool is offered: the \
    answer comes back at once and you plan with it. Otherwise plan this step and put the steps \
    that depend on the answer in `then`; it is shown to you when the task is planned again.";

/// `task` as the model reads it: with the content the task's steps have
/// produced so far, so a later operation can name it by address, and with
/// what they answered (an artwork's size, a document's text), so the rest
/// is planned with it.
fn with_results(task: &str, context: &PlanContext) -> String {
    let mut text = task.to_owned();
    // A re-plan's rest is the model's own words for what was left; the
    // request as given says what those words may have dropped.
    if let Some(request) = context
        .request
        .as_deref()
        .map(str::trim)
        .filter(|request| !request.is_empty() && *request != task.trim())
    {
        let _ = write!(
            text,
            "\n\n{ORIGINAL_INTRO} {request}\n(The steps so far did part of it; plan what the text \
             above asks, and anything of the original it leaves out that is not done yet.)"
        );
    }
    if !context.produced.is_empty() {
        let _ = write!(
            text,
            "\n\n{PRODUCED_INTRO} {} (pass an address where an operation asks for content; an \
             operation that takes what the previous step produced is given it)",
            context.produced.join(", ")
        );
    }
    if !context.observed.is_empty() {
        let _ = write!(
            text,
            "\n\n{OBSERVED_INTRO} (use these values; do not ask for them):"
        );
        for observed in &context.observed {
            text.push_str("\n- ");
            text.push_str(observed);
        }
    }
    if !context.obligations.is_empty() {
        let _ = write!(text, "\n\n{}", context.obligations);
    }
    text
}

/// What `task` asks, without what [`with_results`] added after it: the
/// request as given, the content produced and what was answered are
/// context, not more to do.
fn requested(task: &str) -> &str {
    [
        ORIGINAL_INTRO,
        PRODUCED_INTRO,
        OBSERVED_INTRO,
        OBLIGATIONS_INTRO,
    ]
    .iter()
    .filter_map(|intro| task.find(&format!("\n\n{intro}")))
    .min()
    .and_then(|at| task.get(..at))
    .unwrap_or(task)
}
