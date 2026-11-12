//! The desktop search palette: the toolkit's [`ActionPalette`] over the
//! portable sources. The host owns placement and input routing; a chosen row
//! is handed back through [`DesktopSearch::take_choice`], an action row as
//! the address the host's action door resolves against the live resource
//! tree.

use std::sync::Arc;

use tilo_actions::ResourceSpec;
use ui::{ActionPalette, CommandEntry};

use crate::model::{Held, Hit, LaunchPlace, SearchStream, Source, Target};
use crate::providers::{
    Actions, Answers, AppEntry, Applications, Captures, CommandSpec, Commands, Content, Published,
    PublishedActions, Recents, Switches, WebSearch, WindowEntry, Windows,
};
use crate::window::WindowSearch;
use crate::workers::SearchWorkers;
use ui::search::system::{KindGroup, ResourceKind};
use ui::search::{ActivationModifier, Scope};

/// Row id prefix of the row that lists the rest of a group the palette's
/// height cut short; `more:{i}` expands `more_rows[i]`.
const MORE_PREFIX: &str = "more:";

mod rows;

#[cfg(test)]
mod no_match_tests;
#[cfg(test)]
mod ready_rows_tests;
#[cfg(test)]
mod windowed_host_tests;

use rows::{
    all_results_entry, always_shown, entries_for, everywhere, kind_facets, narrowed,
    nothing_matched, stands_tall,
};

/// The row `id` names when it is a group's "more" row: an id this palette
/// wrote, so one that does not read as a number is no row here.
fn more_row(id: &str) -> Option<usize> {
    let more = id.strip_prefix(MORE_PREFIX)?;
    // ignored-failure: the id is one this palette wrote, and another row has another
    more.parse().ok()
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
    host: Vec<Held>,
    preferences: crate::preferences::SearchPreferences,
    /// Now, in Unix seconds, as the host said when search opened.
    now: u64,
}

impl Sources {
    fn new(data: SearchData) -> Self {
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
                commands: Arc::new(Commands(data.commands)),
                content: Content::new(Vec::new(), Vec::new()),
                actions: Arc::new(Actions(Vec::new())),
                // What the app publishes through the host runs by address there: an app in a
                // process of its own gives the desk no tree to read, so its window's actions are
                // these.
                published: Arc::new(Published(data.published)),
                captures: Arc::new(Captures(Vec::new())),
                answers: None,
                window: Some(WindowSearch::new(data.resources, data.providers).within(anchor)),
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
            commands: Arc::new(Commands(data.commands)),
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
        self.content.waiting() || self.window.as_ref().is_some_and(WindowSearch::waiting)
    }
}

/// How many rows the palette cut its rows to, and whether the card had given
/// the room they stand in when it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowRoom {
    /// The card has not given its room since search was opened: the rows
    /// are cut to a guess it will correct, so they are not what the search
    /// ends with.
    Guess(usize),
    /// The card gave its room: the rows are cut to that many rows of it.
    Room(usize),
}

impl RowRoom {
    /// The rows the palette cut to, however it knew the room.
    fn cut(self) -> usize {
        match self {
            RowRoom::Guess(rows) | RowRoom::Room(rows) => rows,
        }
    }

    /// The same cut, against the room the card gave.
    fn given(self) -> Self {
        RowRoom::Room(self.cut())
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
    recents: Recents,
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
    /// The query an Enter was pressed on before its rows were in: its top row is chosen once the
    /// search has answered it.
    enter_waiting: Option<String>,
    /// [`ui::search::engine_generation`] when the rows were built: the
    /// content rows come again once the index's code arrived.
    shown_generation: u64,
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
    /// How many rows the palette cut its rows to, and whether that is the
    /// card's real room: when the card grew or shrank since, the groups are
    /// cut again.
    rows_room: RowRoom,
    /// Whether the rows the palette holds stand two lines tall: what the
    /// capacity is counted in.
    shown_two_lines: bool,
    /// The row the person moved to in this search, kept while the rows fill
    /// in: a frame that has no room for it yet does not take their choice
    /// away.
    kept_choice: Option<ChosenRow>,
    /// Where this search stands: the workspace, every workspace, one app's
    /// own — so a row carried out by the host can be asked again where the
    /// person was when they chose it.
    scope: Scope,
    /// The row the host could not carry out, with why: it keeps the reason
    /// under its name until another row is chosen or search closes.
    refusal: Option<(Target, String)>,
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

impl DesktopSearch {
    pub fn new() -> Self {
        let mut palette = ActionPalette::rows(Vec::new());
        // Rows arrive already matched and ranked by the shared ranking.
        palette.set_bypass_fuzzy(true);
        palette.set_query_placeholder(placeholder());
        Self {
            palette,
            sources: None,
            recents: Recents::default(),
            usage: crate::usage::Usage::default(),
            rows: Vec::new(),
            more_rows: Vec::new(),
            expanded: Vec::new(),
            everything: false,
            auto_selected: None,
            shown_query: None,
            enter_waiting: None,
            shown_generation: 0,
            shown_kinds: Vec::new(),
            catalogue: false,
            choice: None,
            stream: None,
            workers: SearchWorkers::new(),
            order: Vec::new(),
            rows_room: RowRoom::Guess(0),
            shown_two_lines: false,
            kept_choice: None,
            scope: Scope::Workspace,
            refusal: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.sources.is_some()
    }

    /// Whether an application's answer to the query is still coming: it
    /// arrives on the host's clock, so the host keeps calling
    /// [`Self::refresh`] until it does. A source of the search's own is not
    /// this: it answers on a worker, which wakes the desk's next pass.
    pub fn waiting(&self) -> bool {
        self.sources.as_ref().is_some_and(Sources::waiting)
    }

    /// Whether a source of the open search still owes an answer: it works it
    /// out off the frame, and wakes the desk when it is in.
    pub fn answering(&self) -> bool {
        self.stream.as_ref().is_some_and(SearchStream::pending)
    }

    /// Open over `data` with an empty query.
    pub fn open(&mut self, mut data: SearchData) {
        // The search's engine is fetched now, as the palette opens, so its code is in by the first
        // keystroke rather than being fetched on the query that needed it: on a page the engine is
        // a module of its own, and a search that fetches it has no rows to show until it lands,
        // and says it is still searching that whole time. Linked in, this is nothing. The rows
        // built meanwhile are rebuilt when it arrives, which a palette's refresh already asks for.
        ui::search::prepare_engine();
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

    /// What Escape does in the desk's search: a typed query is cleared first
    /// and search stays open, so a slip of the finger never costs the whole
    /// search; Escape on the empty field closes it. Returns whether the desk
    /// did that.
    ///
    /// A step that asks for a value, a row's other actions, or a question a
    /// window's control asked is the palette's own to back out of, before it
    /// would close ([`ActionPalette::handle_event`]): then this answers
    /// `false` and the key goes to the palette, which goes back a step and
    /// closes only from the results.
    pub fn escape(&mut self) -> bool {
        if !self.is_open() || self.palette.in_argument_mode() {
            return false;
        }
        if self.query().is_empty() {
            self.close();
        } else {
            self.set_query("");
        }
        true
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

    /// The room the card has where it stands: how wide it is, and the
    /// tallest its result list may grow, as the display or window it shows
    /// over gives them.
    pub fn set_room(&mut self, width: f64, list_height: f64) {
        self.palette.set_max_width(Some(width));
        self.palette.set_max_list_height(list_height);
        self.rows_room = self.rows_room.given();
    }

    /// Whether the rows shown are cut to the room the card gave them: before
    /// it has given one, and in the frame it gives a new one, they are not.
    fn rows_placed(&self) -> bool {
        match self.rows_room {
            RowRoom::Guess(_) => false,
            RowRoom::Room(rows) => {
                rows == self.palette.entry_row_capacity_for(0, self.shown_two_lines)
            }
        }
    }

    /// Whether the search has a frame's work of its own left: a source that
    /// holds the frame's state still to ask, or rows to cut to the card's
    /// room. A host draws the frames this asks for; a source on a worker is
    /// not this (its answer wakes the desk), and neither is an
    /// application's answer still coming, which is looked for on the host's
    /// clock ([`Self::waiting`]).
    #[must_use]
    pub fn filling_in(&self) -> bool {
        self.sources.is_some()
            && (self
                .stream
                .as_ref()
                .is_some_and(SearchStream::awaiting_here)
                || !self.rows_placed())
    }

    /// Whether the search is over: the rows that are ready are cut to the
    /// card's room, and no source that is still out holds them back. What
    /// the desk publishes as a search still searching.
    ///
    /// A source still out — a window opened a moment ago whose content
    /// index is being built, an app's own server still looking — does not
    /// make the search "still searching" once the apps, actions and places
    /// that were ready are shown: the person reads those at once, and the
    /// late rows join them when they come ([`Self::expecting`], which a
    /// host polls for). Holding the ready rows behind the slowest source is
    /// what kept a fresh window's search saying "Searching…" for the 14–57 s
    /// its content index took on a page, measured on the page desk.
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
                && (!self.rows.is_empty() || !self.expecting()))
    }

    /// Whether rows are still expected to join the ones shown: a source has
    /// yet to answer, or an application's answer is still coming, or a
    /// source that holds the frame's state still owes its share. What a
    /// host looks again for, so a late row is drawn when it arrives, even
    /// though the rows already shown are the search's answer
    /// ([`Self::settled`]).
    #[must_use]
    pub fn expecting(&self) -> bool {
        self.sources.is_some() && (self.waiting() || self.answering())
    }

    /// Run the query again when the palette's text changed since the rows
    /// were built, the index's code arrived since, or an application's
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
        // the open their frames. Only a query with no rows yet waits for its first.
        if self.shown_query.as_deref() != Some(query.as_str())
            || (self.rows.is_empty() && (self.waiting() || self.answering()))
            || !ui::search::engine_ready()
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
        // shown: an idle desk takes no later pass, and kept it up over the window it opened.
        if !self.palette.is_open() {
            self.finish();
        }
    }

    fn refresh_rows(&mut self) -> bool {
        let Some(sources) = &self.sources else {
            return false;
        };
        let answered = sources.poll();
        let query = self.palette.query().to_owned();
        let generation = ui::search::engine_generation();
        if answered
            || self.shown_query.as_deref() != Some(query.as_str())
            || self.shown_generation != generation
        {
            if self.shown_query.is_some() && self.shown_query.as_deref() != Some(query.as_str()) {
                // "Show all" belongs to the search it was asked in.
                self.expanded.clear();
                self.everything = false;
            }
            self.restart(&query, generation);
            return true;
        }
        // The sources that still owe an answer: the rest of this search's
        // rows, as far as this frame's share of time reaches.
        if self.resume() {
            self.render(&query, generation);
            return true;
        }
        // The card grew or shrank: the groups share what it shows now.
        if self.palette.entry_row_capacity_for(0, self.shown_two_lines) != self.rows_room.cut() {
            self.render(&query, generation);
            return true;
        }
        // A kind chip picked or unpicked since the rows were built: the
        // same search, the rows of the kinds left.
        if self.palette.picked_filters() != self.shown_kinds.as_slice() {
            self.render(&query, generation);
            return true;
        }
        false
    }

    /// Ask the sources of a new search, as far as this frame's share of
    /// time reaches, and show what came in.
    fn restart(&mut self, query: &str, generation: u64) {
        let Some(stream) = self.begin(query) else {
            return;
        };
        self.stream = Some(stream);
        self.render(query, generation);
    }

    /// Open a stream for `query` and let it ask the sources that fit this
    /// frame.
    fn begin(&mut self, query: &str) -> Option<crate::model::SearchStream> {
        let sources = self.sources.as_ref()?;
        let windowed = sources.window.is_some();
        let (mut all, switches): (Vec<&dyn Source>, Option<Switches>) = match &sources.window {
            // A window's search asks what the window holds and what its host adds for it: the
            // app's entries in the session's index, beside the app's own publication.
            Some(window) => (
                std::iter::once(window as &dyn Source)
                    .chain(sources.host.iter().map(|source| source as &dyn Source))
                    .collect(),
                None,
            ),
            None => everywhere(sources, &self.recents),
        };
        if windowed {
            all.retain(|source| sources.preferences.enabled(source.id()));
        }
        if let Some(switches) = &switches {
            all.push(switches);
        }
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
        drop(switches);
        self.order = order;
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
        let (mut all, switches): (Vec<&dyn Source>, Option<Switches>) = match &sources.window {
            Some(window) => (
                std::iter::once(window as &dyn Source)
                    .chain(sources.host.iter().map(|source| source as &dyn Source))
                    .collect(),
                None,
            ),
            None => everywhere(sources, &self.recents),
        };
        if sources.window.is_some() {
            all.retain(|source| sources.preferences.enabled(source.id()));
        }
        if let Some(switches) = &switches {
            all.push(switches);
        }
        let query = self.palette.query().to_owned();
        let now = sources.now;
        let usage = &self.usage;
        let workers = &self.workers;
        let asked = stream.resume(&all, workers, &|hit: &Hit| usage.strength(&query, hit, now));
        drop(all);
        drop(switches);
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

    fn render(&mut self, query: &str, generation: u64) {
        let Some(stream) = &self.stream else {
            return;
        };
        // The chips are the kinds this search found, so they stay offered
        // while one is picked: narrowing the rows must never take away the
        // chip that would widen them again.
        let found = stream.groups();
        self.palette.set_host_facets(kind_facets(&found));
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
        // A large index answers typed text only once the engine's code is
        // in, and the slower sources answer in later frames: while either
        // is still coming, an empty list says it is searching rather than
        // that nothing matched.
        self.palette
            .set_searching(!ui::search::engine_ready() || (rows.is_empty() && pending));
        self.rows = rows;
        self.shown_query = Some(query.to_owned());
        self.shown_generation = generation;
        self.rows_room = match self.rows_room {
            RowRoom::Guess(_) => RowRoom::Guess(capacity),
            RowRoom::Room(_) => RowRoom::Room(capacity),
        };
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
        self.render(&query, ui::search::engine_generation());
        self.palette.set_cursor(cursor);
        // Choosing a row closes a command palette; this one only grew.
        ui::widget::Widget::set_focus(&mut self.palette, true);
    }

    /// List every result the search found, keeping the query and the
    /// selected row where they are: the card scrolls the rows it has no
    /// room for.
    fn show_everything(&mut self) {
        self.everything = true;
        let cursor = self.palette.cursor();
        let query = self.palette.query().to_owned();
        self.kept_choice = None;
        self.render(&query, ui::search::engine_generation());
        self.palette.set_cursor(cursor);
        ui::widget::Widget::set_focus(&mut self.palette, true);
    }

    /// Follow the palette after it handled input: take a chosen row, and
    /// notice that the palette closed itself (a choice or Escape). Returns
    /// whether search closed; the caller then [`refresh`](Self::refresh)es
    /// an open palette for its edited query.
    pub fn observe(&mut self) -> bool {
        if let Some(outcome) = self.palette.take_outcome() {
            let more = more_row(&outcome.id);
            if let Some(more) = more {
                self.expand(more);
            } else if outcome.id == ALL_RESULTS_ID {
                self.show_everything();
            } else if outcome.id == ALL_APPS_ID {
                // The catalogue of every app: the desktop lists them all,
                // by name, as its empty desk's launchpad does.
                self.choice = Some(Target::Command {
                    id: crate::model::ALL_APPS_COMMAND.into(),
                });
                self.palette.close();
            } else if outcome.argument.is_none()
                && self.shown_query.as_deref() != Some(self.palette.query())
            {
                // Enter came in the same batch of input as the keys before it, ahead of the frame
                // that answers them: the rows on screen are an earlier query's, and the row it
                // named is one of those ("canvas" and Enter started a call, the top row of the
                // empty search). Enter waits for the typed query's answer and takes its top row. A
                // row's own line (an action's argument) is typed in the field after its query, so
                // an Enter that carries one is never this.
                self.choose_top_of_typed_query();
            } else if let Some(hit) = self.hit(&outcome.id) {
                self.choose_hit(hit, outcome.argument, self.palette.chosen_with());
            }
        }
        if self.sources.is_none() || self.palette.is_open() {
            return false;
        }
        self.finish();
        true
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
        // A source switched off or on again: the rows change under the
        // switch, which stays where it is so it can be flipped back.
        if let Target::Prefer { source, enabled } = &hit.target {
            let (source, enabled) = (source.clone(), *enabled);
            if let Some(sources) = &mut self.sources {
                sources.preferences.set_enabled(&source, enabled);
            }
            self.choice = Some(hit.target);
            let query = self.palette.query().to_owned();
            self.restart(&query, ui::search::engine_generation());
            return;
        }
        self.remember(&hit);
        // Trying any row is the answer to the one that did not happen: the
        // reason it stood under goes with it.
        self.refusal = None;
        let mut target = hit.target;
        // The place the person asked the window to go with the modifier
        // they held as they chose: the row said what it would be.
        if let (Target::Launch { place, .. }, Some(asked)) = (&mut target, held) {
            *place = match asked {
                ActivationModifier::Shift => LaunchPlace::TabHere,
                ActivationModifier::Ctrl => LaunchPlace::Beside,
                ActivationModifier::Alt => LaunchPlace::Focus,
            };
        }
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
        self.recents.record(hit);
        let now = self.sources.as_ref().map_or(0, |sources| sources.now);
        let query = self.palette.query().to_owned();
        self.usage.record(&query, hit, now);
    }

    /// Choose the shown row `index` ([`Self::results`]) as a person
    /// choosing it does, with `argument` the line its action asks for:
    /// what a caller of the desktop's published search runs. An app's own
    /// action row ([`Target::Action`]) runs only from its window, through
    /// the host's action door. Returns whether there was such a row.
    pub fn choose(&mut self, index: usize, argument: Option<String>) -> bool {
        let Some(hit) = self.rows.get(index).cloned() else {
            return false;
        };
        self.choose_hit(hit, argument, self.palette.held_modifier());
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
    let names = all
        .iter()
        .filter(|source| crate::preferences::switchable(source.id()))
        .map(|source| (source.id().to_owned(), source.name()))
        .collect();
    Some(Switches::new(names, &sources.preferences))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod early_enter;
    use ui::search::AppScope;
    use ui::shortcut_table::{ShortcutAction, ShortcutConfig};
    use ui::winit::keyboard::ModifiersState;

    pub(super) fn data() -> SearchData {
        SearchData {
            applications: vec![
                AppEntry {
                    id: "so.tilo.sheets".into(),
                    title: "Sheets".into(),
                    category: Some("Office".into()),
                    description: None,
                },
                AppEntry {
                    id: "so.tilo.notes".into(),
                    title: "Notes".into(),
                    category: None,
                    description: None,
                },
            ],
            windows: vec![WindowEntry {
                tab: 3,
                title: "Notes".into(),
                app_title: "Notes".into(),
                app: Some("so.tilo.notes".into()),
                place: vec!["Workspace".into(), "Main".into()],
            }],
            scope: Scope::Workspace,
            ..SearchData::default()
        }
    }

    /// Opened before anything is typed, the card names each group it holds:
    /// "Apps" over the applications, "Windows" over the open windows. A
    /// person never reads a bare list, and the field says what those groups
    /// are ("Search apps, windows, documents and actions…").
    #[test]
    fn the_empty_card_names_its_groups() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        assert_eq!(search.query(), "", "nothing typed yet");
        let groups: Vec<&str> = search
            .palette()
            .visible_entries()
            .iter()
            .map(|entry| {
                entry
                    .group
                    .as_deref()
                    .expect("every row is under a named group")
            })
            .collect();
        assert!(groups.contains(&"Apps"), "{groups:?}");
        assert!(groups.contains(&"Windows"), "{groups:?}");
        assert_eq!(
            crate::i18n::t("search-placeholder"),
            "Search apps, windows, documents and actions\u{2026}",
            "the empty field names those groups"
        );
    }

    /// Someone is here: the empty palette leads with them and the call
    /// they could join, and choosing that row joins it.
    #[test]
    fn the_empty_palette_leads_with_who_is_here() {
        use crate::providers::happening::{CallHere, Happening, PersonHere, WorkspaceHere};
        let person = PersonHere {
            id: "ada".into(),
            name: "Ada".into(),
            colour: (200, 100, 50),
            agent: false,
            away: false,
            tab: None,
        };
        let here = WorkspaceHere {
            id: 1,
            workspace: "workspace".into(),
            name: "Studio".into(),
            active: true,
            people: vec![person.clone()],
            windows: Vec::new(),
            calls: vec![CallHere {
                channel: "c_voice".into(),
                name: String::new(),
                people: vec![person],
            }],
        };
        let mut data = data();
        data.sources
            .push(Held::new(Happening::new(false, vec![here])));
        let mut search = DesktopSearch::new();
        search.open(data);
        settled(&mut search);
        let entries = search.palette().visible_entries();
        assert_eq!(
            entries[0].group.as_deref(),
            Some("Happening now"),
            "who is here leads the empty palette"
        );
        assert_eq!(entries[0].label, "Studio");
        assert_eq!(entries[0].faces.len(), 1, "their face is worn");
        let call = entries
            .iter()
            .find(|entry| entry.label == "Call")
            .expect("the call to join");
        assert_eq!(call.outcome.as_deref(), Some("Join"));
        let at = entries
            .iter()
            .position(|entry| entry.id == call.id)
            .unwrap();
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing a row closes search");
        let Some(Target::Invoke(invoke)) = search.take_choice() else {
            panic!("the call is joined by asking Chat");
        };
        assert_eq!(invoke.app, "so.tilo.chat");
        assert_eq!(invoke.address.action.as_str(), "join-call");
    }

    /// Tasks' one-line "New task", as the session publishes the app's actions: `title` is the
    /// entry parameter the typed line fills.
    fn captures() -> Vec<PublishedActions> {
        let capture = tilo_actions::ActionSpec::prompted_param(
            "new_task",
            "New task",
            tilo_actions::ActionParam::required(
                "title",
                tilo_actions::ParamType::Text,
                "What to do",
            )
            .taking_entry(),
            "Call the landlord fri 10am",
            tilo_actions::ExecutionLocus::ClientLocal,
            |text| tilo_actions::EncodedAction {
                type_id: "test.action".into(),
                payload: text.as_bytes().to_vec(),
            },
        );
        let resource =
            tilo_actions::ResourceSpec::new("tasks", "tl.Tasks.task", "Tasks").with_action(capture);
        vec![PublishedActions::of("so.tilo.lang.tasks", "", &[resource])]
    }

    /// A person presses the palette key, types what they want done, and Enter: the row is the
    /// way to write it down, and choosing it runs it with their words — no action opened, no
    /// field to fill in again.
    #[test]
    fn a_capture_chosen_from_the_palette_runs_with_the_line_typed() {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: captures(),
            scope: Scope::Workspace,
            ..data()
        });
        settled(&mut search);
        let line = "Call the landlord fri 10am";
        search.set_query(line);
        settled(&mut search);
        let at = {
            let entries = search.palette().visible_entries();
            let at = entries
                .iter()
                .position(|entry| entry.label == "New task")
                .unwrap_or_else(|| {
                    panic!(
                        "the capture answers the sentence: {:?}",
                        entries.iter().map(|entry| &entry.label).collect::<Vec<_>>()
                    )
                });
            assert!(
                entries[at].argument.is_none(),
                "the row carries the line, so Enter runs it"
            );
            at
        };
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing a capture closes search");
        let Some(Target::Invoke(invoke)) = search.take_choice() else {
            panic!("the capture runs the app's own action");
        };
        assert_eq!(invoke.app, "so.tilo.lang.tasks");
        assert_eq!(invoke.address.action.as_str(), "new_task");
        assert_eq!(
            invoke.arguments.get("title"),
            Some(&serde_json::Value::String(line.to_owned())),
            "the task is made from the words the person typed"
        );
    }

    /// With nothing typed the palette lists what is useful now, in that
    /// order: who is working here, what the person chose last, then the
    /// apps and windows it opens. What is left of the workspace (leaving it
    /// on this computer) is something a person goes looking for, never the
    /// default list.
    #[test]
    fn the_empty_palette_lists_what_is_now_before_the_ways_into_everything() {
        use crate::providers::happening::{Happening, PersonHere, WorkspaceHere};
        use crate::providers::people::{People, PersonEntry};
        let here = WorkspaceHere {
            id: 1,
            workspace: "workspace".into(),
            name: "Studio".into(),
            active: true,
            people: vec![PersonHere {
                id: "ada".into(),
                name: "Ada".into(),
                colour: (200, 100, 50),
                agent: false,
                away: false,
                tab: None,
            }],
            windows: Vec::new(),
            calls: Vec::new(),
        };
        let mut data = data();
        data.commands.push(CommandSpec {
            id: "session-host-leave-workspace".into(),
            title: "Leave workspace".into(),
            subtitle: None,
            icon: Some(ui::icons::Icon::ArrowUpRight),
            unasked: false,
        });
        data.sources
            .push(Held::new(Happening::new(false, vec![here])));
        data.sources.push(Held::new(People::new(
            "workspace",
            vec![PersonEntry {
                id: "mara".into(),
                name: "Mara Lind".into(),
                activity: Some("Sheets".into()),
            }],
        )));
        let mut search = DesktopSearch::new();
        search.open(data);
        settled(&mut search);
        // The row the person chose last stands in Recent.
        let sheets = search
            .results()
            .iter()
            .find(|hit| hit.title == "Sheets")
            .cloned()
            .expect("the app is listed");
        search.recents.record(&sheets);
        search.restart("", ui::search::engine_generation());
        settled(&mut search);

        let entries = search.palette().visible_entries();
        let mut groups: Vec<&str> = Vec::new();
        for entry in &entries {
            if let Some(group) = entry.group.as_deref()
                && groups.last() != Some(&group)
            {
                groups.push(group);
            }
        }
        assert_eq!(
            groups,
            ["Happening now", "People", "Recent", "Apps", "Windows"]
        );
        assert!(
            entries.iter().all(|entry| entry.label != "Leave workspace"),
            "no workspace housekeeping is a default: {entries:?}"
        );
    }

    /// A place the person was in lately: what the empty card continues.
    fn continue_source(
        recent: &crate::providers::places::RecentPlaces,
    ) -> crate::providers::places::Continue {
        crate::providers::places::Continue::new(recent, "workspace", vec![9], 100, |_| {
            "Files".into()
        })
    }

    /// Nothing typed starts where the person was: the place their window is
    /// in leads the card, above the apps and the windows, and no budget cuts
    /// it away however crowded the card gets.
    #[test]
    fn the_empty_palette_continues_where_the_person_was() {
        use crate::providers::places::{Place, RecentPlaces};
        let mut recent = RecentPlaces::default();
        recent.record(Place {
            app: "so.tilo.file-explorer".into(),
            workspace: "workspace".into(),
            tab: 9,
            label: "Home".into(),
            value: String::new(),
            anchor: None,
            at: 3,
            device: None,
        });
        let mut data = data();
        data.sources.push(Held::new(continue_source(&recent)));
        let mut search = DesktopSearch::new();
        search.open(data);
        settled(&mut search);
        let entries = search.palette().visible_entries();
        assert_eq!(
            entries.first().map(|entry| entry.label.as_str()),
            Some("Home"),
            "the place leads the empty card: {entries:?}"
        );
        assert_eq!(entries[0].group.as_deref(), Some("Continue"));
        // A card too small for everything it has makes way with what is
        // least likely wanted then, never with where the person was.
        search.set_room(400.0, 200.0);
        search.restart("", ui::search::engine_generation());
        settled(&mut search);
        let entries = search.palette().visible_entries();
        assert!(
            entries.iter().any(|entry| entry.id == ALL_RESULTS_ID),
            "the small card leaves a group to the all-results row: {entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.label == "Home" && entry.group.as_deref() == Some("Continue")),
            "the place stays on a crowded card: {entries:?}"
        );
    }

    /// An app's hit says what it is: the row it shaped and the preview of
    /// what it holds reach the palette entry unchanged, and choosing it
    /// opens the item it names.
    struct Mail;

    impl ui::search::SearchProvider for Mail {
        fn name(&self) -> &'static str {
            "Mail"
        }
        fn search(&self, query: &str) -> Vec<ui::search::SearchResult> {
            query
                .to_lowercase()
                .contains("invoice")
                .then(|| {
                    ui::search::SearchResult::for_action(
                        "Mail",
                        "Invoice",
                        tilo_actions::ActionAddress::new("m1", "open"),
                    )
                    .with_row(
                        ui::search::SearchRow::message("Ada", "Invoice")
                            .snippet("Attached is March")
                            .unread(true),
                    )
                    .with_preview(
                        ui::search::SearchPreview::titled("Invoice").body("Attached is March"),
                    )
                })
                .into_iter()
                .collect()
        }
    }

    #[test]
    fn a_hit_is_drawn_as_the_row_its_app_gave_it() {
        let mut message = ResourceSpec::new("m1", "mail.message", "Invoice");
        message
            .actions
            .push(tilo_actions::ActionSpec::immediate_encoded(
                "open",
                "Open",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.open".into(),
                    payload: Vec::new(),
                },
            ));
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            resources: vec![message],
            providers: vec![Box::new(Mail)],
            ..data()
        });
        settled(&mut search);
        search.set_query("invoice");
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let at = entries
            .iter()
            .position(|entry| entry.label == "Invoice")
            .unwrap_or_else(|| panic!("the mail is found: {entries:?}"));
        assert_eq!(
            entries[at].row,
            Some(
                ui::search::SearchRow::message("Ada", "Invoice")
                    .snippet("Attached is March")
                    .unread(true)
            ),
            "the app's own row reaches the palette"
        );
        assert!(
            entries[at].preview.is_some(),
            "and what the mail holds is under the list"
        );
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing a row closes search");
        let Some(Target::Action(action)) = search.take_choice() else {
            panic!("the mail opens through its app's open");
        };
        assert_eq!(
            action.address(),
            tilo_actions::ActionAddress::new("m1", "open")
        );
    }

    /// Under the apps the empty palette offers every one of them: the
    /// catalogue, as the empty desk's launchpad opens it.
    #[test]
    fn every_app_is_one_row_under_the_apps() {
        let mut full = data();
        // More apps than the card lists unasked, as a full fleet has.
        full.applications.extend((0..40).map(|i| AppEntry {
            id: format!("so.tilo.app{i}"),
            title: format!("App {i}"),
            category: None,
            description: None,
        }));
        let mut search = DesktopSearch::new();
        search.open(full);
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let last = entries
            .iter()
            .rfind(|entry| entry.group.as_deref() == Some("Apps"))
            .expect("the apps' group");
        assert_eq!(last.id, ALL_APPS_ID);
        assert_eq!(last.label, "All apps");
        assert!(
            entries
                .iter()
                .filter(|entry| entry.group.as_deref() == Some("Apps"))
                .all(|entry| !entry.id.starts_with(MORE_PREFIX)),
            "one way to every app, not a \"Show all\" beside it: {entries:?}"
        );
        let at = entries
            .iter()
            .position(|entry| entry.id == ALL_APPS_ID)
            .unwrap();
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing it closes search");
        assert_eq!(
            search.take_choice(),
            Some(Target::Command {
                id: crate::model::ALL_APPS_COMMAND.into()
            })
        );

        // The catalogue itself is every app: it does not offer itself again.
        let mut catalogue = data();
        catalogue.scope = Scope::WindowGroup(None);
        catalogue.windows.clear();
        catalogue.commands.clear();
        catalogue.resources.clear();
        catalogue.sources.clear();
        let mut search = DesktopSearch::new();
        search.open(catalogue);
        settled(&mut search);
        let entries = search.palette().visible_entries();
        assert!(
            entries.iter().any(|entry| entry.label == "Sheets"),
            "the catalogue lists the apps: {entries:?}"
        );
        assert!(
            entries.iter().all(|entry| entry.id != ALL_APPS_ID),
            "and offers no second way into itself"
        );
    }

    /// Escape clears what was typed before it closes the search: a slip of
    /// the finger costs the query, not the search. A second Escape on the
    /// empty field closes it, as the launcher's did.
    #[test]
    fn escape_clears_what_was_typed_before_it_closes() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("notes");
        settled(&mut search);
        assert!(search.escape(), "the desk's search clears its query");
        assert!(search.is_open(), "the first Escape keeps search open");
        assert_eq!(search.query(), "", "…and clears the query");
        settled(&mut search);
        assert!(
            search.results().iter().any(|hit| hit.title == "Sheets"),
            "the empty field lists everything again"
        );
        assert!(search.escape());
        assert!(!search.is_open(), "Escape on the empty field closes");
        assert!(
            !search.escape(),
            "and Escape on a closed search is not the desk's to handle"
        );
    }

    #[test]
    fn enter_on_a_typed_app_name_chooses_its_launch() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("sheets");
        settled(&mut search);
        assert_eq!(search.results()[0].title, "Sheets");
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing a row closes search");
        assert_eq!(
            search.take_choice(),
            Some(Target::Launch {
                app: "so.tilo.sheets".into(),
                place: LaunchPlace::Focus,
            })
        );
        assert!(!search.is_open());
    }

    /// An application's row says what a modifier held over it does with the
    /// window, and choosing it with that modifier held opens the app there:
    /// a tab of the window in front, or a window beside it.
    #[test]
    fn a_modifier_held_over_a_row_places_the_window_it_names() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("sheets");
        settled(&mut search);
        let hints: Vec<(ui::search::ActivationModifier, String)> = search
            .palette()
            .visible_entries()
            .iter()
            .find(|entry| entry.label == "Sheets")
            .expect("the app's row")
            .alternates
            .iter()
            .map(|alternate| (alternate.modifier, alternate.label.clone()))
            .collect();
        assert_eq!(
            hints,
            vec![
                (
                    ui::search::ActivationModifier::Shift,
                    "Opens as a tab here".to_owned()
                ),
                (
                    ui::search::ActivationModifier::Ctrl,
                    "Opens a window beside".to_owned()
                ),
            ],
            "the row says what each modifier does before one is held"
        );
        for (held, asked) in [
            (ModifiersState::SHIFT, LaunchPlace::TabHere),
            (ModifiersState::CONTROL, LaunchPlace::Beside),
            (ModifiersState::empty(), LaunchPlace::Focus),
        ] {
            let mut search = DesktopSearch::new();
            search.open(data());
            settled(&mut search);
            search.set_query("sheets");
            settled(&mut search);
            search.palette_mut().set_held_modifiers(held);
            search.palette_mut().activate_selected();
            // Let go before the desk carries the choice out: what was held then still counts.
            search
                .palette_mut()
                .set_held_modifiers(ModifiersState::empty());
            assert!(search.observe(), "choosing a row closes search");
            assert_eq!(
                search.take_choice(),
                Some(Target::Launch {
                    app: "so.tilo.sheets".into(),
                    place: asked,
                }),
                "the modifier held as the row was chosen decides where the app opens"
            );
        }
    }

    /// The card offers a chip per kind it found, and picking one leaves the
    /// rows of that kind: the window rows go while "Apps" is picked, and the
    /// chip stays offered so the person can widen them again.
    #[test]
    fn the_kind_chips_narrow_the_rows_to_the_one_picked() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        let chips: Vec<(String, String)> = search
            .palette()
            .facets()
            .iter()
            .flat_map(|facet| {
                facet
                    .values
                    .iter()
                    .map(|value| (value.value.clone(), value.label.clone()))
            })
            .collect();
        assert!(
            chips.contains(&("Apps".to_owned(), "Apps".to_owned())),
            "every app is under the Apps chip: {chips:?}"
        );
        assert!(
            chips.contains(&("Windows".to_owned(), "Windows".to_owned())),
            "the open window is under the Windows chip: {chips:?}"
        );
        let kinds = |search: &DesktopSearch| -> Vec<String> {
            search
                .palette()
                .visible_entries()
                .iter()
                .filter_map(|entry| entry.group.clone())
                .collect()
        };
        assert!(kinds(&search).iter().any(|group| group == "Windows"));
        search
            .palette_mut()
            .toggle_facet(&ui::search::FacetFilter::new("kind", "Apps"));
        settled(&mut search);
        let shown = kinds(&search);
        assert!(!shown.is_empty(), "the apps stay: {shown:?}");
        assert!(
            shown.iter().all(|group| group == "Apps"),
            "picking Apps leaves only its rows: {shown:?}"
        );
        assert!(
            search
                .palette()
                .facets()
                .iter()
                .any(|facet| facet.values.iter().any(|value| value.value == "Windows")),
            "the chip that widens the rows again stays offered"
        );
        search
            .palette_mut()
            .toggle_facet(&ui::search::FacetFilter::new("kind", "Apps"));
        settled(&mut search);
        assert!(
            kinds(&search).iter().any(|group| group == "Windows"),
            "unpicking the chip brings every kind back"
        );
    }

    /// The desktop keymap's binding of `action`: the chord a publisher
    /// declares, and the way the palette paints that chord. A switch's keys
    /// come from the shortcut table, never from a chord written here.
    fn bound_keys(action: ShortcutAction) -> (String, String) {
        let chord = ShortcutConfig::default()
            .bindings
            .get(&action)
            .unwrap_or_else(|| panic!("{action:?} is bound"))
            .clone();
        let painted =
            ui::keycaps::active().chord(chord.modifiers.iter().map(String::as_str), &chord.key);
        (chord.display(), painted)
    }

    /// The shell's published actions: a switch showing its state and the keys
    /// the keymap binds it to, and an action with nothing to act on now.
    fn published() -> Vec<PublishedActions> {
        let action = |id: &str, label: &str| {
            tilo_actions::ActionSpec::immediate_encoded(
                id,
                label,
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "shell.action".into(),
                    payload: Vec::new(),
                },
            )
        };
        let (declared, _) = bound_keys(ShortcutAction::ToggleFullscreen);
        let (resource_id, action_id) = ShortcutAction::ToggleFullscreen
            .action()
            .expect("the full screen toggle is a published action");
        let mut resource = ResourceSpec::new(resource_id, resource_id, "Actions");
        resource
            .actions
            .push(
                action(action_id, "Full screen").with_state(tilo_actions::ActionState {
                    badge: Some("Off".into()),
                    toggles: true,
                    shortcut: Some(declared),
                }),
            );
        resource.actions.push(
            action("projection.close", "Stop projecting")
                .unavailable("nothing_to_act_on", "Nothing is projected"),
        );
        vec![PublishedActions::of("shell", "workspace", &[resource])]
    }

    #[test]
    fn an_apps_name_opens_the_app_before_any_action_named_the_same() {
        // An action can carry an app's name (the empty desk's cards did):
        // a person typing a name wants the app, and Enter on the top row
        // must start it.
        let mut desk = ResourceSpec::new("desktop:desk", "desktop", "Desk");
        desk.actions
            .push(tilo_actions::ActionSpec::immediate_encoded(
                "notes",
                "Notes",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "desk.action".into(),
                    payload: Vec::new(),
                },
            ));
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: vec![PublishedActions::of("shell", "workspace", &[desk])],
            ..data()
        });
        settled(&mut search);
        search.set_query("notes");
        settled(&mut search);
        let first = search.results()[0].clone();
        assert_eq!(
            (first.provider.as_str(), first.title.as_str()),
            ("applications", "Notes"),
            "the app leads the action of the same name"
        );
        assert_eq!(
            first.target,
            Target::Launch {
                app: "so.tilo.notes".into(),
                place: LaunchPlace::Focus,
            }
        );
    }

    #[test]
    fn a_published_switch_shows_its_state_and_stays_open_on_it() {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: published(),
            ..data()
        });
        settled(&mut search);
        search.set_query("full scr");
        settled(&mut search);
        let hit = &search.results()[0];
        assert_eq!(hit.title, "Full screen");
        let (_, painted) = bound_keys(ShortcutAction::ToggleFullscreen);
        let entries = search.palette().visible_entries();
        assert_eq!(entries[0].badge.as_deref(), Some("Off"));
        assert_eq!(
            entries[0].keybinding_label(ui::PlatformConventions::new(ui::Platform::Linux)),
            Some(painted),
            "the keymap's chord, painted as the platform's keycaps"
        );
        assert!(
            entries[0].keybinding.is_none(),
            "a chord is no free-text keybinding"
        );
        drop(entries);
        search.palette_mut().activate_selected();
        assert!(
            !search.observe(),
            "a switch keeps search open on its new state"
        );
        let Some(Target::Invoke(invoke)) = search.take_choice() else {
            panic!("the switch runs by address at its provider");
        };
        assert_eq!(
            (invoke.app.as_str(), invoke.workspace.as_str()),
            ("shell", "workspace")
        );
        let (resource_id, action_id) = ShortcutAction::ToggleFullscreen
            .action()
            .expect("the full screen toggle is a published action");
        assert_eq!(
            invoke.address,
            tilo_actions::ActionAddress::new(resource_id, action_id)
        );
        assert!(search.is_open());
    }

    #[test]
    fn a_published_action_with_nothing_to_act_on_answers_only_its_name() {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: published(),
            ..data()
        });
        settled(&mut search);
        search.set_query("stop proj");
        settled(&mut search);
        assert!(
            search
                .results()
                .iter()
                .all(|hit| hit.title != "Stop projecting"),
            "it takes no row from one that would do something"
        );
        search.set_query("Stop projecting");
        settled(&mut search);
        assert!(
            search
                .results()
                .iter()
                .any(|hit| hit.title == "Stop projecting"),
            "named exactly, it is offered, and running it says why not"
        );
    }

    /// A host's own source, named as a person knows it: the notes a notes
    /// app holds.
    struct Notes;

    impl Source for Notes {
        fn id(&self) -> &'static str {
            "notes"
        }

        fn name(&self) -> String {
            "Notes".into()
        }

        fn query(&self, needle: &str) -> Vec<Hit> {
            if needle.trim().is_empty() {
                return Vec::new();
            }
            vec![Hit {
                row: None,
                preview: None,
                faces: Vec::new(),
                outcome: None,
                kind: ResourceKind::Note,
                provider: self.id().into(),
                id: "groceries".into(),
                title: format!("Groceries for {needle}"),
                subtitle: None,
                facts: Vec::new(),
                score: ui::search::system::MAX_PROVIDER_SCORE,
                icon: None,
                target: Target::Window { tab: 7, app: None },
            }]
        }
    }

    /// A search over the desktop and what one host adds.
    fn with_notes(scope: Scope) -> SearchData {
        SearchData {
            sources: vec![Held::new(Notes)],
            scope,
            ..data()
        }
    }

    #[test]
    fn switching_a_source_off_from_its_row_takes_its_rows_away_and_stays_open() {
        let mut search = DesktopSearch::new();
        search.open(with_notes(Scope::Workspace));
        settled(&mut search);
        search.set_query("groceries tomatoes");
        settled(&mut search);
        assert!(
            search.results().iter().any(|hit| hit.provider == "notes"),
            "{:?}",
            search.results()
        );

        search.set_query("disable notes");
        settled(&mut search);
        let at = search
            .palette()
            .visible_entries()
            .iter()
            .position(|entry| entry.label == "Disable Notes search")
            .expect("the source's switch is offered");
        let entries = search.palette().visible_entries();
        assert_eq!(entries[at].badge.as_deref(), Some("ON"));
        drop(entries);
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();

        assert!(
            !search.observe(),
            "a switch keeps search open on what it switched"
        );
        assert!(search.is_open());
        assert_eq!(
            search.take_choice(),
            Some(Target::Prefer {
                source: "notes".into(),
                enabled: false
            }),
            "the host keeps the person's choice"
        );

        // The switch now offers to switch it back on...
        let switched = |search: &DesktopSearch| {
            search
                .results()
                .iter()
                .find(|hit| matches!(hit.target, Target::Prefer { .. }))
                .map(|hit| hit.title.clone())
        };
        settled(&mut search);
        assert_eq!(switched(&search).as_deref(), Some("Enable Notes search"));
        // ...and what it switched off is gone.
        search.set_query("groceries tomatoes");
        settled(&mut search);
        assert!(
            search.results().iter().all(|hit| hit.provider != "notes"),
            "{:?}",
            search.results()
        );
        // The desktop's own rows stay, whatever was switched: the app.
        search.set_query("sheets");
        settled(&mut search);
        assert!(search.results().iter().any(|hit| hit.title == "Sheets"));
    }

    #[test]
    fn the_actions_stay_whatever_is_switched_off() {
        let mut search = DesktopSearch::new();
        search.open(with_notes(Scope::Workspace));
        settled(&mut search);
        search.set_query("disable");
        settled(&mut search);
        assert!(
            !search.results().iter().any(|hit| matches!(
                &hit.target,
                Target::Prefer { source, .. } if source == "actions"
            )),
            "the actions are how anything is done from search: {:?}",
            search.results()
        );
    }

    #[test]
    fn a_search_over_the_workspace_in_front_offers_to_widen_it() {
        let mut search = DesktopSearch::new();
        search.open(with_notes(Scope::Workspace));
        settled(&mut search);
        assert_eq!(search.palette().scope_token(), Some("Workspace"));
        assert_eq!(
            search.palette().scope_token_action(),
            Some("Search every workspace")
        );

        // Widened, the token is gone: there is nothing left to widen.
        search.close();
        search.open(with_notes(Scope::Everywhere));
        settled(&mut search);
        assert_eq!(search.palette().scope_token(), None);
        assert_eq!(search.palette().scope_token_action(), None);
    }

    /// A search the desktop opens of its own (the apps a new tab may show)
    /// is not narrowed to a workspace, and says nothing about one.
    #[test]
    fn a_search_of_the_desktops_own_names_no_scope() {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            applications: data().applications,
            scope: Scope::WindowGroup(None),
            ..SearchData::default()
        });
        settled(&mut search);
        assert_eq!(search.palette().scope_token(), None);
        assert_eq!(search.palette().scope_token_action(), None);
    }

    #[test]
    fn a_dismissed_palette_chooses_nothing_and_remembers_nothing() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("sheets");
        settled(&mut search);
        search.close();
        assert_eq!(search.take_choice(), None);
        search.open(data());
        settled(&mut search);
        assert!(
            search.results().iter().all(|hit| hit.provider != "recent"),
            "nothing was chosen"
        );
    }

    #[test]
    fn a_chosen_row_leads_the_next_empty_palette() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("sheets");
        settled(&mut search);
        search.palette_mut().activate_selected();
        search.observe();
        search.open(data());
        settled(&mut search);
        assert_eq!(search.results()[0].provider, "recent");
        assert_eq!(search.results()[0].title, "Sheets");
    }

    #[test]
    fn an_action_row_hands_its_address_to_the_host_and_closes() {
        let mut resource = ResourceSpec::new("page-1", "notes.page", "Groceries");
        resource
            .actions
            .push(tilo_actions::ActionSpec::immediate_encoded(
                "duplicate",
                "Duplicate note",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.duplicate".into(),
                    payload: Vec::new(),
                },
            ));
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            resources: vec![resource],
            ..data()
        });
        settled(&mut search);
        search.set_query("duplicate");
        settled(&mut search);
        assert_eq!(search.results()[0].title, "Duplicate note");
        search.palette_mut().activate_selected();
        assert!(search.observe());
        let Some(Target::Action(action)) = search.take_choice() else {
            panic!("the host's action door runs it");
        };
        assert_eq!(
            action.address(),
            tilo_actions::ActionAddress::new("page-1", "duplicate")
        );
    }

    /// The host could not carry out the row the person chose: search opens
    /// again where it stood, over the same query, and the row keeps the
    /// reason under its name instead of the search closing over it.
    #[test]
    fn a_row_that_did_not_happen_keeps_its_reason_and_the_search_stays_open() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        search.set_query("notes");
        settled(&mut search);
        let target = search
            .results()
            .iter()
            .find(|hit| matches!(hit.target, Target::Window { .. }))
            .expect("the open window is a row")
            .target
            .clone();

        search.open(data());
        search.refuse(target.clone(), "its app is gone".to_owned());
        search.set_query("notes");
        settled(&mut search);

        assert!(search.is_open(), "search stays open on the reason");
        assert_eq!(search.query(), "notes", "over the query it stood at");
        assert_eq!(
            search.refusal_for(&target),
            Some("its app is gone"),
            "the row that did not happen is the one that says why"
        );
        let entries = search.palette().visible_entries();
        assert!(
            entries
                .iter()
                .any(|entry| entry.description.as_deref() == Some("its app is gone")),
            "the row says why it did not happen: {:?}",
            entries
                .iter()
                .map(|entry| (&entry.label, &entry.description))
                .collect::<Vec<_>>()
        );
    }

    /// Typing runs the query once: the frame that takes the new text asks
    /// the sources, the frames after it fill in what came back, and a
    /// settled search asks nothing again.
    #[test]
    fn typing_refreshes_the_rows_once_per_query() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        assert!(!search.refresh(), "the empty query is already shown");
        search.palette_mut().set_query("not");
        assert!(search.refresh(), "the new text runs the query");
        // A source answers on a worker: its row is in the frame that
        // collects it, which never waits on any other source.
        settled(&mut search);
        assert!(
            search.results().iter().any(|hit| hit.title == "Notes"),
            "the app named by the text is found"
        );
        assert!(
            !search.refresh(),
            "nothing is asked once every source answered"
        );
    }

    /// An app whose rows for a query come after it is asked, as a framework
    /// app's do: it answers on its next frame.
    struct LateApp {
        arrived: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl ui::search::SearchProvider for LateApp {
        fn name(&self) -> &'static str {
            "Notes"
        }
        fn search(&self, _: &str) -> Vec<ui::search::SearchResult> {
            Vec::new()
        }
        fn search_page(
            &self,
            query: &ui::search::SearchQuery,
            _: Option<&ui::search::SearchCursor>,
            _: usize,
            _: usize,
        ) -> ui::search::SearchPage {
            if !self.arrived.get() {
                let mut page = ui::search::SearchPage::empty();
                page.pending = Some(ui::search::PendingToken::new(1));
                return page;
            }
            let row = ui::search::SearchResult::for_action(
                "Notes",
                format!("{} from the meeting", query.text()),
                tilo_actions::ActionAddress::new("meeting", "open"),
            );
            ui::search::SearchPage::of(vec![row], None)
        }
        fn poll_page(&self, _: &ui::search::PendingToken) -> ui::search::PagePoll {
            if self.arrived.get() {
                ui::search::PagePoll::Answered(ui::search::SearchPage::empty())
            } else {
                ui::search::PagePoll::Pending
            }
        }
    }

    /// Search over `base` with an app whose answer is still coming: what
    /// the app publishes, and the window the person searches it from.
    fn with_a_late_app(base: SearchData) -> (DesktopSearch, std::rc::Rc<std::cell::Cell<bool>>) {
        let arrived = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut meeting = ResourceSpec::new("meeting", "notes.page", "Meeting");
        meeting
            .actions
            .push(tilo_actions::ActionSpec::immediate_encoded(
                "open",
                "Open",
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.open".into(),
                    payload: Vec::new(),
                },
            ));
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            resources: vec![meeting],
            providers: vec![Box::new(LateApp {
                arrived: std::rc::Rc::clone(&arrived),
            })],
            ..base
        });
        settled(&mut search);
        (search, arrived)
    }

    /// A window's own search, listing its one place unasked.
    struct Places;

    impl ui::search::SearchProvider for Places {
        fn name(&self) -> &'static str {
            "Places"
        }
        fn search(&self, _: &str) -> Vec<ui::search::SearchResult> {
            vec![ui::search::SearchResult::new(
                "Places",
                "Home",
                "place:home",
            )]
        }
        fn open_action(&self) -> Option<tilo_actions::ActionAddress> {
            Some(tilo_actions::ActionAddress::new("notes.notebook", "open"))
        }
    }

    /// A window's search lists what the window holds, then what can be
    /// done in it, before anything is typed: the actions are part of it.
    #[test]
    fn a_windows_search_lists_its_resources_then_its_actions() {
        let action = |id: &str, label: &str| {
            tilo_actions::ActionSpec::immediate_encoded(
                id,
                label,
                tilo_actions::ExecutionLocus::ClientLocal,
                tilo_actions::EncodedAction {
                    type_id: "test.action".into(),
                    payload: Vec::new(),
                },
            )
        };
        // The framework's `open`, which shows the item at a hit's anchor.
        let open = tilo_actions::ActionSpec::typed_operation(
            "open",
            "Open",
            "Item to open",
            tilo_actions::OperationKind::View,
            tilo_actions::remote::APPLICATION_OPEN_TYPE,
            vec![tilo_actions::ActionParam::required(
                "anchor",
                tilo_actions::ParamType::Text,
                "Item to open",
            )],
            |_| tilo_actions::EncodedAction {
                type_id: tilo_actions::remote::APPLICATION_OPEN_TYPE.into(),
                payload: Vec::new(),
            },
        );
        let mut notebook = ResourceSpec::new("notes.notebook", "notes.notebook", "Notebook")
            .with_action(open)
            .with_action(action("new-note", "New note"));
        notebook.children = vec![ResourceSpec::new("page:1", "notes.page", "Groceries")];
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            resources: vec![notebook],
            providers: vec![Box::new(Places)],
            scope: Scope::InApp(AppScope::front()),
            ..SearchData::default()
        });
        settled(&mut search);
        let rows: Vec<(&str, &ResourceKind)> = search
            .results()
            .iter()
            .map(|hit| (hit.title.as_str(), &hit.kind))
            .collect();
        assert_eq!(
            rows,
            [
                ("Home", &ResourceKind::Custom("Places".into())),
                ("New note", &ResourceKind::Action),
            ]
        );
    }

    /// Draw the frames a host draws for a search until every source has
    /// answered: a source answers on a worker and wakes the desk, so a test
    /// drives the frames those wakes ask for.
    fn settled(search: &mut DesktopSearch) {
        let now = ui::time::now();
        let deadline = now.checked_add(crate::tests::SETTLE).unwrap_or(now);
        loop {
            // The search is settled when no source owes an answer and a
            // frame found nothing to change.
            if !search.refresh() && !search.answering() {
                return;
            }
            assert!(
                ui::time::now() < deadline,
                "the search never settled for {:?}",
                search.query()
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn selected(search: &DesktopSearch) -> Option<(String, String)> {
        let hit = search.hit(search.palette().selected_id()?)?;
        Some((hit.provider, hit.id))
    }

    /// The person arrowed to a row; an app's answer arriving a frame later
    /// re-ranks the list. Enter must still open the row they moved to, not
    /// whatever slid under the cursor.
    #[test]
    fn an_answer_that_arrives_late_keeps_the_row_the_person_moved_to() {
        let (mut search, arrived) = with_a_late_app(data());
        search.set_query("notes");
        settled(&mut search);
        assert!(search.waiting(), "the app's answer is still coming");
        assert!(search.results().len() >= 2, "{:?}", search.results());
        assert!(arrow_down(&mut search));
        let chosen = selected(&search).expect("a row is selected");

        arrived.set(true);
        assert!(search.refresh(), "the answer re-ranks the rows");
        settled(&mut search);
        assert!(
            search
                .results()
                .iter()
                .any(|hit| hit.title == "notes from the meeting"),
            "the late answer is listed: {:?}",
            search.results()
        );
        assert_eq!(selected(&search), Some(chosen));
    }

    /// The person moved to a group's "Show all" row; an app's answer
    /// arriving a frame later adds its group and moves the rows. Enter must
    /// still show all of the group they chose, not open the row that took
    /// its place.
    #[test]
    fn an_answer_that_arrives_late_keeps_the_show_all_row_the_person_moved_to() {
        // More windows and commands for the query than one card shows.
        let (mut search, arrived) = with_a_late_app(SearchData {
            windows: (0..12)
                .map(|tab| WindowEntry {
                    tab,
                    title: format!("Meeting notes {tab}"),
                    app_title: "Notes".into(),
                    app: Some("so.tilo.notes".into()),
                    place: vec!["Workspace".into(), "Main".into()],
                })
                .collect(),
            commands: (0..12)
                .map(|i| CommandSpec {
                    id: format!("host:{i}"),
                    title: format!("Meeting workspace {i}"),
                    subtitle: None,
                    icon: None,
                    unasked: true,
                })
                .collect(),
            ..data()
        });
        search.set_query("meeting");
        settled(&mut search);
        assert!(search.waiting(), "the app's answer is still coming");
        let mut moves = 0;
        while !search
            .palette()
            .selected_id()
            .is_some_and(|id| id.starts_with(MORE_PREFIX))
        {
            assert!(arrow_down(&mut search), "a group was cut short");
            moves += 1;
        }
        assert!(moves > 0, "the person moved to the row");
        let group = |search: &DesktopSearch| {
            let entries = search.palette().visible_entries();
            entries
                .get(search.palette().cursor())
                .and_then(|entry| entry.group.clone())
        };
        let chosen = group(&search).expect("the row names its group");

        arrived.set(true);
        assert!(search.refresh(), "the answer rebuilds the rows");
        settled(&mut search);
        assert!(!search.waiting(), "the app answered");
        assert!(
            search
                .palette()
                .selected_id()
                .is_some_and(|id| id.starts_with(MORE_PREFIX)),
            "{:?} among {:?}",
            search.palette().selected_id(),
            search
                .palette()
                .visible_entries()
                .iter()
                .map(|entry| (entry.id.clone(), entry.group.clone()))
                .collect::<Vec<_>>()
        );
        assert_eq!(group(&search).as_deref(), Some(chosen.as_str()));
    }

    /// An answer to a query re-ranks it, and the frame it re-ranks in has no
    /// row yet: the card on screen still shows the rows of the frame before.
    /// They stay what the palette presses name, so a key pressed in that
    /// frame opens the row the person saw rather than nothing.
    #[test]
    fn the_frame_a_query_is_reranked_in_keeps_the_rows_it_shows() {
        let (mut search, arrived) = with_a_late_app(data());
        search.set_query("notes");
        settled(&mut search);
        let shown = search.results().len();
        let named = search
            .palette()
            .selected_id()
            .expect("a row is chosen")
            .to_owned();
        assert!(shown >= 2, "{:?}", search.results());
        assert!(search.waiting(), "the app's answer is still coming");

        arrived.set(true);
        assert!(search.refresh(), "the answer re-ranks the rows");
        assert_eq!(
            search.results().len(),
            shown,
            "the rows the card shows stay for the frame the query is re-ranked in"
        );
        assert_eq!(
            search.palette().selected_id(),
            Some(named.as_str()),
            "the row the person presses Enter on is still named"
        );
        assert!(
            search
                .palette()
                .visible_entries()
                .iter()
                .any(|entry| entry.id == named),
            "the card lists the row it shows"
        );

        settled(&mut search);
        assert!(
            search
                .results()
                .iter()
                .any(|hit| hit.title == "notes from the meeting"),
            "the late answer is listed once the query is ranked again: {:?}",
            search.results()
        );
    }

    /// Nobody moved the selection: it is the best result, and follows the
    /// best result when an answer arrives.
    #[test]
    fn an_unmoved_selection_follows_the_best_result() {
        let (mut search, arrived) = with_a_late_app(data());
        search.set_query("notes");
        settled(&mut search);
        arrived.set(true);
        assert!(search.refresh());
        settled(&mut search);
        let best = search.results().first().expect("rows");
        assert_eq!(
            selected(&search),
            Some((best.provider.clone(), best.id.clone()))
        );
    }

    /// A row stands as tall as the line the palette draws under its name: a
    /// place it says there, never one that only repeats the name above.
    #[test]
    fn a_row_stands_as_tall_as_the_line_under_its_name() {
        let row = |subtitle: Option<&str>, facts: Vec<String>| Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ResourceKind::Custom(crate::providers::WINDOWS_KIND.into()),
            provider: "windows".into(),
            id: "w".into(),
            title: "Notes".into(),
            subtitle: subtitle.map(str::to_owned),
            facts,
            score: 1,
            icon: None,
            target: Target::Window { tab: 7, app: None },
        };
        assert!(stands_tall(&row(Some("Tilo/Home"), Vec::new())));
        assert!(!stands_tall(&row(None, Vec::new())));
        assert!(
            !stands_tall(&row(Some("Notes"), Vec::new())),
            "a name said twice says nothing under it"
        );
        assert!(
            stands_tall(&row(None, vec!["Tilo/Home".into()])),
            "a fact is drawn under the name, on the row's second line"
        );
    }

    /// Every app of the registry the web runs, a few windows and commands:
    /// more apps than the palette shows at once, as in a browser.
    fn crowded() -> SearchData {
        let applications: Vec<AppEntry> = crate::availability::catalog()
            .iter()
            .filter(|app| crate::availability::runs_on(app.id, &["web"]))
            .map(|app| AppEntry {
                id: app.id.into(),
                title: app.name.into(),
                category: None,
                description: None,
            })
            .collect();
        SearchData {
            applications,
            windows: (0..3)
                .map(|tab| WindowEntry {
                    tab,
                    title: format!("Window {tab}"),
                    app_title: "Notes".into(),
                    app: Some("so.tilo.notes".into()),
                    place: vec!["Workspace".into(), "Main".into()],
                })
                .collect(),
            commands: (0..10)
                .map(|i| CommandSpec {
                    id: format!("host:{i}"),
                    title: format!("Command {i}"),
                    subtitle: None,
                    icon: None,
                    unasked: true,
                })
                .collect(),
            ..SearchData::default()
        }
    }

    /// The groups of the rows `entries`: one header apiece, in the order
    /// they stand.
    fn groups_of<'a>(entries: &'a [&'a CommandEntry]) -> Vec<&'a str> {
        let mut groups: Vec<&str> = Vec::new();
        for entry in entries {
            if let Some(group) = entry.group.as_deref()
                && groups.last() != Some(&group)
            {
                groups.push(group);
            }
        }
        groups
    }

    /// A card with no room for every group offers the rows it leaves out in
    /// one row, and the rows it does keep stand inside it rather than being
    /// cut at its edge.
    #[test]
    fn a_crowded_card_lists_the_rest_of_its_results_in_one_row() {
        let mut search = DesktopSearch::new();
        // Room for what stays once a group makes way, not for every group.
        search.set_room(500.0, 320.0);
        search.open(crowded());
        settled(&mut search);
        let shown = search.palette().visible_entries();
        let entries: Vec<CommandEntry> = shown.iter().map(|entry| (*entry).clone()).collect();
        let groups = groups_of(&shown).len();
        assert!(groups >= 1, "{entries:?}");
        assert!(
            search.palette().entries_fit(&entries, groups),
            "every row the card keeps stands inside it: {entries:?}"
        );
        let rest = entries
            .iter()
            .find(|entry| entry.id == ALL_RESULTS_ID)
            .unwrap_or_else(|| panic!("a crowded card offers the rest in one row: {entries:?}"));
        let count: usize = rest
            .label
            .split_whitespace()
            .find_map(|word| word.parse().ok())
            .unwrap_or_default();
        assert!(
            rest.label.starts_with("Show all ") && count > 0,
            "the row says how many results it lists: {}",
            rest.label
        );
    }

    /// Choosing the row that lists the rest shows every result the search
    /// found, groups and all, and leaves the card to scroll them: nothing
    /// the query found stays out of reach.
    #[test]
    fn the_rest_of_the_results_are_listed_when_their_row_is_chosen() {
        let mut search = DesktopSearch::new();
        search.set_room(500.0, 160.0);
        search.open(crowded());
        settled(&mut search);
        let before = search.results().len();
        let entries = search.palette().visible_entries();
        let at = entries
            .iter()
            .position(|entry| entry.id == ALL_RESULTS_ID)
            .unwrap_or_else(|| panic!("a crowded card offers the rest: {entries:?}"));
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(!search.observe(), "listing the rest keeps search open");
        assert!(search.is_open(), "the card stays up");
        let after = search.results().len();
        assert!(after > before, "{before} rows became {after}");
        assert!(
            search
                .palette()
                .visible_entries()
                .iter()
                .all(|entry| entry.id != ALL_RESULTS_ID),
            "nothing is left over to offer"
        );
    }

    fn arrow_down(search: &mut DesktopSearch) -> bool {
        let before = search.palette().cursor();
        search.palette_mut().handle_list_key(
            &ui::winit::keyboard::Key::Named(ui::winit::keyboard::NamedKey::ArrowDown),
            false,
            false,
        );
        search.palette().cursor() != before
    }

    /// From an empty palette the keyboard reaches every app: the apps listed, then "All apps",
    /// the one row that leads to the rest, which lists every app one arrow press apart.
    #[test]
    fn the_keyboard_reaches_every_app_from_an_empty_palette() {
        let data = crowded();
        let total = data.applications.len();
        assert!(total > 20, "more apps than one screen: {total}");
        let mut search = DesktopSearch::new();
        search.open(data);
        settled(&mut search);
        let header = search
            .palette()
            .visible_entries()
            .iter()
            .find(|entry| entry.group.as_deref() == Some("Apps"))
            .and_then(|entry| entry.group_total);
        assert_eq!(header, Some(total), "the header counts every app");

        let mut on_all_apps = false;
        for _ in 0..(total * 4) {
            let id = search.palette().selected_id().map(str::to_owned);
            assert!(
                !id.as_deref().is_some_and(|id| id.starts_with(MORE_PREFIX)),
                "no second way to every app beside All apps"
            );
            if id.as_deref() == Some(ALL_APPS_ID) {
                on_all_apps = true;
                break;
            }
            if !arrow_down(&mut search) {
                break;
            }
        }
        assert!(on_all_apps, "the keyboard reaches All apps");
        search.palette_mut().activate_selected();
        assert!(search.observe(), "choosing it closes search");
        assert_eq!(
            search.take_choice(),
            Some(Target::Command {
                id: crate::model::ALL_APPS_COMMAND.into()
            })
        );

        // What it opens: the apps alone, every one of them a press apart.
        let mut catalogue = crowded();
        catalogue.scope = Scope::WindowGroup(None);
        let mut search = DesktopSearch::new();
        search.open(catalogue);
        settled(&mut search);
        let mut reached = std::collections::BTreeSet::new();
        for _ in 0..(total * 4) {
            let id = search.palette().selected_id().map(str::to_owned);
            if let Some(id) = id {
                if id.starts_with(MORE_PREFIX) {
                    search.palette_mut().activate_selected();
                    assert!(!search.observe(), "showing all keeps search open");
                    continue;
                }
                if let Some(Target::Launch { app, .. }) = search.hit(&id).map(|hit| hit.target) {
                    reached.insert(app);
                }
            }
            if !arrow_down(&mut search) {
                break;
            }
        }
        assert_eq!(reached.len(), total, "every app is one arrow press away");
    }

    #[test]
    fn a_show_all_row_is_offered_only_for_a_group_cut_short() {
        let mut search = DesktopSearch::new();
        search.open(crowded());
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let more: Vec<_> = entries
            .iter()
            .filter(|entry| entry.id.starts_with(MORE_PREFIX))
            .collect();
        assert!(!more.is_empty(), "the apps were cut short");
        for row in &more {
            let shown = entries
                .iter()
                .filter(|entry| entry.group == row.group && entry.id.starts_with("hit:"))
                .count();
            assert!(shown < row.group_total.unwrap_or_default(), "{}", row.label);
            assert!(
                row.label
                    .contains(&row.group_total.unwrap_or_default().to_string()),
                "it says how many: {}",
                row.label
            );
        }
        // Typing narrows the list; a few rows need no "Show all".
        search.set_query("calendar");
        settled(&mut search);
        assert!(
            search
                .palette()
                .visible_entries()
                .iter()
                .all(|entry| !entry.id.starts_with(MORE_PREFIX))
        );
    }

    /// Choosing a group's "Show all" keeps that group on the card even when
    /// listing it whole no longer fits: the card scrolls what the person
    /// asked for instead of taking it away again.
    #[test]
    fn a_group_asked_for_whole_scrolls_rather_than_making_way() {
        let mut search = DesktopSearch::new();
        // Room for the three groups the empty card holds, not for all their
        // rows.
        search.set_room(400.0, 340.0);
        search.open(SearchData {
            applications: vec![AppEntry {
                id: "so.tilo.notes".into(),
                title: "Notes".into(),
                category: None,
                description: None,
            }],
            windows: vec![WindowEntry {
                tab: 1,
                title: "Notes".into(),
                app_title: "Notes".into(),
                app: Some("so.tilo.notes".into()),
                place: Vec::new(),
            }],
            commands: (0..6)
                .map(|i| CommandSpec {
                    id: format!("host:{i}"),
                    title: format!("Command {i}"),
                    subtitle: None,
                    icon: None,
                    unasked: true,
                })
                .collect(),
            ..SearchData::default()
        });
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let (at, group, total) = entries
            .iter()
            .enumerate()
            .find_map(|(at, entry)| {
                if !entry.id.starts_with(MORE_PREFIX) {
                    return None;
                }
                Some((at, entry.group.clone()?, entry.group_total?))
            })
            .unwrap_or_else(|| panic!("a cramped card offers the rest: {entries:?}"));
        search.palette_mut().set_cursor(at);
        search.palette_mut().activate_selected();
        assert!(!search.observe(), "showing all keeps search open");
        let shown = search.palette().visible_entries();
        let listed = shown
            .iter()
            .filter(|entry| entry.group.as_deref() == Some(group.as_str()))
            .filter(|entry| entry.id.starts_with("hit:"))
            .count();
        assert_eq!(listed, total, "every row of {group} is on the card");
        assert!(
            shown.iter().all(|entry| !(entry.id.starts_with(MORE_PREFIX)
                && entry.group.as_deref() == Some(group.as_str()))),
            "{group} is not offered itself again"
        );
    }

    #[test]
    fn app_rows_wear_their_own_glyph_and_name_without_a_package_id() {
        let mut search = DesktopSearch::new();
        search.open(crowded());
        settled(&mut search);
        let entries = search.palette().visible_entries();
        let apps: Vec<_> = entries
            .iter()
            .filter(|entry| entry.id.starts_with("hit:"))
            .filter_map(|entry| match search.hit(&entry.id)?.target {
                Target::Launch { app, .. } => Some((app, *entry)),
                _ => None,
            })
            .collect();
        assert!(!apps.is_empty());
        for (app, entry) in &apps {
            assert_eq!(
                entry.icon,
                Some(ui::icons::app_glyph(
                    app,
                    crate::availability::category_for(app)
                )),
                "{app}"
            );
            assert!(!entry.label.contains("so.tilo"), "{}", entry.label);
            let description = entry.description.as_deref().unwrap_or_default();
            assert!(!description.contains("so.tilo"), "{app}: {description}");
        }
        let notes = apps
            .iter()
            .find(|(app, _)| app == "so.tilo.notes")
            .expect("Notes leads the empty list");
        assert_eq!(notes.1.label, "Notes");
        assert_ne!(
            notes.1.icon,
            Some(ui::icons::Icon::LayoutGrid),
            "not the generic grid"
        );
        // A window wears its app's glyph, not the search glass.
        let window = entries
            .iter()
            .find(|entry| {
                entry.id.starts_with("hit:")
                    && search
                        .hit(&entry.id)
                        .is_some_and(|hit| matches!(hit.target, Target::Window { .. }))
            })
            .expect("windows are listed");
        assert_eq!(window.icon, notes.1.icon);
    }
}

#[cfg(all(test, feature = "lazy-engine"))]
mod engine_fetch_tests;
