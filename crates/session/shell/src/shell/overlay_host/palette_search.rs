//! Palette search: opening, the launchpad, results and entries.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Open search the way an app asked (`AppHostIntent::OpenSearch`): focus
    /// the app's tab, open the palette scoped to that app — or drilled into
    /// the part of it the request names — and start with its query. Unlike
    /// the shortcut, a repeated request never closes the palette.
    pub(crate) fn open_app_search(
        &mut self,
        source_tab: crate::shell::state::TabId,
        request: &ui::search::SearchRequest,
    ) {
        use crate::shell::chrome_state::SearchScope;
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let location = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop.active_workspace().and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .find(|group| group.tabs.iter().any(|tab| tab.id == source_tab))
                        .map(|group| (workspace.id, strip.id, group.id))
                })
            })
        };
        // A tab outside the workspace in front asks for nothing the person
        // can see; drop the request rather than switch workspaces.
        let Some((workspace, strip, group)) = location else {
            return;
        };
        chrome
            .borrow_mut()
            .focus_tab(workspace, strip, group, source_tab);
        if let Some(offer) = &request.offer
            && let Some(label) =
                crate::shell::search::providers::actions::app_offerable_label(&offer.action)
        {
            if !self.palette_visible || chrome.borrow().search_scope != SearchScope::All {
                self.open_palette_with_scope(SearchScope::All);
            }
            self.palette.set_query(&label);
            self.palette_offer = Some((offer.clone(), label));
            self.last_search_query = "\0app-offer\0".into();
            self.palette_visible = true;
            return;
        }
        let resource_scope = request.resource_scope().and_then(|scope| {
            let state = chrome.borrow();
            let app_id = state.tab_app_id(strip, group, source_tab)?;
            self.app_actions.as_ref()?.resource_scope_for(
                &state,
                &app_id,
                &scope.anchor,
                &scope.label,
            )
        });
        if !self.palette_visible || chrome.borrow().search_scope != SearchScope::ActiveApp {
            self.open_palette_with_scope(SearchScope::ActiveApp);
        }
        let query = request.query.clone().unwrap_or_default();
        if let Some(scope) = resource_scope {
            let label = scope.label.clone();
            {
                let mut state = chrome.borrow_mut();
                state.search_resource_scope = Some(scope);
                state.bump();
            }
            self.palette.set_query_placeholder(t_args(
                "host-search-in",
                &[("scope", label.as_str().into())],
            ));
            self.last_search_query = "\0resource-scope-enter\0".into();
        }
        self.palette.set_query(&query);
        self.palette_visible = true;
    }

    /// Open the identity chooser, as an app whose workspace has no identity
    /// selected asks (the chooser service's `ChooseIdentity`): the palette
    /// over everything, on choosing the identity this workspace uses
    /// (`identity.select`). The person picks the identity and confirms it
    /// there; nothing is chosen for them. Whether the palette shows it.
    pub(crate) fn open_identity_chooser(&mut self) -> bool {
        use crate::shell::chrome_state::SearchScope;
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        if !self.palette_visible || chrome.borrow().search_scope != SearchScope::All {
            self.open_palette_with_scope(SearchScope::All);
        }
        self.palette.set_query(&t("search-action-identity-select"));
        self.last_search_query = "\0identity-chooser\0".into();
        self.palette_visible = true;
        self.palette_layout_dirty = true;
        true
    }

    /// Open the palette in `scope`, or close it if the same scope fires
    /// twice (mirrors the old second-press-dismisses chord behaviour). A
    /// different scope while open just swaps scope without losing the
    /// typed query. `ActiveApp` degrades to `All` when no tab is focused.
    pub(super) fn open_palette_with_scope(
        &mut self,
        scope: crate::shell::chrome_state::SearchScope,
    ) {
        use crate::shell::chrome_state::SearchScope;
        // An app's prepared action belongs to the search it opened.
        self.palette_offer = None;
        let effective = if matches!(scope, SearchScope::ActiveApp)
            && self
                .chrome
                .as_ref()
                .and_then(|c| c.borrow().active_app_title())
                .is_none()
        {
            SearchScope::All
        } else {
            scope
        };
        let current_scope = self
            .chrome
            .as_ref()
            .map(|c| c.borrow().search_scope)
            .unwrap_or_default();
        if self.palette_visible && current_scope == effective {
            self.palette_visible = false;
            self.palette.close();
            return;
        }
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            st.search_scope = effective;
            st.search_resource_scope = None;
            st.bump();
        }
        if !self.palette_visible {
            self.palette_visible = true;
            self.palette.set_focus(true);
            self.palette.set_query("");
            // Empty query still runs through the registry, so the palette
            // opens with an "everything" list rather than blank.
            self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
        }
    }

    /// Consume one of the collaboration launchpad's six semantic cards.
    /// App cards resolve through the production Applications provider so an
    /// available-but-not-yet-installed app follows the same acquisition path
    /// as the command palette. Chrome cards use their existing overlays.
    pub(super) fn harvest_empty_workspace_launchpad(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) -> bool {
        use tilo_desktop::launchpad::LaunchpadAction;

        if tilo_desktop::launchpad::take_open_app(doc, root) {
            self.open_app_launcher(doc, root);
            return true;
        }
        let Some(action) = LaunchpadAction::take(doc, root) else {
            return false;
        };
        if let Some(target) = action.app_action() {
            self.invoke_launchpad_app_action(doc, root, target, action.label());
            return true;
        }
        if let Some((app_id, display_name)) = action.app_target() {
            self.open_launchpad_app(doc, root, app_id, display_name);
            return true;
        }
        match action {
            LaunchpadAction::Invite => self.open_invite_manager(),
            LaunchpadAction::Activity => {
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome.borrow_mut().open_room_activity(None);
                }
            }
            LaunchpadAction::Chat
            | LaunchpadAction::Notes
            | LaunchpadAction::Tasks
            | LaunchpadAction::Meet => {}
        }
        true
    }

    /// The app launcher Super+N and the empty workspace's "Open an app"
    /// open: the palette over everything, opening beside the focus.
    pub(super) fn open_app_launcher(&mut self, doc: &AutoCommit, root: &ObjId) {
        if let Some(chrome) = self.chrome.clone() {
            let mut st = chrome.borrow_mut();
            st.open_target = crate::shell::chrome_state::OpenTarget::Beside;
            st.search_scope = crate::shell::chrome_state::SearchScope::All;
            st.bump();
        }
        self.palette_visible = true;
        self.palette.set_focus(true);
        self.palette.set_query("");
        self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
        self.refresh_search_results(doc, root);
    }

    /// Open `app_id` from its launchpad card through the Applications
    /// provider's typed `applications/<package id>` action, the same row the
    /// launcher's Enter fires. A card chosen before that provider's index has
    /// taken in the installed apps is retried each tick rather than handing
    /// the person a pre-filled launcher to confirm.
    pub(super) fn open_launchpad_app(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        app_id: &'static str,
        display_name: &'static str,
    ) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        chrome.borrow_mut().open_target = crate::shell::chrome_state::OpenTarget::Beside;
        self.pending_launchpad_app = None;
        if !self.invoke_launchpad_app(doc, root, app_id, display_name) {
            self.pending_launchpad_app = Some(PendingLaunchpadApp {
                app_id,
                display_name,
                give_up_at: Instant::now()
                    .checked_add(LAUNCHPAD_APP_RESOLVE_WINDOW)
                    .unwrap_or_else(Instant::now),
            });
        }
    }

    /// Invoke the typed action a launchpad card names in its app, as the
    /// person this device acts for in the active workspace. The action host
    /// delivers it to the running app or launches the app into the workspace
    /// first, so the card opens Calendar on its tasks or starts the call
    /// itself rather than leaving the person to find it. A session without
    /// an action host still opens the app.
    pub(super) fn invoke_launchpad_app_action(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        target: tilo_desktop::launchpad::LaunchpadAppAction,
        title: String,
    ) {
        use tilo_actions::remote::{InvokeArgument, InvokeRequest, application_action_target};
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let Some(broker) = self.shortcut_triggers.broker() else {
            self.open_launchpad_app(doc, root, target.app_id, target.display_name);
            return;
        };
        let caller = {
            let state = chrome.borrow();
            let tree = state.tree();
            let caller = tree.desktop.active_workspace().map(|workspace| {
                let workspace_room = workspace.room.to_string();
                (
                    tilo_actions::remote::Caller::local_person(
                        state.local_user_for_room(&workspace.room),
                    )
                    .in_room(&workspace_room),
                    workspace_room,
                )
            });
            drop(tree);
            caller
        };
        let Some((caller, workspace_room)) = caller else {
            return;
        };
        chrome.borrow_mut().open_target = crate::shell::chrome_state::OpenTarget::Beside;
        let admission = crate::shell::remote_actions::BrokerActionHost::new(broker).invoke_job(
            &caller,
            InvokeRequest {
                on_behalf_of: None,
                invocation: tilo_actions::InvocationId::new(),
                target: application_action_target(
                    &workspace_room,
                    target.app_id,
                    target.resource,
                    target.action,
                ),
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
            },
        );
        self.pending_launchpad_actions
            .push(PendingLaunchpadAction { title, admission });
        self.poll_pending_launchpad_actions();
    }

    /// Settle launchpad card actions whose admission arrived. One that was
    /// refused names its card and the reason, so a card never fails silently.
    pub(super) fn poll_pending_launchpad_actions(&mut self) {
        use tilo_actions::remote::PendingPoll;
        let chrome = self.chrome.clone();
        self.pending_launchpad_actions.retain_mut(|pending| {
            let refusal = match pending.admission.poll_ready() {
                PendingPoll::Waiting => return true,
                PendingPoll::Ready(Ok(_)) => None,
                PendingPoll::Ready(Err(error)) => Some(error.to_string()),
                PendingPoll::Abandoned => Some(t("launchpad-action-unanswered")),
            };
            if let (Some(reason), Some(chrome)) = (refusal, chrome.as_ref()) {
                tracing::warn!(
                    target: "tilo::app_launch",
                    card = %pending.title,
                    %reason,
                    "launchpad card action was refused"
                );
                chrome.borrow_mut().push_warning_notification(
                    t_args(
                        "host-action-cannot-run",
                        &[("title", pending.title.as_str().into())],
                    ),
                    reason,
                );
            }
            false
        });
    }

    /// Retry a launchpad app card the Applications index could not resolve
    /// yet. Past [`LAUNCHPAD_APP_RESOLVE_WINDOW`] the app is genuinely absent
    /// from this catalogue, so the launcher opens filtered to it instead.
    pub(super) fn retry_pending_launchpad_app(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
    ) {
        let Some(pending) = self.pending_launchpad_app.take() else {
            return;
        };
        if self.invoke_launchpad_app(doc, root, pending.app_id, pending.display_name) {
            return;
        }
        if Instant::now() < pending.give_up_at {
            self.pending_launchpad_app = Some(pending);
            return;
        }
        self.active_kind_filter = vec![crate::shell::search::ResourceKind::Application];
        self.palette.set_query(pending.display_name);
        self.palette_visible = true;
        self.palette.set_focus(true);
        self.last_search_query = "\u{0}launchpad-refresh\u{0}".to_string();
        self.refresh_search_results(doc, root);
    }

    /// Invoke the app's own action when search resolves it: the installed
    /// app, or the catalogue's offer for one not installed, which opens it.
    /// `false` while neither address resolves.
    pub(super) fn invoke_launchpad_app(
        &mut self,
        doc: &mut ui::widget::Edits<'_>,
        root: &ObjId,
        app_id: &str,
        display_name: &str,
    ) -> bool {
        use crate::shell::search::action_api::{AllVisible, ResourceRef};
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let catalogue_id = format!("app:{app_id}");
        let resolved = [
            ("applications", app_id),
            (
                crate::shell::search::providers::store_catalogue::PROVIDER_ID,
                catalogue_id.as_str(),
            ),
        ]
        .into_iter()
        .find_map(|(provider, id)| {
            let resource = ResourceRef::new(provider, id);
            let catalog = {
                let state = chrome.borrow();
                self.action_catalog_for_resource(&resource, &state, doc, root, &AllVisible)
            };
            catalog.contains(&resource).then_some((catalog, resource))
        });
        let Some(action) = resolved
            .as_ref()
            .and_then(|(catalog, resource)| catalog.select(resource))
        else {
            return false;
        };
        match action.invoke_nullary(doc, root, &chrome) {
            // Application actions own their user-facing launch notice (the
            // callback has already translated any typed LaunchError into a
            // bounded, actionable card). Keep this outer launchpad adapter
            // from adding a second raw ActionError card, which used to put
            // paths/backend details into the fixed top-right toast and leave
            // the user with two conflicting errors.
            Err(error) => Self::report_launchpad_action_failure(display_name, &error),
            Ok(_) => {
                if self.palette_visible {
                    self.palette_visible = false;
                    self.palette.close();
                }
            }
        }
        true
    }

    /// The application action callback owns the user-facing notice for a
    /// launch refusal. The launchpad adapter only records the detailed cause
    /// for diagnostics; adding another notification here would duplicate the
    /// callback's bounded card and expose raw backend details in top-right
    /// chrome.
    pub(super) fn report_launchpad_action_failure(
        display_name: &str,
        error: &crate::shell::search::action_api::ActionError,
    ) {
        tracing::error!(
            target: "tilo::app_launch",
            display_name,
            detail = %error,
            "launchpad application action was refused"
        );
    }

    /// Refreshes the palette's rows by fanning the query out to every
    /// registered [`crate::shell::search::SearchProvider`]. Cheap on a warm
    /// cache; the first open pays for the files provider's deferred home walk
    /// and content index open (kept off the `OverlayHost` constructor on purpose).
    pub(super) fn selected_search_identity(&self) -> Option<(String, String)> {
        self.palette
            .selected_id()
            .and_then(|id| id.strip_prefix("result:"))
            .and_then(|id| id.parse::<usize>().ok())
            .and_then(|index| self.search_results.get(index))
            .map(|result| (result.provider_id.clone(), result.id.clone()))
    }

    pub(super) fn refresh_search_results(&mut self, doc: &automerge::AutoCommit, root: &ObjId) {
        self.poll_pending_action_admissions();
        // A step collecting an argument owns the list until it ends.
        if self.param_flow.is_some() || self.secondary_flow.is_some() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        // "Brighter" and "dimmer" act on the display the person works on.
        let focused = self.focused_connector();
        if chrome.borrow().quick_settings.focused_display != focused {
            chrome.borrow_mut().quick_settings.focused_display = focused;
        }
        let current_query = self.palette.query().to_string();
        let preserve_selection = current_query == self.last_search_query;
        if !preserve_selection {
            // "Show all" belongs to the search it was asked in.
            self.expanded_kinds.clear();
        }
        let (scope, active_provider) = {
            let st = chrome.borrow();
            let scope = st.search_scope;
            let active_provider = active_tab_provider_id(&st, &self.search_registry);
            (scope, active_provider)
        };
        let groups = {
            let st = chrome.borrow();
            self.search_chrome_revision = st.revision;
            let args = crate::shell::search::QueryArgs {
                needle: &current_query,
                scope,
                active_provider_id: active_provider.as_deref(),
                wanted_kinds: &self.active_kind_filter,
                chrome: &st,
                doc,
                doc_root: root,
            };
            let revision = self.search_registry.query_revision(&args);
            // Short-circuits no-op refreshes from peer-state polling or
            // palette re-entry, while allowing an open palette to see a
            // changed retained provider index.
            if current_query == self.last_search_query && revision == self.last_search_revision {
                return;
            }
            self.last_search_query.clone_from(&current_query);
            self.last_search_revision = revision;
            // Every hit, grouped; the palette's own height decides how many
            // each group shows below.
            self.search_registry.query_grouped(&args, 0)
        };
        self.note_present_kinds(&groups);
        let groups = palette_flow::trim_groups(
            groups,
            self.palette.entry_row_capacity(),
            &self.expanded_kinds,
        );

        // Provider revisions may insert/re-rank rows without a query edit.
        // Keep the resource the person chose, never its old callback or row
        // index. A selection the person never moved is the old best result:
        // it follows the best result, so a slower provider (Flathub) that
        // ranks above it after typing takes the selection.
        let person_moved = self.palette.selected_id() != self.auto_selected_row.as_deref();
        let previous_selection = if preserve_selection && person_moved {
            self.selected_search_identity()
        } else {
            None
        };

        // Kind travels as the row's section and glyph, not a `[Files]`
        // literal on the subtitle, which pushed the distinguishing part
        // of a path off the right edge.
        let mut results: Vec<crate::shell::search::SearchResult> = Vec::new();
        let mut entries: Vec<CommandEntry> = Vec::new();
        self.more_rows.clear();
        for group in &groups {
            let label = group.kind.localized_label();
            for r in &group.results {
                let idx = results.len();
                let e = self
                    .annotated_entry(r, format!("result:{idx}"), &current_query)
                    .group(label.clone())
                    .group_total(group.total);
                entries.push(e);
                results.push(r.clone());
            }
            if palette_flow::is_capped(group) {
                // The rest of a group the budget cut short is one row away,
                // in place, under its own header.
                let more = self.more_rows.len();
                self.more_rows.push(group.kind.clone());
                entries.push(
                    CommandEntry::new(
                        format!("{}{more}", palette_flow::MORE_PREFIX),
                        t_args("palette-show-all", &[("count", group.total.into())]),
                    )
                    .icon(ui::icons::Icon::ChevronDown)
                    .group(label.clone())
                    .group_total(group.total),
                );
            }
        }

        // Peer-sync rows are ordinary `ActionsProvider` results, ranked
        // against everything else rather than appended below every hit.
        let selected_id = previous_selection
            .and_then(|(provider, id)| {
                results
                    .iter()
                    .position(|result| result.provider_id == provider && result.id == id)
            })
            .map(|index| format!("result:{index}"));
        self.palette.set_entries(entries);
        let kept = if let Some(id) = selected_id
            && let Some(cursor) = self
                .palette
                .visible_entries()
                .iter()
                .position(|entry| entry.id == id)
        {
            self.palette.set_cursor(cursor);
            true
        } else {
            false
        };
        self.auto_selected_row = if kept {
            None
        } else {
            self.palette.selected_id().map(str::to_owned)
        };
        self.search_results = results;
        self.sync_filter_band();
        // New entry count needs a new palette node height on the next
        // rebuild, or the palette stays pinned at its first-paint size.
        self.palette_layout_dirty = true;
    }

    /// Keep an open palette's rows true to what they show: rebuild them when
    /// the query changed without a keystroke (Escape cleared it, a scope
    /// was left) or when the chrome state rows read has moved (a switch
    /// just flipped, a device answered). The registry's own revision check
    /// keeps an unchanged answer from being rebuilt.
    pub(super) fn refresh_open_palette(&mut self, doc: &automerge::AutoCommit, root: &ObjId) {
        if !self.palette_visible || self.param_flow.is_some() || self.secondary_flow.is_some() {
            return;
        }
        let revision = self.chrome.as_ref().map(|chrome| chrome.borrow().revision);
        if self.palette.query() != self.last_search_query
            || revision.is_some_and(|revision| revision != self.search_chrome_revision)
        {
            self.refresh_search_results(doc, root);
        }
        self.sync_scope_token();
    }

    /// Show the scope as a token at the start of the field: nothing for
    /// "Everywhere", else what the search is narrowed to.
    pub(super) fn sync_scope_token(&mut self) {
        use crate::shell::chrome_state::SearchScope;
        let scope = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow().search_scope)
            .unwrap_or_default();
        let token = (scope != SearchScope::All).then(|| self.search_scope_pill_label(scope));
        self.palette.set_scope_token(token);
    }

    /// Backspace took the scope token away: search one scope wider.
    pub(super) fn widen_search_scope(&mut self) {
        use crate::shell::chrome_state::SearchScope;
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let scopes = self.search_scope_list();
        let mut state = chrome.borrow_mut();
        state.search_resource_scope = None;
        let at = scopes
            .iter()
            .position(|scope| *scope == state.search_scope)
            .unwrap_or(0);
        state.search_scope = scopes
            .get(at.saturating_add(1))
            .copied()
            .unwrap_or(SearchScope::All);
        state.bump();
        drop(state);
        // Never equal to a real query: the next frame fans out again.
        self.last_search_query = "\0scope-widened\0".into();
        self.sync_scope_token();
    }

    /// The kinds an unfiltered search found, most hits first: the filter
    /// band offers these and nothing else.
    pub(super) fn note_present_kinds(&mut self, groups: &[crate::shell::search::KindGroup]) {
        if !self.active_kind_filter.is_empty() {
            return;
        }
        // Ranking can split one kind into runs apart (a web row from two
        // providers): it is one chip, counting every hit it holds.
        let mut present: Vec<(usize, crate::shell::search::ResourceKind)> = Vec::new();
        for group in groups {
            match present.iter_mut().find(|(_, kind)| *kind == group.kind) {
                Some((total, _)) => *total = total.saturating_add(group.total),
                None => present.push((group.total, group.kind.clone())),
            }
        }
        present.sort_by_key(|(total, _)| std::cmp::Reverse(*total));
        self.present_kinds = present.into_iter().map(|(_, kind)| kind).collect();
    }

    /// A result's palette row, with what only search knows: whether ←/→
    /// step its value, and the hidden word it matched through.
    pub(super) fn annotated_entry(
        &self,
        r: &crate::shell::search::SearchResult,
        id: String,
        query: &str,
    ) -> CommandEntry {
        let mut e = self.palette_entry_for(r, id);
        if self.search_registry.adjustable(r) {
            e = e.adjustable();
        }
        // A row found through a word it doesn't show says which: a fact of
        // its own after what the row already says, never glued on with a
        // separator glyph.
        if let Some(word) = self.search_registry.match_reason(r, query) {
            let reason = t_args("search-matches", &[("word", word.into())]);
            let mut facts = if e.description_facts.is_empty() {
                e.description.take().into_iter().collect()
            } else {
                std::mem::take(&mut e.description_facts)
            };
            facts.push(reason);
            e = e.facts(facts);
        }
        e
    }

    /// One result as a palette row: its picture, its text, its state, and
    /// where it can open.
    pub(super) fn palette_entry_for(
        &self,
        r: &crate::shell::search::SearchResult,
        id: String,
    ) -> CommandEntry {
        use crate::shell::search::ResourceKind;
        let mut e = CommandEntry::new(id, r.title.clone()).icon(
            self.search_registry
                .row_icon(r)
                .unwrap_or_else(|| ui::search::system::icon_for_kind(&r.kind)),
        );
        // The app's own artwork is what a person recognises it by; the kind
        // glyph is only the fallback. A Tilo app wears its artwork or its
        // own symbol, never the generic grid. Window rows wear the same
        // tile face as the tab strip.
        match &r.kind {
            ResourceKind::HostApplication => {
                if let Some(image) = crate::app_icons::for_desktop_id(&r.id) {
                    e = e.image(image);
                }
            }
            ResourceKind::Application => {
                let package = r.id.strip_prefix("app:").unwrap_or(&r.id);
                e = e.icon(ui::icons::app_glyph(package, None));
                if let Some(image) = crate::app_icons::for_window_app_id(package) {
                    e = e.image(image);
                }
            }
            ResourceKind::Tab | ResourceKind::WindowGroup => {
                if let Some(chrome) = &self.chrome {
                    let state = chrome.borrow();
                    let tree = state.tree();
                    if let Some(tile) = tilo_desktop::search::window_row_tile(&tree.desktop, &r.id)
                    {
                        match crate::app_icons::for_tile(tile) {
                            Some(image) => e = e.image(image),
                            None => e = e.icon(crate::app_icons::glyph_for_tile(tile)),
                        }
                    }
                }
            }
            _ => {}
        }
        // Artwork only the row's provider knows: an app this device does
        // not have yet wears its catalogue icon.
        if let Some(image) = self.search_registry.row_image(r) {
            e = e.image(image);
        }
        if !r.facts.is_empty() {
            e = e.facts(r.facts.clone());
        } else if let Some(sub) = &r.subtitle {
            e = e.description(sub.clone());
            if matches!(r.kind, ResourceKind::File | ResourceKind::Folder) {
                e = e.path_description();
            }
        }
        if let Some(badge) = &r.badge {
            e = e.badge(badge.clone());
        }
        if let Some(shortcut) = &r.shortcut {
            e = e.keybinding(shortcut.clone());
        }
        if r.params.is_empty() && palette_flow::opens_a_window(&r.kind) {
            use ui::widget::command_palette::ActivationModifier;
            e = e
                .alternate(
                    ActivationModifier::Shift,
                    palette_flow::Activation::TabHere.label(),
                )
                .alternate(
                    ActivationModifier::Ctrl,
                    palette_flow::Activation::Beside.label(),
                );
            if r.kind == ResourceKind::File || !self.search_registry.secondary_actions(r).is_empty()
            {
                e = e.alternate(
                    ActivationModifier::Alt,
                    palette_flow::Activation::More.label(),
                );
            }
        }
        e
    }

    /// Show the filter band only when it has something to tell apart: two
    /// kinds or more in the results, or a filter already chosen.
    pub(super) fn sync_filter_band(&mut self) {
        // A chosen filter keeps its band, so it can be changed or cleared
        // (the "+" picker opens on one before anything is typed).
        let shown = !self.active_kind_filter.is_empty()
            || (!self.palette.query().is_empty() && self.present_kinds.len() > 1);
        if self.palette.set_filter_band_shown(shown) {
            self.palette_layout_dirty = true;
        }
    }

    pub(super) fn poll_pending_action_admissions(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        self.pending_action_admissions
            .retain(|(title, ticket)| match ticket() {
                Ok(None) => true,
                Ok(Some(receipt)) => {
                    // Say what started in the action's own words; the
                    // broker's job number is not something people know.
                    tracing::debug!(job = ?receipt.broker_invocation_id, "action admitted");
                    chrome.borrow_mut().push_notification_full(
                        tilo_desktop::notifications::Notification::new(
                            t_args("host-action-started", &[("title", title.as_str().into())]),
                            String::new(),
                        )
                        .with_app_name(t("host-source-app-actions")),
                    );
                    false
                }
                Err(error) => {
                    // The palette row is gone by now, so a notice is the only
                    // place left to say it.
                    tracing::info!(code = error.code(), detail = %error, "action was not admitted");
                    chrome.borrow_mut().push_warning_notification(
                        t_args(
                            "host-action-cannot-run",
                            &[("title", title.as_str().into())],
                        ),
                        error.user_message(),
                    );
                    false
                }
            });
    }
}
