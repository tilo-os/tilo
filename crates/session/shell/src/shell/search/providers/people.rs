//! The people in this workspace, found by name.
//!
//! Collaboration is what Tilo is for, and "go to what Mara is doing" is a
//! search like any other: typing a collaborator's name offers them, with
//! their status line or where they are. Enter follows a connected
//! collaborator, the same request the people panel and the
//! `presence.follow` action make; someone invited or not connected opens the
//! people panel on them instead.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_desktop::collaborators::{RosterRow, collaborator_roster_rows};
use ui::search::{FieldWeight, ListIndex};
use ui::widget::command_palette::RowFace;

use super::happening_now;
use crate::shell::chrome_state::{Chrome, ChromeState, PresenceRequest, SearchScope};
use crate::shell::i18n::t;
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub const PROVIDER_ID: &str = "people";

pub struct PeopleProvider {
    kinds: Vec<ResourceKind>,
    /// The people's names, indexed while they stay the same.
    ranker: ListIndex,
    /// The faces of the launcher rows last listed, by row id.
    faces: RefCell<HashMap<String, Vec<RowFace>>>,
}

impl Default for PeopleProvider {
    fn default() -> Self {
        Self {
            kinds: vec![ResourceKind::Custom(t("search-kind-people"))],
            ranker: ListIndex::default(),
            faces: RefCell::default(),
        }
    }
}

impl PeopleProvider {
    pub fn new() -> Self {
        Self::default()
    }

    /// Happening now in `scope`, remembering each row's faces.
    fn happening(&self, state: &ChromeState, scope: SearchScope) -> Vec<SearchResult> {
        let rows = happening_now::rows(state, scope, &state.live_calls);
        let mut faces = self.faces.borrow_mut();
        faces.clear();
        rows.into_iter()
            .map(|(row, worn)| {
                faces.insert(row.id.clone(), worn);
                row
            })
            .collect()
    }
}

/// A person's second line: what they say they are doing, else where they
/// are, else that they are invited.
fn about(row: &RosterRow) -> Option<String> {
    row.status
        .as_ref()
        .map(|status| status.text.clone())
        .or_else(|| row.activity.clone())
        .or_else(|| row.invited.then(|| t("search-people-invited")))
}

fn result(row: RosterRow, kind: ResourceKind) -> SearchResult {
    let id = row.id.clone();
    let followable = row.followable;
    SearchResult {
        facts: Vec::new(),
        kind,
        operation_kind: tilo_actions::OperationKind::View,
        provider_id: PROVIDER_ID.into(),
        subtitle: about(&row),
        id: row.id,
        title: row.label,
        badge: None,
        shortcut: None,
        score: 0,
        params: Vec::new(),
        contract: Some(tilo_actions::ActionContract::view()),
        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args: &ActionArgs| {
            let mut state = chrome.borrow_mut();
            if followable {
                state.push_presence_request(PresenceRequest::Follow(id.clone()));
            } else {
                state.overlays.collaborators = true;
            }
            state.bump();
            Ok::<(), ActionError>(())
        }),
        on_invoke: None,
        on_begin_invoke: None,
    }
}

impl SearchProvider for PeopleProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn name(&self) -> &'static str {
        "People"
    }

    fn display_name(&self) -> String {
        t("search-provider-people")
    }

    fn resource_kinds(&self) -> &[ResourceKind] {
        &self.kinds
    }

    fn launcher_section(&self) -> Option<crate::shell::search::launcher::LauncherSection> {
        Some(crate::shell::search::launcher::LauncherSection::HappeningNow)
    }

    fn launcher_rows(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        self.happening(ctx.chrome, ctx.effective_scope())
    }

    fn row_outcome(&self, result: &SearchResult) -> Option<String> {
        happening_now::outcome(&result.id)
    }

    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        happening_now::icon(&result.id)
    }

    fn row_faces(&self, result: &SearchResult) -> Vec<RowFace> {
        self.faces
            .borrow()
            .get(&result.id)
            .cloned()
            .unwrap_or_default()
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        use std::hash::{Hash as _, Hasher as _};
        let tree = ctx.chrome.tree();
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(workspace) = tree.desktop.active_workspace() {
            for person in &workspace.collaborators {
                (&person.id, &person.name, person.active_tab, person.invited).hash(&mut hasher);
            }
        }
        for person in &tree.desktop.collaborators {
            (&person.id, &person.name, person.active_tab).hash(&mut hasher);
        }
        // Happening now reads every workspace's presence and the calls here.
        for workspace in &tree.desktop.workspaces {
            (workspace.id.0, &workspace.name).hash(&mut hasher);
            for person in &workspace.collaborators {
                (&person.id, person.active_tab, person.invited).hash(&mut hasher);
            }
        }
        for call in &ctx.chrome.live_calls {
            (call.workspace.0, &call.channel.channel, &call.name).hash(&mut hasher);
            for person in &call.people {
                person.id.hash(&mut hasher);
            }
        }
        drop(tree);
        format!("people:{:016x}", hasher.finish())
    }

    /// A person is addressed by their participant id, found or not by a
    /// search.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if happening_now::is_row_id(id) {
            return self
                .happening(ctx.chrome, SearchScope::All)
                .into_iter()
                .find(|row| row.id == id);
        }
        let kind = self.kinds.first()?;
        let me = crate::shell::state::local_user_id();
        let tree = ctx.chrome.tree();
        let row = collaborator_roster_rows(&tree.desktop, &me)
            .into_iter()
            .find(|row| row.id == id && row.id != me && !row.own_device)?;
        drop(tree);
        Some(result(row, kind.clone()))
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        // People are found by name; the empty palette lists them in the
        // people panel, not here.
        let Some(kind) = self.kinds.first() else {
            return Vec::new();
        };
        if ctx.needle.trim().is_empty() || !ctx.wanted.contains(kind) {
            return Vec::new();
        }
        let me = crate::shell::state::local_user_id();
        let people: Vec<RosterRow> = {
            let tree = ctx.chrome.tree();
            collaborator_roster_rows(&tree.desktop, &me)
                .into_iter()
                .filter(|row| row.id != me && !row.own_device)
                .collect()
        };
        let fields: Vec<Vec<(String, FieldWeight)>> = people
            .iter()
            .map(|row| {
                let mut fields = vec![(row.label.clone(), FieldWeight::TITLE)];
                fields.extend(row.role.clone().map(|role| (role, FieldWeight::METADATA)));
                fields
            })
            .collect();
        let rows = people
            .into_iter()
            .map(|row| result(row, kind.clone()))
            .collect();
        crate::shell::search::rank_rows(&self.ranker, ctx.needle, rows, &fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use automerge::{AutoCommit, ROOT};

    #[test]
    fn a_collaborator_is_found_by_name_and_enter_follows_them() {
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        let workspace = &mut tree.desktop.workspaces[0];
        let mut mara = tilo_desktop_model::Collaborator::new("peer-mara", "Mara");
        mara.active_tab = Some(1);
        mara.session = Some(7);
        workspace.collaborators.push(mara);
        workspace
            .collaborators
            .push(tilo_desktop_model::Collaborator::new("peer-alice", "Alice"));
        tree.seed_doc_from_cache();
        let chrome = ChromeState::new(tree);
        let doc = AutoCommit::new();
        let provider = PeopleProvider::new();
        let kind = provider.resource_kinds().to_vec();
        let found = {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "mar",
                wanted: &kind,
                chrome: &state,
                doc: &doc,
                doc_root: &ROOT,
            })
        };
        assert_eq!(
            found
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["Mara"]
        );
        let mut doc = doc;
        (found[0].on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect("opened");
        let requests = chrome.borrow_mut().take_presence_requests();
        let state = chrome.borrow();
        assert!(
            requests
                .iter()
                .any(|request| matches!(request, PresenceRequest::Follow(id) if id == "peer-mara"))
                || state.overlays.collaborators,
            "Enter follows Mara, or opens the people panel on her: {requests:?}"
        );
    }
}
