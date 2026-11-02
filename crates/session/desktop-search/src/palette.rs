//! The desktop search palette: the toolkit's [`ActionPalette`] over the
//! portable sources. The host owns placement and input routing; a chosen row
//! is handed back through [`DesktopSearch::take_choice`], an action row as
//! the address the host's action door resolves against the live resource
//! tree.

use tilo_actions::{FlatAction, ResourceSpec};
use ui::{ActionPalette, CommandEntry};

use crate::model::{Hit, Source, SourceCosts, Target, frame_deadline};
use crate::providers::{
    Actions, Answers, AppEntry, Applications, CommandSpec, Commands, Content, Published,
    PublishedActions, Recents, Switches, WebSearch, WindowEntry, Windows,
};
use crate::window::WindowSearch;
use ui::search::Scope;
use ui::search::system::{KindGroup, ResourceKind};

/// Row id prefix of the row that lists the rest of a group the palette's
/// height cut short; `more:{i}` expands `more_rows[i]`.
const MORE_PREFIX: &str = "more:";

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
    /// service answers. The search over everything asks them.
    pub sources: Vec<Box<dyn Source>>,
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

struct Sources {
    applications: Applications,
    windows: Windows,
    commands: Commands,
    actions: Actions,
    published: Published,
    content: Content,
    answers: Option<(Answers, WebSearch)>,
    /// The one window searched, when the search is a window's.
    window: Option<WindowSearch>,
    host: Vec<Box<dyn Source>>,
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
                applications: Applications::new(data.applications),
                windows: Windows(data.windows),
                commands: Commands(data.commands),
                content: Content::new(Vec::new(), Vec::new()),
                actions: Actions(Vec::new()),
                published: Published(Vec::new()),
                answers: None,
                window: Some(WindowSearch::new(data.resources, data.providers).within(anchor)),
                host: Vec::new(),
                preferences: data.preferences,
                now: data.now,
            };
        }
        Self {
            applications: Applications::new(data.applications),
            windows: Windows(data.windows),
            commands: Commands(data.commands),
            content: Content::new(data.providers, data.resources.clone()).within(anchor),
            actions: Actions(data.resources),
            published: Published(data.published),
            answers: matches!(data.scope, Scope::Workspace | Scope::Everywhere).then(|| {
                (
                    Answers {
                        rates: data.rates,
                        now: data.now,
                    },
                    data.app_for_scheme
                        .map_or_else(WebSearch::default, |app_for_scheme| WebSearch {
                            app_for_scheme,
                        }),
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
            .map(|source| source.poll())
            .fold(false, |any, one| any | one);
        self.content.poll() || window || host
    }

    fn waiting(&self) -> bool {
        self.content.waiting() || self.window.as_ref().is_some_and(WindowSearch::waiting)
    }
}

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
    /// [`ui::search::engine_generation`] when the rows were built: the
    /// content rows come again once the index's code arrived.
    shown_generation: u64,
    /// Whether this search is the applications alone, each already listed:
    /// then no row offers to list them all again.
    catalogue: bool,
    choice: Option<Target>,
    /// The search filling in, a source at a time: the rows shown are what
    /// came in so far.
    stream: Option<crate::model::SearchStream>,
    /// How long each source took last time: what decides which is asked in
    /// this frame and which waits for a later one.
    costs: SourceCosts,
    /// The sources this search asks, in the order it asks them, and where
    /// each stands in the empty palette.
    order: Vec<(String, (u8, usize))>,
    /// The row capacity the rows were cut to: when the card grew or shrank
    /// since, the groups are cut again.
    shown_capacity: usize,
    /// Whether the rows the palette holds stand two lines tall: what the
    /// capacity is counted in.
    shown_two_lines: bool,
    /// The row the person moved to in this search, kept while the rows fill
    /// in: a frame that has no room for it yet does not take their choice
    /// away.
    kept_choice: Option<ChosenRow>,
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
            shown_generation: 0,
            catalogue: false,
            choice: None,
            stream: None,
            costs: SourceCosts::default(),
            order: Vec::new(),
            shown_capacity: 0,
            shown_two_lines: false,
            kept_choice: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.sources.is_some()
    }

    /// Whether an application's answer to the query is still coming: the
    /// host keeps calling [`Self::refresh`] until it arrives.
    pub fn waiting(&self) -> bool {
        self.sources.as_ref().is_some_and(Sources::waiting)
            || self
                .stream
                .as_ref()
                .is_some_and(crate::model::SearchStream::pending)
    }

    /// Open over `data` with an empty query.
    pub fn open(&mut self, mut data: SearchData) {
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
    }

    /// Run the query again when the palette's text changed since the rows
    /// were built, the index's code arrived since, or an application's
    /// answer that was still coming arrived; and ask the sources that are
    /// still answering, a frame at a time. Returns whether the rows
    /// changed.
    pub fn refresh(&mut self) -> bool {
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
        if self.palette.entry_row_capacity_for(0, self.shown_two_lines) != self.shown_capacity {
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
            Some(window) => (vec![window as &dyn Source], None),
            None => everywhere(sources, &self.recents),
        };
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
        let deadline = frame_deadline(crate::model::FRAME_BUDGET);
        let now = sources.now;
        let usage = &self.usage;
        let costs = &mut self.costs;
        let stream = crate::model::search_stream(query, &all, 0, costs, deadline, &|hit: &Hit| {
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
            Some(window) => (vec![window as &dyn Source], None),
            None => everywhere(sources, &self.recents),
        };
        if let Some(switches) = &switches {
            all.push(switches);
        }
        let query = self.palette.query().to_owned();
        let now = sources.now;
        let deadline = frame_deadline(crate::model::FRAME_BUDGET);
        let usage = &self.usage;
        let costs = &mut self.costs;
        let asked = stream.resume(&all, costs, deadline, &|hit: &Hit| {
            usage.strength(&query, hit, now)
        });
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
        let groups = self.arranged(query, stream.groups());
        let pending = stream.pending();
        // The rows about to be drawn, not the ones the palette still holds:
        // a row with a line under its name is twice as tall, and a list
        // counted in short rows runs past the card.
        let two_lines = groups
            .iter()
            .any(|group| group.results.iter().any(|hit| stands_tall(hit)));
        let capacity = self.palette.entry_row_capacity_for(0, two_lines);
        let browsing = query.trim().is_empty();
        let mut rows = Vec::new();
        let mut more_rows = Vec::new();
        let entries = self.fitted(groups, two_lines, browsing, &mut rows, &mut more_rows);
        self.more_rows = more_rows;
        // A source answering late re-ranks the rows of the same query: the
        // row the person moved to stays chosen, so Enter does not open
        // whatever slid under the cursor. A selection they never moved
        // follows the best result. A frame that lists no room for their row
        // yet keeps it for the frame that does.
        if self.shown_query.as_deref() != Some(query) {
            self.kept_choice = None;
        } else if self.palette.selected_id() != self.auto_selected.as_deref() {
            self.kept_choice = self
                .palette
                .selected_id()
                .and_then(|id| self.chosen_row(id));
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
        self.shown_capacity = capacity;
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
        two_lines: bool,
        browsing: bool,
        rows: &mut Vec<Hit>,
        more: &mut Vec<ResourceKind>,
    ) -> Vec<CommandEntry> {
        let total: usize = groups.iter().map(|group| group.total).sum();
        let mut dropped = false;
        loop {
            let cap = if self.everything {
                usize::MAX
            } else {
                self.palette.entry_row_capacity_for(groups.len(), two_lines)
            };
            // A lone group is kept whole to spare it a "Show all" row it
            // would not need, so its own rows are trimmed to the room left
            // instead. Every group keeps at least its first row.
            let lone = groups.len() == 1;
            let floor = usize::from(!lone);
            let mut allowed = cap;
            let mut last: Option<Vec<CommandEntry>>;
            loop {
                let trimmed = if self.everything {
                    groups.clone()
                } else if lone {
                    let mut alone = groups.clone();
                    alone[0].results.truncate(allowed);
                    alone
                } else {
                    // Rows and the row offering a cut group's rest both
                    // stand in the list, so the groups share what stays once
                    // those rows are paid for; a person asking for
                    // everything lists them whole and scrolls.
                    ui::search::system::trim_groups(groups.clone(), allowed, &self.expanded)
                };
                let mut entries = entries_for(&trimmed, browsing, self.catalogue, more, rows);
                let shown: usize = trimmed.iter().map(|group| group.results.len()).sum();
                if dropped {
                    entries.push(all_results_entry(total.saturating_sub(shown)));
                }
                if self.everything || self.palette.entries_fit(&entries, trimmed.len()) {
                    return entries;
                }
                last = Some(entries);
                if allowed <= floor {
                    break;
                }
                allowed -= 1;
            }
            let last = last.unwrap_or_default();
            if dropped {
                // Nothing is left to leave out: the rows scroll under the
                // card rather than being cut at its edge.
                return last;
            }
            // No room for even this many groups: the lowest-ranked one the
            // search's own rows do not keep makes way for the all-results
            // row, which lists it too.
            let Some(at) = groups.iter().rposition(|group| !always_shown(&group.kind)) else {
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
            if let Some(more) = outcome
                .id
                .strip_prefix(MORE_PREFIX)
                .and_then(|more| more.parse().ok())
            {
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
            } else if let Some(hit) = self.hit(&outcome.id) {
                self.choose_hit(hit, outcome.argument);
            }
        }
        if self.sources.is_none() || self.palette.is_open() {
            return false;
        }
        self.finish();
        true
    }

    /// Choose `hit`, with `argument` the one line its action asks for:
    /// it is remembered, and its target is the choice. A switch stays in
    /// view, showing what it flipped to; anything else closes search.
    fn choose_hit(&mut self, hit: Hit, argument: Option<String>) {
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
        let mut target = hit.target;
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
        self.choose_hit(hit, argument);
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
        self.shown_query = None;
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

/// Whether the row a hit becomes stands two lines tall: it says something
/// under its name. Asked of the row the palette would build, so the height
/// the capacity is counted in is the height the row is drawn at.
fn stands_tall(hit: &Hit) -> bool {
    entry_for(hit, String::new()).description.is_some()
}

/// The rows `groups` list, in order: every hit as its palette row, and
/// under a group the budget cut short the row that offers the rest of it.
/// `rows` collects the hits the `hit:{i}` ids name, `more` the kinds the
/// `more:{i}` ids expand.
fn entries_for(
    groups: &[KindGroup<Hit>],
    browsing: bool,
    catalogue: bool,
    more: &mut Vec<ResourceKind>,
    rows: &mut Vec<Hit>,
) -> Vec<CommandEntry> {
    more.clear();
    rows.clear();
    let mut entries = Vec::new();
    for group in groups {
        let label = group.kind.label().to_owned();
        let capped = group.is_capped();
        let applications = browsing && !catalogue && group.kind == ResourceKind::Application;
        for hit in &group.results {
            entries.push(
                entry_for(hit, format!("hit:{}", rows.len()))
                    .group(label.clone())
                    .group_total(group.total),
            );
            rows.push(hit.clone());
        }
        if capped {
            entries.push(ui::search::system::show_all_entry(
                format!("{MORE_PREFIX}{}", more.len()),
                &label,
                group.total,
            ));
            more.push(group.kind.clone());
        }
        if applications {
            // The rest of what the person has: every app, by its name,
            // where the few they reach for most are listed.
            entries.push(
                CommandEntry::new(ALL_APPS_ID, crate::i18n::t("search-launcher-all-apps"))
                    .icon(ui::icons::Icon::LayoutGrid)
                    .group(label)
                    .group_total(group.total),
            );
        }
    }
    entries
}

/// The row that lists every result the groups above had no room for.
fn all_results_entry(rest: usize) -> CommandEntry {
    CommandEntry::new(
        ALL_RESULTS_ID,
        crate::i18n::t_args(
            "search-show-all-results",
            &[("count", rest.to_string().into())],
        ),
    )
    .icon(ui::icons::Icon::ChevronDown)
}

/// Kinds a crowded card never leaves out: the answer to the question and
/// the way to the web.
fn always_shown(kind: &ResourceKind) -> bool {
    [
        crate::providers::answers_kind(),
        crate::providers::web_kind(),
    ]
    .contains(kind)
        || *kind == crate::providers::ask_kind()
}

/// One hit as a palette row: an application wears its own glyph, the one
/// its windows wear, never the generic grid every app shared.
fn entry_for(hit: &Hit, id: String) -> CommandEntry {
    let (mut entry, icon) = match &hit.target {
        Target::Action(action) => {
            let flat = FlatAction {
                key: id.clone(),
                resource_id: action.resource.clone(),
                resource_kind: action.resource_kind.clone(),
                resource_label: action.resource_label.clone(),
                resource_presentation: None,
                action: &action.spec,
                depth: 0,
            };
            let mut entry = CommandEntry::from_flat_action(&flat);
            if action.preset.is_some() {
                // The row is the hit (a video, a search of the service), not
                // the generic `open` it runs through: only the hit's own
                // subtitle describes it, and it runs at the hit the row
                // already carries instead of asking for it.
                entry = entry.with_preset();
                entry.description = None;
            }
            entry.id = id;
            entry.label.clone_from(&hit.title);
            (
                entry,
                hit.icon
                    .unwrap_or_else(|| ui::search::system::icon_for_kind(&hit.kind)),
            )
        }
        Target::Invoke(invoke) => (
            match &invoke.descriptor {
                Some(descriptor) => CommandEntry::from_descriptor(id, descriptor),
                None => CommandEntry::new(id, hit.title.clone()),
            },
            hit.icon
                .unwrap_or_else(|| ui::search::system::icon_for_kind(&hit.kind)),
        ),
        // A source's switch wears what it is set to now, as a published
        // switch does: `enabled` is what choosing it makes it, so the badge
        // says the other.
        Target::Prefer { enabled, .. } => (
            CommandEntry::new(id, hit.title.clone()).badge(crate::i18n::t(if *enabled {
                "search-source-badge-off"
            } else {
                "search-source-badge-on"
            })),
            hit.icon
                .unwrap_or_else(|| ui::search::system::icon_for_kind(&hit.kind)),
        ),
        Target::Launch { app } | Target::Window { app: Some(app), .. } => (
            CommandEntry::new(id, hit.title.clone()),
            ui::icons::app_glyph(app, crate::availability::category_for(app)),
        ),
        _ => (
            CommandEntry::new(id, hit.title.clone()),
            ui::search::system::icon_for_kind(&hit.kind),
        ),
    };
    // What the row says under its title, never the title again: a source
    // that names the row twice (a page whose subtitle is its own name, a
    // window whose place is the tab's label) says nothing new.
    let (subtitle, facts) = under_title(hit);
    if let Some(subtitle) = subtitle {
        entry.description = Some(subtitle);
    }
    if !facts.is_empty() {
        entry = entry.facts(facts);
    }
    if !hit.faces.is_empty() {
        entry = entry.faces(hit.faces.clone());
    }
    if let Some(outcome) = &hit.outcome {
        entry = entry.outcome(outcome.clone());
    }
    if let Some(row) = &hit.row {
        entry = entry.drawn(row.clone());
    }
    if let Some(preview) = &hit.preview {
        entry = entry.previewed(preview.clone());
    }
    entry.icon(icon)
}

/// The sources a search over everything asks, in the order it asks them:
/// the desktop's own, the ones the host adds, and the row that switches
/// each of them off and on again.
fn everywhere<'a>(
    sources: &'a Sources,
    recents: &'a Recents,
) -> (Vec<&'a dyn Source>, Option<Switches>) {
    let mut all: Vec<&dyn Source> = vec![
        recents,
        &sources.applications,
        &sources.windows,
        &sources.content,
        &sources.actions,
        &sources.published,
        &sources.commands,
    ];
    if let Some((answers, web)) = &sources.answers {
        all.extend([answers as &dyn Source, web]);
    }
    all.extend(sources.host.iter().map(|source| &**source));
    let switches = switches(sources, &all);
    all.retain(|source| sources.preferences.enabled(source.id()));
    (all, switches)
}

/// What a row says under its title: its subtitle and its facts, each
/// dropped when it repeats the title or says what another already said.
fn under_title(hit: &Hit) -> (Option<String>, Vec<String>) {
    let title = ui::search::fold_search_text(hit.title.trim());
    let mut said: Vec<String> = vec![title];
    let subtitle = hit
        .subtitle
        .as_ref()
        .map(|subtitle| subtitle.trim())
        .filter(|subtitle| !subtitle.is_empty())
        .filter(|subtitle| {
            let folded = ui::search::fold_search_text(subtitle);
            if said.contains(&folded) {
                return false;
            }
            said.push(folded);
            true
        })
        .map(str::to_owned);
    let mut facts = Vec::new();
    for fact in &hit.facts {
        let fact = fact.trim();
        if fact.is_empty() {
            continue;
        }
        let folded = ui::search::fold_search_text(fact);
        if said.contains(&folded) {
            continue;
        }
        said.push(folded);
        facts.push(fact.to_owned());
    }
    (subtitle, facts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::AppScope;

    fn data() -> SearchData {
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
            .push(Box::new(Happening::new(false, vec![here])));
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
            title: "Leave this workspace on this computer".into(),
            subtitle: None,
        });
        data.sources
            .push(Box::new(Happening::new(false, vec![here])));
        data.sources.push(Box::new(People::new(
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
            entries
                .iter()
                .all(|entry| !entry.label.starts_with("Leave this workspace")),
            "no workspace housekeeping is a default: {entries:?}"
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
        let full = data();
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
                app: "so.tilo.sheets".into()
            })
        );
        assert!(!search.is_open());
    }

    /// The shell's published actions: a switch showing its state and keys,
    /// and an action with nothing to act on now.
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
        let mut resource = ResourceSpec::new("actions", "actions", "Actions");
        resource.actions.push(
            action("notifications.dnd.toggle", "Do Not Disturb").with_state(
                tilo_actions::ActionState {
                    badge: Some("Off".into()),
                    toggles: true,
                    shortcut: Some("Super+D".into()),
                },
            ),
        );
        resource.actions.push(
            action("projection.close", "Stop projecting")
                .unavailable("nothing_to_act_on", "Nothing is projected"),
        );
        vec![PublishedActions::of("shell", "workspace", &[resource])]
    }

    #[test]
    fn a_published_switch_shows_its_state_and_stays_open_on_it() {
        let mut search = DesktopSearch::new();
        search.open(SearchData {
            published: published(),
            ..data()
        });
        settled(&mut search);
        search.set_query("disturb");
        settled(&mut search);
        let hit = &search.results()[0];
        assert_eq!(hit.title, "Do Not Disturb");
        let entries = search.palette().visible_entries();
        assert_eq!(entries[0].badge.as_deref(), Some("Off"));
        assert_eq!(entries[0].keybinding.as_deref(), Some("Super+D"));
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
        assert_eq!(
            invoke.address,
            tilo_actions::ActionAddress::new("actions", "notifications.dnd.toggle")
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
            sources: vec![Box::new(Notes)],
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

    /// Typing runs the query once: the frame that takes the new text holds
    /// what the sources answered in it, the frames after it only fill in
    /// the rest, and a settled search asks nothing again.
    #[test]
    fn typing_refreshes_the_rows_once_per_query() {
        let mut search = DesktopSearch::new();
        search.open(data());
        settled(&mut search);
        assert!(!search.refresh(), "the empty query is already shown");
        search.palette_mut().set_query("not");
        assert!(search.refresh(), "the new text runs the query");
        assert!(search.results().iter().any(|hit| hit.title == "Notes"));
        settled(&mut search);
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

    /// Show what a host shows: keep asking for the rows of the sources that
    /// have not answered yet, as a running palette does every frame.
    fn settled(search: &mut DesktopSearch) {
        for _ in 0..128 {
            if !search.refresh() {
                return;
            }
        }
        panic!("the search never settled");
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
            "{:?}",
            search.palette().selected_id()
        );
        assert_eq!(group(&search).as_deref(), Some(chosen.as_str()));
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
            !stands_tall(&row(None, vec!["Tilo/Home".into()])),
            "a fact rides the one line the row has"
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
        search.set_room(500.0, 160.0);
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
        assert!(
            rest.label.contains('1') || rest.label.contains('2') || rest.label.contains('3'),
            "the row says how many results it lists: {}",
            rest.label
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

        let mut reached = std::collections::BTreeSet::new();
        for _ in 0..(total * 4) {
            let id = search.palette().selected_id().map(str::to_owned);
            if let Some(id) = id {
                if id.starts_with(MORE_PREFIX) {
                    let cursor = search.palette().cursor();
                    search.palette_mut().activate_selected();
                    assert!(!search.observe(), "showing all keeps search open");
                    assert!(search.is_open() && search.palette().is_open());
                    assert_eq!(search.palette().cursor(), cursor, "the row stays put");
                    assert_ne!(
                        search.palette().selected_id(),
                        Some(id.as_str()),
                        "the rest took the row's place"
                    );
                    continue;
                }
                if let Some(Target::Launch { app }) = search.hit(&id).map(|hit| hit.target) {
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
                Target::Launch { app } => Some((app, *entry)),
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
