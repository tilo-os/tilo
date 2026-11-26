//! The desktop search palette: the toolkit's [`ActionPalette`] over the
//! portable sources. The host owns placement and input routing; a chosen row
//! is handed back through [`DesktopSearch::take_choice`], an action row as
//! the address the host's action API resolves against the live resource
//! tree.

use std::sync::Arc;

use tilo_actions::ResourceSpec;
use tilo_actions::remote::{Pending, PendingPoll};
use ui::{ActionPalette, CommandEntry};

use crate::model::{Held, Hit, SearchStream, Source, Target};
use crate::providers::{
    Actions, Answers, AppEntry, Applications, Captures, CommandSpec, Commands, Content, Published,
    PublishedActions, Recents, ResourceOption, ResourceSource, Switches, WebSearch, WindowEntry,
    Windows,
};
use crate::window::WindowSearch;
use crate::workers::SearchWorkers;
use ui::search::system::{KindGroup, ResourceKind};
use ui::search::{ActivationModifier, Scope};

/// Row id prefix of the pick's items; `pick:{i}` fills the field with `options[i]`.
const PICK_PREFIX: &str = "pick:";

/// Row id prefix of the row that lists the rest of a group the palette's
/// height cut short; `more:{i}` expands `more_rows[i]`.
const MORE_PREFIX: &str = "more:";

mod rows;

#[cfg(test)]
mod focus_tests;
#[cfg(test)]
mod no_match_tests;
#[cfg(test)]
mod own_sources_tests;
#[cfg(test)]
mod pressed_row_tests;
#[cfg(test)]
mod ready_rows_tests;
#[cfg(test)]
mod source_timing_tests;
#[cfg(test)]
mod windowed_host_tests;

use rows::{
    all_results_entry, always_shown, entries_for, kind_facets, narrowed, nothing_matched,
    pressed_hit, stands_tall,
};

/// The row `id` names when it is a group's "more" row: an id this palette
/// wrote, so one that does not read as a number is no row here.
fn more_row(id: &str) -> Option<usize> {
    let more = id.strip_prefix(MORE_PREFIX)?;
    tilo_failure::ignored!(
        "the id is one this palette wrote, and another row has another",
        more.parse().ok()
    )
}

/// The row under the apps that lists every one of them, by its name.
const ALL_APPS_ID: &str = "all-apps";

/// The row that lists every result the card had no room for.
const ALL_RESULTS_ID: &str = "all-results";

/// What the host can search right now, collected when search opens.
#[derive(Default)]
pub struct SearchData {
    pub applications: Vec<AppEntry>,
    pub windows: Vec<WindowEntry>,
    pub commands: Vec<CommandSpec>,
    pub resources: Vec<ResourceSpec>,
    /// What providers publish through the session's action host, run by
    /// address there ([`crate::model::Target::Invoke`]).
    pub published: Vec<PublishedActions>,
    /// What a resource-kind parameter's field offers, where the host serves the
    /// workspace's list actions: the kind's items, picked by what they are called
    /// ([`crate::providers::ResourceSource`]).
    pub resource_options: Option<Arc<dyn ResourceSource>>,
    /// The search providers open applications publish.
    pub providers: Vec<Box<dyn ui::search::SearchProvider>>,
    /// The exchange rates the host last fetched, which money is answered
    /// with, and now, in Unix seconds, to say how old they are.
    pub rates: Option<crate::answers::currency::Rates>,
    pub now: u64,
    /// The installed app that claims a URI scheme, by name: a service an
    /// app serves is searched there ([`WebSearch::app_for_scheme`]).
    pub app_for_scheme: Option<fn(&str) -> Option<String>>,
    /// How far the search reaches and what it is narrowed to: an app's own
    /// resources ([`Scope::InApp`], its `part` one mailbox or folder), the
    /// applications a window group's new tab may show
    /// ([`Scope::WindowGroup`]), the workspace in front, or every workspace
    /// (see [`Scope`]).
    pub scope: Scope,
    /// What the empty field says it searches, where that is not
    /// everything ("Search Tube…").
    pub placeholder: Option<String>,
    /// What the host searches beyond these (Settings' pages, the store's
    /// catalogue, files, what apps hold), each over what is published or a
    /// service answers. The search over everything asks them, off the frame
    /// when their query can move ([`Source::shared`]). A host's source is
    /// `Send + Sync` so that it can move: the palette hands a worker the
    /// handle it holds it by ([`Held`]).
    pub sources: Vec<Held>,
    /// The sources the person switched off, left out whatever they find.
    pub preferences: crate::preferences::SearchPreferences,
}

/// A row the person moved to, as it is found again once the rows are
/// rebuilt: rows are numbered by position, which a late answer changes.
#[derive(Clone)]
enum ChosenRow {
    /// A hit, by its `(provider, id)`.
    Hit(String, String),
    ShowAll(ResourceKind),
}

/// The sources a search asks. Those over plain data stand in an [`Arc`], so
/// a frame hands the same source to a worker ([`Source::shared`]) rather
/// than asking it on the frame.
struct Sources {
    applications: Arc<Applications>,
    windows: Arc<Windows>,
    commands: Arc<Commands>,
    actions: Arc<Actions>,
    published: Arc<Published>,
    captures: Arc<Captures>,
    content: Content,
    answers: Option<(Arc<Answers>, Arc<WebSearch>)>,
    /// The one window searched, when the search is a window's.
    window: Option<WindowSearch>,
    /// What a resource-kind parameter's field lists, where the host serves the workspace's
    /// list actions.
    resource_options: Option<Arc<dyn ResourceSource>>,
    host: Vec<Held>,
    preferences: crate::preferences::SearchPreferences,
    /// Now, in Unix seconds, as the host said when search opened.
    now: u64,
}

impl Sources {
    fn new(mut data: SearchData) -> Self {
        // An app whose window this desktop draws publishes its actions here, in its resources:
        // what it offers through the shell besides, kept from before or declared while it was
        // closed, would be each of them again.
        let drawn: std::collections::BTreeSet<String> = data
            .resources
            .iter()
            .filter_map(|resource| resource.app_id.clone())
            .collect();
        data.published
            .retain(|published| !drawn.contains(&published.app));
        // The part of the app the search is narrowed to, when it is: one
        // mailbox, one folder.
        let anchor = match &data.scope {
            Scope::InApp(app) => app.part.as_ref().map(|part| part.anchor.clone()),
            _ => None,
        };
        if matches!(data.scope, Scope::InApp(_)) {
            return Self {
                applications: Arc::new(Applications::new(data.applications)),
                windows: Arc::new(Windows(data.windows)),
                commands: Arc::new(Commands::new(data.commands)),
                content: Content::new(Vec::new(), Vec::new()),
                actions: Arc::new(Actions(Vec::new())),
                // What the app publishes through the host runs by address there: an app in a
                // process of its own gives the shell no tree to read, so its window's actions are
                // these.
                published: Arc::new(Published(data.published)),
                captures: Arc::new(Captures(Vec::new())),
                answers: None,
                window: Some(WindowSearch::new(data.resources, data.providers).within(anchor)),
                resource_options: data.resource_options,
                // What the host adds for this window: the app's own entries in the session's
                // index, which is what it holds whether or not it runs.
                host: data.sources,
                preferences: data.preferences,
                now: data.now,
            };
        }
        Self {
            applications: Arc::new(Applications::new(data.applications)),
            windows: Arc::new(Windows(data.windows)),
            commands: Arc::new(Commands::new(data.commands)),
            content: Content::new(data.providers, data.resources.clone()).within(anchor),
            actions: Arc::new(Actions(data.resources)),
            published: Arc::new(Published(data.published.clone())),
            captures: Arc::new(Captures(data.published)),
            answers: matches!(data.scope, Scope::Workspace | Scope::Everywhere).then(|| {
                (
                    Arc::new(Answers {
                        rates: data.rates,
                        now: data.now,
                    }),
                    Arc::new(
                        data.app_for_scheme
                            .map_or_else(WebSearch::default, |app_for_scheme| WebSearch {
                                app_for_scheme,
                            }),
                    ),
                )
            }),
            window: None,
            resource_options: data.resource_options,
            host: data.sources,
            preferences: data.preferences,
            now: data.now,
        }
    }

    /// Ask the applications whose answers were still coming.
    fn poll(&self) -> bool {
        let window = self.window.as_ref().is_some_and(WindowSearch::poll);
        let host = self
            .host
            .iter()
            .map(super::model::Source::poll)
            .fold(false, |any, one| any | one);
        self.content.poll() || window || host
    }

    fn waiting(&self) -> bool {
        self.content.waiting()
            || self.window.as_ref().is_some_and(WindowSearch::waiting)
            || self.host.iter().any(super::model::Source::waiting)
    }
}

/// How soon a host looks again while an application's answer to the query is
/// still coming: the answer arrives on the host's clock, so the host asks
/// [`DesktopSearch::refresh`] until it does. A host that draws only what asks
/// for a frame schedules one at this pace for as long as
/// [`DesktopSearch::waiting`] holds.
pub const ANSWER_POLL: ui::time::Duration = ui::time::Duration::from_millis(50);

pub struct DesktopSearch {
    palette: ActionPalette,
    sources: Option<Sources>,
    /// What this palette opened lately: a source a worker asks, so it never
    /// holds the frame that typed.
    recents: Arc<Recents>,
    /// The rows that switch this search's sources off and on again, built
    /// once when the query's search began: a worker asks them, as any other
    /// source over plain data.
    switches: Option<Arc<Switches>>,
    /// What the person chose before, which orders equal matches.
    usage: crate::usage::Usage,
    /// The hits the palette shows; entry `hit:{i}` is `rows[i]`.
    rows: Vec<Hit>,
    /// The kind of each "Show all" row; entry `more:{i}` is `more_rows[i]`.
    more_rows: Vec<ResourceKind>,
    /// The kinds the person asked to see whole in this search.
    expanded: Vec<ResourceKind>,
    /// Whether the person asked for every result the search found: the
    /// groups are then listed whole and the card scrolls.
    everything: bool,
    /// The row the palette selected by itself when the rows were last
    /// built: a selection still on it is the person's best result, not a
    /// row they chose.
    auto_selected: Option<String>,
    shown_query: Option<String>,
    /// The query the sources were last asked: a search begins once per query, while the rows of
    /// an earlier query can stay shown until it has rows of its own ([`Self::render`]).
    asked_query: Option<String>,
    /// The query an Enter was pressed on before its rows were in: its top row is chosen once the
    /// search has answered it.
    enter_waiting: Option<String>,
    /// The kind chips picked when the rows were built: another picked or
    /// unpicked narrows them again.
    shown_kinds: Vec<ui::search::FacetFilter>,
    /// Whether this search is the applications alone, each already listed:
    /// then no row offers to list them all again.
    catalogue: bool,
    choice: Option<Target>,
    /// The search filling in, a source at a time: the rows shown are what
    /// came in so far.
    stream: Option<crate::model::SearchStream>,
    /// The threads the sources answer on: a frame hands them to the pool and
    /// collects what came back, and never waits on one.
    workers: SearchWorkers,
    /// The sources this search asks, in the order it asks them, and where
    /// each stands in the empty palette.
    order: Vec<(String, (u8, usize))>,
    /// How many sources at the head of this search are the desktop's own ([`rows::own_count`]).
    own_sources: usize,
    /// How many rows each group was cut to: the room the palette has for them. When that room
    /// changes since, the groups are cut again.
    rows_cut: usize,
    /// Whether the rows the palette holds stand two lines tall: what the
    /// capacity is counted in.
    shown_two_lines: bool,
    /// The row the person moved to in this search, kept while the rows fill
    /// in: a frame that has no room for it yet does not take their choice
    /// away.
    kept_choice: Option<ChosenRow>,
    /// What each row the palette hands out stands for, by the entry id it is handed out under, as
    /// of the last frame: `hit:{i}` is the thing `rows[i]` is about, `more:{i}` is a group asked
    /// whole. A press names the row the person saw by one of these ids, and is taken as that row
    /// even when a source's answer rebuilt the list in the same frame and renumbered it.
    shown_rows: std::collections::HashMap<String, rows::ShownRow>,
    /// Where this search stands: the workspace, every workspace, one app's
    /// own — so a row carried out by the host can be asked again where the
    /// person was when they chose it.
    scope: Scope,
    /// The row the host could not carry out, with why: it keeps the reason
    /// under its name until another row is chosen or search closes.
    refusal: Option<(Target, String)>,
    /// The chosen action whose required arguments are being collected, one step
    /// at a time, before it runs ([`Collecting`]).
    collecting: Option<Collecting>,
}

impl Default for DesktopSearch {
    fn default() -> Self {
        Self::new()
    }
}

/// What the empty field says the search over everything covers: the groups
/// (kinds) it asks, in the desktop's language.
fn placeholder() -> String {
    crate::i18n::t("search-placeholder")
}

/// One required parameter of a chosen action, as the step that collects it.
struct NamedField {
    /// The parameter's name, what its value is filed under.
    name: String,
    /// What the step asks and the control it uses.
    spec: ui::ArgumentSpec,
    /// What the step collects.
    step: FieldStep,
}

/// How one parameter's step collects its value.
enum FieldStep {
    /// Typed into the field: text, a secret, a number.
    Input {
        /// Whether the value is a number, parsed before it is filed.
        numeric: bool,
    },
    /// Picked from the workspace's items of the resource kind it names, when the host lists
    /// them (`vault.item`): the items are rows under the field, and what is typed there narrows
    /// them.
    Pick { kind: String },
}

/// What the pick's list answered since it was last looked at.
enum PickAnswer {
    /// Still to come.
    Waiting,
    /// Its items came.
    Arrived,
    /// It refused, or named nothing to choose from: the field is asked as text instead.
    Nothing,
}

/// The pick of a resource-kind parameter, open over the list while the person chooses.
struct Picking {
    /// What the field asks, as the rows' group says it.
    prompt: String,
    /// The search's query, put back when the pick ends.
    saved: String,
    /// The list's answer, while it is still to come.
    pending: Option<Pending<Result<Vec<ResourceOption>, String>>>,
    /// What came to choose from.
    options: Vec<ResourceOption>,
}

/// An action whose required arguments the palette collects one step at a time
/// before it runs: the person filled the row's one line by choosing the rows the
/// desktop offers, and an action that takes more than one is asked each in turn,
/// each in the control its type asks for, so every action runs from the search.
struct Collecting {
    /// The row chosen: its id is what the steps answer under, its target what runs.
    hit: Hit,
    /// The parameters still unseen and the ones already answered, in order.
    fields: Vec<NamedField>,
    /// Which field the open step is collecting.
    index: usize,
    /// The values collected so far, by parameter name.
    arguments: serde_json::Map<String, serde_json::Value>,
    /// The modifier the person held over the row.
    held: Option<ActivationModifier>,
    /// The pick open over the field, when the open step's kind has one.
    pick: Option<Picking>,
}

/// What `param` asks for as a palette step, in the control its type takes;
/// `None` for a type the palette has no control for, which is left to the app.
/// A parameter that names a resource kind ([`tilo_actions::ParamType::Resource`]) is picked from
/// what `lists` offers, and left to the app when nothing lists that kind.
fn field_of(
    param: &tilo_actions::ActionParam,
    lists: Option<&dyn ResourceSource>,
) -> Option<NamedField> {
    use tilo_actions::ParamType;
    let name = param.name.to_string();
    let prompt = if param.description.is_empty() {
        name.clone()
    } else {
        param.description.to_string()
    };
    let bound = |key: &str| {
        param
            .schema
            .as_ref()
            .and_then(|schema| schema.get(key))
            .and_then(serde_json::Value::as_f64)
    };
    let (kind, step) = match param.ty {
        ParamType::Text => (ui::ArgumentKind::Text, FieldStep::Input { numeric: false }),
        ParamType::Secret => (
            ui::ArgumentKind::Secret,
            FieldStep::Input { numeric: false },
        ),
        ParamType::Integer | ParamType::Number => (
            ui::ArgumentKind::Number {
                min: bound("minimum"),
                max: bound("maximum"),
                step: 1.0,
            },
            FieldStep::Input { numeric: true },
        ),
        ParamType::Resource => {
            let kind = param.resource_kind()?.to_owned();
            if !lists.is_some_and(|lists| lists.lists(&kind)) {
                return None;
            }
            (ui::ArgumentKind::Text, FieldStep::Pick { kind })
        }
        _ => return None,
    };
    Some(NamedField {
        name,
        spec: ui::ArgumentSpec {
            prompt,
            placeholder: String::new(),
            prefill: None,
            kind,
        },
        step,
    })
}

/// The required parameters of `invoke`'s action the person still has to answer,
/// when there is more than the one the palette's own step collects: every
/// required parameter gets a control, or `None` leaves the row to the app when
/// one of them is a type the palette cannot ask.
fn fields_to_collect(
    invoke: &crate::model::InvokeTarget,
    lists: Option<&dyn ResourceSource>,
) -> Option<Vec<NamedField>> {
    let descriptor = invoke.descriptor.as_ref()?;
    let mut fields = Vec::new();
    for param in &descriptor.params {
        if !param.required || invoke.arguments.contains_key(param.name.as_ref()) {
            continue;
        }
        fields.push(field_of(param, lists)?);
    }
    (fields.len() >= 2).then_some(fields)
}

impl DesktopSearch {
    pub fn new() -> Self {
        let mut palette = ActionPalette::rows(Vec::new());
        // Rows arrive already matched and ranked by the shared ranking.
        palette.set_bypass_fuzzy(true);
        // The shell opens and closes its search: a press beside the card, Escape, the chord or a
        // choice. The frame taking the keyboard's focus off the card in the pass that opened it,
        // as when a window's field held the keyboard as the chord came, must not close it: the
        // chord would open nothing.
        palette.set_host_keeps_open(true);
        palette.set_query_placeholder(placeholder());
        Self {
            palette,
            sources: None,
            recents: Arc::default(),
            switches: None,
            usage: crate::usage::Usage::default(),
            rows: Vec::new(),
            more_rows: Vec::new(),
            expanded: Vec::new(),
            everything: false,
            auto_selected: None,
            shown_query: None,
            asked_query: None,
            enter_waiting: None,
            shown_kinds: Vec::new(),
            catalogue: false,
            choice: None,
            stream: None,
            workers: SearchWorkers::new(),
            order: Vec::new(),
            own_sources: 0,
            rows_cut: 0,
            shown_two_lines: false,
            kept_choice: None,
            shown_rows: std::collections::HashMap::new(),
            scope: Scope::Workspace,
            refusal: None,
            collecting: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.sources.is_some()
    }

    /// Whether an application's answer to the query is still coming: it
    /// arrives on the host's clock, so the host keeps calling
    /// [`Self::refresh`] until it does. A source of the search's own is not
    /// this: it answers on a worker, which wakes the shell's next pass.
    fn waiting(&self) -> bool {
        self.sources.as_ref().is_some_and(Sources::waiting)
    }

    /// Whether a source of the open search still owes an answer: it works it
    /// out off the frame, and wakes the shell when it is in.
    fn answering(&self) -> bool {
        self.stream.as_ref().is_some_and(SearchStream::pending)
    }

    /// Open over `data` with an empty query.
    pub fn open(&mut self, mut data: SearchData) {
        self.scope = data.scope.clone();
        let placeholder = data.placeholder.take().unwrap_or_else(placeholder);
        self.palette.set_query_placeholder(placeholder.as_str());
        // A search over the workspace in front says so at the start of the
        // field, and offers the one control that widens it; over every
        // workspace there is nothing left to say or widen.
        let (token, widen) = match data.scope {
            Scope::Workspace => (
                Some(crate::i18n::t("search-scope-workspace")),
                Some(crate::i18n::t("search-launcher-widen")),
            ),
            Scope::InApp(_) | Scope::WindowGroup(_) | Scope::Everywhere => (None, None),
        };
        self.palette.set_scope_token(token);
        self.palette.set_scope_token_action(widen);
        // The apps alone, listed whole: nothing else is searched, so there
        // is nothing a row could lead to that is not already here.
        self.catalogue = matches!(data.scope, Scope::WindowGroup(_));
        self.sources = Some(Sources::new(data));
        self.choice = None;
        self.shown_query = None;
        self.asked_query = None;
        self.shown_rows.clear();
        self.shown_kinds.clear();
        self.stream = None;
        self.expanded.clear();
        self.everything = false;
        self.palette.open();
        self.refresh();
    }

    /// Close without choosing anything.
    pub fn close(&mut self) {
        self.palette.close();
        self.finish();
    }

    pub fn set_query(&mut self, query: &str) {
        self.palette.set_query(query);
        self.refresh();
    }

    /// The host could not carry out the row `target` names, for `why`: search
    /// opens again over the person's query with the reason on that row.
    pub fn refuse(&mut self, target: Target, why: String) {
        self.refusal = Some((target, why));
    }

    /// Why the row `target` names could not be carried out, while it still
    /// stands under that row: the line the palette draws there.
    #[must_use]
    pub fn refusal_for(&self, target: &Target) -> Option<&str> {
        let (refused, why) = self.refusal.as_ref()?;
        (refused == target).then_some(why.as_str())
    }

    /// Where this search stands, for asking it again where it was.
    #[must_use]
    pub fn scope(&self) -> Scope {
        self.scope.clone()
    }

    pub fn query(&self) -> &str {
        self.palette.query()
    }

    /// The hits currently shown, in palette order.
    pub fn results(&self) -> &[Hit] {
        &self.rows
    }

    pub fn palette(&self) -> &ActionPalette {
        &self.palette
    }

    pub fn palette_mut(&mut self) -> &mut ActionPalette {
        &mut self.palette
    }

    /// The room a host gives the rows: how wide they stand and how tall their list may be. The
    /// groups are cut to it on the next refresh.
    pub fn set_room(&mut self, width: f64, list_height: f64) {
        self.palette.set_max_width(Some(width));
        self.palette.set_max_list_height(list_height);
    }

    /// Whether the rows shown are cut to the room the palette has for them now.
    fn rows_placed(&self) -> bool {
        self.rows_cut == self.palette.entry_row_capacity_for(0, self.shown_two_lines)
    }

    /// Whether the search is over: the rows that are ready are cut to the
    /// card's room, and no source that is still out holds them back. What
    /// the shell publishes as a search still searching.
    ///
    /// A source still out — a window opened a moment ago whose content
    /// index is being built, an app's own server still looking — does not
    /// make the search "still searching" once the apps, actions and places
    /// that were ready are shown: the person reads those at once, and the
    /// late rows join them when they come ([`Self::expecting`], which a
    /// host polls for). Holding the ready rows behind the slowest source is
    /// what kept a fresh window's search saying "Searching…" for the 14–57 s
    /// its content index took on a page, measured on the page shell.
    #[must_use]
    pub fn settled(&self) -> bool {
        // With nothing open there is nothing to wait for. Rows that are ready
        // are the search's answer, so a source still out does not make it
        // searching. With nothing ready yet, it is still searching while a
        // source owes a row; a query that found nothing at all, with no
        // source left to answer, is settled too. Rows that answer an earlier
        // query are no answer to the text in the field: an Enter on them
        // waits for the typed query's rows (`take_waiting_enter`).
        self.sources.is_none()
            || (self.shown_query.as_deref() == Some(self.palette.query())
                && self.rows_placed()
                && !self.own_sources_owe()
                && (!self.rows.is_empty() || !self.expecting()))
    }

    /// Whether one of the desktop's own sources ([`rows::own_count`]) is still to answer the
    /// query shown: a row of another source in before them is not the search's answer.
    fn own_sources_owe(&self) -> bool {
        self.stream
            .as_ref()
            .is_some_and(|stream| stream.owes_any_before(self.own_sources))
    }

    /// Whether rows are still expected to join the ones shown: a source has
    /// yet to answer, or an application's answer is still coming, or a
    /// source that holds the frame's state still owes its share, or the card
    /// still says it is searching. What a host looks again for, so a late row
    /// is drawn when it arrives, even though the rows already shown are the
    /// search's answer ([`Self::settled`]).
    ///
    /// The card is looked at, not only the sources: an application's answer
    /// is waited for only so long ([`Self::waiting`]), and the pass that ends
    /// that wait is the one that stops the card saying it is searching. A
    /// host that stopped looking the moment the wait ended would leave the
    /// card saying "Searching…" with nothing left to draw it again.
    #[must_use]
    pub fn expecting(&self) -> bool {
        self.sources.is_some()
            && (self.waiting() || self.answering() || self.palette.is_searching())
    }

    /// Whether the card says it is searching for what is typed: an empty list says so while an
    /// answer it waits for is still coming.
    fn searching_for(&self, rows_empty: bool) -> bool {
        rows_empty && (self.answering() || self.waiting())
    }

    /// Run the query again when the palette's text changed since the rows
    /// were built, or an application's
    /// answer that was still coming arrived; and take the answers the
    /// sources sent and ask the source that holds the frame's state next.
    /// Returns whether the rows changed.
    pub fn refresh(&mut self) -> bool {
        let changed = self.refresh_rows();
        self.take_waiting_enter();
        changed
    }

    /// An Enter that waited for its query's rows: once the search has answered that query, its
    /// top row is chosen; a person who typed on since meant something else.
    fn take_waiting_enter(&mut self) {
        let Some(query) = self.enter_waiting.clone() else {
            return;
        };
        if self.palette.query() != query || !self.palette.is_open() {
            self.enter_waiting = None;
            return;
        }
        // The rows shown are the query's answer, as for an Enter on rows already shown: a source
        // still answering, or an app's answer that may never come, must not hold the key or cost
        // the open their frames. Only a query with no rows waits, and any for the desktop's own.
        if self.shown_query.as_deref() != Some(query.as_str())
            || (self.rows.is_empty() && (self.waiting() || self.answering()))
            || self.own_sources_owe()
        {
            return;
        }
        self.enter_waiting = None;
        let top = self
            .palette
            .visible_entries()
            .iter()
            .find_map(|entry| self.hit(&entry.id));
        if let Some(hit) = top {
            self.choose_hit(hit, None, self.palette.held_modifier());
        }
        // The choice ends search in this pass, as [`Self::observe`] does for an Enter on rows
        // shown: an idle shell takes no later pass, and kept it up over the window it opened.
        if !self.palette.is_open() {
            self.finish();
        }
    }

    fn refresh_rows(&mut self) -> bool {
        // A resource-kind pick is open over the list: its own items answer it, and the search's
        // sources wait until it is done.
        if self.collecting.as_ref().is_some_and(|c| c.pick.is_some()) {
            let query = self.palette.query().to_owned();
            let answered = match self.poll_pick() {
                PickAnswer::Arrived => {
                    self.render_pick(&query);
                    true
                }
                PickAnswer::Nothing => {
                    self.type_picked_field();
                    true
                }
                PickAnswer::Waiting => {
                    let moved = self.shown_query.as_deref() != Some(query.as_str());
                    if moved {
                        self.render_pick(&query);
                    }
                    moved
                }
            };
            return answered;
        }
        let Some(sources) = &self.sources else {
            return false;
        };
        let answered = sources.poll();
        let query = self.palette.query().to_owned();
        if answered || self.asked_query.as_deref() != Some(query.as_str()) {
            if self.asked_query.is_some() && self.asked_query.as_deref() != Some(query.as_str()) {
                // "Show all" belongs to the search it was asked in.
                self.expanded.clear();
                self.everything = false;
            }
            self.restart(&query);
            return true;
        }
        // The sources that still owe an answer: the rest of this search's
        // rows, as far as this frame's share of time reaches.
        if self.resume() {
            self.render(&query);
            return true;
        }
        // Rows of an earlier query kept while the typed one was answered: every source has
        // answered it now, with no row, so the card says nothing matched.
        if self.shown_query.as_deref() != Some(query.as_str())
            && !self.answering()
            && !self.waiting()
        {
            self.render(&query);
            return true;
        }
        // The card grew or shrank: the groups share what it shows now.
        if self.palette.entry_row_capacity_for(0, self.shown_two_lines) != self.rows_cut {
            self.render(&query);
            return true;
        }
        // A kind chip picked or unpicked since the rows were built: the
        // same search, the rows of the kinds left.
        if self.palette.picked_filters() != self.shown_kinds.as_slice() {
            self.render(&query);
            return true;
        }
        // The card says it is searching while an answer it waits for is still coming. When none
        // is — the workspace host's answer is waited for only so long ([`Self::waiting`]) — it
        // stops saying so here, because no answer will come to redraw it: without this the card
        // read "Searching…" for as long as it stood, over a search that had ended.
        if self.palette.is_searching()
            && self.shown_query.as_deref() == Some(query.as_str())
            && !self.searching_for(self.rows.is_empty())
        {
            self.palette.set_searching(false);
            return true;
        }
        false
    }

    /// Ask the sources of a new search, as far as this frame's share of
    /// time reaches, and show what came in.
    fn restart(&mut self, query: &str) {
        let Some(stream) = self.begin(query) else {
            return;
        };
        self.stream = Some(stream);
        self.asked_query = Some(query.to_owned());
        self.render(query);
    }

    /// Open a stream for `query` and let it ask the sources that fit this
    /// frame.
    fn begin(&mut self, query: &str) -> Option<crate::model::SearchStream> {
        let sources = self.sources.as_ref()?;
        let windowed = sources.window.is_some();
        // The switches name every source this search could ask, so they are
        // built once, as the query's search begins, and kept for its frames.
        self.switches = if windowed {
            None
        } else {
            switches(sources, &rows::switchable(sources, &self.recents)).map(Arc::new)
        };
        let all = rows::asked_sources(sources, &self.recents, self.switches.as_ref());
        let order: Vec<(String, (u8, usize))> = if windowed {
            Vec::new()
        } else {
            all.iter()
                .enumerate()
                .map(|(at, source)| (source.id().to_owned(), (source.browse_rank(), at)))
                .collect()
        };
        let now = sources.now;
        let usage = &self.usage;
        let workers = &self.workers;
        let stream = crate::model::search_stream(query, &all, 0, workers, &|hit: &Hit| {
            usage.strength(query, hit, now)
        });
        drop(all);
        self.order = order;
        self.own_sources = rows::own_count(sources, &self.recents);
        Some(stream)
    }

    /// Ask the sources that are still answering what the frame's time
    /// reaches. Returns whether any answered.
    fn resume(&mut self) -> bool {
        let Some(mut stream) = self.stream.take() else {
            return false;
        };
        let Some(sources) = self.sources.as_ref() else {
            return false;
        };
        let all = rows::asked_sources(sources, &self.recents, self.switches.as_ref());
        let query = self.palette.query().to_owned();
        let now = sources.now;
        let usage = &self.usage;
        let workers = &self.workers;
        let asked = stream.resume(&all, workers, &|hit: &Hit| usage.strength(&query, hit, now));
        drop(all);
        self.stream = Some(stream);
        asked
    }

    /// The groups in the order this search shows them.
    fn arranged(&self, query: &str, mut groups: Vec<KindGroup<Hit>>) -> Vec<KindGroup<Hit>> {
        // A window's search is about what the window holds: its resources
        // lead, and what can be done follows them, those acting on the
        // selection first.
        if self
            .sources
            .as_ref()
            .is_some_and(|sources| sources.window.is_some())
        {
            let (mut actions, mut held): (Vec<_>, Vec<_>) = groups
                .into_iter()
                .partition(|group| group.kind == ResourceKind::Action);
            for group in &mut actions {
                crate::window::selection_first(&mut group.results);
            }
            held.extend(actions);
            return held;
        }
        if query.trim().is_empty() {
            // Nothing typed: the groups stand in the order their sources ask
            // for a browse ([`Source::browse_rank`]), so what is happening
            // now leads, what the person reached for last follows, and the
            // ways into everything else come after.
            groups.sort_by_key(|group| {
                group
                    .results
                    .first()
                    .and_then(|hit| self.order.iter().find(|(id, _)| *id == hit.provider))
                    .map_or(
                        (crate::model::BROWSE_RANK_LATER, usize::MAX),
                        |(_, place)| *place,
                    )
            });
        }
        groups
    }

    fn render(&mut self, query: &str) {
        let Some(stream) = &self.stream else {
            return;
        };
        // The chips are the kinds this search found, so they stay offered
        // while one is picked: narrowing the rows must never take away the
        // chip that would widen them again.
        let found = stream.groups();
        let facets = kind_facets(&found);
        let picked = self.palette.picked_filters().to_vec();
        self.shown_kinds.clone_from(&picked);
        let mut groups = narrowed(self.arranged(query, found), &picked);
        // A query that found nothing but the row every query gets says that
        // nothing matched, and Enter on it opens nothing.
        if nothing_matched(&groups) {
            groups.clear();
        }
        let pending = stream.pending();
        // The rows about to be drawn, not the ones the palette still holds:
        // a row with a line under its name is twice as tall, and a list
        // counted in short rows runs past the card.
        let two_lines = groups
            .iter()
            .any(|group| group.results.iter().any(stands_tall));
        let capacity = self.palette.entry_row_capacity_for(0, two_lines);
        let browsing = query.trim().is_empty();
        let mut rows = Vec::new();
        let mut more_rows = Vec::new();
        let entries = self.fitted(groups, browsing, &mut rows, &mut more_rows);
        // A key typed while the rows of the query before it are shown: the new query's sources are
        // still answering and none has a row yet. The card keeps the rows it shows, dimmed as
        // stale, rather than collapsing to "Searching…" and growing again a moment later; the
        // first rows of the typed query, or every source answering with none, replace them. An
        // Enter meanwhile waits for the typed query's rows (`take_waiting_enter`).
        let stale = rows.is_empty()
            && !self.rows.is_empty()
            && self
                .shown_query
                .as_deref()
                .is_some_and(|shown| shown != query)
            && (pending || self.waiting() || self.answering());
        if stale {
            self.palette.set_stale(true);
            self.palette.set_searching(true);
            return;
        }
        self.palette.set_host_facets(facets);
        // A source that answered re-ranks the same query, and the plan of the
        // frame it starts in is not the query's answer yet: the sources after
        // the one that answered have not been asked, and a card not yet given
        // its room cuts the rows to a guess. That plan can hold fewer rows
        // than the frame before, while the card on screen still shows those.
        // They stay — what the person sees, and what their Enter acts on: a
        // plan short of them would leave the key naming a row that is not
        // there, or none at all, and the press would go nowhere. The search
        // says it is still filling in.
        let unfitted = rows.len() < self.rows.len();
        let refilling = unfitted
            && !self.rows.is_empty()
            && self.shown_query.as_deref() == Some(query)
            && (pending || self.waiting() || self.answering());
        if refilling {
            self.palette.set_searching(true);
            return;
        }
        // What is selected on screen is named by the rows of the frame the
        // person sees, before this frame replaces them.
        let on_screen = self
            .palette
            .selected_id()
            .and_then(|id| self.chosen_row(id));
        self.more_rows = more_rows;
        // A source answering late re-ranks the rows of the same query: the
        // row the person moved to stays chosen, so Enter does not open
        // whatever slid under the cursor. A selection they never moved
        // follows the best result. A frame that lists no room for their row
        // yet keeps it for the frame that does.
        if self.shown_query.as_deref() != Some(query) {
            self.kept_choice = None;
        } else if self.palette.selected_id() != self.auto_selected.as_deref()
            && let Some(chosen) = on_screen
        {
            self.kept_choice = Some(chosen);
        }
        let chosen = self.kept_choice.clone();
        let kept = chosen.and_then(|chosen| match chosen {
            ChosenRow::Hit(provider, id) => rows
                .iter()
                .position(|hit| hit.provider == provider && hit.id == id)
                .map(|index| format!("hit:{index}")),
            ChosenRow::ShowAll(kind) => self
                .more_rows
                .iter()
                .position(|more| *more == kind)
                .map(|index| format!("{MORE_PREFIX}{index}")),
        });
        self.palette.set_entries(entries);
        self.shown_rows = rows::shown_rows(&rows, &self.more_rows);
        let cursor = kept.and_then(|id| {
            self.palette
                .visible_entries()
                .iter()
                .position(|entry| entry.id == id)
        });
        if let Some(cursor) = cursor {
            self.palette.set_cursor(cursor);
        }
        self.auto_selected = if cursor.is_some() {
            None
        } else {
            self.palette.selected_id().map(str::to_owned)
        };
        // The slower sources answer in later frames: while one is still
        // coming, an empty list says it is searching rather than that
        // nothing matched.
        self.palette
            .set_searching(self.searching_for(rows.is_empty()));
        self.rows = rows;
        self.shown_query = Some(query.to_owned());
        self.rows_cut = capacity;
        self.shown_two_lines = two_lines;
    }

    /// The rows the card holds for `groups`: each group trimmed to the share
    /// the card's room gives it, the groups left over listed by one row that
    /// offers every result the card could not show. The answer and the way
    /// to the web are never left over: a crowded card loses things to open
    /// before the question's own answer.
    fn fitted(
        &self,
        mut groups: Vec<KindGroup<Hit>>,
        browsing: bool,
        rows: &mut Vec<Hit>,
        more: &mut Vec<ResourceKind>,
    ) -> Vec<CommandEntry> {
        let total: usize = groups.iter().map(|group| group.total).sum();
        let mut dropped = false;
        loop {
            // Short rows hold more than tall ones, so the room is counted in
            // the shortest a row can be and every plan is weighed at its own
            // line heights: a list that lost a row it could have kept would
            // hide a result for nothing.
            let cap = if self.everything {
                usize::MAX
            } else {
                self.palette.entry_row_capacity_for(groups.len(), false)
            };
            // A lone group is kept whole to spare it a "Show all" row it
            // would not need, so its own rows are trimmed to the room left
            // instead. Every group keeps at least its first row.
            let lone = groups.len() == 1;
            // A lone group the person asked to see whole stays whole and
            // the card scrolls it: it is not offered itself again.
            let whole = lone
                && groups
                    .first()
                    .is_some_and(|group| self.expanded.contains(&group.kind));
            let floor = usize::from(!lone);
            let mut allowed = cap;
            let mut last: Option<Vec<CommandEntry>>;
            loop {
                let trimmed = if self.everything {
                    groups.clone()
                } else if lone {
                    let mut alone = groups.clone();
                    if !whole && let Some(only) = alone.first_mut() {
                        only.results.truncate(allowed);
                    }
                    alone
                } else {
                    // Rows and the row offering a cut group's rest both
                    // stand in the list, so the groups share what stays once
                    // those rows are paid for; a person asking for
                    // everything lists them whole and scrolls.
                    ui::search::system::trim_groups(groups.clone(), allowed, &self.expanded)
                };
                let mut entries = entries_for(
                    &trimmed,
                    browsing,
                    self.catalogue,
                    self.refusal.as_ref(),
                    more,
                    rows,
                );
                let shown: usize = trimmed.iter().map(|group| group.results.len()).sum();
                if dropped {
                    entries.push(all_results_entry(total.saturating_sub(shown)));
                }
                if whole || self.everything || self.palette.entries_fit(&entries, trimmed.len()) {
                    return entries;
                }
                last = Some(entries);
                if allowed <= floor {
                    break;
                }
                allowed = allowed.saturating_sub(1);
            }
            let last = last.unwrap_or_default();
            if dropped {
                // Nothing is left to leave out: the rows scroll under the
                // card rather than being cut at its edge.
                return last;
            }
            // No room for even this many groups: the lowest-ranked one the
            // search's own rows do not keep makes way for the all-results
            // row, which lists it too. A group the person asked to see whole
            // never makes way: the card scrolls it.
            let Some(at) = groups.iter().rposition(|group| {
                !always_shown(&group.kind) && !self.expanded.contains(&group.kind)
            }) else {
                return last;
            };
            groups.remove(at);
            dropped = true;
        }
    }

    /// List every row of the group behind "Show all" row `more`, keeping
    /// the query and the selected row where they are.
    fn expand(&mut self, more: usize) {
        let Some(kind) = self.more_rows.get(more).cloned() else {
            return;
        };
        if !self.expanded.contains(&kind) {
            self.expanded.push(kind);
        }
        let cursor = self.palette.cursor();
        let query = self.palette.query().to_owned();
        self.kept_choice = None;
        self.render(&query);
        self.palette.set_cursor(cursor);
        // Choosing a row closes a command palette; this one only grew.
        tilo_ui_base::widget::Widget::set_focus(&mut self.palette, true);
    }

    /// List every result the search found, keeping the query and the
    /// selected row where they are: the card scrolls the rows it has no
    /// room for.
    fn show_everything(&mut self) {
        self.everything = true;
        let cursor = self.palette.cursor();
        let query = self.palette.query().to_owned();
        self.kept_choice = None;
        self.render(&query);
        self.palette.set_cursor(cursor);
        tilo_ui_base::widget::Widget::set_focus(&mut self.palette, true);
    }

    /// What lists a resource-kind parameter's items, where this host serves the workspace's list
    /// actions.
    fn resource_options(&self) -> Option<&dyn ResourceSource> {
        self.sources.as_ref()?.resource_options.as_deref()
    }

    /// Open the step for the current field of the chosen action: the palette's
    /// own field, in the control that field's type asks for; a resource kind's
    /// items are listed under it to pick from.
    fn open_current_field(&mut self) {
        let Some(collecting) = self.collecting.as_ref() else {
            return;
        };
        let Some(field) = collecting.fields.get(collecting.index) else {
            return;
        };
        let (id, spec) = (collecting.hit.id.clone(), field.spec.clone());
        match field.step {
            FieldStep::Input { .. } => {
                self.palette.open_argument_step(id, spec);
                tilo_ui_base::widget::Widget::set_focus(&mut self.palette, true);
            }
            FieldStep::Pick { ref kind } => {
                let kind = kind.clone();
                self.open_pick(&kind, &spec.prompt);
            }
        }
    }

    /// List the items of `kind` for the open field, and show them as rows: the person picks one
    /// instead of typing its address. A host with no list for the kind, or one that answers
    /// nothing, leaves the field as typed text, as before.
    fn open_pick(&mut self, kind: &str, prompt: &str) {
        let lists = self
            .sources
            .as_ref()
            .and_then(|sources| sources.resource_options.clone());
        let Some(pending) = lists.and_then(|lists| lists.ask(kind)) else {
            self.type_picked_field();
            return;
        };
        // What the search asked, kept aside while the field narrows the items; a text step the
        // palette was still collecting is done, and the pick takes the field.
        let saved = self.palette.query().to_owned();
        self.palette.cancel_host_step();
        if let Some(collecting) = self.collecting.as_mut() {
            collecting.pick = Some(Picking {
                prompt: prompt.to_owned(),
                saved,
                pending: Some(pending),
                options: Vec::new(),
            });
        }
        self.palette.set_query("");
        self.palette.set_query_placeholder(prompt);
        self.render_pick("");
        tilo_ui_base::widget::Widget::set_focus(&mut self.palette, true);
    }

    /// Ask for the pick's answer again, without waiting: what the host's wake calls for.
    fn poll_pick(&mut self) -> PickAnswer {
        let Some(pick) = self.collecting.as_mut().and_then(|c| c.pick.as_mut()) else {
            return PickAnswer::Nothing;
        };
        let Some(mut pending) = pick.pending.take() else {
            return PickAnswer::Waiting;
        };
        match pending.poll_ready() {
            // Nothing of the kind is kept: the field is asked as text, so its address can still
            // be typed.
            PendingPoll::Ready(Ok(options)) if options.is_empty() => PickAnswer::Nothing,
            PendingPoll::Ready(Ok(options)) => {
                pick.options = options;
                PickAnswer::Arrived
            }
            PendingPoll::Ready(Err(_)) | PendingPoll::Abandoned => PickAnswer::Nothing,
            PendingPoll::Waiting => {
                pick.pending = Some(pending);
                PickAnswer::Waiting
            }
        }
    }

    /// Show the pick's items, narrowed to what the person typed, under a row that says what the
    /// field asks.
    fn render_pick(&mut self, query: &str) {
        let Some(pick) = self.collecting.as_ref().and_then(|c| c.pick.as_ref()) else {
            return;
        };
        let needle = query.trim().to_lowercase();
        let entries: Vec<CommandEntry> = pick
            .options
            .iter()
            .enumerate()
            .filter(|(_, option)| option.matches(&needle))
            .map(|(index, option)| {
                let mut entry = CommandEntry::new(format!("{PICK_PREFIX}{index}"), option.label())
                    .group(pick.prompt.clone());
                if let Some(subtitle) = option.subtitle() {
                    entry = entry.description(subtitle);
                }
                entry
            })
            .collect();
        self.palette.set_entries(entries);
        self.palette
            .set_searching(pick.pending.is_some() && pick.options.is_empty());
        self.shown_rows.clear();
        self.rows.clear();
        self.more_rows.clear();
        self.shown_query = Some(query.to_owned());
    }

    /// The row `id` names among the pick's items, when one is open.
    fn picked_row(&self, id: &str) -> Option<usize> {
        let pick = self.collecting.as_ref()?.pick.as_ref()?;
        let index = id.strip_prefix(PICK_PREFIX)?.parse().ok()?;
        (index < pick.options.len()).then_some(index)
    }

    /// The person chose the pick's item `index`: it fills the field with the item's reference and
    /// the next field opens, or the action runs.
    fn chose_picked(&mut self, index: usize) {
        let Some(collecting) = self.collecting.as_mut() else {
            return;
        };
        let Some(pick) = collecting.pick.as_ref() else {
            return;
        };
        let Some(option) = pick.options.get(index) else {
            return;
        };
        let value = serde_json::Value::String(option.value.clone());
        let Some(field) = collecting.fields.get(collecting.index) else {
            return;
        };
        let name = field.name.clone();
        collecting.arguments.insert(name, value);
        collecting.index = collecting.index.saturating_add(1);
        let done = collecting.index >= collecting.fields.len();
        let saved = collecting
            .pick
            .take()
            .map_or_else(String::new, |pick| pick.saved);
        self.palette.set_query_placeholder(placeholder());
        self.palette.set_query(&saved);
        if done {
            self.finish_collected();
        } else {
            self.restart(&saved);
            tilo_ui_base::widget::Widget::set_focus(&mut self.palette, true);
            self.open_current_field();
        }
    }

    /// Back out of the open pick: the list is where the person was, and the action is not run.
    fn leave_pick(&mut self) {
        let Some(collecting) = self.collecting.as_mut() else {
            return;
        };
        let Some(pick) = collecting.pick.take() else {
            return;
        };
        self.collecting = None;
        self.palette.set_query_placeholder(placeholder());
        self.palette.open();
        self.palette.set_query(&pick.saved);
        self.restart(&pick.saved);
        tilo_ui_base::widget::Widget::set_focus(&mut self.palette, true);
    }

    /// The open field's kind lists nothing here: ask for its address in the field, as a text
    /// parameter is asked.
    fn type_picked_field(&mut self) {
        let Some(collecting) = self.collecting.as_mut() else {
            return;
        };
        let saved = collecting.pick.take().map(|pick| pick.saved);
        let Some(field) = collecting.fields.get_mut(collecting.index) else {
            return;
        };
        field.step = FieldStep::Input { numeric: false };
        field.spec.kind = ui::ArgumentKind::Text;
        self.palette.set_query_placeholder(placeholder());
        if let Some(saved) = saved {
            self.palette.set_query(&saved);
            self.restart(&saved);
        }
        self.open_current_field();
        tilo_ui_base::widget::Widget::set_focus(&mut self.palette, true);
    }

    /// File the value an open step answered, and open the next field or run the
    /// action once every parameter has one. Returns whether the outcome was one
    /// of the collected fields (and so is not a fresh row choice).
    fn answered_collected_field(&mut self, outcome: &ui::CommandOutcome) -> bool {
        let Some(value) = outcome.argument.as_ref() else {
            return false;
        };
        let done = {
            let Some(collecting) = self.collecting.as_mut() else {
                return false;
            };
            if outcome.id != collecting.hit.id {
                return false;
            }
            let Some(field) = collecting.fields.get(collecting.index) else {
                return false;
            };
            let FieldStep::Input { numeric } = &field.step else {
                return false;
            };
            let numeric = *numeric;
            let json = if numeric {
                // A text that is not a number runs the action with the words as
                // typed, which the action then says are no number in its own
                // words; the step does not refuse it here.
                let number = tilo_failure::ignored!(
                    "a number step's text that is not a number is passed on as words",
                    value.trim().parse::<f64>().ok()
                );
                number.and_then(serde_json::Number::from_f64).map_or_else(
                    || serde_json::Value::String(value.clone()),
                    serde_json::Value::Number,
                )
            } else {
                serde_json::Value::String(value.clone())
            };
            let name = field.name.clone();
            collecting.arguments.insert(name, json);
            collecting.index = collecting.index.saturating_add(1);
            collecting.index >= collecting.fields.len()
        };
        if done {
            self.finish_collected();
        } else {
            self.open_current_field();
        }
        true
    }

    /// Run the chosen action now every required parameter has a value: the row
    /// it came from, carried out with all of them at once.
    fn finish_collected(&mut self) {
        let Some(collecting) = self.collecting.take() else {
            return;
        };
        let mut target = collecting.hit.target;
        target.place_by(collecting.held);
        if let Target::Invoke(invoke) = &mut target {
            for (name, value) in collecting.arguments {
                invoke.arguments.entry(name).or_insert(value);
            }
        }
        self.choice = Some(target);
        self.palette.close();
    }

    /// Follow the palette after it handled input: take a chosen row, and
    /// notice that the palette closed itself (a choice or Escape). Returns
    /// whether search closed; the caller then [`refresh`](Self::refresh)es
    /// an open palette for its edited query.
    pub fn observe(&mut self) -> bool {
        // Backing out of a step abandons the action it was collecting for; the
        // list is where the person was, so the search stays open over it.
        if self.palette.take_argument_cancelled() {
            self.collecting = None;
        }
        if let Some(outcome) = self.palette.take_outcome() {
            self.handle_outcome(outcome);
        }
        if self.sources.is_none() || self.palette.is_open() {
            return false;
        }
        // Escape closed the pick: the list is where the person was, and the action it was
        // collecting for is not run.
        if self.collecting.as_ref().is_some_and(|c| c.pick.is_some()) {
            self.leave_pick();
            return false;
        }
        self.finish();
        true
    }

    /// Take the row the palette handed out: a pick's item, a step of the action being
    /// collected, a group asked whole, or a row the person chose.
    fn handle_outcome(&mut self, outcome: ui::CommandOutcome) {
        if let Some(index) = self.picked_row(&outcome.id) {
            self.chose_picked(index);
        } else if self.answered_collected_field(&outcome) {
            // A field of the action being collected: the next step, or the run.
        } else if let Some(more) = more_row(&outcome.id) {
            self.expand(more);
        } else if outcome.id == ALL_RESULTS_ID {
            self.show_everything();
        } else if outcome.id == ALL_APPS_ID {
            // The catalogue of every app: the desktop lists them all,
            // by name, as its empty shell's launchpad does.
            self.choice = Some(Target::Command {
                id: crate::model::ALL_APPS_COMMAND.into(),
            });
            self.palette.close();
        } else if outcome.argument.is_none()
            && (self.shown_query.as_deref() != Some(self.palette.query())
                || (self.own_sources_owe()
                    && self.auto_selected.as_deref() == Some(outcome.id.as_str())))
        {
            // Enter came in the same batch of input as the keys before it, ahead of the frame
            // that answers them: the rows on screen are an earlier query's, and the row it
            // named is one of those ("canvas" and Enter started a call, the top row of the
            // empty search). Enter waits for the typed query's answer and takes its top row. A
            // row's own line (an action's argument) is typed in the field after its query, so
            // an Enter that carries one is never this. So is one on the row put first while the
            // desktop's own sources are still out: the person pressed for the best row.
            self.choose_top_of_typed_query();
        } else if let Some(hit) = pressed_hit(self, &outcome.id) {
            self.choose_hit(hit, outcome.argument, self.palette.chosen_with());
        }
    }

    /// Enter on what the field holds, typed before the search was up to show any of it: the top row
    /// of its answer is chosen once the search has given one ([`Self::choose_top_of_typed_query`]).
    pub fn choose_top_when_answered(&mut self) {
        if self.palette.is_open() {
            self.choose_top_of_typed_query();
        }
    }

    /// Hold an Enter pressed on rows of an earlier query until the search has answered the query
    /// the field holds: the search stays open on it, and its top row, which the highlight sits on,
    /// is chosen as the rows come in ([`Self::take_waiting_enter`]).
    fn choose_top_of_typed_query(&mut self) {
        let query = self.palette.query().to_owned();
        self.palette.open();
        self.palette.set_query(&query);
        self.enter_waiting = Some(query);
    }

    /// Choose `hit`, with `argument` the one line its action asks for:
    /// it is remembered, and its target is the choice. A switch stays in
    /// view, showing what it flipped to; anything else closes search.
    fn choose_hit(&mut self, hit: Hit, argument: Option<String>, held: Option<ActivationModifier>) {
        // An action that takes more than the palette's one line collects every
        // required parameter first, one step at a time, then runs: choosing the
        // row opens the first step rather than running it short.
        if argument.is_none()
            && let Target::Invoke(invoke) = &hit.target
            && let Some(fields) = fields_to_collect(invoke, self.resource_options())
        {
            self.remember(&hit);
            self.refusal = None;
            self.collecting = Some(Collecting {
                hit,
                fields,
                index: 0,
                arguments: serde_json::Map::new(),
                held,
                pick: None,
            });
            self.open_current_field();
            return;
        }
        // A source switched off or on again: the rows change under the
        // switch, which stays where it is so it can be flipped back.
        if let Target::Prefer { source, enabled } = &hit.target {
            let (source, enabled) = (source.clone(), *enabled);
            if let Some(sources) = &mut self.sources {
                sources.preferences.set_enabled(&source, enabled);
            }
            self.choice = Some(hit.target);
            let query = self.palette.query().to_owned();
            self.restart(&query);
            return;
        }
        self.remember(&hit);
        // Trying any row is the answer to the one that did not happen: the
        // reason it stood under goes with it.
        self.refusal = None;
        let mut target = hit.target;
        target.place_by(held);
        let mut toggles = false;
        match &mut target {
            Target::Invoke(invoke) => {
                if let Some(descriptor) = &invoke.descriptor {
                    toggles = descriptor.state.toggles;
                    if let (Some(param), Some(value)) = (descriptor.prompt_param(), argument) {
                        invoke
                            .arguments
                            .insert(param.to_owned(), serde_json::Value::String(value));
                    }
                }
            }
            // An app's own action row runs with the value the person typed,
            // where the row asks for one.
            Target::Action(action) => {
                toggles = action.spec.state.toggles;
                if let (Some(param), Some(value)) = (action.spec.prompt_param(), argument) {
                    action
                        .preset
                        .get_or_insert_with(|| (param.to_owned(), value));
                }
            }
            _ => {}
        }
        self.choice = Some(target);
        if !toggles {
            self.palette.close();
        }
    }

    fn remember(&mut self, hit: &Hit) {
        Arc::make_mut(&mut self.recents).record(hit);
        let now = self.sources.as_ref().map_or(0, |sources| sources.now);
        let query = self.palette.query().to_owned();
        self.usage.record(&query, hit, now);
    }

    /// Choose the shown row `index` ([`Self::results`]) as a person
    /// choosing it does, with `argument` the line its action asks for:
    /// what a caller of the desktop's published search runs. An app's own
    /// action row ([`Target::Action`]) runs only from its window, through
    /// the host's action API. Returns whether there was such a row.
    pub fn choose(&mut self, index: usize, argument: Option<String>) -> bool {
        self.choose_with(index, argument, self.palette.held_modifier())
    }

    /// Choose the shown row `index` as a caller that knows the modifier `held` the person held:
    /// the way its action opens (a tab here, a window beside) follows it.
    pub fn choose_with(
        &mut self,
        index: usize,
        argument: Option<String>,
        held: Option<ActivationModifier>,
    ) -> bool {
        let Some(hit) = self.rows.get(index).cloned() else {
            return false;
        };
        self.choose_hit(hit, argument, held);
        true
    }

    /// What the person chose before, to keep with the rest of what the host
    /// keeps.
    #[must_use]
    pub fn usage(&self) -> &crate::usage::Usage {
        &self.usage
    }

    /// What the person chose before, as the host kept it.
    pub fn set_usage(&mut self, usage: crate::usage::Usage) {
        self.usage = usage;
    }

    /// The chosen row, for the host to perform.
    pub fn take_choice(&mut self) -> Option<Target> {
        self.choice.take()
    }

    /// Whether the person took the field's scope token away since the last
    /// call: the search is narrowed no more, and the host opens it again
    /// over every workspace.
    pub fn take_scope_widened(&mut self) -> bool {
        self.palette.take_scope_token_removed()
    }

    fn hit(&self, id: &str) -> Option<Hit> {
        let index: usize = id.strip_prefix("hit:")?.parse().ok()?;
        self.rows.get(index).cloned()
    }

    /// What the row `id` shows, to find it again among rebuilt rows: a hit,
    /// or the "Show all" row of a group.
    fn chosen_row(&self, id: &str) -> Option<ChosenRow> {
        if let Some(more) = id.strip_prefix(MORE_PREFIX) {
            let kind = self.more_rows.get(more.parse::<usize>().ok()?)?;
            return Some(ChosenRow::ShowAll(kind.clone()));
        }
        self.hit(id).map(|hit| ChosenRow::Hit(hit.provider, hit.id))
    }

    fn finish(&mut self) {
        self.sources = None;
        self.stream = None;
        self.order.clear();
        self.rows.clear();
        self.more_rows.clear();
        self.expanded.clear();
        self.everything = false;
        self.auto_selected = None;
        self.kept_choice = None;
        self.refusal = None;
        self.shown_query = None;
        self.asked_query = None;
        self.enter_waiting = None;
    }
}

/// The rows that switch a source off and on again, one per switchable
/// source of a search over everything, by the name a person knows it by.
/// A search of the desktop's own (the apps only, one window) has no host
/// sources and nothing to keep a choice in, and offers none.
fn switches(sources: &Sources, all: &[&dyn Source]) -> Option<Switches> {
    if sources.host.is_empty() {
        return None;
    }
    // Two sources may search one kind of thing under one id (the records this device holds and
    // the host's search of the rest): one switch turns both off.
    let mut names: Vec<(String, String)> = Vec::new();
    for source in all {
        if crate::preferences::switchable(source.id())
            && !names.iter().any(|(id, _)| id == source.id())
        {
            names.push((source.id().to_owned(), source.name()));
        }
    }
    Some(Switches::new(names, &sources.preferences))
}

#[cfg(test)]
mod tests;
