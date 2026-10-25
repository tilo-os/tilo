//! Launches, session actions and cross-strip detaches.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Preview and commit cross-strip tab drops with one retained
    /// [`TabDropTarget`]. A release outside every sibling strip restores the
    /// source exactly; it never invents a same-strip fallback group.
    ///
    /// SAFETY: each pointer addresses a `GroupRow` owned by the content
    /// tree, the host outlives the content, and each row is only ever
    /// touched by one reconcile sweep at a time — the frame loop runs
    /// them sequentially, never re-entrantly.
    pub(super) fn dispatch_cross_strip_detaches(&self) {
        if self.group_row_ptrs.is_empty() {
            return;
        }
        // Drain sources first: a later `accept_cross_strip_drop` borrows
        // the destination row mutably, conflicting with a held source
        // payload reference.
        let mut detaches: Vec<(StripId, CrossStripDetach)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs` — pointer was
            // captured from a `Box<GroupRow>` owned through the content
            // tree, the host outlives the content, and reconcile sweeps
            // run sequentially so no other path is aliasing.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            if let Some(payload) = row.take_pending_cross_strip_detach() {
                detaches.push((strip_id, payload));
            }
        }
        if detaches.is_empty() {
            let source = self.group_row_ptrs.iter().find_map(|(&strip_id, &ptr)| {
                // SAFETY: read-only use under the stable row-pointer contract
                // documented above.
                unsafe { (&*ptr).tab_drag_active() }.then_some(strip_id)
            });
            let destination = source.and_then(|source| {
                let cursor = self.last_cursor?;
                self.strip_over(cursor, source)
                    .map(|strip_id| (strip_id, cursor))
            });
            for (&strip_id, &ptr) in &self.group_row_ptrs {
                // SAFETY: each distinct row is visited once, sequentially.
                let row: &mut GroupRow = unsafe { &mut *ptr };
                row.set_cross_strip_tab_drag_hover(
                    destination
                        .filter(|(destination, _)| *destination == strip_id)
                        .map(|(_, cursor)| cursor),
                );
            }
            return;
        }
        // The destination row already resolved and painted the complete
        // target while the pointer moved. Release consumes that enum verbatim.
        for (src_strip_id, payload) in detaches {
            let release = payload.release_point;
            let dest_strip_id = self.strip_over(release, src_strip_id);
            let Some(dest_strip_id) = dest_strip_id else {
                if let Some(&source_ptr) = self.group_row_ptrs.get(&src_strip_id) {
                    // SAFETY: source and destination routing is sequential.
                    unsafe { &mut *source_ptr }.restore_cross_strip_detach(payload);
                }
                continue;
            };
            // SAFETY: same contract as the drain above. We're mutating a
            // single row at a time and never aliasing across rows.
            let Some(&dest_ptr) = self.group_row_ptrs.get(&dest_strip_id) else {
                continue;
            };
            let target = {
                let dest_row: &GroupRow = unsafe { &*dest_ptr };
                dest_row
                    .cross_strip_drop_target()
                    // Synthetic/headless tests can stage a detach without a
                    // preceding motion frame; production uses the retained
                    // preview above.
                    .or_else(|| dest_row.tab_drop_target_at(release))
            };
            let Some(target) = target else {
                if let Some(&source_ptr) = self.group_row_ptrs.get(&src_strip_id) {
                    unsafe { &mut *source_ptr }.restore_cross_strip_detach(payload);
                }
                continue;
            };
            // Commit the cache + replicated document before moving the live
            // host. If the destination or document is stale, returning the
            // payload to its source avoids a visual-only layout that a later
            // resync would appear to resurrect.
            //
            // One id, minted in the tree's namespace, is shared by the
            // rendered group, the durable cache and the replicated document.
            // Generating an id independently inside each strip made a later
            // resync unable to identify the group the person had just moved.
            let committed = self.chrome.clone().and_then(|chrome| {
                let st = chrome.borrow_mut();
                let mut tree = st.tree_mut();
                let new_group_id = tree.mint_id();
                tree.move_tab_across_strips(
                    src_strip_id,
                    dest_strip_id,
                    payload.tab_id,
                    new_group_id,
                    target,
                )
                .then_some(new_group_id)
            });
            if let Some(new_group_id) = committed {
                let Some(&target_ptr) = self.group_row_ptrs.get(&dest_strip_id) else {
                    continue;
                };
                let target_row: &mut GroupRow = unsafe { &mut *target_ptr };
                let accepted = target_row.accept_cross_strip_drop(
                    new_group_id,
                    payload.desc,
                    payload.host,
                    target,
                );
                debug_assert!(accepted, "durable target must remain live through dispatch");
            } else if let Some(&source_ptr) = self.group_row_ptrs.get(&src_strip_id) {
                let source_row: &mut GroupRow = unsafe { &mut *source_ptr };
                source_row.restore_cross_strip_detach(payload);
            }
        }
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: dispatch is complete; retire the one-frame preview from
            // every row without touching content-drag state.
            unsafe { &mut *ptr }.set_cross_strip_tab_drag_hover(None);
        }
    }

    /// Turns app-launcher worker-thread failures into notifications (a clean
    /// launch/exit is silent — the window is the feedback). Returns whether
    /// anything was posted, so the caller can mark the frame dirty.
    #[expect(
        clippy::too_many_lines,
        reason = "launch failures, expiries and placement repair share one ordered drain"
    )]
    pub(crate) fn drain_launch_events(&self) -> bool {
        use crate::shell::launcher::LaunchEventKind;
        if self.desktop_controls.is_explicit() {
            return false;
        }
        // Expiry is independent of new launcher events: a daemonising parent
        // can exit and never produce another event or window.
        let timed_out = crate::shell::launcher::expire_pending_placements();
        let events = crate::shell::launcher::drain_events();
        if events.is_empty() && timed_out.is_empty() {
            return false;
        }
        let Some(chrome_handle) = self.chrome.clone() else {
            return false;
        };
        let mut posted = false;
        for package_id in timed_out {
            let settled = {
                let chrome = chrome_handle.borrow();
                let mut tree = chrome.tree_mut();
                crate::shell::peer_windows::mark_timed_out_restore(&mut tree, &package_id)
                    | crate::shell::peer_windows::settle_install(
                        &mut tree,
                        &package_id,
                        &t("host-app-did-not-open-body"),
                    )
            };
            if settled {
                chrome_handle.borrow_mut().bump();
            }
            crate::shell::catalogue::take_pending_launch(&package_id);
            {
                let mut st = chrome_handle.borrow_mut();
                st.push_notification_full(
                    tilo_desktop::notifications::Notification::new(
                        t("host-app-did-not-open-title"),
                        t("host-app-did-not-open-body"),
                    )
                    .with_severity(tilo_desktop::notifications::NotificationSeverity::Error)
                    .with_app_name(t("host-source-applications"))
                    .with_dismiss_after(crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
                );
                st.bump();
            }
            tracing::error!(
                target: "tilo::app_launch",
                %package_id,
                timeout_seconds = 15_u64,
                "application started but did not present a window before the launch deadline"
            );
            posted = true;
        }
        for event in events {
            // A window that went away without being closed was lost, however
            // the process then exited; a new start forgets an older loss.
            let lost = match &event.kind {
                LaunchEventKind::Exited | LaunchEventKind::Failed { .. } => {
                    crate::shell::launcher::take_launch_vanished(&event.package_id)
                }
                LaunchEventKind::Started { .. } => {
                    crate::shell::launcher::note_launch_restarted(&event.package_id);
                    false
                }
                LaunchEventKind::OutOfMemory(end) if end.closed_the_app() => {
                    crate::shell::launcher::take_launch_vanished(&event.package_id);
                    false
                }
                LaunchEventKind::SpawnFailed { .. }
                | LaunchEventKind::WindowLost { .. }
                | LaunchEventKind::OutOfMemory(_) => false,
            };
            // The windows that went away with the launch are worth keeping
            // only when memory ended it: then its notice reopens them where
            // they were. Any other end of the app forgets them.
            let lost_tabs = match &event.kind {
                LaunchEventKind::OutOfMemory(end) if !end.closed_the_app() => Vec::new(),
                LaunchEventKind::Started { .. } => Vec::new(),
                _ => crate::shell::launcher::take_lost_tabs(&event.package_id),
            };
            let event = event.with_window_lost(lost);
            // Memory ended the app or one of its jobs: say which and why,
            // whether or not its window had shown, and offer the app back
            // where it was.
            if let LaunchEventKind::OutOfMemory(end) = &event.kind {
                if end.closed_the_app() {
                    crate::shell::launcher::clear_pending_placement(&event.package_id);
                    crate::shell::catalogue::take_pending_launch(&event.package_id);
                    let _ = crate::shell::launcher::take_launch_presented(&event.package_id);
                }
                event.log_failure();
                if let Some(notice) = event.failure_notice() {
                    let mut st = chrome_handle.borrow_mut();
                    st.push_out_of_memory_notice(notice, lost_tabs);
                    st.bump();
                    posted = true;
                }
                continue;
            }
            // A failure settles the pending launch here. `Started`
            // doesn't — the window is still seconds away — that's
            // [`Self::settle_launches_of_opened_windows`]'s job. A clean
            // exit also settles it: a process can quit before presenting a
            // window, and the app must stay openable again.
            if matches!(
                event.kind,
                LaunchEventKind::SpawnFailed { .. } | LaunchEventKind::Failed { .. }
            ) {
                // A client can map before its launcher reports a terminal
                // failure.  Its foreign tab has no content yet, and keeping
                // it leaves an "Opening…" pane beside the failure toast.
                // The launcher only yields a surface when this is the sole
                // in-flight launch for the package, so concurrent launches
                // cannot make one failure close another app's window.
                if let Some(surface_id) =
                    crate::shell::launcher::take_sole_claimed_surface_for_failed_launch(
                        &event.package_id,
                    )
                    && {
                        let chrome = chrome_handle.borrow();
                        let mut tree = chrome.tree_mut();
                        crate::compositor::tab_binding::retire_starting_foreign_surface(
                            &mut tree, surface_id,
                        )
                    }
                {
                    chrome_handle.borrow_mut().bump();
                    posted = true;
                }
                // A failed process can never claim its promised toplevel, so
                // clear it at the same boundary as the pending launch.
                crate::shell::launcher::clear_pending_placement(&event.package_id);
                crate::shell::catalogue::take_pending_launch(&event.package_id);
            }
            if matches!(
                event.kind,
                LaunchEventKind::Exited | LaunchEventKind::WindowLost { .. }
            ) {
                // A successful launcher parent may have daemonised. Its GUI
                // child still owns this launch for a short bounded interval,
                // so settle the launch now but keep placement until a
                // matching toplevel claims it or the grace expires.
                crate::shell::launcher::retire_pending_placement_after_grace(&event.package_id);
                crate::shell::catalogue::take_pending_launch(&event.package_id);
            }
            match &event.kind {
                LaunchEventKind::Started { .. }
                | LaunchEventKind::Exited
                | LaunchEventKind::WindowLost { .. }
                | LaunchEventKind::OutOfMemory(_) => {
                    let _ = crate::shell::launcher::take_launch_presented(&event.package_id);
                }
                // Its window was up, so it did open; the window, and its
                // tab, are already gone.
                // A window that stopped on its own is never an ending the
                // person asked for, shown or not.
                LaunchEventKind::Failed { code, stderr_tail }
                    if !stderr_tail.contains(ui::app::WINDOW_LOST)
                        && crate::shell::launcher::take_launch_presented(&event.package_id) =>
                {
                    tracing::info!(
                        target: "shell::launcher",
                        package_id = %event.package_id,
                        display_name = %event.display_name,
                        exit_code = ?code,
                        "application exited with an error after showing its window"
                    );
                    continue;
                }
                // Geoclue and nm-applet are autostart helpers. They are not the
                // app the person opened, so their exit must not toast
                // "crashed before opening" for that launch.
                LaunchEventKind::Failed { .. }
                    if crate::shell::launcher::unasked_session_helper(&event.package_id) =>
                {
                    tracing::info!(
                        target: "shell::launcher",
                        package_id = %event.package_id,
                        display_name = %event.display_name,
                        "session helper exited without a window the user asked to open"
                    );
                    crate::shell::launcher::restart_session_helper(&event.package_id);
                    continue;
                }
                LaunchEventKind::Failed { .. } | LaunchEventKind::SpawnFailed { .. } => {}
            }
            match &event.kind {
                LaunchEventKind::Started { pid } => {
                    tracing::info!(
                        target: "shell::launcher",
                        package_id = %event.package_id,
                        display_name = %event.display_name,
                        pid,
                        "application process started"
                    );
                    continue;
                }
                LaunchEventKind::Exited => {
                    tracing::info!(
                        target: "shell::launcher",
                        package_id = %event.package_id,
                        display_name = %event.display_name,
                        "application process exited successfully"
                    );
                    continue;
                }
                LaunchEventKind::SpawnFailed { .. }
                | LaunchEventKind::Failed { .. }
                | LaunchEventKind::WindowLost { .. }
                | LaunchEventKind::OutOfMemory(_) => {}
            }
            event.log_failure();
            let Some(notice) = event.failure_notice() else {
                continue;
            };
            let settled = {
                let chrome = chrome_handle.borrow();
                let mut tree = chrome.tree_mut();
                crate::shell::peer_windows::settle_local_restore(
                    &mut tree,
                    &event.package_id,
                    &notice.body,
                ) | crate::shell::peer_windows::settle_install(
                    &mut tree,
                    &event.package_id,
                    &notice.body,
                )
            };
            if settled {
                chrome_handle.borrow_mut().bump();
            }
            let mut st = chrome_handle.borrow_mut();
            st.push_notification_full(
                tilo_desktop::notifications::Notification::new(notice.title, notice.body)
                    .with_severity(tilo_desktop::notifications::NotificationSeverity::Error)
                    .with_app_name(t("host-source-applications"))
                    .with_dismiss_after(crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
            );
            st.bump();
            posted = true;
        }
        posted
    }

    /// Typed `session.*` actions invoked since the last update, run by the
    /// executor the power flyout uses.
    pub(super) fn run_requested_session_actions(&self) {
        let Some(chrome) = self.chrome.as_ref() else {
            return;
        };
        let requests = chrome.borrow_mut().take_session_action_requests();
        for action in requests {
            self.execute_session_action(action);
        }
    }

    pub(super) fn drain_session_action_results(&self) -> bool {
        let mut posted = false;
        while let Ok(outcome) = self.session_action_channel.1.try_recv() {
            let Err(error) = outcome.result else {
                continue;
            };
            tracing::warn!(
                target: "tilo::os_actions",
                action = ?outcome.action,
                error = %error,
                "session action failed"
            );
            let Some(chrome) = self.chrome.as_ref() else {
                continue;
            };
            let body = match outcome.action {
                os_actions::SessionAction::Lock => t("host-session-lock-failed"),
                os_actions::SessionAction::Logout => t("host-session-logout-failed"),
                os_actions::SessionAction::SwitchUser => t("host-session-switch-user-failed"),
                os_actions::SessionAction::Suspend => t("host-session-suspend-failed"),
                os_actions::SessionAction::Restart => t("host-session-restart-failed"),
                os_actions::SessionAction::ShutDown => t("host-session-shut-down-failed"),
            };
            chrome.borrow_mut().push_notification_full(
                tilo_desktop::notifications::Notification::new(
                    t("host-session-action-failed"),
                    body,
                )
                .with_severity(tilo_desktop::notifications::NotificationSeverity::Error)
                .with_app_name(t("host-source-system"))
                .with_dismiss_after(crate::shell::launcher::LAUNCH_FAILURE_NOTICE_LIFETIME),
            );
            posted = true;
        }
        posted
    }

    /// Settles the pending launch of any app now on screen, so it can be
    /// opened again. Neither `Started` (forked, no window yet) nor an
    /// adopted-but-not-presenting tab counts — what settles it is the app's
    /// first frame or placeholder card, whichever shows first.
    pub(crate) fn settle_launches_of_opened_windows(&self) -> bool {
        if self.desktop_controls.is_explicit() {
            return false;
        }
        let pending = crate::shell::catalogue::pending_launches();
        if pending.is_empty() {
            return false;
        }
        let Some(chrome_handle) = self.chrome.clone() else {
            return false;
        };
        let painted: Vec<String> = {
            let st = chrome_handle.borrow();
            st.tree()
                .desktop
                .workspaces
                .iter()
                .flat_map(|w| w.strips.iter())
                .flat_map(|s| s.window_groups.iter())
                .flat_map(|g| g.tabs.iter())
                .filter_map(|t| match &t.tile {
                    crate::shell::state::TileContent::Foreign { app_id, surface_id }
                        if {
                            let store = crate::compositor::foreign::store();
                            store.is_presented(*surface_id) || store.is_opening(*surface_id)
                        } =>
                    {
                        Some(app_id.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        if painted.is_empty() {
            return false;
        }
        let mut retired = false;
        for package_id in pending {
            let showing = painted
                .iter()
                .any(|id| crate::shell::launcher::app_id_matches_package(id, &package_id));
            if !showing {
                continue;
            }
            retired |= crate::shell::catalogue::take_pending_launch(&package_id);
        }
        retired
    }

    pub fn pump_permission_prompt(&mut self) {
        let Some(chrome_handle) = self.chrome.clone() else {
            return;
        };

        // A retired native/portal request cannot leave a stale grant sheet.
        chrome_handle
            .borrow_mut()
            .pending_grants
            .retain(|pending| !pending.is_retired());

        // The widget and semantic actions answer the same exact pending request.
        let choice = self
            .permission_prompt
            .as_ref()
            .and_then(|p| p.pending_choice);
        if let Some(choice) = choice {
            if let Some(id) = self.permission_prompt_id {
                let _ = crate::shell::search::providers::permission_prompts::answer_pending_prompt(
                    &mut chrome_handle.borrow_mut(),
                    id,
                    choice,
                );
            }
            self.permission_prompt = None;
            self.permission_prompt_id = None;
        }
        let head_id = chrome_handle.borrow().pending_grants.front().map(|p| p.id);
        if self.permission_prompt_id != head_id {
            self.permission_prompt = None;
            self.permission_prompt_id = None;
        }

        // Step 2 — latch a new prompt when the queue's front needs
        // surfacing and we don't already hold a widget.
        if self.permission_prompt.is_none() {
            let st = chrome_handle.borrow();
            if let Some(pending) = st.pending_grants.front() {
                self.permission_prompt_id = Some(pending.id);
                self.permission_prompt_app = Some(pending.app_id.clone());
                let label = prompt_label(&pending.request);
                let is_owner = is_active_workspace_owner(&st);
                let prompt = PermissionPrompt::new(pending.app_id.clone(), label, is_owner);
                self.permission_prompt = Some(match &pending.request {
                    crate::shell::chrome_state::PromptRequest::DesktopControl(ask) => {
                        prompt.with_choices(ask.choices())
                    }
                    _ => prompt,
                });
            }
        }
    }
}
