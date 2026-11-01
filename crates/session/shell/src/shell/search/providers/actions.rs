//! The shell's actions as search rows: each declaration in
//! [`crate::shell::actions`] becomes a palette row, ranked by its label,
//! address, subtitle and the other words it declares, beside the rows of
//! the workspace views, archived workspaces, governance and search-source
//! switches built from the chrome.

use std::rc::Rc;
use ui::search::{FieldWeight, ListIndex};

#[allow(
    clippy::wildcard_imports,
    reason = "every declaration in the shell's action table is a palette row, and the provider's tests reach them the same way"
)]
use crate::shell::actions::*;
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::i18n::{t, t_args};
use crate::shell::search::action_api::{ActionArgs, ActionError};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

pub struct ActionsProvider {
    /// The rows' searchable text, indexed while it stays the same.
    ranker: ListIndex,
    search_preferences: Option<crate::shell::search::SearchPreferencesHandle>,
    provider_catalog: Option<crate::shell::search::SearchProviderCatalogHandle>,
    /// Every row this provider offers, built once per state they are built
    /// from rather than once per keystroke or lookup.
    catalogue: std::cell::RefCell<Option<Rc<Catalogue>>>,
    /// How many times the catalogue has been built.
    #[cfg(test)]
    builds: Rc<std::cell::Cell<usize>>,
}

/// The shell's action rows for one [`CatalogueKey`], before any query
/// narrows them.
struct Catalogue {
    key: CatalogueKey,
    rows: Vec<SearchResult>,
    /// Each row's searchable text, in row order.
    fields: Vec<Vec<(String, FieldWeight)>>,
    /// The first row with each id.
    by_id: std::collections::HashMap<String, usize>,
    /// The per-source on/off switches, offered only to a query about search
    /// itself ([`asks_about_search_sources`]).
    source_switches: std::ops::Range<usize>,
    /// Whether a display change waits for Keep or Revert, as the settings
    /// document the catalogue was built from says: the catalogue is rebuilt
    /// when that document changes, so this is never read per keystroke.
    display_waiting: bool,
}

/// Everything the catalogue's rows are built from: whether a row exists,
/// what it says, its contract, its parameters and what its callbacks hold.
/// Invocation still rechecks authority against the live workspace.
#[derive(Clone, PartialEq, Eq)]
struct CatalogueKey {
    state: CatalogueState,
    /// Whose ownership and role actions the governance rows offer.
    actor: crate::shell::state::UserId,
}

/// The part of [`CatalogueKey`] that is cheap to read on every palette
/// refresh, and so also the provider's search revision.
#[derive(Clone, PartialEq, Eq, Hash)]
struct CatalogueState {
    chrome: u64,
    /// Workspaces, their names and governance: the chrome bumps this on every
    /// tree mutation it makes...
    chrome_revision: u64,
    /// ...and mutations from outside the chrome bump this, until the next
    /// tick folds them into `chrome_revision`.
    external_tree_revision: u64,
    locale: String,
    peer_sync: bool,
    /// Whether the person at the screen may invite into the active workspace
    /// ([`ChromeState::may_invite_here`]): the `peer.invite` rows are offered
    /// only then, as the invite composer is.
    invite_offered: bool,
    collab: bool,
    active_workspace: Option<crate::shell::state::WorkspaceId>,
    /// What the rows that show a setting's state say: Do Not Disturb, axis
    /// navigation.
    states: [bool; 2],
    /// The settings document the rows read: the appearance mode and high
    /// contrast their badges show, and the shortcuts and keyboard layout
    /// their keycaps spell.
    settings: Option<settings_store::SettingsStamp>,
    /// The search sources and whether each is on, for their switches.
    search_sources: Vec<(String, String, bool)>,
}

impl ActionsProvider {
    pub fn new() -> Self {
        Self {
            search_preferences: None,
            provider_catalog: None,
            ranker: ListIndex::default().with_ranks(ui::search::system::action_ranks),
            catalogue: std::cell::RefCell::default(),
            #[cfg(test)]
            builds: Rc::default(),
        }
    }

    pub(crate) fn with_all_sources(
        search_preferences: crate::shell::search::SearchPreferencesHandle,
        provider_catalog: crate::shell::search::SearchProviderCatalogHandle,
    ) -> Self {
        Self {
            search_preferences: Some(search_preferences),
            provider_catalog: Some(provider_catalog),
            ranker: ListIndex::default().with_ranks(ui::search::system::action_ranks),
            catalogue: std::cell::RefCell::default(),
            #[cfg(test)]
            builds: Rc::default(),
        }
    }

    pub fn with_preferences(
        search_preferences: crate::shell::search::SearchPreferencesHandle,
    ) -> Self {
        Self {
            search_preferences: Some(search_preferences),
            provider_catalog: None,
            ..Self::new()
        }
    }

    pub fn with_preferences_and_catalog(
        search_preferences: crate::shell::search::SearchPreferencesHandle,
        provider_catalog: crate::shell::search::SearchProviderCatalogHandle,
    ) -> Self {
        Self {
            search_preferences: Some(search_preferences),
            provider_catalog: Some(provider_catalog),
            ..Self::new()
        }
    }
}

impl Default for ActionsProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// The provider id the shell's own actions are addressed by.
pub(crate) const PROVIDER_ID: &str = "actions";

/// The id of the action selecting the workspace `id` names in its form from
/// before action ids named the room (`workspace.select.<number>`): that
/// number is a workspace's id here, or one a workspace carried before
/// ([`crate::shell::state::WorkspaceTree::resolve_workspace`]). `None` for
/// any other id.
fn current_workspace_select(
    id: &str,
    chrome: &crate::shell::chrome_state::ChromeState,
) -> Option<String> {
    let number: u64 = id.strip_prefix("workspace.select.")?.parse().ok()?;
    let room = {
        let tree = chrome.tree();
        tree.resolve_workspace(crate::shell::state::WorkspaceId(number))
            .and_then(|workspace| tree.room_of(workspace))
    }?;
    Some(format!("workspace.select.{room}"))
}

impl SearchProvider for ActionsProvider {
    fn row_icon(&self, result: &SearchResult) -> Option<ui::icons::Icon> {
        action_icon(&result.id)
    }
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }
    fn name(&self) -> &'static str {
        "Actions"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::Action]
    }
    fn toggles_in_place(&self, result: &SearchResult) -> bool {
        toggles_in_place(&result.id)
    }
    fn match_reason(&self, result: &SearchResult, needle: &str) -> Option<String> {
        let catalogue = self.catalogue.borrow().clone()?;
        let at = *catalogue.by_id.get(&result.id)?;
        crate::shell::search::match_reason_in(needle, result, catalogue.fields.get(at)?)
    }
    /// An action with nothing to act on (closing a sheet that is not open,
    /// declining a pairing nobody asked for, stopping a follow that is not
    /// happening) only takes a row from one that would do something: it is
    /// offered only when the query names it exactly, where running it says
    /// why not.
    fn hidden_from_people(
        &self,
        result: &SearchResult,
        needle: &str,
        chrome: &ChromeState,
    ) -> bool {
        let display_waiting = self
            .catalogue
            .borrow()
            .as_ref()
            .is_some_and(|catalogue| catalogue.display_waiting);
        !action_available(&result.id, chrome, display_waiting)
            && tilo_search::fold(needle.trim()) != tilo_search::fold(&result.title)
    }
    /// An address is looked up in the whole table. The shell offers more
    /// actions than one palette page shows, so the first page of an empty
    /// query would leave the later ones unreachable by id.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return None;
        }
        let found = if let Some(action) = Action::find(id) {
            // Workspace navigation bumps the chrome revision on each invocation.
            // Rebuilding the whole palette here also rebuilt the shortcut table
            // for every bound row, synchronously on the compositor's loop.
            self.static_action(
                action,
                ctx,
                &crate::shell::actions::host::Live::read(ctx.chrome),
            )
        } else {
            let catalogue = self.catalogue(&ctx);
            // An action id from before ids named the room selects the
            // workspace that number named, through the device's aliases.
            let current = current_workspace_select(id, ctx.chrome);
            let lookup = current.as_deref().unwrap_or(id);
            match catalogue.by_id.get(lookup) {
                Some(&at) => catalogue.rows.get(at).cloned().map(|mut row| {
                    id.clone_into(&mut row.id);
                    row
                }),
                None => crate::shell::notification::alarm_action_rows(ctx.chrome)
                    .into_iter()
                    .find(|row| row.id == id),
            }
        };
        found.map(|mut row| {
            // What an empty query scores it.
            row.score = crate::shell::search::provider_score(0);
            row
        })
    }

    fn search_revision(&self, ctx: &SearchContext<'_>) -> String {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.catalogue_state(ctx).hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if !ctx.wanted.contains(&ResourceKind::Action) {
            return Vec::new();
        }
        let catalogue = self.catalogue(&ctx);
        // Alarm actions follow the notification log, so they are read fresh.
        let alarms = crate::shell::notification::alarm_action_rows(ctx.chrome);
        let mut alarm_fields = ActionCandidates::default();
        for row in &alarms {
            alarm_fields.record(
                &row.title.to_lowercase(),
                &row.subtitle.clone().unwrap_or_default().to_lowercase(),
                &row.id,
                Vec::new(),
            );
        }
        let withheld = if asks_about_search_sources(ctx.needle) {
            0..0
        } else {
            catalogue.source_switches.clone()
        };
        let offered = || {
            catalogue
                .rows
                .iter()
                .zip(&catalogue.fields)
                .enumerate()
                .filter(|(at, _)| !withheld.contains(at))
                .map(|(_, offered)| offered)
        };
        let mut ranks = self
            .ranker
            .ranks(
                ctx.needle,
                offered()
                    .map(|(_, fields)| fields)
                    .chain(&alarm_fields.0)
                    .map(|fields| fields.iter().map(|(text, weight)| (text.as_str(), *weight))),
            )
            .into_iter();
        let mut out = Vec::new();
        for (row, _) in offered() {
            if let Some(rank) = ranks.next().flatten() {
                out.push(SearchResult {
                    score: crate::shell::search::provider_score(rank),
                    ..row.clone()
                });
            }
        }
        for (mut row, rank) in alarms.into_iter().zip(ranks) {
            if let Some(rank) = rank {
                row.score = crate::shell::search::provider_score(rank);
                out.push(row);
            }
        }
        // Equal matches: what is chosen most comes first, then the action
        // that does it in one step before the one asking for input, and a
        // switch before a view of the same thing, so "wi-fi" finds the
        // switch ahead of the setter it drives and the list of networks.
        crate::shell::search::usage::order_ties_by(&mut out, |row| {
            i32::from(row.params.is_empty())
                .saturating_mul(2)
                .saturating_add(i32::from(toggles_in_place(&row.id)))
        });
        out
    }
}

impl ActionsProvider {
    /// The catalogue for the current state, built only if that is not the
    /// state the retained one was built from.
    fn catalogue(&self, ctx: &SearchContext<'_>) -> Rc<Catalogue> {
        let key = CatalogueKey {
            state: self.catalogue_state(ctx),
            actor: ctx.chrome.active_local_user(),
        };
        let retained = self.catalogue.borrow().clone();
        if let Some(catalogue) = retained.filter(|catalogue| catalogue.key == key) {
            return catalogue;
        }
        let catalogue = Rc::new(self.build_catalogue(*ctx, key));
        *self.catalogue.borrow_mut() = Some(Rc::clone(&catalogue));
        catalogue
    }

    fn catalogue_state(&self, ctx: &SearchContext<'_>) -> CatalogueState {
        let tree = ctx.chrome.tree();
        let external_tree_revision = tree.desktop.external_revision;
        let active_workspace = tree
            .desktop
            .active_workspace()
            .map(|workspace| workspace.id);
        drop(tree);
        CatalogueState {
            chrome: ctx.chrome.instance(),
            chrome_revision: ctx.chrome.revision,
            external_tree_revision,
            locale: crate::shell::i18n::current_locale(),
            peer_sync: ctx.chrome.peer_sync_enabled,
            invite_offered: ctx.chrome.may_invite_here(),
            collab: ctx.chrome.collab_available(),
            active_workspace,
            states: [ctx.chrome.dnd(), ctx.chrome.axis_navigation_enabled()],
            settings: settings_store::stamp(),
            search_sources: self.search_sources(),
        }
    }

    /// Each search source and whether it is on: what its switch says and
    /// does. Empty without the preferences the switches write.
    fn search_sources(&self) -> Vec<(String, String, bool)> {
        let (Some(preferences), Some(catalog)) = (&self.search_preferences, &self.provider_catalog)
        else {
            return Vec::new();
        };
        let preferences = preferences.borrow();
        let sources = catalog
            .borrow()
            .iter()
            .map(|(id, name)| (id.clone(), name.clone(), preferences.provider_enabled(id)))
            .collect();
        drop(preferences);
        sources
    }

    /// Build one table action, shared by palette enumeration and exact invocation.
    /// Exact addresses must not build unrelated rows after every chrome mutation.
    fn static_action(
        &self,
        action: &'static Action,
        ctx: SearchContext<'_>,
        live: &crate::shell::actions::host::Live<'_>,
    ) -> Option<SearchResult> {
        // The row shows what the shell publishes of the action; one it does
        // not offer here has no row.
        let spec = action.spec(live);
        if matches!(
            &spec.contract.availability,
            tilo_actions::Availability::Unavailable { code, .. } if code == "not_offered"
        ) {
            return None;
        }
        let label = spec.label.clone();
        let subtitle = spec.description.clone().unwrap_or_default();
        {
            let score = 0;
            let creation_context = (action.id == "workspace.create")
                .then(|| workspace_create_context(ctx.chrome).ok());
            let on_open = Rc::new(
                move |_doc: &mut ui::widget::Edits<'_>,
                      _root: &automerge::ObjId,
                      chrome: &Chrome,
                      args: &ActionArgs| {
                    if let Some(expected) = &creation_context
                        && workspace_create_context(&chrome.borrow()).ok().as_ref()
                            != expected.as_ref()
                    {
                        return Err(ActionError::Refused(
                            "workspace creation context changed; resolve the action again".into(),
                        ));
                    }
                    apply(chrome, action, args)
                },
            ) as Rc<crate::shell::search::OpenFn>;
            let lane = action.lane();
            let on_begin_invoke = (lane == Lane::Lifecycle).then(|| {
                Rc::new(
                    move |_doc: &mut ui::widget::Edits<'_>,
                          _root: &automerge::ObjId,
                          chrome: &Chrome,
                          args: &ActionArgs| begin(chrome, action, args),
                ) as Rc<crate::shell::search::BeginInvokeFn>
            });
            let on_invoke = (lane == Lane::Answer).then(|| {
                Rc::new(
                    move |_doc: &mut ui::widget::Edits<'_>,
                          _root: &automerge::ObjId,
                          chrome: &Chrome,
                          args: &ActionArgs| {
                        match begin(chrome, action, args)? {
                            crate::shell::search::InvocationDisposition::Accepted(receipt) => {
                                Ok(receipt)
                            }
                            crate::shell::search::InvocationDisposition::Pending(_) => {
                                Err(ActionError::Refused(format!("{} answers later", action.id)))
                            }
                        }
                    },
                ) as Rc<crate::shell::search::InvokeFn>
            });
            Some(SearchResult {
                facts: Vec::new(),
                shortcut: spec.state.shortcut.clone(),
                kind: ResourceKind::Action,
                operation_kind: action.kind,
                provider_id: self.id().to_string(),
                id: action.id.to_string(),
                title: label,
                subtitle: Some(subtitle),
                badge: spec.state.badge.clone(),
                score,
                params: spec.params().to_vec(),
                contract: Some(action.contract()),
                on_open,
                on_invoke,
                on_begin_invoke,
                resource: None,
            })
        }
    }

    /// Build every row the provider offers, whatever the needle; see
    /// [`CatalogueKey`] for what they are built from.
    #[expect(
        clippy::too_many_lines,
        reason = "the catalog lists every shell action with its contract in one table-like pass"
    )]
    fn build_catalogue(&self, ctx: SearchContext<'_>, key: CatalogueKey) -> Catalogue {
        #[cfg(test)]
        self.builds.set(self.builds.get().wrapping_add(1));
        let mut out = Vec::new();
        // Every row is built with its searchable text; a query ranks them
        // together to keep the matches and score them.
        let mut candidates = ActionCandidates::default();
        let live = crate::shell::actions::host::Live::read(ctx.chrome);
        for action in Action::ALL {
            if let Some(row) = self.static_action(action, ctx, &live) {
                candidates.record(
                    &row.title.to_lowercase(),
                    &row.subtitle.as_deref().unwrap_or_default().to_lowercase(),
                    &row.id,
                    declared_keywords(action.keywords),
                );
                out.push(row);
            }
        }
        let descriptors = tilo_desktop::activity::view_actions::workspace_view_descriptors(
            &ctx.chrome.tree().desktop,
            true,
        );
        for descriptor in descriptors {
            let (subtitle, keywords) = workspace_view_copy(&descriptor.request);
            candidates.record(
                &descriptor.label.to_lowercase(),
                &format!("{subtitle} {keywords}").to_lowercase(),
                &descriptor.id,
                Vec::new(),
            );
            let score = 0;
            let request = descriptor.request;
            out.push(SearchResult {
                facts: Vec::new(),
                shortcut: None,
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::View,
                provider_id: self.id().into(),
                id: descriptor.id,
                title: descriptor.label,
                subtitle: Some(subtitle),
                badge: None,
                score,
                params: vec![],
                contract: Some(tilo_actions::ActionContract::view()),
                on_open: Rc::new(move |_, _, chrome, _| {
                    chrome
                        .borrow_mut()
                        .request_workspace_view(request.clone())
                        .map_err(ActionError::Refused)
                }),
                on_invoke: None,
                on_begin_invoke: None,
                resource: None,
            });
        }
        // Each archived workspace: bring it back, or delete it with its
        // room, as workspace settings offer.
        let archived: Vec<(crate::shell::state::RoomId, String)> = ctx
            .chrome
            .tree()
            .archived_workspaces
            .iter()
            .map(|workspace| (workspace.room(), workspace.name.clone()))
            .collect();
        for (room, name) in archived {
            for delete in [false, true] {
                let (id, title, subtitle) = if delete {
                    (
                        format!("workspace.archived.delete.{room}"),
                        t_args(
                            "search-action-archived-delete",
                            &[("workspace", name.clone().into())],
                        ),
                        t("search-action-archived-delete-subtitle"),
                    )
                } else {
                    (
                        format!("workspace.archived.restore.{room}"),
                        t_args(
                            "search-action-archived-restore",
                            &[("workspace", name.clone().into())],
                        ),
                        t("search-action-archived-restore-subtitle"),
                    )
                };
                candidates.record(
                    &title.to_lowercase(),
                    &subtitle.to_lowercase(),
                    &id,
                    declared_keywords(if delete {
                        "search-kw-workspace-archived-delete"
                    } else {
                        "search-kw-workspace-archived-restore"
                    }),
                );
                let mut contract = tilo_actions::ActionContract::view();
                if delete {
                    contract.kind = tilo_actions::ActionKind::Outside;
                    contract.risk = tilo_actions::RiskLevel::Destructive;
                    contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
                    contract.retry = tilo_actions::RetryPolicy::AtMostOnce;
                }
                out.push(SearchResult {
                    facts: Vec::new(),
                    shortcut: None,
                    kind: ResourceKind::Action,
                    operation_kind: tilo_actions::OperationKind::View,
                    provider_id: self.id().into(),
                    id,
                    title,
                    subtitle: Some(subtitle),
                    badge: None,
                    score: 0,
                    params: vec![],
                    contract: Some(contract),
                    on_open: Rc::new(move |_, _, chrome, _| {
                        let mut state = chrome.borrow_mut();
                        let done = if delete {
                            state.delete_archived_workspace(room)
                        } else {
                            state.restore_archived_workspace(room)
                        };
                        if done {
                            Ok(())
                        } else {
                            Err(ActionError::Refused(
                                "that archived workspace is not here".into(),
                            ))
                        }
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                    resource: None,
                });
            }
        }
        let morning = ctx.chrome.tree().active_workspace().map(|workspace| {
            (
                workspace.id,
                workspace.room(),
                crate::shell::room_activity::morning_source_actions(workspace),
            )
        });
        if let Some((workspace, room, actions)) = morning {
            for (id, label) in actions {
                let subtitle = t("activity-morning-description");
                candidates.record(
                    &label.to_lowercase(),
                    &subtitle.to_lowercase(),
                    &id,
                    Vec::new(),
                );
                let command = id.starts_with(tilo_desktop::activity::morning::MORNING_REFRESH)
                    || id.starts_with(tilo_desktop::activity::morning::MORNING_DRAFT);
                let kind = if command {
                    tilo_actions::OperationKind::Effect
                } else {
                    tilo_actions::OperationKind::View
                };
                let contract = if command {
                    tilo_actions::ActionContract {
                        kind: tilo_actions::ActionKind::Read,
                        ..Default::default()
                    }
                } else {
                    tilo_actions::ActionContract::view()
                };
                let request = id.clone();
                out.push(SearchResult {
                    facts: Vec::new(),
                    shortcut: None,
                    kind: ResourceKind::Action,
                    operation_kind: kind,
                    provider_id: self.id().into(),
                    id,
                    title: label,
                    subtitle: Some(subtitle),
                    badge: None,
                    score: 0,
                    params: vec![],
                    contract: Some(contract),
                    on_open: Rc::new(move |_, _, chrome, _| {
                        let mut state = chrome.borrow_mut();
                        if !state
                            .tree()
                            .active_workspace()
                            .is_some_and(|active| active.id == workspace && active.room() == room)
                        {
                            return Err(ActionError::Refused("Workspace changed".into()));
                        }
                        state
                            .request_history_selection_action(&request)
                            .map_err(ActionError::Refused)
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                    resource: None,
                });
            }
        }
        for &(id, label_key, subtitle_key, keywords_key) in HISTORY_VIEWS {
            let label = t(label_key);
            let subtitle = t(subtitle_key);
            let keywords = t(keywords_key);
            candidates.record(
                &label.to_lowercase(),
                &format!("{subtitle} {keywords}").to_lowercase(),
                id,
                Vec::new(),
            );
            out.push(SearchResult {
                facts: Vec::new(),
                shortcut: None,
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::View,
                provider_id: self.id().into(),
                id: id.to_string(),
                title: label,
                subtitle: Some(subtitle),
                badge: None,
                score: 0,
                params: vec![],
                contract: Some(tilo_actions::ActionContract::view()),
                on_open: Rc::new(move |_, _, chrome, _| {
                    chrome
                        .borrow_mut()
                        .request_history_selection_action(id)
                        .map_err(ActionError::Refused)
                }),
                on_invoke: None,
                on_begin_invoke: None,
                resource: None,
            });
        }
        let actor = key.actor.clone();
        let profile_resource = {
            let tree = ctx.chrome.tree();
            tree.desktop.active_workspace().map(|workspace| {
                tilo_desktop::workspace_settings::governance_resource(workspace, &actor)
            })
        };
        if let Some(resource) = profile_resource {
            let mut surfaces = vec![(resource.id.as_str().to_string(), resource.actions)];
            surfaces.extend(
                resource
                    .children
                    .into_iter()
                    .map(|child| (child.id.as_str().to_string(), child.actions)),
            );
            for (resource_id, actions) in surfaces {
                for action in actions {
                    let subtitle = action
                        .description
                        .clone()
                        .unwrap_or_else(|| t("search-action-permission-roles-subtitle"));
                    candidates.record(
                        &action.label.to_lowercase(),
                        &subtitle.to_lowercase(),
                        &action.id,
                        action.keywords.clone(),
                    );
                    let score = 0;
                    let spec = action.clone();
                    let params = action.params().to_vec();
                    out.push(SearchResult {
                        facts: Vec::new(),
                        shortcut: None,
                        kind: ResourceKind::Action,
                        operation_kind: tilo_actions::OperationKind::Document,
                        provider_id: self.id().to_string(),
                        id: format!("{}#{}", resource_id, action.id),
                        title: action.label.clone(),
                        subtitle: Some(subtitle),
                        badge: None,
                        score,
                        params,
                        contract: Some(action.contract.clone()),
                        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, arguments| {
                            let mut state = chrome.borrow_mut();
                            apply_governance_action(&state, &spec, arguments)?;
                            state.bump();
                            Ok(())
                        }),
                        on_invoke: None,
                        on_begin_invoke: None,
                        resource: None,
                    });
                }
            }
        }
        let mut source_switches = 0..0;
        if let Some(preferences) = &self.search_preferences {
            let id = "search.provider.set";
            let label = t("search-action-search-provider-set");
            let subtitle = t("search-action-search-provider-set-subtitle");
            {
                candidates.record(
                    &label.to_lowercase(),
                    &subtitle.to_lowercase(),
                    id,
                    Vec::new(),
                );
                let score = 0;
                let preferences = Rc::clone(preferences);
                out.push(SearchResult {
            facts: Vec::new(),
                    shortcut: None,
                    kind: ResourceKind::Action,
                    operation_kind: tilo_actions::OperationKind::Effect,
                    provider_id: self.id().to_string(),
                    id: id.to_string(),
                    title: label,
                    subtitle: Some(subtitle),
                    badge: None,
                    score,
                    params: SEARCH_PROVIDER_SET_PARAMS.to_vec(),
                    contract: None,
                    on_open: Rc::new(move |_doc, _root, chrome: &Chrome, args: &ActionArgs| {
                        let provider = args.text("provider").unwrap_or_default().trim();
                        let enabled = args.boolean("enabled").unwrap_or(true);
                        if provider.is_empty() {
                            return Err(ActionError::Refused(
                                "a search provider id is required".into(),
                            ));
                        }
                        if provider == "actions" && !enabled {
                            return Err(ActionError::Refused(
                                "the Actions source cannot be disabled because it is the shared accessibility and automation door".into(),
                            ));
                        }
                        let mut next = preferences.borrow().clone();
                        next.set_provider_enabled(provider, enabled);
                        next.save().map_err(|error| {
                            ActionError::Refused(format!(
                                "could not save the search-source preference: {error}"
                            ))
                        })?;
                        *preferences.borrow_mut() = next;
                        chrome.borrow_mut().bump();
                        Ok(())
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                    resource: None,
                });
            }
            if let Some(catalog) = &self.provider_catalog {
                source_switches.start = out.len();
                for (provider_id, provider_name) in catalog.borrow().iter() {
                    if provider_id == "actions" {
                        continue;
                    }
                    let enabled = preferences.borrow().provider_enabled(provider_id);
                    let source: &[(&str, ui::i18n::ArgValue)] = &[
                        ("source", provider_name.clone().into()),
                        ("id", provider_id.clone().into()),
                    ];
                    let (label, subtitle) = if enabled {
                        (
                            t_args("search-action-provider-disable", source),
                            t_args("search-action-provider-disable-subtitle", source),
                        )
                    } else {
                        (
                            t_args("search-action-provider-enable", source),
                            t_args("search-action-provider-enable-subtitle", source),
                        )
                    };
                    let id = format!("search.provider.toggle:{provider_id}");
                    candidates.record(
                        &label.to_lowercase(),
                        &format!("{subtitle} {}", t("search-action-provider-keywords"))
                            .to_lowercase(),
                        &id,
                        Vec::new(),
                    );
                    let score = 0;
                    let preferences = Rc::clone(preferences);
                    let provider_id = provider_id.clone();
                    out.push(SearchResult {
                        facts: Vec::new(),
                        shortcut: None,
                        kind: ResourceKind::Action,
                        operation_kind: tilo_actions::OperationKind::Effect,
                        provider_id: self.id().to_string(),
                        id,
                        title: label,
                        subtitle: Some(subtitle),
                        badge: Some(if enabled {
                            t("search-action-provider-badge-on")
                        } else {
                            t("search-action-provider-badge-off")
                        }),
                        score,
                        params: Vec::new(),
                        contract: None,
                        on_open: Rc::new(move |_doc, _root, chrome: &Chrome, _args| {
                            let mut next = preferences.borrow().clone();
                            next.set_provider_enabled(&provider_id, !enabled);
                            next.save().map_err(|error| {
                                ActionError::Refused(format!(
                                    "could not save the search-source preference: {error}"
                                ))
                            })?;
                            *preferences.borrow_mut() = next;
                            chrome.borrow_mut().bump();
                            Ok(())
                        }),
                        on_invoke: None,
                        on_begin_invoke: None,
                        resource: None,
                    });
                }
                source_switches.end = out.len();
            }
        }
        // Each row that has a shortcut shows it, as the keyboard layout in use
        // spells it.
        let shortcuts = settings_store::hardware::ShortcutConfig::load();
        for row in &mut out {
            row.shortcut = action_shortcut(&shortcuts, &row.id);
        }
        let mut by_id = std::collections::HashMap::with_capacity(out.len());
        for (at, row) in out.iter().enumerate() {
            by_id.entry(row.id.clone()).or_insert(at);
        }
        Catalogue {
            key,
            rows: out,
            fields: candidates.0,
            by_id,
            source_switches,
            display_waiting: display_change_waiting(),
        }
    }
}

/// Whether a query is about search itself, so a per-source on/off switch is
/// an answer to it. "notes" looks for notes, not for hiding them; an empty
/// listing and an exact `search.provider.toggle:…` address still see every
/// switch.
fn asks_about_search_sources(needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() || needle.starts_with("search.provider.") {
        return true;
    }
    let keywords = t("search-action-provider-keywords").to_lowercase();
    needle
        .split_whitespace()
        .filter(|word| word.chars().count() >= 3)
        .any(|word| {
            keywords
                .split_whitespace()
                .any(|keyword| keyword.starts_with(word))
        })
}

/// A workspace view's subtitle and the extra words it answers to. Switching
/// to a workspace is not an answer to "history", and each Activity view says
/// what it shows rather than that it is a view.
fn workspace_view_copy(
    request: &tilo_desktop::activity::view_actions::WorkspaceViewRequest,
) -> (String, String) {
    use tilo_desktop::activity::ActivityView;
    use tilo_desktop::activity::view_actions::WorkspaceViewRequest;
    let activity = |view: ActivityView| {
        let subtitle = match view {
            ActivityView::CatchUp => "search-action-activity-catch-up-subtitle",
            ActivityView::Explore => "search-action-activity-explore-subtitle",
            ActivityView::Replay => "search-action-activity-replay-subtitle",
        };
        (t(subtitle), t("search-action-activity-keywords"))
    };
    match request {
        WorkspaceViewRequest::Select { .. } => (
            t("search-action-workspace-switch-subtitle"),
            t("search-action-workspace-switch-keywords"),
        ),
        WorkspaceViewRequest::OpenActivity { view, .. } => activity(*view),
        WorkspaceViewRequest::CloseActivity { .. } => activity(ActivityView::CatchUp),
    }
}

/// A typed action id is a verbatim identity — scripts and power users name
/// actions by it — so it weighs close to the label.
pub(crate) const ACTION_ID_WEIGHT: FieldWeight = FieldWeight::new(90);

/// Each action row's label, id and subtitle, in row order.
#[derive(Default)]
struct ActionCandidates(Vec<Vec<(String, FieldWeight)>>);

impl ActionCandidates {
    /// Record the searchable text of the row pushed next: its label, id
    /// and subtitle, and the other words its declaration gives for it.
    fn record(&mut self, label: &str, subtitle: &str, id: &str, keywords: Vec<String>) {
        let mut row = vec![
            (label.to_owned(), FieldWeight::TITLE),
            (id.to_owned(), ACTION_ID_WEIGHT),
            (subtitle.to_owned(), FieldWeight::SUBTITLE),
        ];
        row.extend(
            keywords
                .into_iter()
                .map(|word| (word, FieldWeight::KEYWORD)),
        );
        // "Join a workspace…" is also found by the community site's name
        // ("tilo.coffee"), which is the image's configuration, not a word.
        if id == "peer.connect"
            && let Some(site) = crate::shell::network::configured_community_site()
            && let Some(host) = site.host_str()
        {
            row.push((host.to_owned(), FieldWeight::KEYWORD));
        }
        self.0.push(row);
    }
}

#[cfg(test)]
mod keyword_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::shell::search::action_api::ParamType;
    use crate::shell::shared_display_actions;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::WorkspaceAuthority as _;
    use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
    use automerge::AutoCommit;
    use settings_store::display_mode::DisplayLayoutMode;
    use std::sync::Arc;

    #[test]
    fn adding_a_bot_asks_the_active_workspace_once_the_person_confirms() {
        use crate::shell::network::BotAsk;
        use crate::shell::overlay_host::PeerSyncAction;
        let chrome = chrome_with_room();
        let room = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .room();
        let standup = serde_json::json!({
            "entry": "so.tilo.bots.standup",
            "version": "1.2.0",
        });
        assert!(
            invoke_shell_action(&chrome, BOT_ADD_ID, &standup).is_err(),
            "nothing is asked of the workspace before the person confirms"
        );
        assert!(chrome.borrow_mut().take_peer_sync_requests().is_empty());
        let mut confirmed = standup;
        confirmed["confirm"] = true.into();
        invoke_shell_action(&chrome, BOT_ADD_ID, &confirmed).unwrap();
        assert_eq!(
            chrome.borrow_mut().take_peer_sync_requests(),
            vec![PeerSyncAction::AskForBot {
                room,
                ask: BotAsk::Add {
                    entry: "so.tilo.bots.standup".into(),
                    version: "1.2.0".into(),
                },
            }]
        );
        assert!(
            invoke_shell_action(
                &chrome,
                BOT_ADD_ID,
                &serde_json::json!({"entry": "../standup", "version": "1.2.0", "confirm": true}),
            )
            .is_err(),
            "a bot is a directory entry, never a path"
        );
        invoke_shell_action(
            &chrome,
            BOT_REMOVE_ID,
            &serde_json::json!({"entry": "so.tilo.bots.standup", "confirm": true}),
        )
        .unwrap();
        assert_eq!(
            chrome.borrow_mut().take_peer_sync_requests(),
            vec![PeerSyncAction::AskForBot {
                room,
                ask: BotAsk::Remove {
                    entry: "so.tilo.bots.standup".into(),
                },
            }]
        );
        // An app may prepare adding a bot for the person, and nothing else.
        assert!(app_offerable_label(BOT_ADD_ID).is_some());
        assert!(app_offerable_label(BOT_REMOVE_ID).is_none());
        assert!(app_offerable_label(WORKSPACE_PUBLISH_ID).is_none());
    }

    #[test]
    fn display_outputs_inspection_reads_the_published_displays_and_the_waiting_change() {
        use settings_store::hardware::{
            DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplayTransaction,
            DisplayTransactionState, LiveOutputPlacement, OutputDescriptor, OutputMode,
        };
        let _home = tilo_test_env::TestHome::new().unwrap();
        let chrome = chrome_with_room();
        assert!(
            invoke_shell_action(&chrome, DISPLAY_OUTPUTS_INSPECT_ID, &serde_json::json!({}))
                .is_err(),
            "with nothing published there is nothing to inspect"
        );
        let mode = OutputMode {
            width: 1920,
            height: 1080,
            refresh_mhz: 60_000,
        };
        let output = |connector: &str, builtin: bool| DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: connector.into(),
                modes: vec![mode],
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: mode,
            scale: if builtin { 2.0 } else { 1.0 },
            builtin,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            unavailable_reason: None,
            color_profile_labels: std::collections::BTreeMap::new(),
        };
        let (publisher_pid, publisher_start_ticks) =
            settings_store::hardware::publisher_identity().unwrap();
        let outputs = vec![output("eDP-1", true), output("HDMI-A-1", false)];
        DisplayHardwareSnapshot {
            publisher_pid,
            publisher_start_ticks,
            outputs: outputs.clone(),
            layout: vec![LiveOutputPlacement {
                connector: "HDMI-A-1".into(),
                x: 960,
                y: 0,
                width: 1920,
                height: 1080,
            }],
            ..DisplayHardwareSnapshot::default()
        }
        .save()
        .unwrap();
        let inspect = |chrome: &Chrome| {
            let Ok(crate::shell::search::InvocationDisposition::Accepted(receipt)) =
                invoke_shell_action(chrome, DISPLAY_OUTPUTS_INSPECT_ID, &serde_json::json!({}))
            else {
                panic!("inspection completes immediately")
            };
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
                panic!("inspection returns the displays")
            };
            output.value.unwrap()
        };
        let value = inspect(&chrome);
        let listed = value["outputs"].as_array().unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0]["output"], "eDP-1");
        assert_eq!(listed[0]["builtin"], true);
        assert_eq!(listed[0]["scale"], 2.0);
        assert_eq!(listed[1]["rect"]["x"], 960);
        assert_eq!(listed[1]["mode"]["width"], 1920);
        assert!(value["waiting"].is_null());

        let set = settings_store::display_mode::attached_set(&outputs);
        let candidate = settings_store::display_mode::plan_layout(
            DisplayLayoutMode::Mirror,
            &outputs,
            &settings_store::hardware::DisplayConfig::default(),
        )
        .unwrap();
        let mut staged = DisplayTransaction::request(set, candidate).unwrap();
        staged.state = DisplayTransactionState::AwaitingConfirmation;
        staged.save().unwrap();
        let value = inspect(&chrome);
        assert_eq!(value["waiting"]["transaction"], staged.id);
        assert_eq!(value["waiting"]["keep"], DISPLAY_KEEP_ID);
        assert_eq!(value["layout"], DISPLAY_MIRROR_ID);
        assert_eq!(value["outputs"][1]["mirror_of"], "eDP-1");
        assert_eq!(
            declared(DISPLAY_OUTPUTS_INSPECT_ID).1,
            tilo_actions::OperationKind::View
        );
    }

    #[test]
    fn shared_display_public_actions_inspect_and_edit_only_the_current_certified_pair() {
        let _home = shared_display_actions::test_support::publish();
        let chrome = chrome_with_room();
        let inspect = invoke_shell_action(
            &chrome,
            shared_display_actions::INSPECT_ID,
            &serde_json::json!({}),
        )
        .unwrap();
        let crate::shell::search::InvocationDisposition::Accepted(receipt) = inspect else {
            panic!("inspection completes immediately")
        };
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = receipt.update else {
            panic!("inspection returns current facts")
        };
        let value = output.value.unwrap();
        let outputs = value["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 3);
        assert!(
            outputs
                .iter()
                .any(|row| row["device"] == "bb02" && row["number"] == 2 && row["revision"] == 0)
        );
        assert!(
            outputs
                .iter()
                .any(|row| row["device"] == "cc03" && row["online"] == false)
        );
        let before = settings_store::load();
        let args =
            serde_json::json!({"device":"bb02","output":"panel","revision":0,"included":false});
        invoke_shell_action(&chrome, shared_display_actions::INCLUDE_ID, &args).unwrap();
        let saved = settings_store::person_displays::Arrangement::load();
        assert!(!saved.entry("bb02", "panel").unwrap().enabled);
        assert!(
            saved.entry("aa01", "panel").unwrap().enabled,
            "same output ID on another device is independent"
        );
        assert_eq!(
            settings_store::load().get("continuity"),
            before.get("continuity"),
            "inclusion cannot grant consent or input"
        );
        let after = settings_store::load();
        assert!(
            invoke_shell_action(&chrome, shared_display_actions::INCLUDE_ID, &args).is_err(),
            "old arrangement revision must fail"
        );
        assert!(invoke_shell_action(&chrome, shared_display_actions::INCLUDE_ID,
            &serde_json::json!({"device":"bb02","output":"connector-guess","revision":0,"included":true})).is_err());
        assert!(invoke_shell_action(&chrome, shared_display_actions::INCLUDE_ID,
            &serde_json::json!({"device":"bb02","output":"panel","revision":0,"included":"true"})).is_err());
        assert_eq!(settings_store::load(), after);
    }

    #[test]
    fn selected_shared_display_action_rechecks_workspace_authority_before_writing() {
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        let _home = shared_display_actions::test_support::publish();
        let chrome = chrome_with_room();
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(Rc::new(ActionsProvider::new()));
        let mut document = AutoCommit::new();
        let resource = ResourceRef::new("actions", shared_display_actions::INCLUDE_ID);
        let catalogue = ActionCatalog::build_exact(
            &registry,
            &resource,
            crate::shell::search::SearchScope::All,
            None,
            &chrome.borrow(),
            &document,
            &automerge::ROOT,
            &AllVisible,
        );
        let action = catalogue
            .select(&resource)
            .expect("published inclusion action");
        chrome
            .borrow_mut()
            .tree_mut()
            .desktop
            .active_workspace_mut()
            .unwrap()
            .set_owner("foreign-owner");
        assert!(!current_shell_action_authorized(
            &chrome.borrow(),
            shared_display_actions::INCLUDE_ID
        ));
        let before = settings_store::load();
        assert!(action.invoke_json(&mut ui::widget::Edits::lend(&mut document), &automerge::ROOT,
            &chrome, serde_json::json!({"device":"bb02","output":"panel","revision":0,"included":false}).as_object().unwrap()).is_err());
        assert_eq!(
            settings_store::load(),
            before,
            "revoked authority never saves an arrangement"
        );
    }

    fn chrome_rc() -> crate::shell::chrome_state::Chrome {
        let handle: WorkspaceTreeHandle =
            crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
        ChromeState::new_with_handle(handle)
    }

    /// A session with a room this machine administers — what `agent.invite` needs.
    pub(crate) fn chrome_with_room() -> crate::shell::chrome_state::Chrome {
        use crate::shell::state::{Workspace, WorkspaceId};
        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut ws = Workspace::new(id, "Demo");
        // An explicit identity: the session registry lives under the user's
        // configuration directory, which is not this test's to read.
        ws.set_owner("shell-fixture");
        tree.desktop.workspaces.push(ws);
        tree.desktop.active = id;
        let handle: WorkspaceTreeHandle = crate::shell::state::WorkspaceTreeHandle::new(tree);
        ChromeState::new_with_fixture_handle(
            handle,
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        )
    }

    pub(crate) fn action_results(
        provider: &ActionsProvider,
        chrome: &Chrome,
    ) -> Vec<crate::shell::search::SearchResult> {
        let document = AutoCommit::new();
        let state = chrome.borrow();
        provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &document,
            doc_root: &automerge::ROOT,
        })
    }

    fn invoke_action(results: &[crate::shell::search::SearchResult], id: &str, chrome: &Chrome) {
        let result = results
            .iter()
            .find(|result| result.id == id)
            .unwrap_or_else(|| panic!("action provider did not return {id}"));
        let mut document = AutoCommit::new();
        (result.on_open)(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            chrome,
            &ActionArgs::none(),
        )
        .unwrap();
    }

    fn begin_display_answer(chrome: &Chrome, id: &str) -> crate::shell::search::PendingInvocation {
        let results = action_results(&ActionsProvider::new(), chrome);
        let row = results
            .iter()
            .find(|row| row.id == id)
            .expect("published answer");
        let mut doc = AutoCommit::new();
        let disposition = row.on_begin_invoke.as_ref().expect("answer lifecycle")(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            chrome,
            &ActionArgs::none(),
        )
        .unwrap();
        let crate::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
            panic!("storage must retain a real job");
        };
        ticket
    }

    #[test]
    fn published_display_answer_requires_an_atomic_storage_receipt_even_if_state_matches() {
        use settings_store::SettingsSource as _;
        use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};
        use std::sync::mpsc;
        let _home = tilo_test_env::TestHome::new().unwrap();
        let transaction = DisplayTransaction {
            id: 77,
            state: DisplayTransactionState::AwaitingConfirmation,
            ..DisplayTransaction::default()
        };
        let document = serde_json::json!({"display_transaction": transaction})
            .as_object()
            .unwrap()
            .clone();
        let source = Arc::new(settings_store::HostDocument::new(document, |_| Ok(())));
        let settings = crate::shell::test_support::install_settings(source.clone());
        let (entered, started) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        crate::shell::durable_writer::global()
            .submit_ordered(move || {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
            })
            .unwrap();
        started.recv().unwrap();
        let ticket = begin_display_answer(&chrome_with_room(), DISPLAY_KEEP_ID);
        assert!(ticket().unwrap().is_none());
        // Another legitimate writer reaches the desired state while the
        // answer waits. Its write is not this answer's storage receipt: the
        // answer is stored only over the transaction it was given, so it is
        // refused as replaced.
        source
            .update_section(
                "display_transaction",
                serde_json::json!({"state":"confirm_requested"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        release.send(()).unwrap();
        crate::shell::durable_writer::global().flush();
        let answered = ticket();
        assert!(
            matches!(&answered, Err(ActionError::Refused(message)) if message.contains("replaced before the answer was recorded")),
            "{answered:?}"
        );
        assert_eq!(
            source.load()["display_transaction"]["state"],
            "confirm_requested"
        );
        drop(settings);
    }

    #[test]
    fn axis_navigation_action_door_is_session_local_and_opt_in() {
        let chrome = chrome_with_room();
        let results = action_results(&ActionsProvider::new(), &chrome);
        assert!(!chrome.borrow().axis_navigation_enabled());
        let gather = results
            .iter()
            .find(|row| row.id == "navigation.axis.gather")
            .unwrap();
        let mut document = AutoCommit::new();
        assert!(
            (gather.on_open)(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none()
            )
            .is_err()
        );
        invoke_action(&results, "navigation.axis.enable", &chrome);
        assert!(chrome.borrow().axis_navigation_enabled());
        invoke_action(&results, "navigation.axis.gather", &chrome);
        assert!(chrome.borrow_mut().take_axis_gather_request());
        let other = chrome_with_room();
        assert!(!other.borrow().axis_navigation_enabled());
        invoke_action(&results, "navigation.axis.disable", &chrome);
        assert!(!chrome.borrow().axis_navigation_enabled());
        assert!(!chrome.borrow_mut().take_axis_gather_request());
    }

    fn begin_lifecycle_action(
        results: &[crate::shell::search::SearchResult],
        id: &str,
        chrome: &Chrome,
    ) -> crate::shell::search::InvocationDisposition {
        let result = results
            .iter()
            .find(|result| result.id == id)
            .unwrap_or_else(|| panic!("action provider did not return {id}"));
        let begin = result
            .on_begin_invoke
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must use lifecycle dispatch"));
        let mut document = AutoCommit::new();
        begin(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            chrome,
            &ActionArgs::none(),
        )
        .unwrap()
    }

    #[test]
    fn panel_actions_share_the_provider_dispatch_path() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let results = action_results(&provider, &chrome);

        invoke_action(&results, "settings.open", &chrome);
        assert!(chrome.borrow().overlays.ws_settings);

        invoke_action(&results, "quick-settings.open", &chrome);
        {
            let state = chrome.borrow();
            assert!(state.overlays.quick_settings);
            assert!(!state.overlays.ws_settings);
            assert_eq!(
                state.quick_settings.current_level(),
                &tilo_quick_settings::QsLevel::Root
            );
        }

        invoke_action(&results, "quick-settings.clipboard", &chrome);
        assert_eq!(
            chrome.borrow().quick_settings.section,
            Some(tilo_quick_settings::QsSection::Clipboard)
        );

        invoke_action(&results, "quick-settings.devices", &chrome);
        assert_eq!(
            chrome.borrow().quick_settings.section,
            Some(tilo_quick_settings::QsSection::Devices)
        );
    }

    #[test]
    fn collaboration_actions_enqueue_typed_requests_through_the_provider() {
        let provider = ActionsProvider::new();
        let (chrome, _governed) =
            crate::shell::native_workspace_owner::test_chrome_governing_its_room();
        chrome.borrow_mut().peer_sync_enabled = true;
        let results = action_results(&provider, &chrome);

        for id in ["peer.invite", "peer.connect"] {
            assert!(matches!(
                begin_lifecycle_action(&results, id, &chrome),
                crate::shell::search::InvocationDisposition::Pending(_)
            ));
        }

        let queued = chrome.borrow_mut().take_peer_sync_requests();
        assert_eq!(queued.len(), 2);
        let actions = queued
            .into_iter()
            .map(|request| match request {
                crate::shell::overlay_host::PeerSyncAction::Tracked { action, .. } => *action,
                other => panic!("lifecycle request must remain tracked, got {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actions,
            vec![
                crate::shell::overlay_host::PeerSyncAction::PromptInvite,
                crate::shell::overlay_host::PeerSyncAction::BrowseDirectory,
            ]
        );
    }

    #[test]
    fn room_activity_action_opens_the_sheet_on_open() {
        let p = ActionsProvider::new();
        let c = chrome_with_room();
        let mut doc = AutoCommit::new();
        let out = {
            let cs = c.borrow();
            p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "activity",
                wanted: &[ResourceKind::Action],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let action = out
            .iter()
            .find(|r| r.id == "room.activity.open")
            .expect("typing 'activity' must surface the room-activity action");
        assert_eq!(action.title, "Show workspace activity");
        assert!(
            c.borrow_mut().take_activity_open_request().is_none(),
            "starts closed"
        );
        (action.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &c,
            &ActionArgs::none(),
        )
        .expect("a nullary chrome action must run");
        assert!(
            c.borrow_mut().take_activity_open_request().is_some(),
            "invoking the exported action opens the Activity app"
        );
        assert!(
            !c.borrow().overlays.room_activity,
            "Activity is an app's window, not a modal that captures input"
        );
        assert!(
            !c.borrow().overlays.collaborators,
            "the dropdown Activity can also be opened from is dismissed with it"
        );
    }

    #[test]
    fn dnd_action_toggles_flag_on_open() {
        let p = ActionsProvider::new();
        let c = chrome_with_room();
        let mut doc = AutoCommit::new();
        let out = {
            let cs = c.borrow();
            p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "do not disturb",
                wanted: &[ResourceKind::Action],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let dnd = out
            .iter()
            .find(|r| r.id == "notifications.dnd.toggle")
            .expect("DND action must be present");
        assert!(!c.borrow().dnd(), "DND starts off");
        let fire = |doc: &mut ui::widget::Edits<'_>| {
            (dnd.on_open)(doc, &automerge::ROOT, &c, &ActionArgs::none())
                .expect("a nullary chrome action must run");
        };
        fire(&mut ui::widget::Edits::lend(&mut doc));
        assert!(c.borrow().dnd(), "opening the action turns DND on");
        fire(&mut ui::widget::Edits::lend(&mut doc));
        assert!(!c.borrow().dnd(), "opening it again turns DND back off");
    }

    #[test]
    fn tab_duplicate_failure_reaches_the_invoker_and_keeps_user_feedback() {
        let action = Action::ALL
            .iter()
            .find(|action| action.id == "tab.duplicate")
            .expect("duplicate action is registered");
        let chrome = ChromeState::new_with_handle(crate::shell::state::WorkspaceTreeHandle::new(
            crate::shell::state::WorkspaceTree::default(),
        ));
        let args = ActionArgs::bind(action.params, []).expect("duplicate is nullary");
        let error = (action.apply)(&mut chrome.borrow_mut(), &args)
            .expect_err("no active tab must be a machine-readable failure");
        assert!(
            matches!(error, ActionError::Refused(message) if message.contains("no tab") || message.contains("No tab"))
        );
        assert!(
            !chrome.borrow().notifications().is_empty(),
            "the person still receives useful visible feedback"
        );
    }

    #[test]
    fn record_bring_reports_the_actual_queued_result() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let mut doc = AutoCommit::new();
        let result = {
            let state = chrome.borrow();
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "record.bring",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id == "record.bring")
                .expect("record action")
        };
        let args = ActionArgs::bind(
            &result.params,
            [(
                "id".into(),
                crate::shell::search::action_api::ArgValue::Text("file:/tmp/example".into()),
            )],
        )
        .expect("record arguments");
        let disposition = result
            .on_begin_invoke
            .as_ref()
            .expect("record bring has lifecycle admission")(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .expect("request is admitted to the room index");
        let crate::shell::search::InvocationDisposition::Pending(ticket) = disposition else {
            panic!("queued record work must not report terminal success");
        };
        assert!(ticket().expect("pending poll").is_none());
        chrome
            .borrow_mut()
            .complete_record_request(1, Err("controlled index refusal".into()));
        assert!(
            matches!(ticket(), Err(ActionError::Refused(message)) if message.contains("controlled"))
        );
    }

    #[test]
    fn prefix_beats_substring_beats_subtitle() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "open",
            wanted: &[ResourceKind::Action],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        let opens: Vec<_> = out.iter().filter(|r| r.title.starts_with("Open")).collect();
        assert!(opens.iter().all(|r| r.score == 100));
    }

    #[test]
    fn action_ids_and_out_of_order_terms_are_searchable() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let score = |needle: &str| {
            p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome: &cs,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
            .into_iter()
            .find(|row| row.id == "settings.privacy.open")
            .map(|row| row.score)
        };
        assert_eq!(score("settings.privacy.open"), Some(100));
        assert_eq!(
            score("security workspace"),
            Some(crate::shell::search::provider_score(
                ui::search::MatchTier::Spanning.percent()
            )),
        );
    }

    #[test]
    fn empty_needle_returns_everything() {
        let p = ActionsProvider::new();
        // Peer-sync and agent rows included, so "everything" means the whole table.
        let (c, _governed) = crate::shell::native_workspace_owner::test_chrome_governing_its_room();
        c.borrow_mut().peer_sync_enabled = true;
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(
            Action::ALL
                .iter()
                .all(|action| out.iter().any(|result| result.id == action.id))
        );
        assert!(out.iter().all(|r| r.score == 0));
    }

    #[test]
    fn unrequested_kind_returns_empty() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "open",
            wanted: &[ResourceKind::File],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(out.is_empty());
    }

    #[test]
    fn a_typo_still_matches() {
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "workspace setings",
            wanted: &[ResourceKind::Action],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        assert!(out.iter().any(|r| r.id == "settings.open"));
    }

    #[test]
    fn typing_open_on_a_german_desktop_does_not_prefer_the_tour() {
        crate::shell::i18n::ensure();
        ui::i18n::I18nManager::set_locale("de-DE");
        let p = ActionsProvider::new();
        let c = chrome_rc();
        let cs = c.borrow();
        let doc = AutoCommit::new();
        let out = p.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "open",
            wanted: &[ResourceKind::Action],
            chrome: &cs,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        ui::i18n::I18nManager::set_locale("en-US");
        assert!(
            !out.is_empty(),
            "an English 'open' still finds the *.open addresses"
        );
        assert_ne!(
            out[0].id,
            "desktop-tour.open",
            "equal id-scores must not put the tour first; got {:?}",
            out.iter()
                .take(5)
                .map(|r| (&r.id, r.score))
                .collect::<Vec<_>>()
        );
        let tour = out.iter().position(|r| r.id == "desktop-tour.open");
        let settings = out.iter().position(|r| r.id == "settings.open");
        assert!(
            settings.is_some() && tour.is_some() && settings < tour,
            "settings.open ({settings:?}) should outrank the tour ({tour:?})"
        );
    }

    /// Every offered action resolves by its address, however many are
    /// offered: the table is longer than a palette page.
    #[test]
    fn every_offered_action_resolves_by_its_address() {
        let p = ActionsProvider::new();
        let doc = AutoCommit::new();
        let c = chrome_with_room();
        c.borrow_mut().peer_sync_enabled = true;
        let chrome = c.borrow();
        let ctx = || SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &chrome,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let offered: Vec<String> = p.query(ctx()).into_iter().map(|result| result.id).collect();
        assert!(
            offered.len() > crate::shell::search::SearchBudget::PALETTE.max_results,
            "the table is longer than a page, or this checks nothing: {}",
            offered.len()
        );
        for id in &offered {
            assert!(
                p.resolve(id, ctx()).is_some_and(|result| &result.id == id),
                "{id} is offered but does not resolve"
            );
        }
    }

    /// The rows `provider` offers for `needle` against `chrome` as it is now.
    fn rows_for(
        provider: &ActionsProvider,
        chrome: &Chrome,
        needle: &str,
    ) -> Vec<crate::shell::search::SearchResult> {
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        })
    }

    fn row<'a>(
        rows: &'a [crate::shell::search::SearchResult],
        id: &str,
    ) -> Option<&'a crate::shell::search::SearchResult> {
        rows.iter().find(|row| row.id == id)
    }

    #[test]
    fn typing_builds_the_catalogue_once() {
        // The catalogue is rebuilt when the settings document changes, and
        // which document that is follows the environment: hold it still, as
        // every test that changes either does, or a neighbour's settings
        // write reads as this test's rebuild.
        let _env = tilo_test_env::lock();
        let provider = Rc::new(ActionsProvider::new());
        let builds = Rc::clone(&provider.builds);
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(provider);
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let grouped = |needle: &str| {
            registry.query_grouped(
                &crate::shell::search::QueryArgs {
                    needle,
                    scope: crate::shell::search::SearchScope::All,
                    active_provider_id: None,
                    wanted_kinds: &[],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                },
                // The palette's row budget.
                8,
            )
        };

        // Nothing typed is the launcher, which actions are not part of: it
        // asks the catalogue nothing.
        assert!(grouped("").is_empty(), "no action is suggested unasked");
        assert_eq!(builds.get(), 0, "the launcher builds no catalogue");
        for needle in ["d", "da", "dar"] {
            assert!(!grouped(needle).is_empty(), "{needle:?} finds actions");
        }
        let found = grouped("dark")
            .into_iter()
            .flat_map(|group| group.results)
            .any(|result| result.id == "appearance.theme.dark");
        assert!(found, "\"dark\" finds the dark theme");
        assert_eq!(
            builds.get(),
            1,
            "built once, typing narrows the retained rows"
        );
    }

    #[test]
    fn actions_with_nothing_to_act_on_are_kept_from_people() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let row = |id: &str, title: &str| {
            SearchResult::nullary(
                ResourceKind::Action,
                PROVIDER_ID,
                id,
                title,
                None,
                0,
                |_, _, _| {},
            )
        };
        let finish = row("dictation.finish", "Finish dictation");
        let close = row("projection.close", "Close the display sheet");
        let keep = row(DISPLAY_KEEP_ID, "Keep display settings");
        {
            let state = chrome.borrow();
            assert!(
                provider.hidden_from_people(&finish, "fi", &state),
                "not dictating"
            );
            assert!(
                provider.hidden_from_people(&close, "close", &state),
                "no sheet open"
            );
            assert!(
                provider.hidden_from_people(&keep, "keep", &state),
                "no change waits"
            );
            assert!(
                !provider.hidden_from_people(&close, "close the display sheet", &state),
                "named exactly, it is offered and says why it can't run"
            );
        }
        {
            let mut state = chrome.borrow_mut();
            state.search_context.dictating = true;
            state.overlays.projection = true;
        }
        let state = chrome.borrow();
        assert!(!provider.hidden_from_people(&finish, "fi", &state));
        assert!(!provider.hidden_from_people(&close, "close", &state));
    }

    /// Action ids name the room a workspace is; an id of the form from
    /// before (`workspace.select.<number>`) still selects the workspace that
    /// number names, and one a workspace carried before it was joined to its
    /// room's own workspace selects that one.
    #[test]
    fn a_workspace_action_named_by_its_old_number_still_selects_it() {
        let _env = tilo_test_env::lock();
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let (workspace, room) = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .map(|active| (active.id, active.room()))
            .expect("fixture");
        let old = crate::shell::state::WorkspaceId(0x5eed);
        chrome
            .borrow()
            .tree_mut()
            .workspace_aliases
            .insert(old, room);
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let ctx = || SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let current = provider
            .resolve(&format!("workspace.select.{room}"), ctx())
            .expect("the room names its workspace's action");
        for legacy in [workspace, old] {
            let id = format!("workspace.select.{}", legacy.0);
            let row = provider
                .resolve(&id, ctx())
                .unwrap_or_else(|| panic!("{id} resolves"));
            assert_eq!(row.id, id, "answered under the id it was asked by");
            assert_eq!(row.title, current.title, "the same workspace's action");
        }
        assert!(
            provider
                .resolve("workspace.select.123456789", ctx())
                .is_none()
        );
    }

    #[test]
    fn resolve_does_not_rank() {
        // The catalogue is rebuilt when the settings document changes, and
        // which document that is follows the environment: hold it still, as
        // every test that changes either does, or a neighbour's settings
        // write reads as this test's rebuild.
        let _env = tilo_test_env::lock();
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let ctx = |needle| SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle,
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let tour = provider
            .resolve("desktop-tour.open", ctx(""))
            .expect("the tour resolves by its address");
        assert_eq!(tour.id, "desktop-tour.open");
        assert_eq!(tour.title, t("search-action-desktop-tour-open"));
        assert_eq!(
            provider.builds.get(),
            0,
            "an exact action builds only its row"
        );
        assert!(
            provider.resolve("shortcut-help.toggle", ctx("")).is_some()
                && provider.resolve("no.such.action", ctx("")).is_none()
        );
        assert!(
            provider
                .resolve("desktop-tour.open", ctx("something else entirely"))
                .is_some(),
            "an address is not a search"
        );
        assert_eq!(
            provider
                .query(ctx("tour"))
                .first()
                .map(|row| row.id.as_str()),
            Some("desktop-tour.open")
        );
        assert_eq!(
            provider.builds.get(),
            1,
            "lookups and queries in one state share one build"
        );
        assert!(
            provider
                .resolve(
                    "desktop-tour.open",
                    SearchContext {
                        wanted: &[ResourceKind::File],
                        ..ctx("")
                    }
                )
                .is_none(),
            "a lookup that does not want actions finds none"
        );
    }

    #[test]
    fn exact_table_actions_keep_the_palette_contract_and_presentation() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        for peer_sync in [false, true] {
            chrome.borrow_mut().peer_sync_enabled = peer_sync;
            let state = chrome.borrow();
            let ctx = SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            };
            let offered = provider.query(ctx);
            for action in Action::ALL {
                let listed = row(&offered, action.id);
                let resolved = provider.resolve(action.id, ctx);
                assert_eq!(listed.is_some(), resolved.is_some(), "{}", action.id);
                if let Some((listed, resolved)) = listed.zip(resolved) {
                    assert_eq!(listed.title, resolved.title, "{}", action.id);
                    assert_eq!(listed.subtitle, resolved.subtitle, "{}", action.id);
                    assert_eq!(listed.badge, resolved.badge, "{}", action.id);
                    assert_eq!(listed.shortcut, resolved.shortcut, "{}", action.id);
                    assert_eq!(listed.params, resolved.params, "{}", action.id);
                    assert_eq!(listed.contract, resolved.contract, "{}", action.id);
                    assert_eq!(
                        listed.operation_kind, resolved.operation_kind,
                        "{}",
                        action.id
                    );
                    assert_eq!(
                        listed.on_invoke.is_some(),
                        resolved.on_invoke.is_some(),
                        "{}",
                        action.id
                    );
                    assert_eq!(
                        listed.on_begin_invoke.is_some(),
                        resolved.on_begin_invoke.is_some(),
                        "{}",
                        action.id
                    );
                }
            }
        }
    }

    #[test]
    fn workspace_action_invocations_do_not_rebuild_the_palette_catalogue() {
        use crate::shell::search::action_api::{ActionCatalog, AllVisible, ResourceRef};
        use crate::shell::state::{Workspace, WorkspaceId};

        let provider = Rc::new(ActionsProvider::new());
        let chrome = chrome_with_room();
        let first = chrome.borrow().tree().desktop.active;
        let second = WorkspaceId::new();
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let mut workspace = Workspace::new(second, "Second");
            workspace.set_owner("shell-fixture");
            tree.desktop.workspaces.push(workspace);
        }
        // The smoke journey searches before switching workspaces. Its retained
        // palette rows become stale after each switch and chrome revision bump.
        assert!(!rows_for(&provider, &chrome, "workspace").is_empty());
        assert_eq!(provider.builds.get(), 1);
        let mut registry = crate::shell::search::SearchRegistry::new();
        registry.register(provider.clone());
        let mut host = crate::shell::overlay_host::ShellOverlayHost::with_fixture_content(
            Box::new(ui::widget::Container::new()),
        )
        .with_chrome(chrome.clone());
        let mut doc = AutoCommit::new();
        for _ in 0..40 {
            for (id, expected) in [("workspace.next", second), ("workspace.previous", first)] {
                let resource = ResourceRef::new("actions", id);
                let catalogue = ActionCatalog::build_for_resource(
                    &registry,
                    &resource,
                    crate::shell::search::SearchScope::All,
                    None,
                    &chrome.borrow(),
                    &doc,
                    &automerge::ROOT,
                    &AllVisible,
                );
                catalogue
                    .select(&resource)
                    .expect("workspace action resolves even with stale palette rows")
                    .invoke(
                        &mut ui::widget::Edits::lend(&mut doc),
                        &automerge::ROOT,
                        &chrome,
                        [],
                    )
                    .expect("workspace action succeeds");
                host.run_requested_shortcut_actions();
                assert_eq!(chrome.borrow().tree().desktop.active, expected);
                assert_eq!(
                    provider.builds.get(),
                    1,
                    "invoking one addressed action must not rebuild every palette row"
                );
            }
        }
        drop(host);
    }

    #[test]
    fn catalogue_rebuilds_on_locale_and_workspace_change() {
        struct RestoreLocale;
        impl Drop for RestoreLocale {
            fn drop(&mut self) {
                ui::i18n::I18nManager::set_locale("en-US");
            }
        }
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let language = |provider: &ActionsProvider| {
            row(&rows_for(provider, &chrome, ""), "appearance.language")
                .map(|row| row.title.clone())
        };

        // Locale.
        assert_eq!(language(&provider).as_deref(), Some("Change language"));
        let _restore = RestoreLocale;
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_eq!(crate::shell::i18n::current_locale(), "de-DE");
        assert_eq!(language(&provider).as_deref(), Some("Sprache ändern"));
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(language(&provider).as_deref(), Some("Change language"));

        // Active workspace: governance rows address the workspace now active.
        let (first_room, second, second_room) = {
            use crate::shell::state::{Workspace, WorkspaceId};
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            let first_room = tree.desktop.active_workspace().expect("fixture").room();
            let id = WorkspaceId::new();
            let mut workspace = Workspace::new(id, "Second");
            workspace.set_owner("shell-fixture");
            let room = workspace.room();
            tree.desktop.workspaces.push(workspace);
            drop(tree);
            (first_room, id, room)
        };
        assert_ne!(first_room, second_room);
        let grant_row = |room| {
            format!(
                "{}#{}",
                tilo_desktop_model::governance_resource_id(&room),
                tilo_desktop::workspace_settings::GRANT_ACTION_ID
            )
        };
        let offers = |provider: &ActionsProvider, room| {
            row(&rows_for(provider, &chrome, ""), &grant_row(room)).is_some()
        };
        assert!(offers(&provider, first_room));
        chrome.borrow().tree_mut().desktop.active = second;
        assert!(
            offers(&provider, second_room) && !offers(&provider, first_room),
            "switching workspace rebuilds its governance rows"
        );
    }

    #[test]
    fn catalogue_rebuilds_on_tree_mutations_and_per_desktop() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let (workspace_id, workspace_room) = {
            let state = chrome.borrow();
            let tree = state.tree();
            let workspace = tree.desktop.active_workspace().expect("fixture");
            let located = (workspace.id, workspace.room());
            drop(tree);
            located
        };

        // A tree mutation the chrome made, which it bumps the revision for.
        let switch_to = |provider: &ActionsProvider| {
            row(
                &rows_for(provider, &chrome, ""),
                &format!("workspace.select.{workspace_room}"),
            )
            .map(|row| row.title.clone())
            .expect("every workspace can be switched to")
        };
        assert!(switch_to(&provider).contains("Demo"));
        {
            let mut state = chrome.borrow_mut();
            state
                .tree_mut()
                .desktop
                .active_workspace_mut()
                .expect("workspace")
                .name = "Renamed".into();
            state.bump();
        }
        assert!(switch_to(&provider).contains("Renamed"));

        // A mutation from outside the chrome, before a tick folds it in.
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            tree.desktop.active_workspace_mut().expect("workspace").name = "Remote".into();
            tree.desktop.external_revision = tree.desktop.external_revision.wrapping_add(1);
        }
        assert!(switch_to(&provider).contains("Remote"));

        // Another desktop that differs only in being another: the same
        // workspace id and revisions, its own room.
        let other = {
            use crate::shell::state::Workspace;
            let mut tree = WorkspaceTree::default();
            let mut workspace = Workspace::new(workspace_id, "Remote");
            workspace.set_owner("shell-fixture");
            tree.desktop.workspaces.push(workspace);
            tree.desktop.active = workspace_id;
            tree.desktop.external_revision = chrome.borrow().tree().desktop.external_revision;
            ChromeState::new_with_fixture_handle(
                crate::shell::state::WorkspaceTreeHandle::new(tree),
                crate::shell::hosted_inputs::HostedTileInputs::default(),
            )
        };
        other.borrow_mut().revision = chrome.borrow().revision;
        let other_room = other
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("fixture")
            .room();
        assert_ne!(other_room, workspace_room);
        let other_grant = format!(
            "{}#{}",
            tilo_desktop_model::governance_resource_id(&other_room),
            tilo_desktop::workspace_settings::GRANT_ACTION_ID
        );
        assert!(
            row(&rows_for(&provider, &other, ""), &other_grant).is_some(),
            "one provider asked about two desktops answers for each"
        );
    }

    /// The palette offers the invite rows exactly where the one invitation
    /// policy lets this device issue, as the invite composer is: in a room
    /// whose host this person governs, never in one another person's host
    /// governs, where the issuer would refuse.
    #[test]
    fn invite_rows_follow_the_invitation_policy() {
        use crate::shell::native_workspace_owner::grant_invite;
        use crate::shell::state::person_id_of;

        let (_directory, owner, room, _now, _key) =
            crate::shell::native_workspace_owner::test_enrolled_owner();
        let inviter = person_id_of(
            owner
                .with_current(room, |view| Ok(view.owner()))
                .expect("the room's admitted governor"),
        )
        .expect("person id");
        let other = person_id_of(
            ed25519_dalek::SigningKey::from_bytes(&[64; 32])
                .verifying_key()
                .to_bytes(),
        )
        .expect("person id");
        let handle = crate::shell::native_workspace_owner::test_tree_of(&owner);
        {
            let mut tree = handle.write();
            let workspace = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room() == room)
                .expect("the enrolled room's workspace");
            grant_invite(workspace, &inviter);
            let local = workspace.id;
            tree.desktop.active = local;
        }
        let provider = ActionsProvider::new();
        let chrome = ChromeState::new_with_fixture_handle(
            handle,
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        chrome.borrow_mut().peer_sync_enabled = true;
        let offers = |id: &str| row(&rows_for(&provider, &chrome, ""), id).is_some();

        chrome.borrow_mut().act_as(inviter);
        assert!(
            chrome.borrow().may_invite_here(),
            "the person the room's policy lets invite may"
        );
        assert!(offers("peer.invite"), "so the palette offers the composer");

        chrome.borrow_mut().act_as(other);
        assert!(
            !chrome.borrow().may_invite_here(),
            "another person the policy does not name may not, whoever hosts the room"
        );
        assert!(!offers("peer.invite"), "so no invite row is offered");
        assert!(!offers("peer.invite.named"), "named or not");
        assert!(offers("peer.connect"), "joining a room is not inviting");
    }

    #[test]
    fn catalogue_rebuilds_on_collaboration_and_search_sources() {
        // Peer sync and the collaboration broker.
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let offers = |provider: &ActionsProvider, id: &str| {
            row(&rows_for(provider, &chrome, ""), id).is_some()
        };
        assert!(!offers(&provider, "peer.connect"));
        chrome.borrow_mut().peer_sync_enabled = true;
        assert!(offers(&provider, "peer.connect"), "peer sync started");
        assert!(offers(&provider, "agent.invite"));
        chrome
            .borrow_mut()
            .set_collab_unavailable("the broker could not start");
        assert!(!offers(&provider, "agent.invite"), "the broker went away");

        // Search sources: one registered, and one turned off.
        let mut registry = crate::shell::search::SearchRegistry::new();
        let provider = ActionsProvider::with_preferences_and_catalog(
            registry.preferences_handle(),
            registry.provider_catalog_handle(),
        );
        let chrome = chrome_rc();
        let switch = |provider: &ActionsProvider, id: &str| {
            row(
                &rows_for(provider, &chrome, ""),
                &format!("search.provider.toggle:{id}"),
            )
            .map(|row| row.title.clone())
        };
        let windows = super::super::desktop_windows::DesktopWindowsProvider::new();
        let windows_id = windows.id().to_string();
        let windows_name = windows.display_name();
        assert_eq!(switch(&provider, &windows_id), None);
        registry.register(Rc::new(windows));
        let source: &[(&str, ui::i18n::ArgValue)] = &[
            ("source", windows_name.into()),
            ("id", windows_id.clone().into()),
        ];
        assert_eq!(
            switch(&provider, &windows_id),
            Some(t_args("search-action-provider-disable", source)),
            "a newly registered source gets its switch"
        );
        registry.set_provider_enabled(&windows_id, false);
        assert_eq!(
            switch(&provider, &windows_id),
            Some(t_args("search-action-provider-enable", source)),
            "a source turned off is offered to turn back on"
        );
    }

    #[test]
    fn workspace_activity_exact_local_actions_are_exported() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let out = provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "activity",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        for id in [
            "room.activity.open",
            "room.activity.explore",
            "room.activity.replay",
            "room.activity.acknowledge",
        ] {
            assert!(
                out.iter().any(|action| action.id == id
                    && action.operation_kind == tilo_actions::OperationKind::View),
                "{id}"
            );
        }
    }

    #[test]
    fn undo_and_restore_name_the_selected_row_actions() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let query = |needle| {
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        for (needle, id) in [
            ("undo", tilo_desktop::activity::HISTORY_UNDO),
            ("restore", tilo_desktop::activity::HISTORY_RESTORE),
            ("compare", tilo_desktop::activity::HISTORY_COMPARE),
        ] {
            assert!(
                query(needle).iter().any(|row| row.id == id),
                "{needle} must surface {id}"
            );
        }
    }

    #[test]
    fn tab_menu_is_searchable_as_a_view_and_refuses_empty_focus() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let rows = {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "tab menu",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let menu = rows
            .iter()
            .find(|row| row.id == "tab.menu")
            .expect("search discovers the menu");
        assert_eq!(menu.operation_kind, tilo_actions::OperationKind::View);
        let mut doc = AutoCommit::new();
        let result = (menu.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        );
        assert!(matches!(result, Err(ActionError::Explained(_))));
        assert!(chrome.borrow_mut().take_window_requests().is_empty());
    }

    #[test]
    fn history_finds_activity_views_and_not_a_workspace_switch() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let state = chrome.borrow();
        let query = |needle| {
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };

        let history = query("history");
        let explore = history
            .iter()
            .find(|row| row.id == "room.activity.explore")
            .expect("history names the full timeline");
        assert_eq!(
            explore.subtitle.as_deref(),
            Some("Every change here, by person, app and time")
        );
        assert!(
            history
                .iter()
                .all(|row| !row.id.starts_with("workspace.select.")),
            "switching workspace is not an answer to “history”: {:?}",
            history.iter().map(|row| &row.title).collect::<Vec<_>>()
        );

        let switch = query("switch");
        let row = switch
            .iter()
            .find(|row| row.id.starts_with("workspace.select."))
            .expect("a workspace is still reachable by switching to it");
        assert_eq!(
            row.subtitle.as_deref(),
            Some("Go to this workspace and its windows")
        );
    }

    #[test]
    fn a_search_for_a_source_finds_it_not_its_off_switch() {
        let preferences = Rc::new(std::cell::RefCell::new(
            crate::shell::search::SearchPreferences::default(),
        ));
        let catalog = Rc::new(std::cell::RefCell::new(vec![(
            "notes".to_string(),
            "Notes".to_string(),
        )]));
        let provider = ActionsProvider::with_preferences_and_catalog(preferences, catalog);
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let switch = |needle| {
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle,
                    wanted: &[ResourceKind::Action],
                    chrome: &chrome.borrow(),
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|row| row.id == "search.provider.toggle:notes")
        };

        assert!(switch("notes").is_none(), "“notes” looks for notes");
        let row = switch("disable notes").expect("asking to disable it finds the switch");
        assert_eq!(
            row.subtitle.as_deref(),
            Some("Hide Notes results from search and agent discovery")
        );
        assert!(
            switch("").is_some(),
            "a complete listing keeps every switch"
        );
        assert!(
            switch("search.provider.toggle:notes").is_some(),
            "an exact address still resolves"
        );
    }

    #[test]
    fn search_source_customization_is_a_typed_exported_action() {
        let preferences = Rc::new(std::cell::RefCell::new(
            crate::shell::search::SearchPreferences::default(),
        ));
        let catalog = Rc::new(std::cell::RefCell::new(vec![
            ("actions".to_string(), "Actions".to_string()),
            ("files".to_string(), "Files".to_string()),
        ]));
        let provider =
            ActionsProvider::with_preferences_and_catalog(Rc::clone(&preferences), catalog);
        let chrome = chrome_rc();
        let doc = AutoCommit::new();
        let results = provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "search source",
            wanted: &[ResourceKind::Action],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        let action = results
            .iter()
            .find(|result| result.id == "search.provider.set")
            .expect("search customization must be reachable without clicking settings UI");
        assert_eq!(action.params, SEARCH_PROVIDER_SET_PARAMS);
        assert_eq!(action.params[0].ty, ParamType::Text);
        assert_eq!(action.params[1].ty, ParamType::Boolean);
        let direct = results
            .iter()
            .find(|result| result.id == "search.provider.toggle:files")
            .expect("people can customize a named source without knowing its opaque id");
        assert_eq!(direct.title, "Disable Files search");
        assert_eq!(direct.badge.as_deref(), Some("ON"));
        assert!(direct.params.is_empty());
        assert!(
            results
                .iter()
                .all(|result| result.id != "search.provider.toggle:actions"),
            "the accessibility and automation source cannot offer a disable affordance"
        );

        preferences
            .borrow_mut()
            .set_provider_enabled("files", false);
        let disabled = provider.query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "files search",
            wanted: &[ResourceKind::Action],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        let direct = disabled
            .iter()
            .find(|result| result.id == "search.provider.toggle:files")
            .expect("a disabled source remains directly re-enableable");
        assert_eq!(direct.title, "Enable Files search");
        assert_eq!(direct.badge.as_deref(), Some("OFF"));
    }

    #[test]
    fn workspace_privacy_search_result_deep_links_to_exact_lens_tab() {
        let p = ActionsProvider::new();
        let c = chrome_with_room();
        let mut doc = AutoCommit::new();
        let out = {
            let state = c.borrow();
            p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "workspace privacy",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let result = out
            .iter()
            .find(|result| result.id == "settings.privacy.open")
            .expect("privacy governance is directly searchable");

        (result.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &c,
            &ActionArgs::none(),
        )
        .unwrap();

        let state = c.borrow();
        assert!(state.overlays.ws_settings);
        assert_eq!(
            state.ws_settings_tab,
            tilo_desktop::workspace_settings::WsTab::Permissions
        );
    }

    #[test]
    fn status_privacy_is_a_typed_semantic_action() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let mut doc = AutoCommit::new();
        let results = {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "status privacy",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let action = results
            .iter()
            .find(|result| result.id == "presence.status-sharing.set")
            .expect("agents must be able to control the same status privacy setting");
        let args = ActionArgs::bind(
            &action.params,
            [(
                "mode".to_string(),
                tilo_actions::ArgValue::Text("off".into()),
            )],
        )
        .unwrap();

        (action.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .unwrap();

        assert_eq!(
            chrome.borrow_mut().take_presence_requests(),
            vec![
                crate::shell::chrome_state::PresenceRequest::SetStatusSharing(
                    ui::StatusSharing::Off
                )
            ]
        );
    }

    #[test]
    fn desktop_tour_is_searchable_and_requests_the_trusted_chrome_modal() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let mut doc = AutoCommit::new();
        let results = {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "desktop tour",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let result = results
            .iter()
            .find(|result| result.id == "desktop-tour.open")
            .expect("the completed first-use tour must remain discoverable");

        (result.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .unwrap();
        assert!(chrome.borrow().overlays.desktop_tour);
    }

    #[test]
    fn shortcut_help_search_result_teaches_both_help_routes() {
        let action = Action::ALL
            .iter()
            .find(|action| action.id == "shortcut-help.toggle")
            .expect("shortcut help action");
        assert_eq!(t(action.label), "Show keyboard shortcuts");
        let subtitle = t(action.subtitle);
        assert!(subtitle.contains("Super+/"));
        assert!(subtitle.contains("F1"));
        assert!(subtitle.contains("same guide"));
    }

    /// Row titles and subtitles are catalogue keys, which the source scan
    /// cannot see through the table; every one must resolve in every locale.
    #[test]
    fn every_action_row_resolves_to_real_copy() {
        crate::shell::i18n::ensure();
        for (locale, title, subtitle) in [
            (
                "en-US",
                "Show workspace overview",
                "Show all workspaces and their windows",
            ),
            (
                "de-DE",
                "Arbeitsbereichsübersicht zeigen",
                "Alle Arbeitsbereiche und ihre Fenster zeigen",
            ),
        ] {
            ui::i18n::I18nManager::set_locale(locale);
            let editor = Action::ALL
                .iter()
                .find(|action| action.id == "overview.toggle")
                .expect("overview action");
            assert_eq!(t(editor.label), title);
            assert_eq!(t(editor.subtitle), subtitle);
            for action in Action::ALL {
                for key in [action.label, action.subtitle] {
                    let copy = t(key);
                    assert_ne!(copy, key, "{locale}: {key} has no entry in the catalogue");
                    assert!(
                        !copy.trim().is_empty(),
                        "{locale}: {key} resolves to nothing"
                    );
                }
            }
        }
        ui::i18n::I18nManager::set_locale("en-US");
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn theme_and_language_actions_are_searchable_and_write_the_settings_store() {
        use settings_store::appearance::{HIGH_CONTRAST, MODE};
        // The person's settings records, kept apart from every other test's.
        let _home = tilo_test_env::TestHome::new().unwrap();
        let config = tempfile::tempdir().expect("isolated settings");
        // The session keeps the person's settings, as it does in a session.
        let paths = config.path().to_path_buf();
        let records = settings_store::Store::open(&paths).expect("records");
        let _settings = crate::shell::test_support::install_settings(records);

        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let query = |needle: &str| {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        let id_of = |needle: &str, want: &str| {
            query(needle)
                .into_iter()
                .find(|row| row.id == want)
                .unwrap_or_else(|| panic!("{needle:?} must find {want}"))
                .id
        };
        assert_eq!(
            id_of("dark theme", "appearance.theme.dark"),
            "appearance.theme.dark"
        );
        assert_eq!(
            id_of("light theme", "appearance.theme.light"),
            "appearance.theme.light"
        );
        assert_eq!(
            id_of("language", "appearance.language"),
            "appearance.language"
        );

        // Unset reads as dark, so a change to dark is shown from light: a
        // field edit stores only what differs from what is read now.
        invoke_action(&query("light theme"), "appearance.theme.light", &chrome);
        assert_eq!(MODE.get().as_deref(), Some("light"));
        let generation = chrome.borrow().appearance_generation;
        invoke_action(&query("dark theme"), "appearance.theme.dark", &chrome);
        assert!(
            chrome.borrow().appearance_generation > generation,
            "theme actions must wake the live frame, not only write the store"
        );
        assert_eq!(MODE.get().as_deref(), Some("dark"));
        assert!(!HIGH_CONTRAST.get());

        let language = query("language")
            .into_iter()
            .find(|row| row.id == "appearance.language")
            .expect("language action");
        let args = ActionArgs::bind(
            &language.params,
            [(
                "language".into(),
                crate::shell::search::action_api::ArgValue::Text("German".into()),
            )],
        )
        .expect("language arguments");
        let mut document = AutoCommit::new();
        (language.on_open)(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .unwrap();
        assert_eq!(
            settings_store::str_at(&settings_store::load(), "appearance", "language"),
            Some("de-DE")
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_theme_action_records_the_field_edits_it_makes_with_both_leaves() {
        // The person's settings records, kept apart from every other test's.
        let _home = tilo_test_env::TestHome::new().unwrap();
        let config = tempfile::tempdir().expect("isolated settings");
        let paths = config.path().to_path_buf();
        let records = settings_store::Store::open(&paths).expect("records");
        let _settings = crate::shell::test_support::install_settings(records.clone());

        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let doc = AutoCommit::new();
        let rows = {
            let state = chrome.borrow();
            provider.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "light theme",
                wanted: &[ResourceKind::Action],
                chrome: &state,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };
        // High contrast on, so choosing a mode changes both leaves.
        settings_store::appearance::HIGH_CONTRAST
            .set(true)
            .expect("high contrast on");
        let before = records.change_log().len();
        invoke_action(&rows, "appearance.theme.light", &chrome);

        let entries: Vec<serde_json::Value> =
            records.change_log().into_iter().skip(before).collect();
        let mut leaves: Vec<_> = entries
            .iter()
            .map(|entry| {
                assert_eq!(entry["section"], "appearance");
                // The palette changes settings through Settings' own field
                // edits, so each leaf names the edit that set it, and the
                // palette action that ran it: "light mode, from the palette"
                // stays distinct from the same edit made in Settings.
                assert_eq!(entry["action"], "settings.appearance/field-edit");
                assert_eq!(
                    entry["invoked_by"],
                    serde_json::json!({ "action": "appearance.theme.light" }),
                    "{entry}"
                );
                (
                    entry["leaf"].as_str().unwrap_or_default(),
                    entry["after"].clone(),
                )
            })
            .collect();
        leaves.sort_by_key(|(leaf, _)| *leaf);
        assert_eq!(
            leaves,
            [
                ("high_contrast", serde_json::json!(false)),
                ("mode", serde_json::json!("light")),
            ],
            "the action sets both leaves, each attributed to its field edit and the palette action"
        );
        assert_eq!(
            settings_store::SettingsSource::load(&records)["appearance"]["mode"],
            "light",
            "every reader sees the typed write"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn language_action_switches_the_live_catalogue() {
        struct RestoreLocale;
        impl Drop for RestoreLocale {
            fn drop(&mut self) {
                ui::i18n::I18nManager::set_locale("en-US");
            }
        }
        let config = tempfile::tempdir().expect("isolated settings");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", config.path());
        let _restore = RestoreLocale;

        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let language = {
            let state = chrome.borrow();
            let doc = AutoCommit::new();
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "language",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|row| row.id == "appearance.language")
                .expect("language action")
        };
        let args = ActionArgs::bind(
            &language.params,
            [(
                "language".into(),
                crate::shell::search::action_api::ArgValue::Text("German".into()),
            )],
        )
        .expect("language arguments");
        let mut document = AutoCommit::new();
        (language.on_open)(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &chrome,
            &args,
        )
        .unwrap();

        assert_eq!(
            settings_store::str_at(&settings_store::load(), "appearance", "language"),
            Some("de-DE")
        );
        assert_eq!(
            crate::shell::i18n::current_locale(),
            "de-DE",
            "writing the store without selecting the live catalogue leaves chrome English"
        );
        assert_eq!(t("tab-closed-undo"), "Rückgängig");
        assert_eq!(t("search-action-appearance-language"), "Sprache ändern");
    }

    // Offered only by a session running peer sync, and when offered,
    // naming one selects it: a label-prefix match outranks other providers.
    #[test]
    fn collaboration_rows_appear_only_with_sync_and_lead_when_named() {
        let p = ActionsProvider::new();
        let doc = AutoCommit::new();
        let query = |chrome: &ChromeState, needle: &str| {
            p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle,
                wanted: &[ResourceKind::Action],
                chrome,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
        };

        let off = chrome_rc();
        {
            let cs = off.borrow();
            assert!(
                query(&cs, "invite").is_empty(),
                "a session without peer sync must not offer to invite anyone",
            );
        }

        // Invitations go into a room this device governs: a roomless
        // session has nothing to invite into. Without a broker the agent
        // row (also an "Invite …") stays out of this ranking, as it does
        // for a roomless session.
        let (on, _governed) =
            crate::shell::native_workspace_owner::test_chrome_governing_its_room();
        on.borrow_mut().peer_sync_enabled = true;
        on.borrow_mut()
            .set_collab_unavailable("this phase ranks the peer rows alone");
        let cs = on.borrow();
        let mut hits = query(&cs, "invite");
        hits.sort_by_key(|r| -r.score);
        assert_eq!(
            hits.first().map(|r| r.id.as_str()),
            Some("peer.invite"),
            "typing \"invite\" must rank the invite command first, got {:?}",
            hits.iter().map(|r| (&r.id, r.score)).collect::<Vec<_>>(),
        );
        assert_eq!(
            hits[0].score, 100,
            "a label-prefix match is the top of this provider's scale, which \
             is what puts the row above hits from other providers",
        );
        let mut join = query(&cs, "join");
        join.sort_by_key(|r| -r.score);
        assert_eq!(join.first().map(|r| r.id.as_str()), Some("peer.connect"),);
    }

    // Needs a room (the grant is recorded *in* one) and a working broker
    // (how an agent reaches the room). Not peer sync — an agent uses its
    // own broker socket.
    #[test]
    fn the_agent_row_is_offered_only_where_it_can_work() {
        let p = ActionsProvider::new();
        let doc = AutoCommit::new();
        let offered = |chrome: &ChromeState| {
            p.query(SearchContext {
                query_scope: None,
                include_programmatic: false,
                needle: "agent",
                wanted: &[ResourceKind::Action],
                chrome,
                doc: &doc,
                doc_root: &automerge::ROOT,
            })
            .iter()
            .any(|r| r.id == "agent.invite")
        };

        let roomless = chrome_rc();
        assert!(
            !offered(&roomless.borrow()),
            "a session with no room has nowhere to record the grant, so it \
             must not offer to admit anyone",
        );

        let broker_down = chrome_with_room();
        broker_down
            .borrow_mut()
            .set_collab_unavailable("the broker could not start");
        assert!(
            !offered(&broker_down.borrow()),
            "an agent reaches the room through the broker; with none there \
             is nothing for the grant to admit",
        );

        let ready = chrome_with_room();
        assert!(
            offered(&ready.borrow()),
            "a session with a room and a working broker must offer the row",
        );
        assert!(
            !ready.borrow().peer_sync_enabled,
            "and must do so without peer sync, which this row does not need",
        );
    }

    // Asserted through `agent_members`, the same reader the roster paints
    // from, not a row in a vector this test put there.
    #[test]
    fn invoking_it_admits_the_agent_to_the_room() {
        let c = chrome_with_room();

        let before = c
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("fixture has a room")
            .agent_members();
        assert!(
            before.is_empty(),
            "nobody is admitted before the action runs"
        );

        let action = Action::ALL
            .iter()
            .find(|a| a.id == "agent.invite")
            .expect("the action is registered");
        (action.apply)(&mut c.borrow_mut(), &ActionArgs::none())
            .expect("the room's owner may admit an agent");
        governed(&c, FIXTURE_USER);

        let after = c
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("fixture has a room")
            .agent_members();
        assert_eq!(
            after,
            vec![(
                DEFAULT_AGENT_NAME.to_lowercase(),
                tilo_action_policy::KindSet::READ
            )],
            "the shipped agent is admitted under its own name, case-folded",
        );
        assert!(
            c.borrow().overlays.ws_settings,
            "and the dedicated workspace Agents roster is shown so the \
             person who admitted it can review its scope",
        );
    }

    // A name typed in any case names one agent. Two spellings would be
    // two subjects, which reads on screen as a grant that did not take.
    #[test]
    fn the_name_is_case_folded_into_one_principal() {
        let c = chrome_with_room();
        let action = Action::ALL
            .iter()
            .find(|a| a.id == "agent.invite")
            .expect("registered");
        for spelling in ["Scribe", "SCRIBE", "  scribe  "] {
            let args = ActionArgs::bind(
                AGENT_INVITE_PARAMS,
                [(
                    "name".to_string(),
                    crate::shell::search::action_api::ArgValue::Text(spelling.to_string()),
                )],
            )
            .expect("name is a declared text parameter");
            (action.apply)(&mut c.borrow_mut(), &args).expect("owner may admit");
            // The governor answers each before the next is typed, so a
            // later spelling finds the agent already admitted.
            c.borrow()
                .tree_mut()
                .govern_requests_here_for_test(&FIXTURE_USER.to_owned());
        }
        let members = c
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .expect("fixture has a room")
            .agent_members();
        assert_eq!(
            members.len(),
            1,
            "three spellings of one name must be one agent, got {members:?}",
        );
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the authorization regression retains the chrome guard until each ambient-authority assertion completes"
    )]
    fn non_admin_cannot_manage_identity_devices() {
        use crate::shell::state::{Workspace, WorkspaceId};

        let mut tree = WorkspaceTree::default();
        let id = WorkspaceId::new();
        let mut workspace = Workspace::new(id, "Someone else's workspace");
        workspace.set_owner("a-different-person");
        tree.desktop.workspaces.push(workspace);
        tree.desktop.active = id;
        let chrome =
            ChromeState::new_with_handle(crate::shell::state::WorkspaceTreeHandle::new(tree));

        for action_id in [
            "identity.devices.copy",
            "identity.device.enroll",
            "identity.device.revoke",
        ] {
            let action = Action::ALL
                .iter()
                .find(|action| action.id == action_id)
                .expect("device action is registered");
            let args = if action_id == "identity.device.revoke" {
                ActionArgs::bind(
                    action.params,
                    [
                        (
                            "device".into(),
                            crate::shell::search::action_api::ArgValue::Text("0".repeat(64)),
                        ),
                        (
                            "confirm".into(),
                            crate::shell::search::action_api::ArgValue::Boolean(true),
                        ),
                    ],
                )
                .expect("confirmed revocation arguments bind")
            } else {
                ActionArgs::none()
            };
            let error = (action.apply)(&mut chrome.borrow_mut(), &args)
                .expect_err("a non-admin must be refused before device state is touched");
            assert!(
                matches!(&error, ActionError::Refused(message) if message.contains("may not administer devices")),
                "unexpected refusal for {action_id}: {error:?}",
            );
        }
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the authorization regression retains the chrome guard until each ambient-authority assertion completes"
    )]
    fn setup_profile_is_recorded_but_never_becomes_ambient_authority() {
        let c = chrome_with_room();
        let action = Action::ALL
            .iter()
            .find(|a| a.id == "agent.invite")
            .expect("registered");
        let args = ActionArgs::bind(
            AGENT_INVITE_PARAMS,
            [
                (
                    "name".to_string(),
                    crate::shell::search::action_api::ArgValue::Text("Operator".to_string()),
                ),
                (
                    "preset".to_string(),
                    crate::shell::search::action_api::ArgValue::Text("operator".to_string()),
                ),
            ],
        )
        .expect("declared setup parameters bind");
        (action.apply)(&mut c.borrow_mut(), &args).expect("owner admits agent");

        let state = c.borrow();
        let user = state.active_local_user();
        let mut tree = state.tree_mut();
        let asked: Vec<_> = tree
            .take_governance_requests()
            .into_iter()
            .filter_map(|(_, request)| match request {
                crate::shell::state::GovernanceRequest::Policy(
                    tilo_action_policy::Change::Grant(grant),
                ) => Some(grant),
                _ => None,
            })
            .collect();
        assert!(
            asked
                .iter()
                .any(|grant| grant.id.as_str() == "agent-member:operator:operator"),
            "the admission is asked of the room's governor"
        );
        let ws = tree.desktop.active_workspace().expect("fixture has room");
        assert!(
            ws.agent_members()
                .into_iter()
                .all(|(agent, _)| agent != "operator"),
            "nothing changes here before the room's admitted document carries it"
        );
        // What the governor applies and the admitted document carries.
        let ws = tree
            .desktop
            .active_workspace_mut()
            .expect("fixture has room");
        for grant in asked {
            ws.govern_as_at(
                &user,
                tilo_action_policy::Change::Grant(grant),
                tilo_clock::unix_now(),
            )
            .expect("the governor admits the agent");
        }
        let ws = tree.desktop.active_workspace().expect("fixture has room");
        assert_eq!(
            ws.agent_members()
                .into_iter()
                .find(|(agent, _)| agent == "operator")
                .map(|(_, kinds)| kinds),
            Some(tilo_action_policy::KindSet::ALL),
            "the durable admission carries the selected preset"
        );
        let answer = |kind| {
            ws.subject_authority(tilo_action_policy::Subject::agent("operator"))
                .decide(
                    &ws.node().app("so.tilo.notes"),
                    kind,
                    &tilo_desktop_model::context_at(tilo_clock::unix_now()),
                )
                .answer
        };
        assert_eq!(
            answer(tilo_action_policy::ActionKind::Undoable),
            tilo_action_policy::Answer::Allow,
            "an Operator makes undoable changes"
        );
        assert_eq!(
            answer(tilo_action_policy::ActionKind::Outside),
            tilo_action_policy::Answer::Ask,
            "and asks first before reaching outside the room"
        );
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the authorization regression retains the chrome guard until each ambient-authority assertion completes"
    )]
    fn retained_shell_action_rechecks_authority_before_mutation() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let mut doc = AutoCommit::new();
        let result = {
            let state = chrome.borrow();
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "do not disturb",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id == "notifications.dnd.toggle")
                .expect("authorized action is discovered")
        };
        {
            let state = chrome.borrow_mut();
            let mut tree = state.tree_mut();
            let workspace = tree.desktop.active_workspace_mut().expect("workspace");
            workspace.policy.owner = None;
            workspace.policy.members.clear();
            drop(tree);
        }
        let error = (result.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &ActionArgs::none(),
        )
        .expect_err("revocation between discovery and execution must win");
        assert!(matches!(error, ActionError::Refused(_)));
        assert!(!chrome.borrow().dnd());
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the authorization regression retains the chrome guard until each ambient-authority assertion completes"
    )]
    fn permanent_device_revocation_requires_admin_and_confirmation() {
        let action = Action::ALL
            .iter()
            .find(|action| action.id == "identity.device.revoke")
            .expect("device revocation is registered");
        let contract = declared(action.id).0;
        assert_eq!(contract.kind, tilo_actions::ActionKind::Outside);
        assert_eq!(
            contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        );
        assert_eq!(contract.risk, tilo_actions::RiskLevel::Privileged);

        let args = ActionArgs::bind(
            action.params,
            [(
                "device".into(),
                crate::shell::search::action_api::ArgValue::Text("0".repeat(64)),
            )],
        );
        assert!(
            matches!(args, Err(crate::shell::search::action_api::ArgError::Missing { name }) if name == "confirm"),
            "confirmation is an enforced typed argument, not display-only metadata"
        );
    }

    #[test]
    fn window_actions_are_published_with_focus_and_layout_contracts() {
        let focus = [
            "window.focus-previous",
            "window.focus-next",
            "strip.focus-previous",
            "strip.focus-next",
            "workspace.previous",
            "workspace.next",
            "tab.previous",
            "tab.next",
            "window.focus-nth",
            "strip.focus-nth",
        ];
        for id in focus {
            assert!(Action::find(id).is_some(), "{id} is not published");
        }
        let layout: Vec<&str> = Action::ALL
            .iter()
            .filter(|action| action.rearranges)
            .map(|action| action.id)
            .collect();
        assert_eq!(layout.len(), 15, "{layout:?}");
        for id in focus {
            assert_eq!(declared(id).1, tilo_actions::OperationKind::View);
            let contract = declared(id).0;
            assert_eq!(
                contract.kind,
                tilo_actions::ActionKind::Read,
                "{id} only moves focus"
            );
            assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
        }
        for id in layout {
            assert_eq!(declared(id).1, tilo_actions::OperationKind::Effect);
            let contract = declared(id).0;
            assert_eq!(
                contract.kind,
                tilo_actions::ActionKind::Undoable,
                "{id} rearranges the shared desk"
            );
            assert!(contract.can_be_undone());
            assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
        }
    }

    #[test]
    fn every_shell_action_exposes_a_valid_operational_contract() {
        let chrome = chrome_with_room();
        let state = chrome.borrow();
        let doc = AutoCommit::new();
        let results = ActionsProvider::new().query(SearchContext {
            query_scope: None,
            include_programmatic: false,
            needle: "",
            wanted: &[ResourceKind::Action],
            chrome: &state,
            doc: &doc,
            doc_root: &automerge::ROOT,
        });
        for result in results {
            let contract = result
                .contract
                .unwrap_or_else(|| panic!("{} has no contract", result.id));
            assert!(
                contract.validate_declaration().is_empty(),
                "{} has invalid contract: {:?}",
                result.id,
                contract.validate_declaration()
            );
        }
    }

    #[test]
    fn contracts_match_current_availability_risk_and_document_mutation() {
        for id in ["identity.connect", "identity.select"] {
            let contract = declared(id).0;
            assert_eq!(contract.risk, tilo_actions::RiskLevel::Privileged);
            assert_eq!(
                contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
            assert!(
                Action::ALL
                    .iter()
                    .find(|action| action.id == id)
                    .expect("identity action")
                    .params
                    .iter()
                    .any(|parameter| parameter.name == "confirm")
            );
        }
        assert_eq!(
            declared("agent.invite").1,
            tilo_actions::OperationKind::Document
        );
        assert_eq!(
            declared("peer.invite.named").1,
            tilo_actions::OperationKind::Document
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "save then assign through the same headless door, with the decision checked"
    )]
    fn an_agent_can_create_a_grant_set_then_assign_it_through_the_headless_actions() {
        use crate::shell::search::action_api::ArgValue;

        let chrome = chrome_with_room();
        let mut doc = AutoCommit::new();
        let provider = ActionsProvider::new();
        let open_note = tilo_action_policy::TreePath::root()
            .app("so.tilo.notes")
            .resource("notes.page:1")
            .action("note.open");
        let rules = serde_json::to_string(&vec![tilo_action_policy::Rule::new(
            open_note.clone(),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Allow,
        )])
        .unwrap();

        let save = {
            let state = chrome.borrow();
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "preset.save",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id.ends_with("#preset.save"))
                .expect("the role creation action is headlessly discoverable")
        };
        let save_args = ActionArgs::bind(
            &save.params,
            [
                ("name".into(), ArgValue::Text("Notes reader".into())),
                (
                    "description".into(),
                    ArgValue::Text("May inspect individual notes".into()),
                ),
                (
                    "rules".into(),
                    ArgValue::Array(serde_json::from_str(&rules).expect("test rules are JSON")),
                ),
            ],
        )
        .unwrap();
        (save.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &save_args,
        )
        .unwrap();
        governed(&chrome, FIXTURE_USER);

        let assign = {
            let state = chrome.borrow();
            provider
                .query(SearchContext {
                    query_scope: None,
                    include_programmatic: false,
                    needle: "assign permission role",
                    wanted: &[ResourceKind::Action],
                    chrome: &state,
                    doc: &doc,
                    doc_root: &automerge::ROOT,
                })
                .into_iter()
                .find(|result| result.id.ends_with("#preset.assign"))
                .expect("the new grant set immediately becomes assignable")
        };
        let serde_json::Value::Object(researcher) =
            serde_json::to_value(tilo_action_policy::Subject::agent("researcher")).unwrap()
        else {
            panic!("a subject is an object");
        };
        let assign_args = ActionArgs::bind(
            &assign.params,
            [
                ("preset".into(), ArgValue::Text("Notes reader".into())),
                ("subject".into(), ArgValue::Object(researcher)),
            ],
        )
        .unwrap();
        (assign.on_open)(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            &assign_args,
        )
        .unwrap();
        governed(&chrome, FIXTURE_USER);

        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace().unwrap();
        assert_eq!(
            workspace
                .subject_authority(tilo_action_policy::Subject::agent("researcher"))
                .decide(
                    &workspace.node().join(&open_note),
                    tilo_action_policy::ActionKind::Read,
                    &tilo_desktop_model::context_at(tilo_clock::unix_now()),
                )
                .answer,
            tilo_action_policy::Answer::Allow
        );
        drop(tree);
    }

    const FIXTURE_USER: &str = "shell-fixture";

    mod membership;
    use membership::governed;
    pub(crate) use membership::invoke_shell_action;

    /// The notification centre's controls are actions too: an agent or a
    /// screen reader opens the list, clears it and brings it back as a click
    /// on "Clear all" and its Undo do, by address, with nothing to click.
    #[test]
    fn notifications_open_clear_and_come_back_by_action() {
        let provider = ActionsProvider::new();
        let chrome = chrome_with_room();
        let resolve = |id: &str| {
            let document = AutoCommit::new();
            let state = chrome.borrow();
            provider
                .resolve(
                    id,
                    SearchContext {
                        query_scope: None,
                        include_programmatic: true,
                        needle: "",
                        wanted: &[ResourceKind::Action],
                        chrome: &state,
                        doc: &document,
                        doc_root: &automerge::ROOT,
                    },
                )
                .unwrap_or_else(|| panic!("{id} is published"))
        };
        let run = |result: &crate::shell::search::SearchResult| {
            let mut document = AutoCommit::new();
            (result.on_open)(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
                &chrome,
                &ActionArgs::none(),
            )
        };

        let open = resolve("notifications.open");
        assert_eq!(open.operation_kind, tilo_actions::OperationKind::View);
        run(&open).expect("opening the list always works");
        {
            let state = chrome.borrow();
            assert!(state.overlays.quick_settings);
            assert_eq!(
                state.quick_settings.nav_stack.last(),
                Some(&tilo_quick_settings::QsLevel::Notifications)
            );
        }

        let clear = resolve("notifications.clear");
        assert!(
            matches!(run(&clear), Err(ActionError::Refused(_))),
            "an empty list says there is nothing to clear"
        );
        let id = chrome.borrow_mut().push_notification("Build finished", "");
        chrome.borrow_mut().dismiss_notification(id);
        assert_eq!(chrome.borrow().notification_history().len(), 1);
        run(&clear).expect("a list with something in it clears");
        assert!(chrome.borrow().notification_history().is_empty());
        assert!(chrome.borrow().can_undo_notification_history_clear());

        let undo = resolve("notifications.clear.undo");
        run(&undo).expect("what was just cleared comes back");
        let state = chrome.borrow();
        assert_eq!(state.notification_history().len(), 1);
        assert_eq!(state.notification_history()[0].title, "Build finished");
        assert!(!state.can_undo_notification_history_clear());
        drop(state);
        assert!(
            matches!(run(&undo), Err(ActionError::Refused(_))),
            "nothing is left to bring back"
        );
        for id in ["notifications.clear", "notifications.clear.undo"] {
            assert_eq!(
                declared(id).0.kind,
                tilo_actions::ActionKind::Undoable,
                "{id}"
            );
        }

        // The undo lasts its eight seconds whether or not anything painted.
        let id = chrome.borrow_mut().push_notification("Backup done", "");
        chrome.borrow_mut().dismiss_notification(id);
        run(&clear).expect("a list with something in it clears");
        let ended = ui::time::now() + tilo_desktop::notifications::CLEAR_UNDO_WINDOW;
        ui::time::testing::with_monotonic_time(ended, || {
            let offered = action_available("notifications.clear.undo", &chrome.borrow(), false);
            assert!(!offered && matches!(run(&undo), Err(ActionError::Refused(_))));
        });
    }

    #[test]
    fn radio_details_navigation_actions_are_views_and_do_not_toggle_radios() {
        for (id, section) in [
            (
                "quick-settings.network",
                tilo_quick_settings::QsSection::Wifi,
            ),
            (
                "quick-settings.bluetooth",
                tilo_quick_settings::QsSection::Bluetooth,
            ),
        ] {
            let provider = ActionsProvider::new();
            let chrome = chrome_with_room();
            let before = (
                chrome.borrow().quick_settings.wifi,
                chrome.borrow().quick_settings.bluetooth,
            );
            let results = action_results(&provider, &chrome);
            let result = results
                .iter()
                .find(|result| result.id == id)
                .expect("published view action");
            assert_eq!(result.operation_kind, tilo_actions::OperationKind::View);
            for _ in 0..2 {
                invoke_action(&results, id, &chrome);
                let state = chrome.borrow();
                assert!(state.overlays.quick_settings);
                // The list opens in place on the root page.
                assert_eq!(
                    state.quick_settings.nav_stack,
                    [tilo_quick_settings::QsLevel::Root]
                );
                assert_eq!(state.quick_settings.section, Some(section));
                assert_eq!(
                    (state.quick_settings.wifi, state.quick_settings.bluetooth),
                    before
                );
            }
        }
    }

    #[test]
    fn workspace_rename_names_the_workspace_on_screen_and_refuses_a_bad_name() {
        let chrome = chrome_with_room();
        let name = || {
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.name.clone())
        };
        invoke_shell_action(
            &chrome,
            "workspace.rename",
            &serde_json::json!({ "name": "  Thesis  " }),
        )
        .expect("the person here may rename their workspace");
        assert_eq!(name().as_deref(), Some("Thesis"));
        for bad in ["", "   ", "Line\nbreak"] {
            assert!(
                invoke_shell_action(
                    &chrome,
                    "workspace.rename",
                    &serde_json::json!({ "name": bad }),
                )
                .is_err(),
                "{bad:?} is no name"
            );
        }
        assert!(
            invoke_shell_action(
                &chrome,
                "workspace.rename",
                &serde_json::json!({ "name": "x".repeat(121) }),
            )
            .is_err(),
            "a name is at most 120 characters"
        );
        assert_eq!(name().as_deref(), Some("Thesis"));
    }
}
