//! The host as a [`Widget`]: the frame passes, events and accessibility, and the
//! component lifecycle that reaches the kept workspaces too.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl Widget for ShellOverlayHost {
    fn has_pending_visual_work(&self) -> bool {
        // Artwork that arrived since the rows were built is owed to them
        // as much as artwork still on its way.
        let mut pending = self.palette_visible
            && (self.search_indexes_settling
                || crate::app_icons::resolving()
                || crate::app_icons::artwork_generation() != self.search_artwork);
        self.for_each_child(&mut |child| pending |= child.has_pending_visual_work());
        pending
    }

    ui::provides!(dyn ui::component_lifecycle::ComponentLifecycle);

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn set_interaction_environment(&mut self, environment: ui::InteractionEnvironment) {
        self.interaction = environment;
        for child in self.children_mut() {
            child.set_interaction_environment(environment);
        }
    }

    fn sync(&mut self, s: &AppModel, p: &ModelPath) {
        let (s, p) = ui::doc::document_ref(s, p);
        let open_desktop_tour = self.onboarding.is_none()
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().overlays.desktop_tour);
        if open_desktop_tour {
            if let Some(chrome) = self.chrome.as_ref() {
                let mut state = chrome.borrow_mut();
                state.overlays.desktop_tour = false;
                state.bump();
            }
            self.onboarding = Some(crate::onboarding_view::OnboardingView::new_tour());
            self.register_onboarding_actions();
            self.onboarding_released = false;
            self.onboarding_focus_reset = false;
        }
        self.start_requested_identity_setup();
        if let Some(onboarding) = self.onboarding.as_mut() {
            let had_identity = onboarding.flow().is_done();
            onboarding.sync(s, p);
            let acquired_identity = !had_identity && onboarding.flow().is_done();
            let finished = onboarding.finished();
            // A verified identity becomes the session's principal immediately,
            // including during Backup/Codecs. Waiting for desktop release leaves
            // that principal outside the workspaces the Unix account created.
            if (acquired_identity || finished)
                && let Some(chrome) = self.chrome.as_ref()
            {
                chrome.borrow_mut().adopt_provisioned_identity();
            }
            if finished {
                self.onboarding = None;
                self.onboarding_released = true;
                self.onboarding_focus_reset = true;
            }
            return;
        }
        self.content.sync(s, p);
        self.palette.sync(s, p);
        self.agent_quick_composer.sync(s, p);
        self.person_quick_composer.sync(s, p);
        if self.peer_sync_overlay.visible() {
            self.peer_sync_overlay.sync(s, p);
        }
        if self.bar_popovers.profile.is_open() {
            self.bar_popovers.profile.sync(s, p);
        }
        self.capture_studio.sync(s, p);
        for (_, mirror) in &mut self.capture_mirrors {
            mirror.sync(s, p);
        }
        if let Some(pairing) = self.pairing.view_mut() {
            pairing.sync(s, p);
        }
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        if self.onboarding.is_none() {
            self.settle_launchpad(edits, path);
            self.retry_pending_launchpad_app(edits, path);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the compositor tick is deliberately ordered in one place so source refreshes precede palette input and every frame-side effect remains visible"
    )]
    fn update(&mut self, doc: &AppModel, path: &ModelPath) -> bool {
        // Read the person's region formats here, on the tick, so painting
        // the dates and numbers it formats never reads the settings file.
        let _ = ui::format::region::current();
        let mut steps = UpdateSteps::start();
        // The roster, the display sheet's list and the quick settings page
        // glide on after a flick, repainted as a wheel's scroll is.
        let glided = self.collaborators_overlay.advance_scroll()
            | self.projection_overlay.advance_scroll()
            | self.advance_qs_root_scroll();
        if glided && let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bump();
        }
        let person_activation = self.pump_person_cursor_activation();
        let person_context_dirty = self.validate_person_composer();
        let person_action_dirty = {
            let person_source = self.marker_action_session_source();
            self.person_quick_composer.pump(person_source)
        };
        let (doc, path) = ui::doc::document_ref(doc, path);
        self.import_room_tasks();
        let tracker = self
            .tracker
            .as_deref()
            .map(|tracker| tracker as &dyn agent_actions::RunSource);
        let action_dirty = self
            .agent_actions
            .values_mut()
            .fold(false, |changed, actions| {
                actions.pump(&mut self.agent_quick_composer, tracker) | changed
            });
        let action_dirty = action_dirty | self.pump_agent_asks() | self.switch_composer_agent();
        if action_dirty {
            self.agent_quick_composer.sync(doc, path);
        }
        let agent_quick_dirty = action_dirty | self.agent_quick_composer.update(doc, path);
        let person_quick_dirty = self.person_quick_composer.update(doc, path)
            | (self.peer_sync_overlay.visible() && self.peer_sync_overlay.update(doc, path))
            | self.run_peer_sync_sheet_request()
            | self.pump_app_sheets()
            | person_activation
            | person_context_dirty
            | person_action_dirty;
        let bar_popover_dirty =
            self.bar_popovers.profile.is_open() && self.bar_popovers.profile.update(doc, path);
        let marker_dirty = self
            .agent_markers
            .retain(|source| source.is_fresh(Instant::now()));
        crate::shell::settings_records::pump();
        // Who is here is looked at before Activity reads what changed, so an
        // arrival said on this frame is part of what Activity saw.
        let arrival_dirty = self.refresh_presence_notices();
        // Records Activity reads from disk are read off the loop; one that
        // came in refreshes the glance and what the Activity app reads on this frame.
        let activity_records_dirty = self.room_activity.take_persisted_reads();
        if activity_records_dirty {
            self.activity_refresh.invalidate();
        }
        self.pump_room_activity();
        self.serve_activity();
        if let Some(chrome) = &self.chrome
            && let Some(apps) = chrome.borrow().hosted_inputs.in_process()
        {
            let mut apps = apps.borrow_mut();
            // Only Linux enrollment asks for a provider's network access.
            #[cfg(target_os = "linux")]
            if let Some(prompts) = &self.portal_prompts {
                apps.bind_messaging_network_prompts(prompts.clone());
            }
            apps.pump_messaging_records();
            apps.pump_feedback();
        }
        self.settle_workspace_emoji();
        steps.mark("in-process-apps");
        if self.onboarding.is_some() {
            // Compositor-owned sheets (polkit administrator prompts, network
            // secrets) are raised by the system on behalf of the person in
            // front of the screen, including while first-use setup is still
            // running. Their requesters block until the sheet is answered,
            // so the queue must keep moving under onboarding too.
            let dialog_dirty = self.pump_portal_dialogs();
            let onboarding_dirty = self
                .onboarding
                .as_mut()
                .is_some_and(|onboarding| onboarding.update(doc, path));
            return onboarding_dirty || dialog_dirty;
        }
        let pairing_dirty = self.pairing.pump(doc, path);
        let radio_dirty = self.poll_quick_settings_radio();
        self.rescan_open_wifi_section(Instant::now());
        // Devices and the Clipboard follow the fabric and the history while
        // they show.
        self.refresh_qs_hosted_rows();
        let quick_settings_dirty = self.apply_quick_settings_readings();
        let projection_open = self.overlay_flags().projection;
        let display_storage_dirty =
            self.projection_overlay.poll_storage(projection_open) | self.close_settled_projection();
        steps.mark("settings-readings");
        // Runs first so an invite token / prompt is in chrome before draw.
        let peer_sync_dirty = self.pump_peer_sync();
        steps.mark("peer-sync");
        self.tick_shortcut_triggers();
        self.pump_shortcut_step_prompts();
        self.record_finished_shortcut_runs();
        steps.mark("shortcut-triggers");
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            state.sync_external_tree_revision();
            state.expire_notification_history_clear(Instant::now());
            state.flush_notification_history();
        }
        steps.mark("notification-history");
        let mut search_rows_stale = false;
        // Retained search indexes are maintained on the compositor tick,
        // before palette input is routed.  Providers may do a source refresh
        // here, but the keystroke/query path below only reads ready indexes.
        if self.palette_visible
            && let Some(chrome) = self.chrome.clone()
        {
            let current_query = self.palette.query().to_string();
            let (scope, active_provider) = {
                let state = chrome.borrow();
                (
                    state.search_scope,
                    active_tab_provider_id(&state, &self.search_registry),
                )
            };
            let state = chrome.borrow();
            let args = crate::shell::search::QueryArgs {
                needle: &current_query,
                scope,
                active_provider_id: active_provider.as_deref(),
                wanted_kinds: &self.active_kind_filter,
                chrome: &state,
                doc,
                doc_root: path,
            };
            search_rows_stale = self.search_registry.refresh_indexes(&args);
            self.search_indexes_settling = self.search_registry.indexes_settling(&args);
            // Rows take their artwork when built: artwork found since is
            // theirs only once they are built again.
            let artwork = crate::app_icons::artwork_generation();
            if artwork != self.search_artwork {
                self.search_artwork = artwork;
                search_rows_stale = true;
            }
        } else {
            self.search_indexes_settling = false;
            if let Some(chrome) = self.chrome.as_ref() {
                self.search_registry
                    .maintain_launcher_indexes(&chrome.borrow(), doc, path);
            }
        }
        steps.mark("search-index");
        self.poll_pending_launchpad_actions();
        // Before the resync, so what an action moved is spliced this frame.
        self.run_requested_shortcut_actions();
        self.run_device_local_actions();
        self.latch_overview_transition();
        steps.mark("launchpad-and-shortcut-actions");
        // Workspace switch before anything walking `group_row_ptrs` (it
        // replaces the whole row set), then splice the tree's structural
        // changes into the rows — both before the children's own `update`
        // so freshly-spliced groups join this frame's sync pass.
        // A split or transfer the rows made from this frame's input reaches
        // the tree first: the resync below rebuilds the rows from the tree
        // whenever anything else bumped the chrome since the gesture, and
        // would otherwise put the dragged tab back where it came from.
        self.harvest_pending_group_topology();
        let ws_swapped = self.resync_workspace_content();
        self.follow_brought_forward();
        let chrome_dirty = self.resync_strips_from_tree() || ws_swapped;
        // Before children update, so a promotion/restore this frame shows.
        self.sync_fullscreen_from_chrome();
        // The top bar steps aside for a window maximized this frame (and
        // comes back with its restore) in the same pass: the bar and the
        // windows' insets are laid out and painted from it below.
        self.sync_top_bar_presence(Instant::now());
        steps.mark("resync");
        let content_dirty = self.content.update(doc, path);
        steps.mark("content-update");
        // Normally drained synchronously by input dispatch. Keep this
        // fallback for accessibility/synthetic activation paths that can
        // queue a widget close without returning through pointer dispatch.
        let close_dirty = self.harvest_pending_tab_closes();
        // Same fallback for a right-click request that arrived outside
        // pointer dispatch; and drop a menu whose tab has since gone.
        self.harvest_pending_tab_context_menus();
        self.prune_stale_tab_context_menu();
        // Same for a popover the bar asked for, and what was chosen in one.
        self.harvest_bar_popover_request();
        self.settle_bar_popovers();
        self.harvest_floating_surface_requests();
        self.harvest_floating_surface_source_updates();
        self.harvest_capture_studio_requests();
        steps.mark("harvest-requests");
        let palette_dirty = self.palette.update(doc, path);
        if search_rows_stale {
            // A worker published an index, or artwork arrived, after the
            // rows were built. Force one normal fanout this frame so the
            // visible list updates without another character typed.
            self.last_search_revision.clear();
            self.refresh_search_results(doc, path);
        }
        self.refresh_open_palette(doc, path);
        #[allow(
            clippy::unnecessary_fold,
            reason = "every mirror update has side effects and must run even after one reports dirty"
        )]
        let mirror_dirty = self
            .capture_mirrors
            .iter_mut()
            .map(|(_, mirror)| mirror.update(doc, path))
            .fold(false, |dirty, next| dirty || next);
        let switcher_dirty = self.refresh_strip_switcher_model();
        steps.mark("palette-mirrors-switcher");
        // The harvesters run after `content.update` so this frame's
        // gestures are visible to them, mirroring one-shot widget-side
        // changes into the persisted tree so they survive a restart and
        // replicate to peers.
        self.reconcile_active_group_off_closing();
        self.harvest_pending_maximize_changes();
        self.harvest_pending_width_changes();
        self.harvest_pending_group_reorder();
        self.harvest_pending_tab_reorder();
        self.harvest_pending_group_topology();
        self.harvest_pending_search_open();
        let tab_rename_dirty = self.harvest_tab_renames();
        let hosted_windows_dirty = self.sync_hosted_windows();
        let hosted_windows_dirty = self.sync_window_shares() | hosted_windows_dirty;
        let chips_dirty = self.sync_desktop_control_chips();
        // Refreshing here (not inside the harvest) means the palette never
        // shows "Nothing to show" before the user's first keystroke.
        let opened_for_new_tab = self.harvest_pending_new_tab();
        if opened_for_new_tab {
            self.refresh_search_results(doc, path);
        }
        steps.mark("harvest-changes");
        self.clear_stale_open_target();
        self.clear_stale_wifi_password();
        self.sync_status_from_bridge();
        let presence_dirty = self.pump_presence_requests();
        let presence_dirty = self.sync_self_presence() | presence_dirty;
        let presence_dirty = arrival_dirty | presence_dirty;
        self.pump_screenshot_sends();
        let follow_dirty = self.reconcile_followed_peer();
        self.follow_focus_across_displays();
        // Edge-triggered on `active_group`. Input already flushed the
        // widget-side tab into the tree before this update.
        self.scroll_active_group_into_view();
        self.pick_capture_window();
        let capture_targets = self.capture_targets();
        self.capture_studio.set_candidates(capture_targets);
        self.dispatch_cross_strip_detaches();
        steps.mark("presence-and-follow");
        // Offer every open tab's document to its own writer, debounced and
        // off-thread — without this the store never writes to disk.
        if let Some(chrome) = self.chrome.as_ref() {
            let state = chrome.borrow();
            // Release native document launch data once its tab is gone —
            // covers removals through workspace/peer reconciliation as well
            // as the explicit close action. Looked for through the guard
            // first, so a frame with nothing to release writes nothing.
            let mut tree = state.tree_mut();
            if state.hosted_inputs.documents_to_prune(&tree)
                && state.hosted_inputs.prune_documents(&mut tree)
            {
                tree.queue_save();
            }
            drop(tree);
            let records = state.records().clone();
            drop(state);
            records.tick();
        }
        steps.mark("documents-and-records");
        // Before the prompt pump, so a request arriving this frame is on
        // screen by the end of it, not one frame later.
        let portal_dirty = self.drain_portal_prompts();
        let picker_dirty = self.pump_file_picker();
        let dialog_dirty = self.pump_portal_dialogs();
        self.pump_permission_prompt();
        steps.mark("prompts");
        let launch_dirty = self.drain_launch_events() | self.settle_launches_of_opened_windows();
        self.run_requested_session_actions();
        let session_action_dirty = self.drain_session_action_results();
        steps.mark("launches-and-session-actions");
        // Also driven by the widget's own `update`, but routed here too so
        // chrome stays consistent for tests that never call the widget.
        let notifications_dropped = if let Some(chrome) = self.chrome.as_ref() {
            let dropped = chrome
                .borrow_mut()
                .update_notification_lifecycle(std::time::Instant::now());
            dropped > 0
        } else {
            false
        };
        // Stamp the wall clock on whatever `doc_*` left open, or automerge
        // closes it implicitly with a zero timestamp — undatable later. A
        // frame that left nothing open has nothing to stamp, and takes no
        // write that would announce a change.
        if let Some(chrome) = self.chrome.as_ref() {
            use automerge::transaction::Transactable as _;
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            if tree.doc.pending_ops() > 0 {
                tree.commit_room_change();
            }
        }
        let desk_dirty = self.update_desk(ui::time::now());
        // A window bar's tooltip whose dwell ran out shows on this tick: its
        // deadline woke the loop, and a deadline alone paints nothing.
        let tooltip_dirty = self.bar_tooltip.tick(ui::time::now());
        steps.mark("commit");
        drop(steps);
        desk_dirty
            || tooltip_dirty
            || agent_quick_dirty
            || person_quick_dirty
            || bar_popover_dirty
            || marker_dirty
            || content_dirty
            || quick_settings_dirty
            || radio_dirty
            || display_storage_dirty
            || pairing_dirty
            || close_dirty
            || palette_dirty
            || switcher_dirty
            || notifications_dropped
            || launch_dirty
            || session_action_dirty
            || chrome_dirty
            || portal_dirty
            || picker_dirty
            || dialog_dirty
            || mirror_dirty
            || presence_dirty
            || chips_dirty
            || follow_dirty
            || peer_sync_dirty
            || activity_records_dirty
            || hosted_windows_dirty
            || tab_rename_dirty
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the host assembles one ordered overlay layout lifecycle"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let contextual_tour = self
            .onboarding
            .as_ref()
            .is_some_and(crate::onboarding_view::OnboardingView::presents_desktop_context);
        if !contextual_tour && let Some(onboarding) = self.onboarding.as_mut() {
            let onboarding_node = onboarding.build_layout(taffy, theme, node_map);
            self.onboarding_node = Some(onboarding_node);
            // Setup remains an independent modal workflow, but Print Screen
            // must be able to put Capture Studio above it without destroying
            // the setup step.  Keep the capture node in this otherwise
            // dedicated tree so the chooser has both geometry and an input
            // target while OOBE is active.
            let capture_node = self.capture_studio.build_layout(taffy, theme, node_map);
            self.place_on_focused_display(taffy, capture_node);
            self.capture_node = Some(capture_node);
            let node = taffy
                .new_with_children(
                    Style {
                        size: Size {
                            width: Dimension::percent(1.0),
                            height: Dimension::percent(1.0),
                        },
                        ..Default::default()
                    },
                    &[onboarding_node, capture_node],
                )
                .unwrap_or(onboarding_node);
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            return node;
        }
        // The overview's row heights follow the theme's type sizes; resolve
        // them here, where the theme is in hand, so this pass's hit-testing
        // and paint share one answer.
        self.resolve_overview_layout(theme);
        self.shortcut_navigation_step = shortcut_navigation_step(theme);
        self.onboarding_node = None;
        // Both pushed before the content tree is built, so this pass's
        // fullscreen collapse and per-display width resolution are in
        // effect — the latter is what makes a single-frame offscreen
        // render match the live shell's geometry.
        // A client can request fullscreen before its group is adopted into
        // the rendered tree. Keep that request in chrome for the later
        // reconcile, but suppress desktop chrome only once a live row owns
        // the group. Otherwise a stale or racing id blanks the entire shell.
        self.sync_fullscreen_from_chrome();
        let fullscreen = self.fullscreen_group_is_owned();
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().set_fullscreen_presentation(fullscreen);
        }
        // On a desktop spread across displays a fullscreen takes its own
        // display's whole rect (`fullscreen_lane_area`); the column keeps
        // its spacing, so the other displays' windows stay where they were.
        let expands_column = fullscreen
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().fullscreen_display().is_none());
        if let Some(surface) = self.desktop_surface_mut() {
            surface.style_work_area(expands_column);
        }
        let spans = self.strip_output_spans();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — pointer captured from a `Box<GroupRow>` we own
            // through the content tree; host outlives content; `&mut self`
            // → unaliased mutable access.
            unsafe { (&mut *ptr).set_output_spans(&spans) };
        }
        self.push_display_layout();
        let content_node = self.content.build_layout(taffy, theme, node_map);
        // The list grows with its rows up to most of the display it is on,
        // then scrolls; what fits decides how many rows each group shows.
        let palette_area = self.overlay_area();
        if palette_area.height() > 0.0 {
            self.palette.set_max_list_height(
                (palette_area.height() * PALETTE_LIST_SHARE)
                    .min(PALETTE_LIST_MAX * f64::from(theme.text_scale))
                    .max(160.0),
            );
        }
        let palette_node = self.palette.build_layout(taffy, theme, node_map);
        // Match children(): a closed composer has no live controls to measure
        // or query. Visibility changes retire/rebuild the map below.
        self.agent_quick_composer_node = self.agent_quick_composer.is_open().then(|| {
            self.agent_quick_composer
                .build_layout(taffy, theme, node_map)
        });
        self.person_quick_composer_node = self.person_quick_composer.is_open().then(|| {
            self.person_quick_composer
                .build_layout(taffy, theme, node_map)
        });
        self.peer_sync_node = self.build_peer_sync_layout(taffy, theme, node_map);
        let app_sheet_window = self.build_app_sheet_layout(taffy, theme, node_map);
        if app_sheet_window.is_none() {
            self.app_sheet_nodes = None;
        }
        let capture_node = self.capture_studio.build_layout(taffy, theme, node_map);
        self.place_on_focused_display(taffy, capture_node);
        self.capture_node = Some(capture_node);
        self.screenshot_markup_node = self
            .screenshot_markup
            .as_mut()
            .map(|markup| markup.build_layout(taffy, theme, node_map));
        if let Some(node) = self.screenshot_markup_node {
            self.place_on_focused_display(taffy, node);
        }
        self.capture_mirror_nodes = self
            .capture_mirrors
            .iter_mut()
            .map(|(_, mirror)| mirror.build_layout(taffy, theme, node_map))
            .collect();
        // Override the widget's own narrow 360px default so the palette
        // reads as a centered modal sheet, not a top-right popover.
        // `taffy.style` can only fail for a `NodeId` foreign to this tree
        // or already removed — `palette_node` is neither, it was just
        // inserted above — but the override is skippable, so a failure
        // here degrades to the widget's own default layout instead of
        // panicking.
        // Centred on the focused display: across two, the host's own
        // centre is the seam between them.
        let area = self.overlay_area();
        if let Ok(style) = taffy.style(palette_node) {
            let mut palette_style = style.clone();
            palette_style.position = Position::Absolute;
            let frame = self.palette_frame(theme.text_scale);
            let palette_target_w = frame.width();
            palette_style.max_size.height =
                Dimension::length(f32_of_f64((area.height() - 112.0).max(200.0)));
            palette_style.size = Size {
                width: Dimension::length(f32_of_f64(palette_target_w)),
                height: palette_style.size.height,
            };
            palette_style.margin = TaffyRect {
                left: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::auto(),
                top: LengthPercentageAuto::length(0.0),
                bottom: LengthPercentageAuto::auto(),
            };
            palette_style.inset = TaffyRect {
                left: LengthPercentageAuto::length(f32_of_f64(frame.x0 - self.rect.x0)),
                right: LengthPercentageAuto::auto(),
                top: LengthPercentageAuto::length(f32_of_f64(frame.y0 - self.rect.y0)),
                bottom: LengthPercentageAuto::auto(),
            };
            let _ = taffy.set_style(palette_node, palette_style);
        }
        // `self.rect` is last frame's, so frame 1 sizes off a zero-width
        // host and lands on the 560 floor; remembering what it sized
        // against lets `apply_layout` re-dirty and converge.
        self.palette_layout_host = Some((self.rect, area));

        self.content_node = Some(content_node);
        self.palette_node = Some(palette_node);

        let mut children: Vec<NodeId> = vec![content_node, palette_node, capture_node];
        children.extend(self.agent_quick_composer_node);
        children.extend(self.person_quick_composer_node);
        children.extend(
            self.person_markers
                .build(taffy, theme, node_map, self.rect.origin()),
        );
        children.extend(self.build_bar_popovers(taffy, theme, node_map));
        children.extend(
            self.agent_markers
                .build(taffy, theme, node_map, self.rect.origin()),
        );
        if let Some(markup_node) = self.screenshot_markup_node {
            children.push(markup_node);
        }
        children.extend(self.capture_mirror_nodes.iter().copied());
        if contextual_tour && let Some(onboarding) = self.onboarding.as_mut() {
            let onboarding_node = onboarding.build_layout(taffy, theme, node_map);
            // The desktop already consumes the root flex row. The tutorial is
            // modal chrome above that desktop, not another sibling pane: make
            // it fill the viewport without participating in flex placement.
            if let Ok(style) = taffy.style(onboarding_node) {
                let mut overlay_style = style.clone();
                overlay_style.position = Position::Absolute;
                overlay_style.inset = TaffyRect {
                    left: LengthPercentageAuto::length(0.0),
                    right: LengthPercentageAuto::length(0.0),
                    top: LengthPercentageAuto::length(0.0),
                    bottom: LengthPercentageAuto::length(0.0),
                };
                let _ = taffy.set_style(onboarding_node, overlay_style);
            }
            self.onboarding_node = Some(onboarding_node);
            children.push(onboarding_node);
        }
        if let Some(pairing_node) = self.pairing.build_layout(taffy, theme, node_map) {
            children.push(pairing_node);
        }
        children.extend(self.peer_sync_node);
        children.extend(app_sheet_window);
        // `new_with_children` only errs on a foreign/removed child
        // `NodeId`, and every id in `children` was just created above in
        // this same tree — but rather than assert that, fall back to the
        // content node alone: a host that (impossibly) can't wrap its
        // overlays still shows the desktop instead of dying.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the host applies one ordered overlay layout lifecycle"
    )]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // A missing layout for `node` would mean taffy never computed
        // this frame's tree at all — nothing downstream has a sensible
        // rect to work with, so skip the frame rather than panic.
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = ui::widget::placed(l, origin);
        self.workspace_settings_overlay.layout(self.overlay_area());
        let contextual_tour = self
            .onboarding
            .as_ref()
            .is_some_and(crate::onboarding_view::OnboardingView::presents_desktop_context);
        if !contextual_tour && let Some(onboarding) = self.onboarding.as_mut() {
            if let Some(onboarding_node) = self.onboarding_node {
                onboarding.apply_layout(
                    taffy,
                    onboarding_node,
                    Point::new(self.rect.x0, self.rect.y0),
                );
            }
            if let Some(capture_node) = self.capture_node {
                self.capture_studio.apply_layout(
                    taffy,
                    capture_node,
                    Point::new(self.rect.x0, self.rect.y0),
                );
            }
            return;
        }
        // The dedicated OOBE tree does not mount desktop agent controls.
        // Their cached nodes belong only to the desktop layout built below.
        if let Some(agent_node) = self.agent_quick_composer_node {
            self.agent_quick_composer
                .apply_layout(taffy, agent_node, self.rect.origin());
        }
        self.apply_bar_popovers_layout(taffy);
        self.agent_markers.apply(taffy, self.rect.origin());
        self.person_markers.apply(taffy, self.rect.origin());
        if let Some(node) = self.person_quick_composer_node {
            self.person_quick_composer
                .apply_layout(taffy, node, self.rect.origin());
        }
        // One more pass if the card sized against stale host dimensions or
        // focus moved to another display; settles in a single extra frame.
        if self.palette_layout_host != Some((self.rect, self.overlay_area())) {
            self.palette_layout_dirty = true;
        }
        // Re-derive strip widths now the host's rect is known: frame 1's
        // `build_layout` had no viewport yet. A no-op once the
        // compositor has pushed real output geometry.
        let settled_spans = self.strip_output_spans();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same contract as the other group_row_ptrs
            // accessors — pointer captured from a `Box<GroupRow>` we
            // own through the content tree; host outlives content;
            // &mut self → unaliased mutable access.
            unsafe {
                let row = &mut *ptr;
                row.set_scroll_quantum(self.strip_pan_quantum);
                row.set_leading_inset(STRIP_EDGE_GUTTER_PX);
                row.set_output_spans(&settled_spans);
                // App-owned call/navigation surfaces cross into this same
                // combined-output coordinate space. Push the real boundary
                // before child layout so both sides resolve one identical
                // rect on the ownership-transfer frame.
                ui::widget::FloatingSurfaceWidgetExt::set_floating_surface_bounds(row, self.rect);
            }
        }
        let Ok(children) = taffy.children(node) else {
            return;
        };
        if let Some(&content_child) = children.first() {
            self.content
                .apply_layout(taffy, content_child, Point::new(self.rect.x0, self.rect.y0));
        }
        if let Some(&palette_child) = children.get(1) {
            self.palette
                .apply_layout(taffy, palette_child, Point::new(self.rect.x0, self.rect.y0));
        }
        if let Some(capture_node) = self.capture_node {
            self.capture_studio.apply_layout(
                taffy,
                capture_node,
                Point::new(self.rect.x0, self.rect.y0),
            );
        }
        if let (Some(markup), Some(markup_node)) =
            (self.screenshot_markup.as_mut(), self.screenshot_markup_node)
        {
            markup.apply_layout(taffy, markup_node, Point::new(self.rect.x0, self.rect.y0));
        }
        for ((_, mirror), mirror_node) in self
            .capture_mirrors
            .iter_mut()
            .zip(self.capture_mirror_nodes.iter().copied())
        {
            mirror.apply_layout(taffy, mirror_node, Point::new(self.rect.x0, self.rect.y0));
        }
        if contextual_tour
            && let (Some(onboarding), Some(onboarding_node)) =
                (self.onboarding.as_mut(), self.onboarding_node)
        {
            onboarding.apply_layout(
                taffy,
                onboarding_node,
                Point::new(self.rect.x0, self.rect.y0),
            );
        }
        self.pairing
            .apply_layout(taffy, Point::new(self.rect.x0, self.rect.y0));
        self.apply_app_sheet_layout(taffy);
        if let Some(node) = self.peer_sync_node {
            // The sheet centres on, and measures itself against, the
            // display it opens on.
            let area = self.overlay_area();
            let origin = self.rect.origin();
            let sheet = &mut self.peer_sync_overlay;
            ui::widget::modal_dialog::within_window(node, area, || {
                sheet.apply_layout(taffy, node, origin);
            });
        }
        self.finish_axis_gather();
        self.publish_strip_viewport();
        if let Some(desk) = &self.shared_desk {
            desk.set_tiles(self.tab_tiles());
            desk.set_strip(self.shown_strip());
        }
    }

    /// The sections the compositor presents, composed into one scene, so
    /// captures, shots and tests show what the screen shows.
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // One scene is in screen space: nothing is drawn apart to be panned.
        let _screen_space = ui::retained_present::RetainedScene::begin(false);
        self.draw_scene_sections(transform, text_renderer, theme, presence)
            .compose_into(scene);
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        if self.layers.emoji_picker.is_open() {
            return self.layers.emoji_picker.primary_vertical_scroll();
        }
        let st = self.chrome.as_ref()?.borrow();
        if st.overlays.collaborators {
            return self.collaborators_overlay.primary_vertical_scroll();
        }
        if st.overlays.quick_settings
            && matches!(
                st.quick_settings.current_level(),
                crate::shell::quick_settings::QsLevel::Notifications
            )
        {
            self.notification_center.reading_scroll()
        } else {
            None
        }
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        if self.primary_vertical_scroll().is_none() {
            return ui::widget::PrimaryScrollResult::Unsupported;
        }
        let result = if self.layers.emoji_picker.is_open() {
            self.layers
                .emoji_picker
                .apply_primary_vertical_scroll(action)
        } else if self.overlay_flags().collaborators {
            self.collaborators_overlay
                .apply_primary_vertical_scroll(action)
        } else {
            self.notification_center.apply_reading_scroll(action)
        };
        if matches!(result, ui::widget::PrimaryScrollResult::Changed(_))
            && let Some(chrome) = &self.chrome
        {
            chrome.borrow_mut().bump();
        }
        result
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.route_window_event(event, ctx);
        // An event that opened the display sheet (Super+P, its action) has
        // it take what it shows now, before anything reads or draws it.
        let projection_open = self.overlay_flags().projection;
        self.projection_overlay.take_open_state(projection_open);
    }

    fn name(&self) -> &'static str {
        "ShellOverlayHost"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(self.host_accessibility())
    }

    /// The semantic route into an open layer: an assistive technology, a
    /// script or the bench activates a published target (`tabmenu:<slug>`)
    /// and gets the same operation a click or Enter on that row performs.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if target.starts_with("participant:") && target.ends_with(":chat") {
            if self.person_cursor_controls_suppressed() {
                return true;
            }
            return self
                .person_markers
                .semantic_action(target, action, state, path);
        }
        if target.starts_with(strip_switcher::TARGET_PREFIX)
            && matches!(action, ui::a11y::Action::Activate)
            && let Some(handled) = self.activate_strip_switcher_target(target)
        {
            return handled;
        }
        if target.starts_with(agent_markers::TARGET_PREFIX) {
            if self.accessible_agent_marker_request_is_inert(target) {
                return true;
            }
            return self
                .agent_markers
                .semantic_action(target, action, state, path);
        }
        // The palette is published inside this host's tree (see
        // `accessibility_represents_child`), so its query's semantic target
        // arrives here, not at the palette widget. Setting it is typing it:
        // the results are recomputed for the new text.
        if target == PALETTE_QUERY_TARGET && self.palette_visible {
            let before = self.palette.query().to_string();
            let handled = self
                .palette
                .perform_semantic_action(target, action, state, path);
            if handled
                && self.palette.query() != before
                && self.param_flow.is_none()
                && self.secondary_flow.is_none()
            {
                let (doc, root) = ui::doc::document(state, path);
                self.refresh_search_results(doc.doc(), root);
            }
            return handled;
        }
        if let Some(handled) = self.perform_sheet_action(target, action, state, path) {
            // A sheet's target never falls back to a pointer click: a stale
            // one must not press whatever is now under its old bounds.
            if !handled {
                tracing::debug!(target, ?action, "a11y: sheet action not applied");
            }
            return true;
        }
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some(intent) = self.layers.semantic_intent(target) else {
            return false;
        };
        self.apply_layer_intent(intent)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        self.host_synthetic_accessibility_rect(path)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        self.host_focus_synthetic_accessibility(path)
    }

    /// Expose content + palette as direct children so trait walkers can
    /// recurse into the live tree, in `build_layout`'s child-node order.
    /// The open palette is published once, by [`Self::accessibility`] under the
    /// command-palette layer with the host's filter band inside it; its widget
    /// (the [`Self::children`] entry after the content) is not exported again.
    fn accessibility_represents_child(&self, child_index: usize) -> bool {
        self.onboarding.is_none() && self.palette_visible && child_index == 1
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        let mut targets = self
            .layers
            .dictation
            .preview_target()
            .map(|rect| vec![("dictation.live-preview".into(), rect)])
            .unwrap_or_default();
        if self.onboarding.is_none() && self.palette_visible && self.palette.is_open() {
            // The legacy widget id covers the result card. A touch aimed at
            // the query must use the same row as its editable semantic node.
            targets.push((PALETTE_QUERY_TARGET.into(), self.palette.input_row_rect()));
        }
        if self.onboarding.is_none() {
            targets.extend(self.desk_test_rects());
            targets.extend(self.strip_switcher_test_rects());
            targets.extend(self.directory_sheet.test_rects());
        }
        targets
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(onboarding) = self.onboarding.as_ref() {
            return vec![onboarding, &self.capture_studio];
        }
        let mut out: Vec<&dyn Widget> =
            Vec::with_capacity(5_usize.saturating_add(self.capture_mirrors.len()));
        out.push(self.content.as_ref());
        if self.palette_visible {
            out.push(&self.palette);
        }
        out.push(&self.capture_studio);
        if let Some(markup) = self.screenshot_markup.as_ref() {
            out.push(markup);
        }
        out.extend(
            self.capture_mirrors
                .iter()
                .map(|(_, mirror)| mirror as &dyn Widget),
        );
        if let Some(pairing) = self.pairing.view() {
            out.push(pairing);
        }
        if self.agent_quick_composer.is_open() {
            out.push(&self.agent_quick_composer);
        }
        if self.bar_popovers.workspace_menu.is_open() {
            out.push(&self.bar_popovers.workspace_menu);
        }
        if self.bar_popovers.profile.is_open() {
            out.push(&self.bar_popovers.profile);
        }
        out.extend(self.agent_markers.children());
        if self.person_quick_composer.is_open() {
            out.push(&self.person_quick_composer);
        }
        out.extend(self.person_markers.children());
        if self.peer_sync_node.is_some() {
            out.push(&self.peer_sync_overlay);
        }
        if let (Some(sheet), Some(_)) = (&self.app_sheet, self.app_sheet_nodes) {
            out.push(sheet.dialog());
        }
        out
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(onboarding) = self.onboarding.as_mut() {
            return vec![onboarding, &mut self.capture_studio];
        }
        let mut out: Vec<&mut (dyn Widget + '_)> =
            Vec::with_capacity(5_usize.saturating_add(self.capture_mirrors.len()));
        out.push(self.content.as_mut());
        if self.palette_visible {
            out.push(&mut self.palette);
        }
        out.push(&mut self.capture_studio);
        if let Some(markup) = self.screenshot_markup.as_mut() {
            out.push(markup);
        }
        out.extend(
            self.capture_mirrors
                .iter_mut()
                .map(|(_, mirror)| mirror as &mut dyn Widget),
        );
        if let Some(pairing) = self.pairing.view_mut() {
            out.push(pairing);
        }
        if self.agent_quick_composer.is_open() {
            out.push(&mut self.agent_quick_composer);
        }
        if self.bar_popovers.workspace_menu.is_open() {
            out.push(&mut self.bar_popovers.workspace_menu);
        }
        if self.bar_popovers.profile.is_open() {
            out.push(&mut self.bar_popovers.profile);
        }
        out.extend(self.agent_markers.children_mut());
        if self.person_quick_composer.is_open() {
            out.push(&mut self.person_quick_composer);
        }
        out.extend(self.person_markers.children_mut());
        if self.peer_sync_node.is_some() {
            out.push(&mut self.peer_sync_overlay);
        }
        if let (Some(sheet), Some(_)) = (&mut self.app_sheet, self.app_sheet_nodes) {
            out.push(sheet.dialog_mut());
        }
        out
    }

    fn take_focus_reset(&mut self) -> bool {
        self.agent_quick_composer.take_focus_reset()
            | self.person_quick_composer.take_focus_reset()
            | self.peer_sync_overlay.take_focus_reset()
            | self
                .app_sheet
                .as_mut()
                .is_some_and(|sheet| sheet.dialog_mut().take_focus_reset())
            | self.bar_popovers.profile.take_focus_reset()
            | self.pairing.take_focus_reset()
            | std::mem::take(&mut self.onboarding_focus_reset)
            | self
                .onboarding
                .as_mut()
                .is_some_and(Widget::take_focus_reset)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A finger on a Quick Settings slider holds it, as the pointer does;
    /// it never turns into a scroll of the page.
    fn claims_raw_touch(&self, point: Point) -> bool {
        self.qs_slider_face_at(point)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.capture_studio.wants_pointer_capture()
            || self
                .screenshot_markup
                .as_ref()
                .is_some_and(Widget::wants_pointer_capture)
            || self.content.wants_pointer_capture()
            || self
                .capture_mirrors
                .iter()
                .any(|(_, mirror)| mirror.wants_pointer_capture())
    }

    /// Bubbles structural-dirty flags up from the content tree so the post-event rebuild picks up new nodes (e.g. a
    /// spliced `WindowGroup`). Without this the default `false` would
    /// shadow every child signal, leaving it at `Rect::ZERO`. Uses `|`,
    /// not `||`, so both children clear their own flags this tick.
    fn take_structure_dirty(&mut self) -> bool {
        if std::mem::take(&mut self.onboarding_released) {
            return true;
        }
        if let Some(onboarding) = self.onboarding.as_mut() {
            return onboarding.take_structure_dirty();
        }
        // A drag carried to the strip deck's edge went to another strip:
        // that strip takes focus, before the tree is read back.
        self.harvest_strip_deck_request();
        // Before resync, or it sees "widget has X, tree doesn't" and evicts X.
        self.harvest_pending_group_topology();
        self.harvest_widget_only_groups_into_tree();
        // Pumped here too since the shots driver's relayout path skips
        // `update()`; `last_chrome_revision` makes both call sites idempotent.
        let ws_swapped = self.resync_workspace_content();
        self.follow_brought_forward();
        let resynced = self.resync_strips_from_tree() | ws_swapped;
        let workspace_layout = std::mem::take(&mut self.workspace_content_layout_dirty);
        let content = self.content.take_structure_dirty();
        let palette = std::mem::take(&mut self.palette_layout_dirty);
        let capture_mirrors = std::mem::take(&mut self.capture_mirrors_structure_dirty);
        let pairing = self.pairing.take_structure_dirty();
        content
            | palette
            | capture_mirrors
            | resynced
            | workspace_layout
            | pairing
            | self.agent_markers.take_structure_dirty()
            | self.person_markers.take_structure_dirty()
            | (self.person_quick_composer.is_open() != self.person_quick_composer_node.is_some())
            | self.person_quick_composer.take_structure_dirty()
            | (self.peer_sync_overlay.visible() != self.peer_sync_node.is_some())
            | self.peer_sync_overlay.take_structure_dirty()
            | self.take_app_sheet_changed()
            | (self.agent_quick_composer.is_open() != self.agent_quick_composer_node.is_some())
            | self.agent_quick_composer.take_structure_dirty()
            | self.bar_popovers.structure_stale()
            | self.bar_popovers.profile.take_structure_dirty()
    }

    fn needs_animation_frame(&self) -> bool {
        self.needs_animation_frame_at(ui::time::now())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.wake_deadline_sources(ui::time::now())
            .into_iter()
            .filter_map(|(_, deadline)| deadline)
            .chain(self.agent_markers.deadline())
            .chain(
                self.agent_actions
                    .values()
                    .filter_map(agent_actions::AgentActions::deadline),
            )
            .chain(self.agent_quick_composer.next_wake_deadline())
            .chain(self.cursor_chat_deadline())
            .chain(self.person_quick_composer.next_wake_deadline())
            .chain(self.person_quick_composer.deadline())
            .chain(self.bar_tooltip.reveal_at())
            .min()
    }
}

/// The kept workspaces' apps are mounted like the shown one's, so every step
/// of closing the session reaches them too, after the widget children.
impl ui::component_lifecycle::ComponentLifecycle for ShellOverlayHost {
    fn unsaved_documents(
        &self,
        doc: &automerge::AutoCommit,
    ) -> Vec<ui::close_admission::UnsavedDocument> {
        let mut out = Vec::new();
        self.for_each_child(&mut |child| {
            out.extend(ui::component_lifecycle::unsaved_documents(child, doc));
        });
        for kept in self.parked_workspaces.contents() {
            out.extend(ui::component_lifecycle::unsaved_documents(kept, doc));
        }
        out
    }

    fn checkpoint_components(&mut self) -> Vec<String> {
        let mut errors: Vec<String> = self
            .children_mut()
            .into_iter()
            .flat_map(ui::component_lifecycle::checkpoint_components)
            .collect();
        for kept in self.parked_workspaces.contents_mut() {
            errors.extend(ui::component_lifecycle::checkpoint_components(kept));
        }
        errors
    }

    fn finish_admitted_close(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let mut reports: Vec<_> = self
            .children_mut()
            .into_iter()
            .flat_map(ui::component_lifecycle::finish_admitted_close)
            .collect();
        for kept in self.parked_workspaces.contents_mut() {
            reports.extend(ui::component_lifecycle::finish_admitted_close(kept));
        }
        reports
    }

    fn close_components(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let mut reports: Vec<_> = self
            .children_mut()
            .into_iter()
            .flat_map(ui::component_lifecycle::close_components)
            .collect();
        // Closed content is not kept: nothing would put it back usable.
        reports.extend(parked_workspaces::close(self.parked_workspaces.drain()));
        reports
    }

    fn take_component_close_reports(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        let mut reports = std::mem::take(&mut self.desktop_close_reports);
        for child in self.children_mut() {
            reports.extend(ui::component_lifecycle::take_component_close_reports(child));
        }
        for kept in self.parked_workspaces.contents_mut() {
            reports.extend(ui::component_lifecycle::take_component_close_reports(kept));
        }
        reports
    }
}

impl ShellOverlayHost {
    #[allow(
        clippy::too_many_lines,
        reason = "event routing stays linear so modal gates retain their security-sensitive order"
    )]
    fn route_window_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Motion follows the input: what this event asks to open or close
        // lands at once from a key, and animates from a click.
        if let Some(source) = ui::motion::source_of(event) {
            self.input_source = source;
        }
        // The toolkit's own menus and popovers inside the shell follow it too.
        ui::motion::note_input(event);
        // Focus rings show for keyboard focus only: a click hides them.
        ui::focus_visible::note(event);
        // MouseInput carries no coordinates. Keep the host's pointer cache
        // current before any modal can return early, because onboarding and
        // the other compositor-owned gates hit-test the following button edge
        // against this value. The normal desktop path used to do this only
        // below the onboarding return, making a real DRM pointer visibly move
        // while every onboarding click was evaluated without a position.
        self.track_cursor_position(event, ctx.scale_factor);
        self.observe_window_focus();
        // Picking what to report takes every event: nothing underneath
        // reacts to a pick, and Escape leaves.
        if self.route_feedback_picker_event(event, ctx.modifiers.state()) {
            *ctx.handled = true;
            return;
        }
        let people_open = self.overlay_flags().collaborators;
        if self
            .collaborators_overlay
            .consume_sequence_edge(event, people_open)
        {
            *ctx.handled = true;
            return;
        }

        if self.consume_workspace_rename_submit_edge(event) {
            *ctx.handled = true;
            return;
        }
        if !self.overlay_flags().collaborators
            && self.route_collaborators_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }
        // The capture tool and the screenshot editor take a finger as the
        // pointer held down, which the compositor sends them through the
        // pointer path; their raw touch is left unclaimed for it to do so.
        if matches!(event, WindowEvent::Touch(_)) && self.touch_works_as_held_pointer() {
            return;
        }
        // Print Screen has an explicit arbitration policy. It must run before
        // OOBE, portal and consent gates so Capture Studio can temporarily
        // own input above those preserved workflows instead of being consumed
        // by whichever modal was first in the dispatch chain.
        if self.handle_print_screen_priority(event, ctx.modifiers.state(), ctx.handled) {
            return;
        }
        // Capture Studio is deliberately above every ordinary session modal:
        // it lets people capture a broken setup, chooser, or permission
        // surface, then returns them to the intact workflow on Escape.
        if self.capture_studio.is_open() {
            if let Some(capture_node) = self.capture_node {
                self.capture_studio
                    .handle_event(event, &mut ctx.reborrow(ctx.path, capture_node));
            }
            if !self.capture_studio.is_open()
                && let Some(chrome) = self.chrome.as_ref()
            {
                chrome.borrow_mut().overlays.capture = false;
            }
            // While a window is being chosen, the desktop's own navigation
            // (moving focus with the keys, scrolling the strip) is how one
            // is chosen: what the tool leaves unhandled goes on to it.
            if *ctx.handled
                || (self.capture_studio.modal_capture().is_some()
                    && !self.capture_studio.lets_desktop_navigation_through())
            {
                return;
            }
        }
        // A Bluetooth pairing question owns input until it is answered:
        // nothing behind it may be clicked into agreeing, and no desktop
        // shortcut runs while it waits.
        if self.onboarding.is_none()
            && self
                .pairing
                .handle_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            self.bump_chrome();
            return;
        }
        // A display tool's change waits for Keep or Revert, possibly with a
        // screen blanked: the answer comes before any other surface.
        if self.route_layer_band(
            Band::Session,
            event,
            &mut ctx.reborrow(ctx.path, ctx.node_id),
        ) {
            return;
        }
        // A compositor-owned sheet raised during first-use setup (an
        // administrator prompt, a network secret) is drawn above onboarding
        // and is modal over it: its keys and clicks must reach the sheet,
        // not the setup control that still holds focus underneath. Focus is
        // left untouched, so onboarding resumes where it was once the sheet
        // is answered or dismissed.
        if self.onboarding.is_some()
            && self.route_portal_dialog_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }
        if let Some(onboarding) = self.onboarding.as_mut() {
            // A first-login ceremony is the first child of its dedicated
            // tree, but a tour reopened from Search is appended after the
            // live desktop so that the desktop remains visible beneath it.
            // Route against the node captured when onboarding was built;
            // using `children.first()` here sends contextual-tour pointer
            // events through the desktop's taffy subtree instead.
            let child_node = self.onboarding_node.unwrap_or(ctx.node_id);
            onboarding.handle_event(event, &mut ctx.reborrow(ctx.path, child_node));
            // First-login is a modal machine surface. Even an event the
            // current control does not consume must never fall through to
            // hidden desktop chrome. Leave the one standard Tab path
            // unclaimed, though: FrameBuilder owns global focus traversal,
            // and must be allowed to advance between the onboarding
            // controls. Enter/Space similarly stay unclaimed when the
            // focused widget did not consume them: native FrameBuilder owns
            // the standard focused-control activation fallback that
            // `ui::App` normally supplies. Modified Tab remains a
            // shell/application chord and is consumed here with every other
            // event.
            let is_unmodified_tab = matches!(
                event,
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && matches!(event.logical_key, Key::Named(NamedKey::Tab))
                        && !ctx.modifiers.state().control_key()
                        && !ctx.modifiers.state().alt_key()
                        && !ctx.modifiers.state().super_key()
            );
            let is_unmodified_activation = matches!(
                event,
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && matches!(
                            event.logical_key,
                            Key::Named(NamedKey::Enter | NamedKey::Space)
                        )
                        && !ctx.modifiers.state().control_key()
                        && !ctx.modifiers.state().alt_key()
                        && !ctx.modifiers.state().super_key()
            );
            if !is_unmodified_tab && !is_unmodified_activation {
                *ctx.handled = true;
            }
            return;
        }
        // Split into named gates and dispatch stages — was one
        // 1591-line function. Each helper (in the other `impl
        // ShellOverlayHost` block) covers one gate or stage, called in
        // the same order this ran inline; a `bool` return means "this
        // gate/stage fully resolved the event, stop here", exactly where
        // the original inline code hit a bare `return`.
        if let WindowEvent::Touch(touch) = event {
            let shell_owned_before = self.touch_gestures.shell_owns_current();
            let phase = match touch.phase {
                winit::event::TouchPhase::Started => ui::gesture::GesturePhase::Begin,
                winit::event::TouchPhase::Moved => ui::gesture::GesturePhase::Update,
                winit::event::TouchPhase::Ended => ui::gesture::GesturePhase::End,
                winit::event::TouchPhase::Cancelled => ui::gesture::GesturePhase::Cancel,
            };
            let point = Point::new(
                touch.location.x / ctx.scale_factor,
                touch.location.y / ctx.scale_factor,
            );
            let events = self.touch_gestures.on_touch(touch.id, point, phase);
            let shell_owned = shell_owned_before || self.touch_gestures.shell_owns_current();
            let mut claimed = false;
            for gesture in events {
                use ui::gesture::{TouchGestureEvent, TouchSwipeDirection};
                match gesture {
                    TouchGestureEvent::Swipe {
                        fingers: 3,
                        direction: TouchSwipeDirection::Left,
                    } => claimed |= self.step_focus_along_strip(1),
                    TouchGestureEvent::Swipe {
                        fingers: 3,
                        direction: TouchSwipeDirection::Right,
                    } => claimed |= self.step_focus_along_strip(-1),
                    // Three fingers are strips, four workspaces: the touchpad's
                    // levels (`backend::drm::gestures`).
                    TouchGestureEvent::Swipe {
                        fingers,
                        direction: direction @ (TouchSwipeDirection::Up | TouchSwipeDirection::Down),
                    } if fingers >= 3 => {
                        let step = if direction == TouchSwipeDirection::Up {
                            -1
                        } else {
                            1
                        };
                        if let Some(chrome) = self.chrome.as_ref() {
                            let mut chrome = chrome.borrow_mut();
                            claimed |= if fingers == 3 {
                                chrome.switch_strip(step)
                            } else {
                                chrome.switch_workspace(step)
                            };
                        }
                    }
                    TouchGestureEvent::Pinch {
                        fingers: 3..,
                        scale,
                        ..
                    } if scale <= 0.8 => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut state = chrome.borrow_mut();
                            state.overlays.overview = true;
                            state.bump();
                            drop(state);
                            self.latch_overview_transition_from(ui::tokens::MotionSource::Gesture);
                            claimed = true;
                        }
                    }
                    TouchGestureEvent::Pinch {
                        fingers: 3..,
                        scale,
                        ..
                    } if scale >= 1.25 => {
                        if let Some(chrome) = self.chrome.clone() {
                            let mut state = chrome.borrow_mut();
                            state.overlays.overview = false;
                            state.bump();
                            drop(state);
                            self.latch_overview_transition_from(ui::tokens::MotionSource::Gesture);
                            claimed = true;
                        }
                    }
                    _ => {}
                }
            }
            // A swipe down from a display's top edge reveals what hides
            // there: the top bar the person hides, a fullscreen window's bar.
            if !shell_owned {
                claimed |= self.top_edge_swipe(touch.id, point, touch.phase);
            }
            if claimed || shell_owned {
                *ctx.handled = true;
                return;
            }
        }

        if let (Some(markup), Some(markup_node)) =
            (self.screenshot_markup.as_mut(), self.screenshot_markup_node)
        {
            markup.handle_event(event, &mut ctx.reborrow(ctx.path, markup_node));
            // This is a real editing workspace, not a translucent status
            // overlay: drawing and shortcuts must never leak to the app below.
            *ctx.handled = true;
            return;
        }

        if self
            .capture_mirrors
            .iter()
            .any(|(_, mirror)| mirror.wants_pointer_capture())
            && self.route_capture_mirror_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }

        if self.route_portal_dialog_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }

        if self.route_layer_band(Band::Chord, event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }

        // A drag that began inside an app keeps pointer ownership until its
        // release, even after the surface has crossed outside the app/window.
        // Route this before shell overlays so the boundary frame cannot steal
        // the pointer and leave the live surface stranded or one event behind.
        if self.content.wants_pointer_capture() {
            if let Some(content_node) = self.content_node {
                self.content
                    .handle_event(event, &mut ctx.reborrow(ctx.path, content_node));
                // A right-click that lands while a drag still owns the
                // pointer is a request like any other; opened here, not on
                // whichever later event happens to run the fallback.
                self.harvest_pending_tab_context_menus();
            }
            return;
        }

        // The menu on something the desktop draws sits over everything it
        // can be opened on, the file chooser's sheet included.
        if self.layers.tab_menu.is_open_on_thing()
            && self.route_layer_band(
                Band::Popover,
                event,
                &mut ctx.reborrow(ctx.path, ctx.node_id),
            )
        {
            return;
        }
        if self.open_thing_menu_on_press(event) {
            *ctx.handled = true;
            return;
        }
        if self.route_modal_absorbing_overlays(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }
        // Menus and flyouts sit below the genuinely modal sheets above (a
        // portal answer still wins) and above every other overlay and the
        // windows.
        if self.route_layer_band(
            Band::Popover,
            event,
            &mut ctx.reborrow(ctx.path, ctx.node_id),
        ) {
            return;
        }
        if self.route_capture_mirror_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }
        if self.route_bar_popover_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }
        if self.agent_quick_composer.is_open()
            && let Some(node) = self.agent_quick_composer_node
        {
            self.agent_quick_composer
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled || self.agent_quick_composer.modal_capture().is_some() {
                return;
            }
        }
        if self.person_quick_composer.is_open()
            && let Some(node) = self.person_quick_composer_node
        {
            self.person_quick_composer
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled || self.person_quick_composer.modal_capture().is_some() {
                return;
            }
        }
        if !self.person_cursor_controls_suppressed() {
            self.person_markers.route_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        self.agent_markers.route_event(event, ctx);
        if *ctx.handled {
            return;
        }
        if let Some(chrome) = self.chrome.clone() {
            let reading = {
                let st = chrome.borrow();
                st.overlays.quick_settings
                    && matches!(
                        st.quick_settings.current_level(),
                        crate::shell::quick_settings::QsLevel::Notifications
                    )
                    && self.notification_center.is_reading()
            };
            if reading
                && self
                    .notification_center
                    .reader_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
            {
                chrome.borrow_mut().bump();
            }
            if *ctx.handled {
                return;
            }
        }
        if self.route_qs_pointer_capture(event, ctx.scale_factor, ctx.handled) {
            return;
        }
        if self.route_qs_root_scroll(event, ctx.scale_factor, *ctx.modifiers, ctx.handled) {
            return;
        }
        if let WindowEvent::MouseWheel { delta, .. } = event
            && (self.handle_qs_notification_wheel(*delta, ctx.scale_factor)
                || self.handle_qs_clipboard_wheel(*delta, ctx.scale_factor)
                || self.handle_qs_device_fabric_wheel(*delta, ctx.scale_factor))
        {
            *ctx.handled = true;
            return;
        }
        if Self::is_pointer_event(event)
            && self.route_wifi_password_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }
        if (Self::is_pointer_event(event) || matches!(event, WindowEvent::Focused(false)))
            && self.route_people_overlay_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }
        if (Self::is_pointer_event(event) || matches!(event, WindowEvent::Focused(false)))
            && self.route_collaborators_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }
        if self.route_absorbable_pointer_and_toast_click(
            event,
            &mut ctx.reborrow(ctx.path, ctx.node_id),
        ) {
            return;
        }
        if self.route_permission_prompt_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }
        if self.route_dropped_file_event(event, ctx.handled) {
            return;
        }
        // An opening pointer/accessibility/search action can be followed by
        // a key before the next draw. Observe the authoritative chrome edge
        // here so every entry point starts keyboard traversal from the same
        // visible first control rather than a retained selection.
        self.reconcile_quick_settings_open_edge(self.overlay_flags().quick_settings);
        // The shell keyboard helper intentionally handles only pressed keys;
        // absorb releases here as well so a key held across a QS opening
        // cannot be delivered to the client behind the modal.
        if self.overlay_flags().quick_settings
            && matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: KeyEvent {
                        state: ElementState::Released,
                        ..
                    },
                    ..
                }
            )
        {
            *ctx.handled = true;
            return;
        }
        // Cursor chat takes every key while it is open, and the `/` the
        // keyboard route kept from the window opens it. Talking is not input
        // to the followed window, so it does not end following.
        if self.route_cursor_chat_event(event, ctx.modifiers.state()) {
            *ctx.handled = true;
            return;
        }
        // The first local key is the handoff edge out of Follow mode. Consume
        // that same event after releasing the peer so a shell chord (for
        // example Super+W) cannot also mutate the local desktop.
        if self.release_follow_for_local_key(event) {
            *ctx.handled = true;
            return;
        }
        // A window's location edited in its bar has the keyboard, ahead of
        // the chords for the same reason as the password below.
        if self.route_window_location_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }
        // So does a tab's name field, while a tab is named in its pill.
        if self.route_tab_rename_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            return;
        }
        // Ahead of every chord: a password can contain any character, and
        // a `k` in one must not open search instead.
        if !Self::is_pointer_event(event)
            && !self.keyboard_held_ahead_of_wifi_password()
            && self.route_wifi_password_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }
        if !Self::is_pointer_event(event)
            && !matches!(event, WindowEvent::Focused(false))
            && self.route_people_overlay_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }
        if !Self::is_pointer_event(event)
            && self.route_collaborators_event(event, &mut ctx.reborrow(ctx.path, ctx.node_id))
        {
            return;
        }
        if self.route_window_search_input(event, &mut ctx.reborrow(ctx.path, ctx.node_id)) {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::Ime(winit::event::Ime::Commit(text)) = event
            && self.commit_into_chrome_field(text)
        {
            *ctx.handled = true;
            return;
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    repeat,
                    ..
                },
            ..
        } = event
            && self.handle_shell_keyboard_event(
                logical_key,
                *ctx.modifiers,
                *repeat,
                &ctx.state,
                ctx.path,
                ctx.handled,
            )
        {
            return;
        }
        // Tilo-reserved shell chords above always win.  All remaining
        // physical keys are offered to the portal registry, which can only
        // match an accelerator installed after a reviewed, authenticated
        // GlobalShortcuts grant. Over a focused client the compositor's
        // router has already claimed the press; this covers shell chrome
        // holding focus. Pressing twice is idempotent, and the release sends
        // `Deactivated` exactly once whichever path saw it first.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    physical_key,
                    state,
                    ..
                },
            ..
        } = event
        {
            let grants = crate::portal::global_shortcuts::ShellGlobalShortcutsService::global();
            let claimed = match state {
                ElementState::Pressed => grants.press(physical_key, *ctx.modifiers),
                ElementState::Released => grants.release(physical_key),
            };
            if claimed {
                *ctx.handled = true;
                return;
            }
        }
        // Desk widgets above the windows: a press on a showing one is
        // theirs, before the palette reads it as a click away.
        if self.route_desk_event(event) {
            *ctx.handled = true;
            return;
        }
        if self.handle_shell_mouse_click_event(event, ctx.handled) {
            return;
        }
        if self.handle_shell_overview_pointer_event(event, ctx.scale_factor, ctx.handled) {
            return;
        }

        // No children means taffy never laid this node out this frame —
        // there is nothing to route the event to.
        let Ok(children) = ctx.taffy.children(ctx.node_id) else {
            return;
        };
        if self.route_palette_event(
            event,
            &mut ctx.reborrow(ctx.path, ctx.node_id),
            children.get(1).copied(),
        ) {
            return;
        }
        if self.followed_peer.is_some() && matches!(event, WindowEvent::MouseWheel { .. }) {
            // Release before forwarding so this input changes the local view
            // immediately rather than being overwritten by the next peer
            // view-presence frame.
            self.stop_following_for_local_input();
        }
        self.dispatch_pointer_and_content_event(
            event,
            &mut ctx.reborrow(ctx.path, ctx.node_id),
            &children,
        );
    }
}
