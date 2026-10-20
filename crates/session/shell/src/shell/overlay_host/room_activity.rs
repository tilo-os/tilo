//! The room activity sheet: refresh, awareness, inferred status and its events.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// The room-activity sheet, ahead of the general absorb gate — it
    /// wants exactly the events that gate would otherwise swallow
    /// (hover, wheel); the gate still catches whatever the sheet
    /// doesn't claim.
    pub(super) fn route_room_activity_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        handled: &mut bool,
    ) -> bool {
        let pointer_event = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::MouseInput { .. }
        );
        let keyboard_event = matches!(event, WindowEvent::KeyboardInput { .. });
        let focused = self.chrome.as_ref().is_some_and(|chrome| {
            crate::shell::tab_binding::focused_workspace_activity_tab(&chrome.borrow().tree())
                .is_some()
        });
        if !((pointer_event && self.activity_window_rect().is_some())
            || (keyboard_event && focused))
        {
            return false;
        }
        let sheet = self.room_activity.rect();
        let pointer_inside = self.last_cursor.is_some_and(|p| sheet.contains(p));
        let is_key = matches!(event, WindowEvent::KeyboardInput { .. });
        // Escape belongs to the dismissal ladder below, except while a
        // name field or typeahead is active — there it abandons the field
        // instead. The tiled window itself is closed through window chrome.
        let naming = self.room_activity.wants_keyboard();
        let filtering = !self.room_activity.timeline().author_query().is_empty();
        let is_escape = !naming
            && !filtering
            && matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        ..
                    },
                    ..
                }
            );
        if (pointer_inside || (is_key && !is_escape))
            && let Some(chrome) = self.chrome.clone()
        {
            let cursor = self.last_cursor;
            let outcome = {
                let st = chrome.borrow();
                let records = st.records().clone();
                let mut tree = st.tree_mut();
                self.room_activity.handle_event_with_records(
                    event,
                    scale_factor,
                    &modifiers,
                    cursor,
                    &mut *tree,
                    Some(&records),
                )
            };
            let consumed = outcome.handled || (pointer_inside && !is_key);
            self.apply_room_activity_outcome(&chrome, outcome);
            // Pointer events over the sheet absorb regardless; keys
            // only when consumed, or an arrow key with the cursor
            // resting over the sheet would vanish.
            if consumed {
                *handled = true;
                return true;
            }
        }
        false
    }

    pub(super) fn apply_room_activity_outcome(
        &mut self,
        chrome: &Chrome,
        mut outcome: crate::shell::room_activity::RoomActivityOutcome,
    ) {
        if let Some(draft) = outcome.create_native_manual.take() {
            let tree = chrome.borrow().tree_handle();
            match crate::shell::state::native_manual_tasks::create(&tree, draft) {
                Ok(id) => {
                    self.room_activity
                        .complete_native_manual_task(id, &mut *tree.write());
                    outcome.task_changed = true;
                }
                Err(error) => {
                    tracing::warn!(%error, "native manual task refused");
                    outcome.task_refused = true;
                }
            }
        }
        if let Some(draft) = outcome.create_native_managed.take() {
            let tree = chrome.borrow().tree_handle();
            match crate::shell::state::native_manual_tasks::create_managed(&tree, draft) {
                Ok(id) => {
                    self.room_activity
                        .complete_native_manual_task(id, &mut *tree.write());
                    outcome.task_changed = true;
                }
                Err(error) => {
                    tracing::warn!(%error, "native managed task refused");
                    outcome.task_refused = true;
                }
            }
        }
        // Every caller has released the tree by now; the broker reads it.
        if let Some(request) = outcome.dispatch_task.take() {
            if self.room_activity.dispatch_task(request) {
                let tree = chrome.borrow().tree_handle();
                self.room_activity.refresh(&mut *tree.write());
                outcome.task_changed = true;
            } else {
                outcome.agent_runtime_changed = true;
            }
        }
        if outcome.task_refused {
            let mut state = chrome.borrow_mut();
            state.push_notification(
                t("host-history-task-refused-title"),
                t("host-history-task-refused-body"),
            );
            state.bump();
        }
        // A full revert, comparison, name or task change is the person's own
        // action and shows in History itself; only a partial undo has
        // something to say that the pane doesn't.
        if outcome.reverted {
            let mut state = chrome.borrow_mut();
            state.bump();
            if outcome.undo_partial {
                state.push_notification(
                    t("host-history-partial-undo-title"),
                    t("host-history-partial-undo-body"),
                );
            }
        }
        if let Some(target) = outcome.open_target {
            Self::open_room_activity_target(chrome, &target);
        }
        if outcome.compared || outcome.named || outcome.task_changed {
            chrome.borrow_mut().bump();
        }
        if outcome.agent_runtime_changed {
            // Starting or retrying a process is local, not a durable task
            // transition. Redraw its factual lifecycle notice without
            // claiming shared work has begun.
            chrome.borrow_mut().bump();
        }
    }

    pub(super) fn open_room_activity_target(chrome: &Chrome, target: &ui::HistoryResourceTarget) {
        let Some(app_id) = target.app_id.as_deref() else {
            return;
        };
        // Opening from history is the person opening the app: the launch
        // must see an author grant or the document comes up view-only.
        let Some(workspace) = ({
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            tree.workspace_after_opening_app(app_id)
        }) else {
            return;
        };
        // Queued: this runs on the compositor thread, and a launch's file
        // work belongs on the preflight worker.
        let (document, uri) = match history_target_address(target) {
            HistoryTargetAddress::File(path) => (Some(path), None),
            HistoryTargetAddress::Uri(uri) => (None, Some(uri)),
            HistoryTargetAddress::App => (None, None),
        };
        // A refusal arrives later as a launch event, which settles its
        // placement and posts its notice under this target's label.
        let result = crate::shell::launcher::queue_package_open(
            app_id.to_owned(),
            Some(target.label.clone()),
            &workspace,
            document,
            uri,
        );
        if let Err(error) = result {
            // A previous document/app launch can have staged a one-shot
            // placement for this package. This history action did not claim
            // a toplevel, so a preflight refusal must retire any note before
            // a later unrelated window gets adopted into the old target.
            crate::shell::launcher::clear_pending_placement(app_id);
            // LaunchError's full Display includes package ids, paths, and
            // backend diagnostics. Keep those in the structured log; the
            // fixed-height top-right card gets the same short recovery copy
            // used by palette launches.
            tracing::error!(
                target: "tilo::app_launch",
                app_id,
                label = %target.label,
                detail = %error,
                "workspace activity application launch was refused"
            );
            chrome.borrow_mut().push_notification_full(
                tilo_desktop::notifications::Notification::new(
                    crate::shell::launcher::launch_failure_title(&target.label),
                    error.notification_message(),
                )
                .with_severity(tilo_desktop::notifications::NotificationSeverity::Error)
                .with_app_name(t("host-source-applications"))
                .with_dismiss_after(crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
            );
        }
    }
}

impl ShellOverlayHost {
    /// Lays the room-activity sheet out (by hand, not taffy, like the
    /// other floating overlays) and re-derives its rows from the document.
    pub(super) fn apply_pending_history_action(&mut self) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let Some(id) = chrome.borrow_mut().take_history_action_request() else {
            return;
        };
        let st = chrome.borrow();
        let records = st.records().clone();
        let mut tree = st.tree_mut();
        self.room_activity.refresh(&mut *tree);
        let outcome =
            self.room_activity
                .apply_selection_history_action(&id, &mut *tree, Some(&records));
        drop(tree);
        drop(st);
        self.apply_room_activity_outcome(&chrome, outcome);
    }

    pub(super) fn refresh_room_activity(&mut self) {
        if let Some(viewport) = self.activity_window_rect() {
            self.room_activity.layout_window(viewport);
        }
        if let Some(chrome) = self.chrome.clone() {
            let (tree, records) = {
                let st = chrome.borrow();
                (st.tree_handle(), st.records().clone())
            };
            if self.room_activity.take_persisted_reads() {
                // The glance derives from the same reads.
                self.activity_refresh.invalidate();
            }
            let local = self.room_records.borrow();
            self.room_activity
                .refresh_observed_local(&tree, &records, &local, Instant::now());
        }
        self.refresh_history_narrative();
    }

    /// Content viewport of the active personal Activity tab. Its surrounding
    /// group is an ordinary tiled window and therefore owns close, resize,
    /// reorder, focus and clipping.
    pub(super) fn activity_window_rect(&self) -> Option<Rect> {
        let chrome = self.chrome.as_ref()?;
        let (strip, group, tab) = {
            let state = chrome.borrow();
            crate::shell::tab_binding::active_workspace_activity_tab(&state.tree())?
        };
        let row_ptr = *self.group_row_ptrs.get(&strip)?;
        // SAFETY: `group_row_ptrs` points into the retained content tree and
        // is read sequentially on the compositor thread; see the field's
        // ownership contract and the other accessors in this impl.
        let row: &GroupRow = unsafe { &*row_ptr };
        row.groups
            .iter()
            .find(|candidate| candidate.id == group)
            .and_then(|candidate| candidate.content_rect_for_tab(tab))
    }

    pub(super) fn shortcut_help_button_rect(&self) -> Rect {
        if self.shortcut_help_drawn_rect.area() > 0.0 {
            return self.shortcut_help_drawn_rect;
        }
        let area = self.overlay_area();
        Rect::new(
            area.x1 - 156.0,
            area.y1 - 52.0,
            area.x1 - 16.0,
            area.y1 - 16.0,
        )
    }

    pub(super) fn refresh_history_narrative(&mut self) {
        let Some(narrator) = self.history_narrator.as_mut() else {
            return;
        };
        if let Some(request) = self.room_activity.narrative_request() {
            narrator.request(&request);
            if let Some(narrative) = narrator.take_for(&request)
                && self.room_activity.apply_generated_narrative(narrative)
            {
                // The glance may quote it; publish on the next frame.
                self.activity_refresh.invalidate();
            }
        }
    }

    /// The passive awareness's [`ActivityRefreshKey`] now, read under the
    /// tree's read lock.
    pub(super) fn activity_refresh_key(&self) -> Option<ActivityRefreshKey> {
        use automerge::transaction::Transactable as _;
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        let (external_revision, active, tree_heads, tree_pending) = {
            let tree = state.tree();
            (
                tree.desktop.external_revision,
                tree.desktop.active.raw(),
                tree.doc.base_heads(),
                tree.doc.pending_ops(),
            )
        };
        Some(ActivityRefreshKey {
            chrome_revision: state.revision,
            external_revision,
            active,
            tree_heads,
            tree_pending,
            records: state.records().content_revision(),
            room_records: self.room_records.borrow().revision(),
        })
    }

    /// Record the shortcut runs the broker finished, each as one undoable
    /// run in the workspace document. On the frame pump, never in paint.
    /// Documents on disk are read off the loop, and the tree's write lock
    /// is taken only to write a run that was found.
    pub(super) fn record_finished_shortcut_runs(&mut self) {
        if !self.room_activity.shortcut_runs_pending() {
            return;
        }
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let state = chrome.borrow();
        let records = state.records().clone();
        let local = self.room_records.borrow();
        let recorded = self.room_activity.record_finished_shortcut_runs(
            || state.tree_mut(),
            &records,
            &local,
            Instant::now(),
        );
        if recorded {
            state.tree().queue_save();
        }
    }

    /// Keep passive chrome truthful while the sheet is closed. Runs only on
    /// frames where something it reads may have moved (see
    /// [`ActivityRefreshKey`]), when `force`d, or once a second; publishing
    /// is a no-op unless headline, unread count, or latest content id changed.
    pub(super) fn refresh_room_activity_awareness(&mut self, force: bool) {
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        let force = force || self.activity_pending_reveal.is_some();
        let due = self
            .activity_refresh_key()
            .is_some_and(|key| self.activity_refresh.due(key, force, Instant::now()));
        if !due {
            self.refresh_history_narrative();
            self.refresh_automatic_status();
            return;
        }
        let workspace = {
            let (tree, records) = {
                let st = chrome.borrow();
                (st.tree_handle(), st.records().clone())
            };
            let local = self.room_records.borrow();
            self.room_activity
                .refresh_observed_local(&tree, &records, &local, Instant::now())
        };
        // A run newly paused at an action asking first: a banner points at
        // it, and the person answers in Task Center when they choose.
        for notice in self.room_activity.take_confirmation_notices() {
            chrome.borrow_mut().push_task_confirmation(&notice);
        }
        self.refresh_history_narrative();
        let (headline, unread, pulse) = self.room_activity.glance_signal(workspace);
        let away_changes = self.room_activity.away_change_count(workspace);
        let entering_workspace =
            enter_activity_workspace(&mut self.activity_awareness_workspace, workspace);
        let passive_digest = self.room_activity.digest_for_workspace(workspace);
        let focus =
            passive_digest
                .briefs
                .first()
                .map(|brief| crate::shell::chrome_state::ActivityFocus {
                    task_id: None,
                    author: brief.authors.first().map(|author| author.name.clone()),
                    group_id: brief.group_id.clone(),
                    entry_id: None,
                    resource_id: None,
                    resource_version: None,
                });
        {
            let mut state = chrome.borrow_mut();
            state.publish_activity_glance(headline, unread, pulse, focus);
            let mut attention = state.overlays;
            if self.palette_visible {
                attention.overview = true;
            }
            if take_pending_activity_reveal(
                &mut self.activity_pending_reveal,
                entering_workspace,
                workspace,
                away_changes,
                attention,
            ) {
                self.room_activity.prepare_away_view(workspace);
                state.open_room_activity(None);
            }
        }
        self.refresh_automatic_status();
        // What this refresh wrote itself (a commit, a published glance) is
        // already reflected; only a later change needs another pass.
        if let Some(key) = self.activity_refresh_key() {
            self.activity_refresh.settle(key);
        }
    }

    pub(super) fn refresh_automatic_status(&mut self) {
        if self.status_sharing != ui::StatusSharing::Automatic
            || (self.local_status.is_some()
                && matches!(self.local_status_origin, ui::StatusOrigin::Manual))
        {
            return;
        }
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let local_ids = {
            let state = chrome.borrow();
            let active_room = state
                .tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room);
            let mut ids = vec![state.local_user_id.clone()];
            if let Some(room) = active_room {
                ids.push(state.local_user_for_room(&room));
            }
            ids.retain(|id| !id.is_empty());
            ids
        };
        let now = std::time::SystemTime::now();
        let cutoff = now
            .checked_sub(std::time::Duration::from_secs(5 * 60))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let recent = self
            .room_activity
            .entries()
            .iter()
            .filter(|entry| entry.timestamp >= cutoff)
            .filter(|entry| {
                local_ids.iter().any(|id| {
                    entry.author.name.eq_ignore_ascii_case(id)
                        || entry.author.name.eq_ignore_ascii_case(
                            &ui::history_source::participant_display_label(id),
                        )
                        || entry.author.identity_short == ui::history_source::short_id(id)
                })
            })
            .rev()
            .take(12)
            .cloned()
            .collect::<Vec<_>>();
        if recent.is_empty() {
            if matches!(self.local_status_origin, ui::StatusOrigin::Inferred { .. }) {
                self.local_status = None;
                self.local_status_origin = ui::StatusOrigin::Manual;
                self.status_narrative_request = None;
                self.publish_local_status(None);
            }
            return;
        }
        // Selection above walks newest-first so the bounded window retains
        // the latest action. Grouping, however, consumes causal order. Keep
        // the newest item for the immediate grounded fallback, then restore
        // oldest-first order before building the local-model evidence.
        let (deterministic_status, recent) = prepare_status_evidence(recent);
        let digest = ui::ActivityDigest::from_entries(&recent, cutoff, 3);
        let Some(mut request) = ui::NarrativeRequest::from_digest(&digest) else {
            return;
        };
        request.deterministic_text = deterministic_status;
        if self.status_narrative_request.as_ref() != Some(&request) {
            self.status_narrative_request = Some(request.clone());
            self.install_inferred_status(&request.deterministic_text, "tilo-deterministic".into());
            self.status_narrator.request_status(&request);
        }
        if let Some(narrative) = self.status_narrator.take_status_for(&request)
            && let Some(narrative) = ui::validate_narrative(&request, narrative)
        {
            self.install_inferred_status(&narrative.text, narrative.generator);
        }
    }

    pub(super) fn install_inferred_status(&mut self, text: &str, generator: String) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() || text.chars().count() > ui::MAX_STATUS_CHARS {
            return;
        }
        let unchanged = self.local_status.as_deref() == Some(text.as_str())
            && matches!(self.local_status_origin, ui::StatusOrigin::Inferred { .. });
        if unchanged {
            return;
        }
        self.local_status = Some(text.clone());
        self.local_status_origin = ui::StatusOrigin::Inferred { generator };
        self.publish_local_status(Some(text));
    }

    /// The room-activity sheet, for tests and for the shots driver.
    pub fn room_activity(&self) -> &crate::shell::room_activity::RoomActivityPane {
        &self.room_activity
    }

    #[cfg(feature = "hosted-test-control")]
    pub fn seed_inferred_status_for_shots(&mut self, text: &str) {
        self.status_sharing = ui::StatusSharing::Automatic;
        self.local_status = None;
        self.local_status_origin = ui::StatusOrigin::Manual;
        self.install_inferred_status(text, "shots-local-model".to_string());
    }

    /// Mutable handle to the room-activity sheet, so a test can pin its clock
    /// or drive a filter without synthesising a click.
    pub fn room_activity_mut(&mut self) -> &mut crate::shell::room_activity::RoomActivityPane {
        &mut self.room_activity
    }

    /// Treat the tiled recap as already open and lay it out now, so a
    /// screenshot or a click-driven test sees the settled window on the very
    /// next paint. It draws exactly as a live session does: the recap has no
    /// entry animation for a capture to skip.
    pub fn force_room_activity_settled(&mut self) {
        self.room_activity_was_open = true;
        self.refresh_room_activity();
    }

    /// Forget that the recap was open after a shot driver action closes it,
    /// so a later open is a fresh rising edge.
    pub fn force_room_activity_closed_for_shots(&mut self) {
        self.room_activity_was_open = false;
    }

    /// Name-input rect inside the ws-settings modal. Matches the drawn
    /// rect in `draw_ws_settings` so hit-test and draw stay in sync.
    pub(super) fn ws_settings_name_rect(&self) -> Rect {
        WorkspaceSettingsOverlay::name_rect_for(self.ws_settings_rect())
    }

    /// A contextual lens docked beside the live workspace. Narrow displays
    /// promote the same surface to a nearly full-width sheet.
    pub(super) fn ws_settings_rect(&self) -> Rect {
        // The rect `layout` placed it at: the focused display.
        WorkspaceSettingsOverlay::rect_for(self.overlay_area())
    }
}

impl ShellOverlayHost {
    /// Show the paused action a notification's activation asked for: in the
    /// composer that follows its run when there is one, so the person
    /// answers it beside their work; on its card in Task Center otherwise.
    pub(super) fn reveal_requested_draft(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let Some(task_id) = chrome.borrow_mut().take_task_draft_request() else {
            return false;
        };
        if self.agent_quick_composer.reveal_run(&task_id) {
            return true;
        }
        chrome
            .borrow_mut()
            .open_room_activity(Some(crate::shell::chrome_state::ActivityFocus {
                task_id: Some(task_id),
                ..crate::shell::chrome_state::ActivityFocus::default()
            }));
        false
    }

    /// Tell the open composer what its followed run is doing now, from the
    /// room's task review: the step it is at, and the action it waits for
    /// the person on, which the composer shows and answers as the card
    /// does. Returns whether the composer changed.
    pub(super) fn pump_composer_run(&mut self) -> bool {
        use crate::shell::agent_quick_composer::DraftAnswer;
        let Some(owner) = self.agent_quick_composer.owner().cloned() else {
            return false;
        };
        let glance = self
            .agent_quick_composer
            .followed()
            .map(|run| run.task.task_id.clone())
            .and_then(|task| self.room_activity.run_glance(&owner.room, &task));
        let before = self.agent_quick_composer.shows_paused_any();
        self.agent_quick_composer.glance(&owner, glance);
        let mut changed = before != self.agent_quick_composer.shows_paused_any();
        let answered = match self.agent_quick_composer.take_draft_answer() {
            Some(DraftAnswer::Confirm(draft)) => Some((draft, true)),
            Some(DraftAnswer::Decline(draft)) => Some((draft, false)),
            Some(DraftAnswer::Edit(draft)) => {
                if let Some(chrome) = &self.chrome {
                    chrome.borrow_mut().open_room_activity(Some(
                        crate::shell::chrome_state::ActivityFocus {
                            task_id: Some(draft.task_id.clone()),
                            ..crate::shell::chrome_state::ActivityFocus::default()
                        },
                    ));
                }
                self.room_activity.edit_paused_draft(&draft.task_id);
                changed = true;
                None
            }
            None => None,
        };
        // Answered only while the review still shows that exact draft, so
        // a draft that changed meanwhile is never answered by mistake.
        if let Some((draft, confirm)) = answered
            && self
                .room_activity
                .run_glance(&draft.room, &draft.task_id)
                .is_some_and(|glance| glance.paused.as_ref() == Some(&draft))
        {
            if let Err(error) = self.room_activity.answer_paused_draft(&draft, confirm) {
                tracing::warn!(%error, "the composer could not answer the paused action");
            }
            changed = true;
        }
        changed
    }
}

pub(super) fn should_reveal_activity_on_entry(
    entering_workspace: bool,
    away_changes: usize,
    overlays: crate::shell::chrome_state::OverlayFlags,
) -> bool {
    entering_workspace
        && away_changes > 0
        && overlays == crate::shell::chrome_state::OverlayFlags::default()
}

pub(super) fn enter_activity_workspace(last_workspace: &mut Option<u64>, workspace: u64) -> bool {
    // Restoring the initial workspace is not a user-initiated workspace
    // transition. Its unread history belongs in the passive glance badge;
    // otherwise startup can raise a modal before a newly mapped application
    // receives its first keyboard focus.
    let entering = last_workspace.is_some_and(|previous| previous != workspace);
    *last_workspace = Some(workspace);
    entering
}

/// The longest the passive Activity awareness goes without a refresh while
/// nothing it can observe cheaply has changed: a record rewritten on disk by
/// an app, or the clock crossing the away recap's threshold, is caught here.
pub(super) const ACTIVITY_AWARENESS_FALLBACK: Duration = Duration::from_secs(1);

/// Everything the passive Activity awareness reads that a frame can compare
/// without the tree's write lock: chrome and external tree revisions, the
/// active workspace, the desktop document's heads, the open records'
/// content and the rooms' record corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ActivityRefreshKey {
    pub(super) chrome_revision: u64,
    pub(super) external_revision: u64,
    pub(super) active: u64,
    pub(super) tree_heads: Vec<automerge::ChangeHash>,
    pub(super) tree_pending: usize,
    pub(super) records: String,
    pub(super) room_records: (u64, u64),
}

/// Decides when the passive Activity awareness refreshes: when its key
/// moved, when asked to, or once [`ACTIVITY_AWARENESS_FALLBACK`] passed.
#[derive(Debug, Default)]
pub(super) struct ActivityRefreshGate {
    pub(super) key: Option<ActivityRefreshKey>,
    pub(super) at: Option<Instant>,
}

impl ActivityRefreshGate {
    /// Whether a frame with `key` at `now` must refresh; records the refresh
    /// when it must.
    pub(super) fn due(&mut self, key: ActivityRefreshKey, force: bool, now: Instant) -> bool {
        let stale = self
            .at
            .is_none_or(|at| now.saturating_duration_since(at) >= ACTIVITY_AWARENESS_FALLBACK);
        if !force && !stale && self.key.as_ref() == Some(&key) {
            return false;
        }
        self.key = Some(key);
        self.at = Some(now);
        true
    }

    /// Refresh on the next frame whatever the key says.
    pub(super) fn invalidate(&mut self) {
        self.key = None;
    }

    /// The state a refresh left behind is the state it saw.
    pub(super) fn settle(&mut self, key: ActivityRefreshKey) {
        if self.key.is_some() {
            self.key = Some(key);
        }
    }
}

pub(super) fn take_pending_activity_reveal(
    pending: &mut Option<u64>,
    entering: bool,
    workspace: u64,
    away_changes: usize,
    overlays: crate::shell::chrome_state::OverlayFlags,
) -> bool {
    if entering {
        *pending = (away_changes > 0).then_some(workspace);
    } else if away_changes == 0 && *pending == Some(workspace) {
        *pending = None;
    }
    if overlays.room_activity {
        *pending = None;
        return false;
    }
    if *pending == Some(workspace) && should_reveal_activity_on_entry(true, away_changes, overlays)
    {
        *pending = None;
        return true;
    }
    false
}

pub(super) fn grounded_intent_fallback(entries: &[ui::widget::history::HistoryEntry]) -> String {
    let Some(entry) = entries.first() else {
        return String::new();
    };
    if let Some(context) = entry.context.as_ref() {
        if let Some(purpose) = context
            .purpose
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return purpose.to_string();
        }
        if let Some(group) = context
            .group
            .as_ref()
            .map(|group| group.label.trim())
            .filter(|text| !text.is_empty())
        {
            return t_args("host-activity-working-on", &[("target", group.into())]);
        }
        if let Some(resource) = context
            .resource
            .as_ref()
            .map(|resource| resource.label.trim())
            .filter(|text| !text.is_empty())
        {
            return t_args("host-activity-working-on", &[("target", resource.into())]);
        }
        if let Some(app) = context
            .app
            .as_ref()
            .map(|app| app.label.trim())
            .filter(|text| !text.is_empty())
        {
            return t_args("host-activity-working-in", &[("app", app.into())]);
        }
    }
    match entry.entry_kind {
        ui::widget::EntryKind::Text => t("host-activity-editing"),
        ui::widget::EntryKind::Move => t("host-activity-organizing"),
        ui::widget::EntryKind::Details => t("host-activity-updating-details"),
        ui::widget::EntryKind::Comment => t("host-activity-reviewing-discussion"),
        ui::widget::EntryKind::Delete => t("host-activity-cleaning-up"),
        ui::widget::EntryKind::Restore => t("host-activity-restoring"),
        ui::widget::EntryKind::Access => t("host-activity-managing-access"),
        ui::widget::EntryKind::Compaction => t("host-activity-maintaining-history"),
        ui::widget::EntryKind::Redaction => t("host-activity-reviewing-sensitive"),
    }
}

pub(super) fn prepare_status_evidence(
    mut newest_first: Vec<ui::widget::history::HistoryEntry>,
) -> (String, Vec<ui::widget::history::HistoryEntry>) {
    let fallback = grounded_intent_fallback(&newest_first);
    newest_first.reverse();
    (fallback, newest_first)
}
