//! "Continue", the launcher's third section: where the person's windows
//! were lately, newest first, each reopening where they left off.
//!
//! Read from [`recent_places`]: every window's location as its app declared
//! it to the window bar, with the view's anchor when the app gave one. A row
//! with an anchor opens through the app's `open` at it, as a search hit at
//! that anchor does, the app launched first when it is closed. Without an
//! anchor, the window it was in comes forward while it lasts, else the app
//! opens. An app that declares no location never appears. Scoped to the
//! current workspace, only the places in it.
//!
//! Typed, a place is found by its name and app like any other row, unless
//! its app published an entry opening the same resource, which that entry
//! answers ([`super::app_content`]).

use std::rc::Rc;
use std::sync::Arc;

use crate::shell::chrome_state::{Chrome, ChromeState, SearchScope};
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::app_content_index::AppContentIndex;
use crate::shell::search::launcher::LauncherSection;
use crate::shell::search::recent_places::{self, Place};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

use super::app_content::OpenDoor;

pub const PROVIDER_ID: &str = "continue";

/// The places Continue shows at most.
pub const SHOWN: usize = 6;

/// Row ids: `place:<app>\u{1f}<room>\u{1f}<what it names>`.
const ROW_PREFIX: &str = "place:";

pub struct ContinueProvider {
    door: Option<OpenDoor>,
    /// What apps published, whose entries answer for their own places.
    index: Arc<AppContentIndex>,
    ranker: ui::search::ListIndex,
}

impl ContinueProvider {
    /// Rows that reopen at an anchor through `broker`.
    pub fn new(
        index: Arc<AppContentIndex>,
        broker: crate::shell::collab_broker::AppActionBroker,
    ) -> Self {
        Self {
            door: Some(OpenDoor::new(broker)),
            index,
            ranker: ui::search::ListIndex::default(),
        }
    }

    /// Rows that bring a window forward or open its app, with no broker to
    /// open an anchor through.
    pub fn without_broker(index: Arc<AppContentIndex>) -> Self {
        Self {
            door: None,
            index,
            ranker: ui::search::ListIndex::default(),
        }
    }

    /// Whether an entry its app published opens `place`.
    fn published(&self, place: &Place) -> bool {
        place
            .anchor
            .as_deref()
            .is_some_and(|anchor| self.index.title(&place.room, anchor).is_some())
    }

    fn row(&self, place: &Place, now: std::time::SystemTime) -> Option<SearchResult> {
        let when = ui::widget::history::format_relative_time(place.when()?, now);
        let app = app_name(&place.app_id);
        let opened = t_args("search-launcher-opened", &[("when", when.as_str().into())]);
        let door = self.door.clone();
        let target = place.clone();
        let row = SearchResult {
            facts: Vec::new(),
            kind: ResourceKind::Record,
            operation_kind: tilo_actions::OperationKind::View,
            provider_id: PROVIDER_ID.into(),
            id: row_id(place),
            title: place.label.clone(),
            subtitle: None,
            badge: None,
            shortcut: None,
            score: 0,
            params: Vec::new(),
            contract: Some(tilo_actions::ActionContract::view()),
            on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
                reopen(door.as_ref(), chrome, &target)
            }),
            on_invoke: None,
            on_begin_invoke: None,
        };
        Some(row.with_facts([app, opened]))
    }
}

fn row_id(place: &Place) -> String {
    let names = place
        .anchor
        .as_deref()
        .filter(|anchor| !anchor.is_empty())
        .or_else(|| (!place.value.is_empty()).then_some(place.value.as_str()))
        .unwrap_or(&place.label);
    format!(
        "{ROW_PREFIX}{}\u{1f}{}\u{1f}{names}",
        place.app_id, place.room
    )
}

/// The app's name as the person knows it.
fn app_name(app: &str) -> String {
    crate::shell::launcher::display_name_for_app_id_without_waiting(app)
        .unwrap_or_else(|| app.to_owned())
}

/// The places Continue lists in `scope`: the newest, those of the workspace
/// in front unless widened.
fn shown(state: &ChromeState, scope: SearchScope) -> Vec<Place> {
    kept(state, scope).into_iter().take(SHOWN).collect()
}

/// Every place kept in `scope`, newest first.
fn kept(state: &ChromeState, scope: SearchScope) -> Vec<Place> {
    let here = (scope != SearchScope::All)
        .then(|| {
            state
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room().to_string())
        })
        .flatten();
    recent_places::recent()
        .into_iter()
        .filter(|place| here.as_ref().is_none_or(|room| *room == place.room))
        .collect()
}

/// Reopen `place` where the person left it.
fn reopen(door: Option<&OpenDoor>, chrome: &Chrome, place: &Place) -> Result<(), ActionError> {
    if let Some(anchor) = place.anchor.as_deref() {
        return super::app_content::open_at(door, chrome, &place.app_id, &place.room, anchor);
    }
    let window = {
        let state = chrome.borrow();
        let tree = state.tree();
        tree.desktop.workspaces.iter().find_map(|workspace| {
            workspace.strips.iter().find_map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .find(|group| group.tabs.iter().any(|tab| tab.id == place.tab))
                    .map(|group| (workspace.id, strip.id, group.id))
            })
        })
    };
    if let Some((workspace, strip, group)) = window {
        chrome
            .borrow_mut()
            .select_tab(workspace, strip, group, place.tab);
        return Ok(());
    }
    super::applications::launch_installed_app(chrome, &place.app_id, &app_name(&place.app_id))
}

impl SearchProvider for ContinueProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "Continue"
    }

    fn display_name(&self) -> String {
        t("search-launcher-continue")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Record]
    }

    fn launcher_section(&self) -> Option<LauncherSection> {
        Some(LauncherSection::Continue)
    }

    fn launcher_rows(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let now = std::time::SystemTime::now();
        shown(ctx.chrome, ctx.effective_scope())
            .iter()
            .filter_map(|place| self.row(place, now))
            .collect()
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        let needle = ctx.needle.trim();
        if needle.is_empty() {
            return Vec::new();
        }
        let now = std::time::SystemTime::now();
        let (rows, fields): (Vec<_>, Vec<_>) = kept(ctx.chrome, ctx.effective_scope())
            .iter()
            .filter(|place| !self.published(place))
            .filter_map(|place| {
                let row = self.row(place, now)?;
                let fields = vec![
                    (place.label.clone(), ui::search::FieldWeight::TITLE),
                    (app_name(&place.app_id), ui::search::FieldWeight::KEYWORD),
                ];
                Some((row, fields))
            })
            .unzip();
        crate::shell::search::rank_rows(&self.ranker, needle, rows, &fields)
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for place in recent_places::recent().iter().take(recent_places::KEPT) {
            (&place.app_id, &place.room, &place.label, place.at).hash(&mut hasher);
        }
        ctx.chrome.tree().desktop.active.0.hash(&mut hasher);
        format!("continue:{:016x}", hasher.finish())
    }

    fn resolve(&self, id: &str, _ctx: SearchContext<'_>) -> Option<SearchResult> {
        let now = std::time::SystemTime::now();
        recent_places::recent()
            .iter()
            .find(|place| row_id(place) == id)
            .and_then(|place| self.row(place, now))
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        let app = result.id.strip_prefix(ROW_PREFIX)?.split('\u{1f}').next()?;
        Some(ui::icons::app_glyph(app, None))
    }

    fn row_image(&self, result: &SearchResult) -> Option<ui::icons::AppImage> {
        let app = result.id.strip_prefix(ROW_PREFIX)?.split('\u{1f}').next()?;
        crate::app_icons::for_window_app_id(app)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;
    use std::time::Duration;

    fn provider() -> ContinueProvider {
        ContinueProvider::without_broker(Arc::new(AppContentIndex::in_memory()))
    }

    fn rows(chrome: &Chrome, scope: SearchScope) -> Vec<SearchResult> {
        let state = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        provider().launcher_rows(SearchContext {
            query_scope: Some(scope),
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Record],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
    }

    /// Continue is where the windows were: newest first, at most six, the
    /// workspace in front unless widened, each saying which app and when.
    #[test]
    fn continue_lists_where_the_windows_were_newest_first() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let (home, research) = {
            let state = chrome.borrow();
            let tree = state.tree();
            let home = tree
                .desktop
                .active_workspace()
                .expect("Home")
                .room()
                .to_string();
            let research = tree
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room().to_string() != home)
                .expect("Research")
                .room()
                .to_string();
            drop(tree);
            (home, research)
        };
        assert!(
            rows(&chrome, SearchScope::Workspace).is_empty(),
            "nowhere yet"
        );

        let minutes = |n: u64| Duration::from_secs(n * 60);
        for at in 0..8u64 {
            recent_places::visit_ago(
                "so.tilo.notes",
                &home,
                1,
                &format!("Note {at}"),
                Some(&format!("pages/{at}")),
                minutes(100 - at),
            );
        }
        recent_places::visit_ago(
            "so.tilo.notes",
            &research,
            2,
            "Lab results",
            None,
            minutes(1),
        );

        let here = rows(&chrome, SearchScope::Workspace);
        let titles: Vec<&str> = here.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Note 7", "Note 6", "Note 5", "Note 4", "Note 3", "Note 2"],
            "the six newest of this workspace"
        );
        assert!(
            here.iter()
                .all(|row| row.facts.len() == 2 && row.facts[1].starts_with("Opened ")),
            "each says which app and when: {:?}",
            here[0].facts
        );
        let everywhere = rows(&chrome, SearchScope::All);
        assert_eq!(
            everywhere[0].title, "Lab results",
            "widened, the newest anywhere"
        );
    }
}
