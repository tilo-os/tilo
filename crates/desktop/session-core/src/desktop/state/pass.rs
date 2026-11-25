//! The desktop's own pass: what the session does with its desktop every frame, whoever draws it.
//!
//! The tree takes the verbs queued since the last pass ([`DesktopHandle::apply_queued`]), search
//! follows what was asked of it and carries out what the person chose, the windows are served the
//! workspace's activity, the notifications run their course, and the tree is saved and shared once
//! it changed. None of it reads a widget: the shell that draws the desktop reads the tree and the
//! state this leaves.

use std::rc::Rc;

use tilo_desktop_model::{GroupId, StripId};
use tilo_desktop_search::{LaunchPlace, SearchData, Target};
use ui::search::{AppScope, Scope};

use crate::desktop::navigation::{self, Admitted, NotificationsState};
use crate::desktop::session::{ActivityAsk, ActivityReads, ActivityRequests};

use super::history::{ActivityPaneCode, HistorySource, Served};
use super::notices::{AppInbox, NotificationOutcome};
use super::search::{
    CLOSE_WINDOW, MAXIMIZE_WINDOW, SearchRequest, SearchRow, ShownSearch, TypedAhead, TypedAsk,
    TypedRefusal, commands, host_choice, scope_name, typed_field, window_app, windows,
    workspace_holding,
};
use super::{
    DesktopHandle, DesktopNotifications, RowOp, TileSource, find_group, holder_refusal, keeps,
    route_of, unavailable,
};

/// What the desktop keeps between its passes beyond the tree: the search it carries out, the
/// workspace's activity it serves, and the applications' notifications it shows.
pub struct DesktopPass {
    handle: DesktopHandle,
    /// The window group a new-tab control opened search for: the application chosen there opens
    /// as its tab.
    new_tab_into: Option<(StripId, GroupId)>,
    /// An action chosen in search, for the host's own action API to run as the app's control.
    unrouted_control: Option<ui::tilo_actions::ControlInvocation>,
    platform: ui::platform::PlatformConventions,
    /// The host's retained workspace history, when it keeps one.
    history: Option<Box<dyn HistorySource>>,
    /// What the windows ask of the workspace's activity, when the host serves it to them.
    activity_requests: Option<ActivityRequests>,
    /// The activity model, while a window follows it or asks of it.
    activity: Option<Box<dyn Served>>,
    /// Applications' notifications, when the host routes them here.
    inbox: Option<AppInbox>,
}

impl DesktopPass {
    /// The pass over the desktop `handle` holds.
    #[must_use]
    pub fn new(handle: DesktopHandle) -> Self {
        Self {
            handle,
            new_tab_into: None,
            unrouted_control: None,
            platform: ui::platform::PlatformConventions::default(),
            history: None,
            activity_requests: None,
            activity: None,
            inbox: None,
        }
    }

    /// The desktop this pass runs over.
    #[must_use]
    pub fn handle(&self) -> &DesktopHandle {
        &self.handle
    }

    /// The keys the platform names its chords with, which search shows.
    pub fn set_platform_conventions(&mut self, platform: ui::platform::PlatformConventions) {
        self.platform = platform;
    }

    /// Read workspace history from `source`: the Activity app then shows it, once the windows are
    /// served what they ask of it ([`Self::serve_activity`]).
    pub fn set_history_source(&mut self, source: Box<dyn HistorySource>) {
        self.history = Some(source);
        let mut shared = self.handle.shared.borrow_mut();
        let mut next = shared.presentation.clone();
        next.retained_history = true;
        shared.present(next);
    }

    /// Answer what the windows ask of the workspace's activity through `requests`.
    pub fn serve_activity(&mut self, requests: ActivityRequests) {
        self.activity_requests = Some(requests);
    }

    /// Carry out what the person does with `notifications`: the banners and the centre the shell
    /// draws, which a `notification-center` command opens.
    pub fn set_notifications(&mut self, notifications: DesktopNotifications) {
        self.handle.shared.borrow_mut().notifications = Some(notifications);
        self.refresh_notifications();
    }

    /// Show applications' notifications from `inbox`, attributed to the window that posted each.
    /// Choosing one focuses that application's window and hands the application its activation.
    pub fn set_app_notifications(&mut self, inbox: AppInbox) {
        self.inbox = Some(inbox);
    }

    /// The action a row chosen in search runs, for the host's action API to run as the app's own
    /// control would.
    pub fn take_unrouted_control(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.unrouted_control.take()
    }

    /// One pass over the desktop, launching what it asks through `tiles`. Whether anything the
    /// shell shows changed.
    pub fn step(&mut self, tiles: &mut dyn TileSource) -> bool {
        let _wake = self.handle.shared.borrow().wake.enter();
        let before = self.handle.shared.borrow().revision;
        let search_revision = self.handle.shared.borrow().search_revision;
        self.apply(tiles);
        // Search opens over the tree as these verbs left it, and what it chose applies in this
        // same pass.
        self.step_search(tiles);
        self.apply(tiles);
        self.follow_attention();
        self.sync_activity();
        self.refresh_notifications();
        // The history or the centre opened a window.
        self.apply(tiles);
        self.handle.follow_bars();
        self.home_windows();
        self.publish();
        // Where the windows stand follows from the tree as this pass left it, for every host,
        // and what the strip in front presents beside it: its pan, which the navigation snapshot
        // and the `pan` verb read.
        let moved = super::shown::lay_out(&mut self.handle.shared.borrow_mut());
        super::shown::present_pan(&mut self.handle.shared.borrow_mut());
        let shared = self.handle.shared.borrow();
        moved || shared.revision != before || shared.search_revision != search_revision
    }

    /// Give every window a display, and the display the person is on the strip it shows: a window
    /// that opened with none, or whose display went, stands on the focused one.
    fn home_windows(&self) {
        let mut shared = self.handle.shared.borrow_mut();
        let tree = &mut shared.tree;
        if !tree.is_partitioned() {
            return;
        }
        let homed = tree.outputs.place_unhomed(&tree.workspaces);
        let noted = tree.note_shown_strip();
        if homed || noted {
            shared.changed();
        }
    }

    /// Apply the queued verbs to the tree: a window opened over the shell opens as a card beside
    /// the window it opened next to, one asked to float lifts over the middle of the display it
    /// stands on, and the strips bring the windows opened, restored or moved into view and pan as
    /// asked. The shell reads every other change from the tree.
    fn apply(&self, tiles: &mut dyn TileSource) {
        let Some(ops) = self.handle.apply_queued(tiles) else {
            return;
        };
        for op in ops {
            match op {
                RowOp::Open {
                    strip,
                    group,
                    float: true,
                    ..
                } => {
                    let mut shared = self.handle.shared.borrow_mut();
                    let display = shared.tree.outputs.display_of(group);
                    let area = super::shown::work_area(&shared, display);
                    if let Some(beside) =
                        super::floats::open_as_card(&mut shared, strip, group, area)
                    {
                        shared.reveal_asked.push((strip, beside));
                    }
                }
                RowOp::Float(strip, group) => {
                    let mut shared = self.handle.shared.borrow_mut();
                    let display = shared.tree.outputs.display_of(group);
                    let area = super::shown::work_area(&shared, display);
                    // A window that opened as a card goes back to the card it was.
                    if shared.cards_born.contains(&group) {
                        if let Some(beside) =
                            super::floats::open_as_card(&mut shared, strip, group, area)
                        {
                            shared.reveal_asked.push((strip, beside));
                        }
                    } else {
                        drop(shared);
                        self.handle.float_over(strip, group, area);
                    }
                }
                RowOp::Open { strip, group, .. } => {
                    let mut shared = self.handle.shared.borrow_mut();
                    share_with_neighbour(&mut shared, strip, group);
                    shared.reveal_asked.push((strip, group));
                }
                RowOp::Reveal(strip, group) => {
                    self.handle
                        .shared
                        .borrow_mut()
                        .reveal_asked
                        .push((strip, group));
                }
                RowOp::Pan(strip, offset) => {
                    let mut shared = self.handle.shared.borrow_mut();
                    let focused = shared.tree.outputs.focused_display();
                    for ((panned, display), pan) in &mut shared.pans {
                        if *panned == strip && (display.is_none() || *display == focused) {
                            pan.scroll = offset.max(0.0);
                        }
                    }
                    shared.wake.wake();
                }
                _ => {}
            }
        }
    }

    /// Save and share the tree, when it changed since it was last shared.
    fn publish(&self) {
        let publish = {
            let mut shared = self.handle.shared.borrow_mut();
            if shared.published == shared.revision {
                return;
            }
            shared.published = shared.revision;
            shared.publish.clone()
        };
        if let Some(publish) = publish {
            publish(&self.handle);
        }
    }

    /// The window asking for the person stops asking once they look at it, and a closed window
    /// waits for no one. The newest window an agent opened for this person comes into view.
    fn follow_attention(&self) {
        let mut shared = self.handle.shared.borrow_mut();
        let looking = navigation::active_route(&shared.tree).map(|route| route.tab);
        let super::Shared {
            asking,
            tree,
            arrivals,
            opened_by,
            holders,
            revealing,
            ..
        } = &mut *shared;
        asking.retain(|tab| Some(*tab) != looking && route_of(tree, *tab).is_some());
        let mut newest: Option<(ui::time::Instant, tilo_desktop_model::TabId)> = None;
        for arrival in arrivals.iter_mut().filter(|arrival| !arrival.revealed) {
            if opened_by.contains_key(&arrival.tab) && !keeps(holders, arrival.tab) {
                arrival.revealed = true;
                if newest.is_none_or(|(at, _)| at <= arrival.at) {
                    newest = Some((arrival.at, arrival.tab));
                }
            }
        }
        if let Some((_, tab)) = newest {
            revealing.asked = Some(tab);
        }
    }

    /// Retire banners that ran out, open what the centre asked to, and record what the
    /// notifications show.
    fn refresh_notifications(&mut self) {
        let Some(notifications) = self.handle.shared.borrow().notifications.clone() else {
            return;
        };
        if let Some(inbox) = self.inbox.as_mut() {
            let shared = self.handle.shared.borrow();
            let tree = &shared.tree;
            inbox.drain(&notifications, &|tab| {
                let ids = route_of(tree, tab)?;
                let (workspace, model) = super::find_tab(tree, ids.strip, tab)?;
                match &model.tile {
                    tilo_desktop_model::TileContent::LocalApp { app_id, label, .. } => {
                        Some((app_id.clone(), label.clone(), Some(workspace.id())))
                    }
                    _ => None,
                }
            });
        }
        notifications.expire(ui::time::now());
        let mut open_app = notifications.take_open_app();
        for outcome in notifications.take_outcomes() {
            let NotificationOutcome::Activated {
                notification,
                action,
            } = outcome
            else {
                continue;
            };
            if let Some(choice) = action.as_deref().and_then(host_choice) {
                self.handle
                    .shared
                    .borrow_mut()
                    .host_choices
                    .push(choice.to_owned());
                continue;
            }
            if let Some(uri) = self
                .inbox
                .as_mut()
                .and_then(|inbox| inbox.take_activation(notification.id))
            {
                ui::notify::report_activation(&uri);
            }
            if let Some(app) = notification.app_id {
                open_app = Some(app);
            }
        }
        let (active, retained) = notifications.counts();
        let state = NotificationsState {
            active,
            retained,
            center_open: notifications.centre_open(),
        };
        if let Some(app) = open_app
            && !self.handle.focus_app(&app)
        {
            self.handle
                .shared
                .borrow_mut()
                .queue
                .push(Admitted::Launch(app, false));
        }
        let mut shared = self.handle.shared.borrow_mut();
        let mut next = shared.presentation.clone();
        next.notifications = Some(state);
        shared.present(next);
    }

    /// Serve the windows what they asked of the workspace's activity: the activity model is open
    /// while one follows it or asks of it, for the active workspace, and each read it follows is
    /// read again whenever the model moved.
    fn sync_activity(&mut self) {
        let Some(requests) = self.activity_requests.clone() else {
            return;
        };
        // A view offered to the app stays offered until it showed it.
        let wanted = requests.wanted()
            || self.handle.shared.borrow().activity_focus.is_some()
            || self
                .activity
                .as_ref()
                .is_some_and(|model| model.offers_focus());
        let workspace = self.handle.shared.borrow().tree.active;
        if let Some(mut old) = self
            .activity
            .take_if(|model| !wanted || model.workspace() != workspace)
        {
            old.finish();
        }
        if !wanted {
            return;
        }
        let Some(source) = self.history.as_mut() else {
            unavailable(&requests);
            return;
        };
        if self.activity.is_none() {
            match source.activity_pane() {
                ActivityPaneCode::Ready(open) => {
                    let shared = self.handle.shared.borrow();
                    self.activity = open(source.as_mut(), &shared.tree, shared.revision, workspace)
                        .map(|model| model.0);
                }
                // The host still loads it: what the windows asked waits for a later pass.
                ActivityPaneCode::Loading => return,
                ActivityPaneCode::Unavailable => {}
            }
        }
        let Some(model) = self.activity.as_mut() else {
            self.handle.shared.borrow_mut().activity_focus = None;
            unavailable(&requests);
            return;
        };
        let mut opened = None;
        {
            let mut shared = self.handle.shared.borrow_mut();
            model.follow(source.as_mut(), &shared.tree, shared.revision);
            if let Some((view, author)) = shared.activity_focus.take() {
                model.focus(view, author);
            }
        }
        for ask in requests.take() {
            match ask {
                ActivityAsk::Act {
                    action,
                    arguments,
                    answer,
                } => answer.complete(model.act(&action, &arguments).map(|open| {
                    if open.is_some() {
                        opened = open;
                    }
                })),
                ActivityAsk::BeginSummary => model.begin_summary(),
                ActivityAsk::EndSummary => model.end_summary(),
            }
        }
        let reads: &mut dyn ActivityReads = model.as_mut();
        requests.serve(reads);
        requests.wake_waiting();
        // A record opened from the history: its window comes forward.
        if let Some(target) = opened {
            let mut shared = self.handle.shared.borrow_mut();
            if let Some(ids) = source
                .resource_tab(&shared.tree, &target)
                .and_then(|tab| route_of(&shared.tree, tab))
            {
                shared.queue.push(Admitted::Select(ids));
            }
        }
    }

    /// Take a host's search request, follow the palette, and carry out what was chosen.
    fn step_search(&mut self, tiles: &mut dyn TileSource) {
        let search = Rc::clone(&self.handle.shared.borrow().search);
        let opened = search.borrow().is_open();
        let request = self.handle.shared.borrow_mut().search_request.take();
        match (request, opened) {
            (Some(SearchRequest::Again), true) => {
                let (scope, query) = {
                    let search = search.borrow();
                    (search.scope(), search.query().to_owned())
                };
                self.open_search(tiles, scope, Some(query));
            }
            (Some(SearchRequest::Open { scope, query }), _) => {
                self.open_search(tiles, scope, query);
            }
            (Some(SearchRequest::Toggle), false) => {
                self.open_search(tiles, Scope::Workspace, None);
            }
            // Over one app's own search, the person's chord opens the shell's search, and closes
            // the shell's own, as a toggle does.
            (Some(SearchRequest::Toggle), true) => {
                let shell = matches!(
                    search.borrow().scope(),
                    Scope::Workspace | Scope::Everywhere
                );
                if shell {
                    self.close_search();
                } else {
                    self.open_search(tiles, Scope::Workspace, None);
                }
            }
            (Some(SearchRequest::Close), true) => self.close_search(),
            (Some(SearchRequest::Close | SearchRequest::Again) | None, _) => {}
        }
        self.take_up_typed_ahead();
        let choices = std::mem::take(&mut self.handle.shared.borrow_mut().search_asks);
        // What the person's press chose is carried out as that press's, the rows its choice
        // leads to included.
        let press = choices
            .iter()
            .find_map(|(.., press)| *press)
            .or_else(tilo_actions::remote::press_points::handled);
        for (index, argument, way, chosen_by) in choices {
            let mut search = search.borrow_mut();
            if !search.is_open() {
                break;
            }
            let held = match way.as_deref() {
                Some("here") => Some(ui::search::ActivationModifier::Shift),
                Some("beside") => Some(ui::search::ActivationModifier::Ctrl),
                Some("more") => Some(ui::search::ActivationModifier::Alt),
                _ => None,
            };
            tilo_actions::remote::press_points::handling(chosen_by, || {
                search.choose_with(index, argument, held)
            });
        }
        let ask = self.handle.shared.borrow_mut().typed_ask.take();
        if let Some(ask) = ask {
            self.commit_typed(&ask);
        }
        tilo_actions::remote::press_points::handling(press, || self.follow_search(tiles));
        self.publish_search_rows();
    }

    /// Open search at `scope`, with `query` typed: over what the workspace in front holds, over
    /// every workspace, over what an app's window holds, or over what a window group's new tab may
    /// show.
    fn open_search(&mut self, tiles: &dyn TileSource, scope: Scope, query: Option<String>) {
        // An app's search runs in that app's window; a window group's applications open into it.
        // The others stand over the shell.
        let window = match &scope {
            Scope::InApp(app) => self.app_window(app),
            Scope::WindowGroup(Some(group)) => {
                self.group_strip(*group).map(|strip| (strip, *group))
            }
            Scope::WindowGroup(None) | Scope::Workspace | Scope::Everywhere => None,
        };
        let (scope, window, data, usage) = match (scope, window) {
            (Scope::InApp(app), Some((strip, group))) => {
                let scope = Scope::InApp(app);
                let data = self.window_search_data(strip, group, &scope);
                (scope, window, data, None)
            }
            // An app with no window open has nothing of its own to search: the search opens over
            // the workspace instead.
            (Scope::InApp(_), None) => {
                let scope = Scope::Workspace;
                let (data, usage) = self.shell_search_data(tiles, &scope);
                (scope, None, data, usage)
            }
            // A window group's new tab searches as the workspace does, into the group.
            (scope @ Scope::WindowGroup(_), _) => {
                let (data, usage) = self.shell_search_data(tiles, &scope);
                (scope, window, data, usage)
            }
            (scope @ (Scope::Workspace | Scope::Everywhere), _) => {
                let (data, usage) = self.shell_search_data(tiles, &scope);
                (scope, None, data, usage)
            }
        };
        let refusal = self.handle.shared.borrow_mut().search_refusal.take();
        let search = Rc::clone(&self.handle.shared.borrow().search);
        let mut search = search.borrow_mut();
        search.open(data);
        if let Some((target, why)) = refusal {
            search.refuse(target, why);
        }
        if let Some(usage) = usage {
            search.set_usage(usage);
        }
        ui::widget::Widget::set_platform_conventions(search.palette_mut(), self.platform);
        if let Some(query) = query {
            search.set_query(&query);
        }
        drop(search);
        // What a chosen application opens in: a tab of the window group the new-tab control
        // belongs to, else a window of its own.
        self.new_tab_into = match &scope {
            Scope::WindowGroup(_) => window,
            _ => None,
        };
        self.handle.note_search_activity();
    }

    fn close_search(&self) {
        self.handle.shared.borrow().search.borrow_mut().close();
        self.handle.note_search_activity();
    }

    /// The window an app is open in: its window in front, or the window in front when the scope
    /// names no app.
    fn app_window(&self, app: &AppScope) -> Option<(StripId, GroupId)> {
        let shared = self.handle.shared.borrow();
        match &app.app {
            Some(app) => {
                navigation::window_of_app(&shared.tree, app).map(|ids| (ids.strip, ids.group))
            }
            None => navigation::active_route(&shared.tree).map(|ids| (ids.strip, ids.group)),
        }
    }

    /// The window group in front: where a row chosen as "a tab here" opens.
    fn window_in_front(&self) -> Option<(StripId, GroupId)> {
        let shared = self.handle.shared.borrow();
        navigation::active_route(&shared.tree).map(|ids| (ids.strip, ids.group))
    }

    /// The strip holding the window group `group`, wherever it is.
    fn group_strip(&self, group: GroupId) -> Option<StripId> {
        let shared = self.handle.shared.borrow();
        shared
            .tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .find(|strip| strip.window_groups.iter().any(|window| window.id == group))
            .map(|strip| strip.id)
    }

    /// What window `group`'s app publishes into this session's index and the actions it offers,
    /// as its in-app search finds them, narrowed to the part of the app `scope` names.
    fn window_search_data(&self, strip: StripId, group: GroupId, scope: &Scope) -> SearchData {
        let part = match scope {
            Scope::InApp(app) => app.part.clone(),
            _ => None,
        };
        let shared = self.handle.shared.borrow();
        let searched = part
            .as_ref()
            .map(|part| part.label.clone())
            .or_else(|| window_app(&shared.tree, group))
            .filter(|name| !name.is_empty());
        let feed = shared
            .search_host
            .clone()
            .map(|host| {
                let workspace = workspace_holding(&shared.tree, strip).unwrap_or_default();
                host.feed(&workspace, scope)
            })
            .unwrap_or_default();
        SearchData {
            published: feed.published,
            sources: feed.sources,
            resource_options: feed.resource_options,
            scope: scope.clone(),
            placeholder: searched.map(|name| {
                crate::desktop::i18n::t_args(
                    "search-window-placeholder",
                    &[("searched", name.into())],
                )
            }),
            ..SearchData::default()
        }
    }

    /// What the desktop has open now and what its host adds: over the workspace in front, or over
    /// every workspace.
    fn shell_search_data(
        &self,
        tiles: &dyn TileSource,
        scope: &Scope,
    ) -> (SearchData, Option<tilo_desktop_search::Usage>) {
        let (windows, commands, workspace, search_host) = {
            let shared = self.handle.shared.borrow();
            (
                windows(&shared.tree, &shared.holders),
                commands(&shared.tree, &shared.host_actions, &|tab| {
                    keeps(&shared.holders, tab)
                }),
                shared
                    .tree
                    .active_workspace()
                    .map(|workspace| workspace.id().to_string())
                    .unwrap_or_default(),
                shared.search_host.clone(),
            )
        };
        let mut feed = search_host
            .map(|search| search.feed(&workspace, scope))
            .unwrap_or_default();
        let usage = feed.usage.take();
        let mut applications = tiles.search_applications();
        applications.extend(feed.applications);
        let data = SearchData {
            applications,
            windows,
            commands,
            scope: scope.clone(),
            published: feed.published,
            sources: feed.sources,
            resource_options: feed.resource_options,
            preferences: feed.preferences,
            now: feed.now,
            ..SearchData::default()
        };
        (data, usage)
    }

    /// What the person typed while the search they asked for was not up yet goes into it once it
    /// is; nothing of it is left over for a search asked for later.
    fn take_up_typed_ahead(&self) {
        let shared = &self.handle.shared;
        let search = Rc::clone(&shared.borrow().search);
        let mut search = search.borrow_mut();
        if search.is_open() {
            let ahead = std::mem::take(&mut shared.borrow_mut().typed_ahead);
            if !ahead.text.is_empty() {
                // The pass publishes the search's rows whatever changed, so whether the text
                // changed the query asks nothing more of it.
                let _changed = search.palette_mut().commit_query_text(&ahead.text);
            }
            if ahead.enter {
                search.choose_top_when_answered();
            }
        } else {
            shared.borrow_mut().typed_ahead = TypedAhead::default();
        }
    }

    /// Write text committed into the desktop's own field, if the person is still typing into the
    /// field it was meant for.
    fn commit_typed(&self, ask: &TypedAsk) {
        let search = Rc::clone(&self.handle.shared.borrow().search);
        let mut search = search.borrow_mut();
        // The field may have taken an edit or moved its caret since the text was committed: then
        // the text is not the person's to review any more and goes nowhere.
        if search.is_open() && typed_field(&search) == Ok(ask.field) {
            let _changed = search.palette_mut().commit_query_text(&ask.text);
        }
    }

    /// Keep what the open search shows where the desktop publishes it.
    fn publish_search_rows(&self) {
        let search = Rc::clone(&self.handle.shared.borrow().search);
        let search = search.borrow();
        let open = search.is_open();
        let rows: Vec<SearchRow> = if open {
            search
                .results()
                .iter()
                .map(|hit| SearchRow {
                    title: hit.title.clone(),
                    // The reason the host could not carry the row out stands on it, rather than
                    // the row reading as if nothing happened.
                    subtitle: search
                        .refusal_for(&hit.target)
                        .map(str::to_owned)
                        .or_else(|| hit.subtitle.clone()),
                    kind: hit.kind.label().to_owned(),
                    facts: hit.facts.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let shown = ShownSearch {
            query: if open {
                search.query().to_owned()
            } else {
                String::new()
            },
            scope: if open {
                scope_name(&search.scope())
            } else {
                String::new()
            },
            rows,
            searching: open && !search.settled(),
            expecting: open && search.expecting(),
        };
        let typed = if open {
            typed_field(&search)
        } else {
            Err(TypedRefusal::NoField)
        };
        let mut shared = self.handle.shared.borrow_mut();
        let changed = shared.shown_search != shown || shared.typed_field != typed;
        shared.shown_search = shown;
        shared.typed_field = typed;
        if changed {
            shared.search_revision = shared.search_revision.wrapping_add(1);
        }
    }

    /// After the palette handled input, take its outcome.
    fn follow_search(&mut self, tiles: &mut dyn TileSource) {
        let search = Rc::clone(&self.handle.shared.borrow().search);
        // Backspace at the start of the field, or the token's own button, takes the scope away:
        // the same search, over every workspace.
        let widened = {
            let mut search = search.borrow_mut();
            (search.is_open() && search.take_scope_widened()).then(|| search.query().to_owned())
        };
        if let Some(query) = widened {
            self.open_search(tiles, Scope::Everywhere, Some(query));
        }
        let mut open = search.borrow_mut();
        if open.is_open() {
            let _closed = open.observe();
            if open.refresh() {
                self.handle.note_search_activity();
            }
        }
        let choice = open.take_choice();
        let usage = choice.is_some().then(|| open.usage().clone());
        drop(open);
        if let Some((usage, host)) = usage.zip(self.handle.shared.borrow().search_host.clone()) {
            host.chose(&usage);
        }
        if matches!(choice, Some(Target::Launch { .. })) {
            // The person opened something from search themselves.
            ui::control_hints::performed::note(
                ui::control_hints::performed::Performed::OpenedWindow,
            );
        }
        match (choice, self.new_tab_into.take()) {
            // A tab in the window the person asked for: the one the search was opened from (its
            // new-tab control), else the one in front.
            (Some(Target::Launch { app, place }), into) => {
                let group = match place {
                    LaunchPlace::TabHere => into.or_else(|| self.window_in_front()),
                    LaunchPlace::Focus | LaunchPlace::Beside => into,
                };
                match group {
                    Some((strip, group)) => {
                        self.open_tab_in_group(tiles, strip, group, &app);
                    }
                    None => self.perform(Target::Launch { app, place }),
                }
            }
            (Some(Target::Action(action)), _) => self.run_action(&action),
            (Some(target), _) => self.perform(target),
            (None, into) => self.new_tab_into = into,
        }
        let open = search.borrow().is_open();
        if open {
            ui::control_hints::performed::note(
                ui::control_hints::performed::Performed::OpenedSearch,
            );
        } else {
            // The aim is for one choice; search opened otherwise opens applications as their own
            // windows again.
            self.new_tab_into = None;
        }
        let mut shared = self.handle.shared.borrow_mut();
        if shared.search_open != open {
            shared.search_open = open;
            shared.search_revision = shared.search_revision.wrapping_add(1);
        }
    }

    /// Put `app` into window `group` of `strip` as its tab in front: what a window's new-tab
    /// control opens. Whether it opened.
    fn open_tab_in_group(
        &self,
        tiles: &mut dyn TileSource,
        strip: StripId,
        group: GroupId,
        app: &str,
    ) -> bool {
        let mut shared = self.handle.shared.borrow_mut();
        let id = shared.minter.mint();
        let Some(workspace) = shared.tree.active_workspace() else {
            return false;
        };
        let Some(tab) = tiles.launch(workspace, id, app, None) else {
            return false;
        };
        let Some(model) = find_group(&mut shared.tree, strip, group) else {
            return false;
        };
        model.tabs.push(tab.clone());
        model.active_tab = tab.id;
        shared.tree.set_active_group(strip, group);
        shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
        shared.changed();
        true
    }

    /// Run a chosen action row by its address, as the app's own control would: the host's action
    /// API runs it in the app publishing it, which runs in a process of its own.
    fn run_action(&mut self, action: &tilo_desktop_search::ActionTarget) {
        let mut arguments = serde_json::Map::new();
        if let Some((param, value)) = action.preset.clone() {
            arguments.insert(param, serde_json::Value::String(value));
        }
        self.unrouted_control = Some(ui::tilo_actions::ControlInvocation {
            address: action.address(),
            arguments,
        });
    }

    /// Queue what a chosen row does, as the matching verb, or hand it to the host where the host
    /// carries it out.
    fn perform(&self, target: Target) {
        let host = self.handle.shared.borrow().search_host.clone();
        if host.is_some_and(|host| host.performs(&target)) {
            // Where search stood when the row was chosen, so an answer to what the host could not
            // carry out stands on that row again.
            let (scope, query) = {
                let search = Rc::clone(&self.handle.shared.borrow().search);
                let search = search.borrow();
                (search.scope(), search.query().to_owned())
            };
            let mut shared = self.handle.shared.borrow_mut();
            shared.search_choice_context = Some((scope, query, target.clone()));
            shared
                .search_choices
                .push((target, tilo_actions::remote::press_points::handled()));
            return;
        }
        let mut shared = self.handle.shared.borrow_mut();
        let admitted = match target {
            // An app already open is focused, not opened twice: a new window is asked for through
            // a window's new tab instead.
            Target::Launch { app, .. } => Some(
                navigation::window_of_app(&shared.tree, &app)
                    .map_or(Admitted::Launch(app, false), Admitted::Select),
            ),
            Target::Window { tab, .. } => route_of(&shared.tree, tab).map(Admitted::Select),
            Target::Command { id } => {
                let active = navigation::active_route(&shared.tree);
                if let Some(choice) = host_choice(&id) {
                    let choice = choice.to_owned();
                    shared.host_choices.push(choice);
                    None
                } else if id == tilo_desktop_search::ALL_APPS_COMMAND {
                    // Every app by its name: the applications' own search.
                    shared.search_request = Some(SearchRequest::Open {
                        scope: Scope::WindowGroup(None),
                        query: None,
                    });
                    None
                } else if id == CLOSE_WINDOW {
                    match active {
                        Some(ids) if keeps(&shared.holders, ids.tab) => {
                            let refusal = holder_refusal(&shared.holders, ids.tab);
                            shared.say(ui::window_toolbar::ToolbarNotice::new(
                                refusal,
                                ui::widget::toast::ToastKind::Info,
                            ));
                            None
                        }
                        active => active.map(Admitted::Close),
                    }
                } else if id == MAXIMIZE_WINDOW {
                    active.and_then(|ids| {
                        let group = navigation::locate(&shared.tree, ids)?;
                        Some(Admitted::Maximize(ids, !group.maximized))
                    })
                } else {
                    id.strip_prefix(tilo_desktop_search::WORKSPACE_COMMAND_PREFIX)
                        .and_then(tilo_desktop_model::WorkspaceId::parse)
                        .filter(|id| shared.tree.workspaces.iter().any(|w| w.id() == *id))
                        .map(Admitted::Workspace)
                }
            }
            Target::Copy { text } => {
                ui::text_selection::clipboard_copy(&text);
                shared.say(ui::window_toolbar::ToolbarNotice::new(
                    crate::desktop::i18n::t("desktop-search-copied"),
                    ui::widget::toast::ToastKind::Success,
                ));
                None
            }
            // Where the host opens addresses. In a web browser that is a new tab, which the
            // browser allows only within the choosing click or key press, so it opens now and a
            // blocked one is said; a native handler may take its time, so it opens off this
            // thread.
            Target::Open { uri, fallback } => {
                #[cfg(target_arch = "wasm32")]
                if let Err(error) =
                    ui::widget::try_open_external_url(fallback.as_deref().unwrap_or(&uri))
                {
                    shared.say(ui::window_toolbar::ToolbarNotice::new(
                        error.to_string(),
                        ui::widget::toast::ToastKind::Error,
                    ));
                }
                #[cfg(not(target_arch = "wasm32"))]
                match fallback {
                    Some(fallback) => {
                        let host = ui::process::current();
                        std::thread::spawn(move || {
                            ui::process::scoped(&host, || {
                                if ui::widget::try_open_external_url(&uri).is_err() {
                                    ui::widget::open_external_url(&fallback);
                                }
                            });
                        });
                    }
                    None => ui::widget::open_external_url(&uri),
                }
                None
            }
            // An app's action row runs as its control ([`Self::run_action`]), and one published
            // through the host is handed to it. A file a workspace holds opens as the app its
            // bytes name launches, which only the host holding that content knows.
            Target::Action(_)
            | Target::Invoke(_)
            | Target::Content { .. }
            | Target::Cancel { .. }
            | Target::Prefer { .. } => None,
        };
        if let Some(admitted) = admitted {
            shared.queue.push(admitted);
        }
    }
}

/// Make room for the window `group` that just opened in `strip`, beside the window before it on
/// its display, else the one after it: two that don't fit share the display, and when no share
/// leaves both usable the strip pans to the new window instead
/// ([`tilo_desktop_model::width::share_beside`]). So the window the person was in stays on screen
/// beside the new one rather than panning half out of view. A window the workspace holds floating
/// takes no run of the strip and shares nothing.
fn share_with_neighbour(shared: &mut super::Shared, strip: StripId, group: GroupId) {
    if super::floats::holds(shared, strip, group) {
        return;
    }
    let partitioned = shared.tree.is_partitioned();
    let partition = &shared.tree.outputs;
    // A window opened since the displays were last told stands on the display the person works
    // on, where the pass homes it.
    let display_of = |id: GroupId| {
        partitioned.then(|| {
            if partition.home_of(id).is_some() {
                partition.display_of(id)
            } else {
                partition.focused_display()
            }
        })
    };
    let display = display_of(group);
    let Some(held) = super::floats::workspace_with(&shared.tree, strip)
        .and_then(|workspace| workspace.strips.iter().find(|held| held.id == strip))
    else {
        return;
    };
    let Some(index) = held.window_groups.iter().position(|held| held.id == group) else {
        return;
    };
    let beside = |at: usize| {
        held.window_groups.get(at).is_some_and(|next_to| {
            !next_to.maximized
                && !super::floats::holds(shared, strip, next_to.id)
                && display_of(next_to.id) == display
        })
    };
    let neighbour = match index.checked_sub(1) {
        Some(left) if beside(left) => left,
        _ if beside(index.saturating_add(1)) => index.saturating_add(1),
        _ => return,
    };
    let (Some(opened), Some(next_to)) = (
        held.window_groups.get(index),
        held.window_groups.get(neighbour),
    ) else {
        return;
    };
    if opened.maximized {
        return;
    }
    let area = super::shown::work_area(shared, display.flatten());
    let usable = (area.width() - 2.0 * ui::widget::STRIP_EDGE_GUTTER_PX).max(1.0);
    let Some((neighbour_width, opened_width)) = tilo_desktop_model::width::share_beside(
        next_to.width,
        opened.width,
        usable,
        area.width(),
        ui::widget::HANDLE_SLOT_PX,
    ) else {
        return;
    };
    let Some(held) = shared
        .tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
        .find(|held| held.id == strip)
    else {
        return;
    };
    let mut changed = false;
    for (at, width) in [(neighbour, neighbour_width), (index, opened_width)] {
        if let Some(sized) = held.window_groups.get_mut(at)
            && sized.width != width
        {
            sized.width = width;
            changed = true;
        }
    }
    if changed {
        shared.changed();
    }
}
