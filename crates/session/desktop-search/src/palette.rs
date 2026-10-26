//! The desktop search palette: the toolkit's [`CommandPalette`] over the
//! portable sources. The host owns placement and input routing; choosing an
//! action row queues the palette's own control invocation (the host's action
//! door resolves it against the live resource tree), every other row is
//! handed back through [`DesktopSearch::take_choice`].

use tilo_actions::{FlatAction, Resource};
use ui::widget::command_palette::{CommandEntry, CommandPalette};

use crate::model::{Hit, Source, Target, search};
use crate::providers::{
    Actions, AppEntry, Applications, CommandSpec, Commands, Content, Recents, WindowEntry, Windows,
};
use ui::search::system::ResourceKind;

/// Row id prefix of the row that lists the rest of a group the palette's
/// height cut short; `more:{i}` expands `more_rows[i]`.
const MORE_PREFIX: &str = "more:";

/// What the host can search right now, collected when search opens.
#[derive(Default)]
pub struct SearchData {
    pub applications: Vec<AppEntry>,
    pub windows: Vec<WindowEntry>,
    pub commands: Vec<CommandSpec>,
    pub resources: Vec<Resource>,
    /// The search providers open applications publish.
    pub providers: Vec<Box<dyn ui::search::SearchProvider>>,
    /// Search what the resources hold, not what can be done to them: a
    /// search one app or window opened is a search of its content, which
    /// the app's actions (named for automation, not for finding things)
    /// would otherwise crowd out.
    pub content_only: bool,
    /// The part of the application a search it opened is narrowed to (a
    /// [`ui::search::ScopeDecl`] anchor): one mailbox, one folder.
    pub scope: Option<String>,
    /// What the empty field says it searches, where that is not
    /// everything ("Search Tube…").
    pub placeholder: Option<String>,
}

/// A row the person moved to, as it is found again once the rows are
/// rebuilt: rows are numbered by position, which a late answer changes.
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
    content: Content,
}

impl Sources {
    fn new(data: SearchData) -> Self {
        Self {
            applications: Applications::new(data.applications),
            windows: Windows(data.windows),
            commands: Commands(data.commands),
            content: Content::new(data.providers, data.resources.clone()).within(data.scope),
            actions: Actions(if data.content_only {
                Vec::new()
            } else {
                data.resources
            }),
        }
    }
}

pub struct DesktopSearch {
    palette: CommandPalette,
    sources: Option<Sources>,
    recents: Recents,
    /// The hits the palette shows; entry `hit:{i}` is `rows[i]`.
    rows: Vec<Hit>,
    /// The kind of each "Show all" row; entry `more:{i}` is `more_rows[i]`.
    more_rows: Vec<ResourceKind>,
    /// The kinds the person asked to see whole in this search.
    expanded: Vec<ResourceKind>,
    /// The row the palette selected by itself when the rows were last
    /// built: a selection still on it is the person's best result, not a
    /// row they chose.
    auto_selected: Option<String>,
    shown_query: Option<String>,
    /// [`ui::search::engine_generation`] when the rows were built: the
    /// content rows come again once the index's code arrived.
    shown_generation: u64,
    /// The palette is closing because it was dismissed, not because a row
    /// was chosen.
    dismissed: bool,
    choice: Option<Target>,
}

impl Default for DesktopSearch {
    fn default() -> Self {
        Self::new()
    }
}

const PLACEHOLDER: &str = "Search apps, windows, documents and actions\u{2026}";

impl DesktopSearch {
    pub fn new() -> Self {
        let mut palette = CommandPalette::commands();
        // Rows arrive already matched and ranked by the shared ranking.
        palette.set_bypass_fuzzy(true);
        palette.set_query_placeholder(PLACEHOLDER);
        Self {
            palette,
            sources: None,
            recents: Recents::default(),
            rows: Vec::new(),
            more_rows: Vec::new(),
            expanded: Vec::new(),
            auto_selected: None,
            shown_query: None,
            shown_generation: 0,
            dismissed: false,
            choice: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.sources.is_some()
    }

    /// Whether an application's answer to the query is still coming: the
    /// host keeps calling [`Self::refresh`] until it arrives.
    pub fn waiting(&self) -> bool {
        self.sources
            .as_ref()
            .is_some_and(|sources| sources.content.waiting())
    }

    /// Open over `data` with an empty query.
    pub fn open(&mut self, mut data: SearchData) {
        let placeholder = data.placeholder.take();
        self.palette
            .set_query_placeholder(placeholder.as_deref().unwrap_or(PLACEHOLDER));
        self.sources = Some(Sources::new(data));
        self.dismissed = false;
        self.choice = None;
        self.shown_query = None;
        self.expanded.clear();
        self.palette.open();
        self.refresh();
    }

    /// Close without choosing anything.
    pub fn close(&mut self) {
        self.dismissed = true;
        self.palette.close();
        self.finish();
    }

    /// The palette is about to be dismissed by the key it is handed next.
    pub fn dismissing(&mut self) {
        self.dismissed = true;
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

    pub fn palette(&self) -> &CommandPalette {
        &self.palette
    }

    pub fn palette_mut(&mut self) -> &mut CommandPalette {
        &mut self.palette
    }

    /// Run the query again when the palette's text changed since the rows
    /// were built, the index's code arrived since, or an application's
    /// answer that was still coming arrived. Returns whether the rows
    /// changed.
    pub fn refresh(&mut self) -> bool {
        let Some(sources) = &self.sources else {
            return false;
        };
        let answered = sources.content.poll();
        let query = self.palette.query().to_owned();
        let generation = ui::search::engine_generation();
        if !answered
            && self.shown_query.as_deref() == Some(query.as_str())
            && self.shown_generation == generation
        {
            return false;
        }
        if self.shown_query.is_some() && self.shown_query.as_deref() != Some(query.as_str()) {
            // "Show all" belongs to the search it was asked in.
            self.expanded.clear();
        }
        self.rebuild_rows(query, generation);
        true
    }

    fn rebuild_rows(&mut self, query: String, generation: u64) {
        let Some(sources) = &self.sources else {
            return;
        };
        let groups = search(
            &query,
            &[
                &self.recents as &dyn Source,
                &sources.applications,
                &sources.windows,
                &sources.content,
                &sources.actions,
                &sources.commands,
            ],
            0,
        );
        // Each group shares what the palette shows without scrolling; a
        // group cut short offers the rest in place.
        let groups = ui::search::system::trim_groups(
            groups,
            self.palette.entry_row_capacity(),
            &self.expanded,
        );
        // An app's answer arriving late, or the index's code, re-ranks the
        // rows of the same query: the row the person moved to stays chosen,
        // so Enter does not open whatever slid under the cursor. A
        // selection they never moved follows the best result.
        let chosen = (self.shown_query.as_deref() == Some(query.as_str())
            && self.palette.selected_id() != self.auto_selected.as_deref())
        .then(|| {
            self.palette
                .selected_id()
                .and_then(|id| self.chosen_row(id))
        })
        .flatten();
        let mut rows = Vec::new();
        let mut entries = Vec::new();
        self.more_rows.clear();
        for group in groups {
            let label = group.kind.label().to_owned();
            let capped = group.is_capped();
            for hit in group.results {
                entries.push(
                    entry_for(&hit, format!("hit:{}", rows.len()))
                        .group(label.clone())
                        .group_total(group.total),
                );
                rows.push(hit);
            }
            if capped {
                entries.push(ui::search::system::show_all_entry(
                    format!("{MORE_PREFIX}{}", self.more_rows.len()),
                    &label,
                    group.total,
                ));
                self.more_rows.push(group.kind);
            }
        }
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
        // in: while it is on its way, an empty list says it is still
        // searching.
        self.palette.set_searching(!ui::search::engine_ready());
        self.rows = rows;
        self.shown_query = Some(query);
        self.shown_generation = generation;
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
        self.rebuild_rows(query, ui::search::engine_generation());
        self.palette.set_cursor(cursor);
        // Choosing a row closes a command palette; this one only grew.
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
            } else if let Some(hit) = self.hit(&outcome.id) {
                self.recents.record(&hit);
                self.choice = Some(hit.target);
                self.palette.close();
            }
        }
        if self.sources.is_none() || self.palette.is_open() {
            return false;
        }
        if !self.dismissed
            && self.choice.is_none()
            && let Some(hit) = self.palette.selected_id().and_then(|id| self.hit(id))
        {
            // An action row closed the palette after queueing its invocation.
            self.recents.record(&hit);
        }
        self.finish();
        true
    }

    /// The chosen non-action row, for the host to perform.
    pub fn take_choice(&mut self) -> Option<Target> {
        self.choice.take()
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
        self.rows.clear();
        self.more_rows.clear();
        self.expanded.clear();
        self.auto_selected = None;
        self.shown_query = None;
        self.dismissed = false;
    }
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
            if let Some((param, value)) = &action.preset {
                entry = entry.with_action_preset(param.clone(), value.clone());
                // The row is the hit (a video, a search of the service), not
                // the generic `open` it runs through: only the hit's own
                // subtitle describes it.
                entry.description = None;
            }
            entry.id = id;
            entry.label.clone_from(&hit.title);
            (entry, ui::search::system::icon_for_kind(&hit.kind))
        }
        Target::Launch { app } | Target::Window { app: Some(app), .. } => (
            CommandEntry::new(id, hit.title.clone()),
            ui::icons::app_glyph(app, crate::availability::category_for(app)),
        ),
        _ => (
            CommandEntry::new(id, hit.title.clone()),
            ui::search::system::icon_for_kind(&hit.kind),
        ),
    };
    if let Some(subtitle) = &hit.subtitle {
        entry.description = Some(subtitle.clone());
    }
    if !hit.facts.is_empty() {
        entry = entry.facts(hit.facts.clone());
    }
    entry.icon(icon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::automerge::{AutoCommit, ROOT};

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
            ..SearchData::default()
        }
    }

    #[test]
    fn enter_on_a_typed_app_name_chooses_its_launch() {
        let mut search = DesktopSearch::new();
        search.open(data());
        search.set_query("sheets");
        assert_eq!(search.results()[0].title, "Sheets");
        let mut doc = AutoCommit::new();
        search
            .palette_mut()
            .activate_selected(&mut ui::widget::Edits::lend(&mut doc), &ROOT);
        assert!(search.observe(), "choosing a row closes search");
        assert_eq!(
            search.take_choice(),
            Some(Target::Launch {
                app: "so.tilo.sheets".into()
            })
        );
        assert!(!search.is_open());
    }

    #[test]
    fn a_dismissed_palette_chooses_nothing_and_remembers_nothing() {
        let mut search = DesktopSearch::new();
        search.open(data());
        search.set_query("sheets");
        search.close();
        assert_eq!(search.take_choice(), None);
        search.open(data());
        assert!(
            search.results().iter().all(|hit| hit.provider != "recent"),
            "nothing was chosen"
        );
    }

    #[test]
    fn a_chosen_row_leads_the_next_empty_palette() {
        let mut search = DesktopSearch::new();
        search.open(data());
        search.set_query("sheets");
        let mut doc = AutoCommit::new();
        search
            .palette_mut()
            .activate_selected(&mut ui::widget::Edits::lend(&mut doc), &ROOT);
        search.observe();
        search.open(data());
        assert_eq!(search.results()[0].provider, "recent");
        assert_eq!(search.results()[0].title, "Sheets");
    }

    #[test]
    fn an_action_row_queues_its_invocation_for_the_host_and_closes() {
        let mut resource = Resource::new("page-1", "notes.page", "Groceries");
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
        search.set_query("duplicate");
        assert_eq!(search.results()[0].title, "Duplicate note");
        let mut doc = AutoCommit::new();
        search
            .palette_mut()
            .activate_selected(&mut ui::widget::Edits::lend(&mut doc), &ROOT);
        assert!(search.observe());
        assert_eq!(search.take_choice(), None, "the host's action door runs it");
        let invocation = ui::widget::controls::take_control_invocation(search.palette_mut())
            .expect("queued for the host");
        assert_eq!(
            invocation.address,
            tilo_actions::ActionAddress::new("page-1", "duplicate")
        );
    }

    #[test]
    fn typing_refreshes_the_rows_once_per_query() {
        let mut search = DesktopSearch::new();
        search.open(data());
        assert!(!search.refresh(), "the empty query is already shown");
        search.palette_mut().set_query("not");
        assert!(search.refresh());
        assert!(search.results().iter().any(|hit| hit.title == "Notes"));
        assert!(!search.refresh());
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

    /// Search over `base` with an app whose answer is still coming.
    fn with_a_late_app(base: SearchData) -> (DesktopSearch, std::rc::Rc<std::cell::Cell<bool>>) {
        let arrived = std::rc::Rc::new(std::cell::Cell::new(false));
        let mut meeting = Resource::new("meeting", "notes.page", "Meeting");
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
            content_only: true,
            ..base
        });
        (search, arrived)
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
        assert!(search.waiting(), "the app's answer is still coming");
        assert!(search.results().len() >= 2, "{:?}", search.results());
        assert!(arrow_down(&mut search));
        let chosen = selected(&search).expect("a row is selected");

        arrived.set(true);
        assert!(search.refresh(), "the answer re-ranks the rows");
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
                    title: format!("Meeting room {i}"),
                    subtitle: None,
                })
                .collect(),
            ..data()
        });
        search.set_query("meeting");
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
        arrived.set(true);
        assert!(search.refresh());
        let best = search.results().first().expect("rows");
        assert_eq!(
            selected(&search),
            Some((best.provider.clone(), best.id.clone()))
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
        let header = search
            .palette()
            .visible_entries()
            .iter()
            .find(|entry| entry.group.as_deref() == Some("Apps"))
            .and_then(|entry| entry.group_total);
        assert_eq!(header, Some(total), "the header counts every app");

        let mut reached = std::collections::BTreeSet::new();
        let mut doc = AutoCommit::new();
        for _ in 0..(total * 4) {
            let id = search.palette().selected_id().map(str::to_owned);
            if let Some(id) = id {
                if id.starts_with(MORE_PREFIX) {
                    let cursor = search.palette().cursor();
                    search
                        .palette_mut()
                        .activate_selected(&mut ui::widget::Edits::lend(&mut doc), &ROOT);
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
