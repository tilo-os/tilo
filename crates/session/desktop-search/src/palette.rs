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

/// Rows the palette paints before sharing space between groups.
pub const PALETTE_ROW_BUDGET: usize = 12;

/// What the host can search right now, collected when search opens.
#[derive(Default)]
pub struct SearchData {
    pub applications: Vec<AppEntry>,
    pub windows: Vec<WindowEntry>,
    pub commands: Vec<CommandSpec>,
    /// The resources open applications publish.
    pub resources: Vec<Resource>,
    /// The search providers open applications publish.
    pub providers: Vec<Box<dyn ui::search::SearchProvider>>,
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
            applications: Applications(data.applications),
            windows: Windows(data.windows),
            commands: Commands(data.commands),
            content: Content::new(data.providers, data.resources.clone()),
            actions: Actions(data.resources),
        }
    }
}

pub struct DesktopSearch {
    palette: CommandPalette,
    sources: Option<Sources>,
    recents: Recents,
    /// The hits the palette shows; entry `hit:{i}` is `rows[i]`.
    rows: Vec<Hit>,
    shown_query: Option<String>,
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
            shown_query: None,
            dismissed: false,
            choice: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.sources.is_some()
    }

    /// Open over `data` with an empty query.
    pub fn open(&mut self, data: SearchData) {
        self.sources = Some(Sources::new(data));
        self.dismissed = false;
        self.choice = None;
        self.shown_query = None;
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
    /// were built. Returns whether the rows changed.
    pub fn refresh(&mut self) -> bool {
        let Some(sources) = &self.sources else {
            return false;
        };
        let query = self.palette.query().to_owned();
        if self.shown_query.as_deref() == Some(query.as_str()) {
            return false;
        }
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
            PALETTE_ROW_BUDGET,
        );
        let mut rows = Vec::new();
        let mut entries = Vec::new();
        for group in groups {
            let label = group.kind.label().to_owned();
            for hit in group.results {
                let id = format!("hit:{}", rows.len());
                let mut entry = match &hit.target {
                    Target::Action(action) => {
                        let flat = FlatAction {
                            key: id.clone(),
                            resource_id: action.resource.clone(),
                            resource_kind: action.resource_kind.clone(),
                            resource_label: action.resource_label.clone(),
                            resource_presentation: None,
                            action: &action.spec,
                        };
                        let mut entry = CommandEntry::from_flat_action(&flat);
                        entry.id = id;
                        entry.label.clone_from(&hit.title);
                        entry
                    }
                    _ => CommandEntry::new(id, hit.title.clone()),
                };
                if let Some(subtitle) = &hit.subtitle {
                    entry.description = Some(subtitle.clone());
                }
                entries.push(
                    entry
                        .group(label.clone())
                        .group_total(group.total)
                        .icon(hit.kind.icon()),
                );
                rows.push(hit);
            }
        }
        self.palette.set_entries(entries);
        self.rows = rows;
        self.shown_query = Some(query);
        true
    }

    /// Follow the palette after it handled input: take a chosen row, and
    /// notice that the palette closed itself (a choice or Escape). Returns
    /// whether search closed; the caller then [`refresh`](Self::refresh)es
    /// an open palette for its edited query.
    pub fn observe(&mut self) -> bool {
        if let Some(outcome) = self.palette.take_outcome()
            && let Some(hit) = self.hit(&outcome.id)
        {
            self.recents.record(&hit);
            self.choice = Some(hit.target);
            self.palette.close();
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

    fn finish(&mut self) {
        self.sources = None;
        self.rows.clear();
        self.shown_query = None;
        self.dismissed = false;
    }
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
                    unavailable: None,
                },
                AppEntry {
                    id: "so.tilo.notes".into(),
                    title: "Notes".into(),
                    category: None,
                    description: None,
                    unavailable: None,
                },
            ],
            windows: vec![WindowEntry {
                tab: 3,
                title: "Notes".into(),
                app_title: "Notes".into(),
                location: "Workspace · Main".into(),
            }],
            ..SearchData::default()
        }
    }

    #[test]
    fn enter_on_a_typed_app_name_chooses_its_launch() {
        let mut search = DesktopSearch::new();
        search.open(data());
        search.set_query("sheets");
        assert_eq!(search.results()[0].title, "Open Sheets");
        let mut doc = AutoCommit::new();
        search.palette_mut().activate_selected(&mut doc, &ROOT);
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
        search.palette_mut().activate_selected(&mut doc, &ROOT);
        search.observe();
        search.open(data());
        assert_eq!(search.results()[0].provider, "recent");
        assert_eq!(search.results()[0].title, "Open Sheets");
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
        search.palette_mut().activate_selected(&mut doc, &ROOT);
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
        assert!(search.results().iter().any(|hit| hit.title == "Open Notes"));
        assert!(!search.refresh());
    }
}
