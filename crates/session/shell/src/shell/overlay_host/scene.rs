//! Scene sections, animation demand and which overlays are open.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    pub(crate) fn content_animating_leaves(&self) -> Vec<String> {
        crate::shell::animation_diagnostics::animating_leaves(self.content.as_ref())
    }

    /// Stable, allocation-free diagnostics for animation demand. Each bit is
    /// one independent source so the compositor can identify a stuck source
    /// without changing the scheduling decision itself.
    pub(crate) fn animation_demand_bits(&self, now: Instant) -> u32 {
        crate::shell::animation_diagnostics::host_bits(
            &crate::shell::animation_diagnostics::HostDemandInputs {
                onboarding: self.onboarding.is_some(),
                onboarding_animating: self
                    .onboarding
                    .as_ref()
                    .is_some_and(Widget::needs_animation_frame),
                capture_studio: self.capture_studio.needs_animation_frame(),
                root_transition: self.overview_zoom_moving(now),
                workspace_switch: self.workspace_switch_holding_at(now),
                quick_settings: self.qs_motion.is_animating(now)
                    || self.qs_root.is_animating(now)
                    || self.qs_root_scroll_fading(),
                workspace_settings: self.workspace_settings_overlay.is_animating(now),
                palette: self.palette_transition.is_moving(now),
                power_menu: self.layers.power_menu.is_animating(now),
                room_activity: self.room_activity.needs_animation_frame(now),
                follow: self.follow_transition.is_moving(now),
                room_replay: self.room_activity.replay_is_playing(),
                projection: self.projection_overlay.needs_animation_frame(now),
                content: self.content.needs_animation_frame(),
                strip_switcher: self.strip_switcher.needs_animation_frame(now),
                capture_mirror: self
                    .capture_mirrors
                    .iter()
                    .any(|(_, mirror)| mirror.needs_animation_frame()),
                screenshot_markup: self
                    .screenshot_markup
                    .as_ref()
                    .is_some_and(Widget::needs_animation_frame),
                notification: self.chrome.as_ref().is_some_and(|chrome| {
                    self.notification_stack
                        .needs_animation_frame(&ChromeNotifications::new(chrome))
                }),
            },
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the ordered section recording mirrors the established draw pass exactly"
    )]
    /// Record the shell in ordered sections for retained damage. The first and last sections are
    /// intentionally opaque: a caller must establish exact encoded equality before it may reuse
    /// pixels beneath a bounded section.
    pub fn draw_scene_sections(
        &mut self,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) -> Box<ShellSceneSections> {
        self.motion.set(theme.motion());
        // Boxed so each section keeps its address while the caller draws
        // into it too: a floating surface's backdrop layer is found by the
        // scene it is drawn into.
        let mut sections = Box::new(ShellSceneSections {
            backdrop: Scene::new(),
            topbar: Scene::new(),
            workspace: Scene::new(),
            workspace_overlays: Scene::new(),
            topbar_bounds: None,
            quick_settings: Scene::new(),
            above_quick_settings: Scene::new(),
            quick_settings_bounds: None,
            panned: PannedSections::default(),
        });
        sections.register_backdrop_layers();
        sections.track_damage_parts();
        let ShellSceneSections {
            backdrop,
            topbar,
            workspace,
            workspace_overlays,
            quick_settings,
            above_quick_settings,
            panned: panned_slot,
            ..
        } = &mut *sections;
        let contextual_tour = self
            .onboarding
            .as_ref()
            .is_some_and(crate::onboarding_view::OnboardingView::presents_desktop_context);
        if !contextual_tour && let Some(onboarding) = self.onboarding.as_mut() {
            onboarding.draw(backdrop, transform, text_renderer, theme, presence);
            return sections;
        }

        let flags = self.overlay_flags();
        let unbaked = ui::retained_present::unbake_scroll();
        // Strips drawn in canvas coordinates are each drawn on their own:
        // the column skips them and they are drawn below, top to bottom.
        let apart = if unbaked {
            self.rows_on_screen()
        } else {
            Vec::new()
        };
        for &(_, row) in &apart {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor; `&mut self` makes the access exclusive.
            unsafe { (&mut *row).set_drawn_apart(true) };
        }
        let topbar_bounds = self.draw_workspace_sections(
            backdrop,
            topbar,
            workspace,
            transform,
            text_renderer,
            theme,
            presence,
        );
        let mut panned = PannedSections::default();
        for &(strip, row) in &apart {
            let mut scene = Scene::new();
            // A blurred surface drawn into a strip is recorded as drawn into
            // the workspace, so the frame producer sees it.
            ui::backdrop::layer_scene(&scene, backdrop_layer::WORKSPACE);
            // Each window group is a damage part of the strip's drawing.
            let ((), parts) = ui::scene_parts::record(&mut scene, |scene| {
                // SAFETY: as above.
                unsafe {
                    let row = &mut *row;
                    row.draw_apart(scene, text_renderer, theme, presence);
                    row.set_drawn_apart(false);
                }
            });
            panned.strips.push((strip, scene));
            panned.strip_parts.push(parts);
        }
        ui::backdrop::layer_scene(workspace, backdrop_layer::WORKSPACE);
        if unbaked {
            self.draw_real_peers(
                workspace_overlays,
                transform,
                text_renderer,
                theme,
                presence,
            );
            self.draw_desktop_notices(
                workspace_overlays,
                transform,
                text_renderer,
                theme,
                presence,
            );
        } else {
            self.draw_desktop_extras(workspace, transform, text_renderer, theme, presence);
        }
        let rows: Vec<_> = self
            .group_row_ptrs
            .iter()
            .map(|(&id, &row)| (id, row))
            .collect();
        for (strip, row) in rows {
            // A strip drawn in canvas coordinates is panned by the presenter;
            // surfaces floating over its windows move with it.
            // SAFETY: identical sequential traversal to `Widget::draw` above.
            let shift = unsafe { (&*row).canvas_shift() };
            let row_transform = transform.pre_translate(vello::kurbo::Vec2::new(shift, 0.0));
            let scene = match panned.strips.iter_mut().find(|(id, _)| *id == strip) {
                Some((_, scene)) => scene,
                None => &mut *workspace,
            };
            let before = scene.encoding().draw_tags.len();
            // SAFETY: identical sequential traversal to `Widget::draw` above.
            unsafe {
                ui::widget::FloatingSurfaceWidgetExt::draw_floating_surfaces(
                    &mut *row,
                    scene,
                    row_transform,
                    text_renderer,
                    theme,
                    presence,
                );
            }
            panned.floating_drawn |= unbaked && scene.encoding().draw_tags.len() != before;
        }
        *panned_slot = panned;
        let overview_scene = if ui::retained_present::unbake_scroll() {
            &mut *workspace_overlays
        } else {
            &mut *workspace
        };
        self.draw_overview_and_ws_settings_overlays(
            overview_scene,
            transform,
            text_renderer,
            theme,
            flags,
        );
        self.prepare_quick_settings_transition(theme, flags);
        self.prepare_strip_indicators(Instant::now());
        self.sync_top_bar_presence(Instant::now());
        let quick_settings_transition_visible = self.qs_motion.is_visible(ui::time::now());
        // The panel, the ribbon and the rail are each a damage part of the
        // section: a change to one repaints that one.
        let mut quick_settings_bounds = None;
        overlay_part(
            quick_settings,
            "shell.quick-settings",
            transform.transform_rect_bbox(self.rect),
            |scene| {
                quick_settings_bounds =
                    self.draw_quick_settings_dropdown(scene, transform, text_renderer, theme);
                quick_settings_bounds.map(|bounds| vec![transform.transform_rect_bbox(bounds)])
            },
        );
        // Keep the strip in the retained QS section even while the dropdown
        // is closed. Opening QS then changes one bounded section rather than
        // removing chrome from the opaque workspace section, which preserves
        // sparse damage eligibility without changing the final scene order.
        self.draw_strip_chrome(
            quick_settings,
            transform,
            text_renderer,
            theme,
            flags,
            quick_settings_transition_visible,
        );
        let quick_settings_bounds = quick_settings_bounds
            .into_iter()
            .chain(self.strip_chrome_bounds(flags, quick_settings_transition_visible))
            .reduce(|current, rect| current.union(rect));
        // Every overlay above Quick Settings is a damage part of its own,
        // there whether or not the overlay is open: opening, animating or
        // closing one repaints that overlay, not the desktop.
        let desktop = transform.transform_rect_bbox(self.rect);
        let outputs = self.layer_outputs();
        overlay_part(above_quick_settings, "shell.power-menu", desktop, |scene| {
            self.layers.draw(
                LayerId::PowerMenu,
                &mut LayerPaint {
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                    geometry: self.layer_geometry(&outputs),
                    thumbnails: &mut self.window_previews,
                },
            );
            None
        });
        overlay_part(
            above_quick_settings,
            "shell.collaborators",
            desktop,
            |scene| {
                self.draw_collaborators_dropdown(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                    flags,
                );
                None
            },
        );
        overlay_part(
            above_quick_settings,
            "shell.room-activity",
            desktop,
            |scene| {
                self.draw_room_activity_sheet(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    presence,
                    flags,
                );
                None
            },
        );
        overlay_part(above_quick_settings, "shell.projection", desktop, |scene| {
            self.draw_projection_sheet(scene, transform, text_renderer, theme, presence, flags);
            None
        });
        self.draw_top_overlays(
            above_quick_settings,
            transform,
            text_renderer,
            theme,
            presence,
        );
        overlay_part(above_quick_settings, "shell.tour", desktop, |scene| {
            if contextual_tour && let Some(onboarding) = self.onboarding.as_mut() {
                onboarding.draw(scene, transform, text_renderer, theme, presence);
                self.draw_portal_dialog_sheet(scene, transform, text_renderer, theme);
            }
            None
        });
        overlay_part(above_quick_settings, "shell.pairing", desktop, |scene| {
            self.pairing
                .draw(scene, transform, text_renderer, theme, presence);
            None
        });
        overlay_part(
            above_quick_settings,
            "shell.capture-studio",
            desktop,
            |scene| {
                // While it chooses, the picker paints over the held desktop
                // in `draw_capture_overlays` instead of into it.
                if !self.capture_studio.is_choosing_screenshot() {
                    self.capture_studio
                        .draw(scene, transform, text_renderer, theme, presence);
                }
                None
            },
        );
        sections.topbar_bounds = topbar_bounds;
        sections.quick_settings_bounds = quick_settings_bounds;
        sections
    }

    /// Whether overlay `id` is on screen, for a test reading the host.
    #[cfg(test)]
    pub(crate) fn overlay_shown_for_test(&self, id: LayerId) -> bool {
        self.overlay_is_open(id, self.overlay_flags())
    }

    /// Open the palette empty and focused, as its chord does.
    #[cfg(test)]
    pub(crate) fn open_palette_for_test(&mut self) {
        self.palette_visible = true;
        self.palette.set_focus(true);
        self.palette.set_query("");
        self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
    }

    /// The open overlays, topmost first — the order the synthetic
    /// accessibility tree publishes them in.
    pub(super) fn accessible_overlay_order(&self) -> Vec<LayerId> {
        let flags = self.overlay_flags();
        LayerId::ALL
            .into_iter()
            .filter(|&id| self.overlay_is_open(id, flags))
            .collect()
    }

    /// Whether overlay `id` is on screen. The stack answers for its own
    /// layers; the rest are still tracked by the host.
    pub(super) fn overlay_is_open(
        &self,
        id: LayerId,
        flags: crate::shell::chrome_state::OverlayFlags,
    ) -> bool {
        if let Some(layer) = self.layers.layer(id) {
            return layer.is_shown();
        }
        match id {
            LayerId::DisplayLayouts => flags.projection,
            // The palette has a complete semantic tree (query, result list,
            // and focused result).  It is drawn as a top overlay, so publish
            // it from the same ordered synthetic tree as every other overlay
            // rather than leaving screen readers with only the obscured
            // workspace.
            LayerId::CommandPalette => self.palette_visible,
            LayerId::MemberSheet => self.workspace_settings_overlay.member_sheet_is_open(),
            LayerId::Overview => flags.overview,
            LayerId::WorkspaceSettings => flags.ws_settings,
            LayerId::Collaborators => flags.collaborators,
            LayerId::RoomActivity => self.chrome.as_ref().is_some_and(|chrome| {
                crate::shell::tab_binding::active_workspace_activity_tab(&chrome.borrow().tree())
                    .is_some()
            }),
            // Every page publishes: the root, each drill-in, and the Wi-Fi
            // password prompt that replaces the network list.
            LayerId::QuickSettings => flags.quick_settings && self.chrome.is_some(),
            LayerId::NowPlaying => self.now_playing_published(),
            LayerId::ShortcutHelp => flags.shortcut_help,
            // Stacked; answered above.
            LayerId::DisplayConfirmation
            | LayerId::WindowSwitcher
            | LayerId::EmojiPicker
            | LayerId::TabContextMenu
            | LayerId::PowerMenu
            | LayerId::Dictation => false,
        }
    }

    /// Bring the overview's zoom in line with `open`: a change of
    /// intent starts the windows flying from where they are drawn, timed for
    /// `source` (a key keeps the flight as `fast` spatial context, a click
    /// or a pinch is a whole-view change). Returns the zoom's progress now.
    pub(super) fn sync_overview_zoom(
        &mut self,
        open: bool,
        source: ui::tokens::MotionSource,
        motion: ui::tokens::EffectiveMotion,
    ) -> f64 {
        let now = ui::time::now();
        if open != self.overview_zoom.is_open() {
            self.overview_zoom
                .show(open, ui::motion::Zoom::OVERVIEW, motion, source, now);
            self.request_compositor_transition_paints();
            // Reseed from the current room so the first arrow press moves
            // one step from here, not from a stale previous visit.
            self.reseed_overview_selection(open);
            self.overview_scroll_y = 0.0;
            self.overview_naming = None;
        }
        self.overview_zoom.progress(now)
    }

    /// Latch the overview's zoom on the change that asked for it, before the
    /// compositor samples animation demand. Starting only in `update` left
    /// Super+Tab as a timer the loop parks past (W16). The input the toolkit
    /// handled last says how it moves.
    pub(crate) fn latch_overview_transition(&mut self) {
        self.latch_overview_transition_from(ui::motion::input_source());
    }

    /// [`Self::latch_overview_transition`] for a change whose input is known:
    /// Super+Tab, a pinch, a click on "Overview".
    pub fn latch_overview_transition_from(&mut self, source: ui::tokens::MotionSource) {
        // Do not read `ui::host::environment()` here: that is a process
        // input, and fixture pumps forbid ambient starts. The motion the
        // overview was last laid out for says whether motion is reduced.
        let open = self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().overlays.overview);
        let motion = self.overview_layout_theme.motion();
        let _ = self.sync_overview_zoom(open, source, motion);
    }

    /// The overview's zoom is flying.
    pub(super) fn overview_zoom_moving(&self, now: Instant) -> bool {
        self.overview_zoom.is_moving(now)
    }

    /// Whether the workspace-switch frame run is still in flight.
    /// Intermediate frames must not upload the desktop again.
    pub(crate) fn workspace_switch_holding(&self) -> bool {
        self.workspace_switch_holding_at(Instant::now())
    }

    pub(super) fn workspace_switch_holding_at(&self, now: Instant) -> bool {
        self.workspace_switch_until.is_some_and(|until| now < until)
    }

    /// A key opened or closed the overview: the windows fly at `fast`, as
    /// spatial context, and input acts on the new state at once.
    pub(super) fn latch_keyboard_overview(&mut self) {
        self.latch_overview_transition_from(ui::tokens::MotionSource::Keyboard);
    }

    /// Keep the compositor painting for the switch's duration. The
    /// desktop content is already swapped; this is the frame run, not a
    /// second slide implementation. Reduced motion still runs the frames:
    /// snapping the curve made the close flush both scanouts.
    /// Call on the chord, not only from `update`: demand is sampled first.
    ///
    /// A switch is whole-view motion (`slow`); one a shortcut or action
    /// started keeps only a `fast` direction cue.
    pub(crate) fn start_workspace_switch_frames(&mut self, source: ui::tokens::MotionSource) {
        let motion = ui::tokens::Motion::default();
        let duration = motion.context(source, motion.durations.slow());
        self.workspace_switch_source = source;
        let now = Instant::now();
        self.workspace_switch_until = now.checked_add(duration);
        self.workspace_switch_latency.chord(now);
        self.request_compositor_transition_paints();
    }

    /// The switch latency tracker, which the frame loop tells about the
    /// frames it queues and retires (`WORKSPACE-SWITCH`).
    pub(crate) fn workspace_switch_latency_mut(
        &mut self,
    ) -> &mut crate::compositor::switch_latency::SwitchLatency {
        &mut self.workspace_switch_latency
    }

    /// Whether a transition still owes a present. The clock may have been
    /// deferred; this is the signal that keeps the frame run alive.
    pub(crate) fn transition_present_pending(&self) -> bool {
        self.overview_zoom_moving(ui::time::now()) || self.workspace_switch_holding()
    }

    /// Give `by` back to a running overview or workspace switch. Call with
    /// the park spent on a full presenter, before the next sample of the clock.
    pub(crate) fn defer_running_transitions(&mut self, by: Duration) {
        if by.is_zero() {
            return;
        }
        self.overview_zoom.defer(by);
        if let Some(until) = self.workspace_switch_until.as_mut() {
            *until = until.checked_add(by).unwrap_or(*until);
        }
    }

    pub(super) fn request_compositor_transition_paints(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            chrome
                .borrow_mut()
                .request_transition_paints(TRANSITION_PAINT_RUN);
        }
    }

    /// Mark the shared chrome state changed so its readers repaint.
    pub(super) fn bump_chrome(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            chrome.borrow_mut().bump();
        }
    }

    #[must_use]
    pub fn with_chrome(mut self, chrome: Chrome) -> Self {
        chrome.borrow_mut().peer_sync_enabled = self.peer_sync_enabled;
        self.projection_overlay
            .bind_shared_display_authority(chrome.clone());
        self.chrome = Some(chrome);
        self
    }
}

impl ShellOverlayHost {
    pub(super) fn wake_deadline_sources(
        &self,
        now: Instant,
    ) -> [(&'static str, Option<Instant>); 19] {
        // Overview and workspace-switch motion is animation demand, so the
        // frame clock paces it. A 1 ms deadline here woke the loop on every
        // millisecond and still painted nothing until cadence was due.
        [
            (
                "private-feedback",
                self.chrome.as_ref().and_then(|chrome| {
                    chrome
                        .borrow()
                        .hosted_inputs
                        .in_process()
                        .and_then(|apps| apps.borrow().feedback_deadline())
                }),
            ),
            (
                "onboarding",
                self.onboarding
                    .as_ref()
                    .and_then(Widget::next_wake_deadline),
            ),
            (
                "people",
                self.chrome.as_ref().and_then(|chrome| {
                    if chrome.borrow().overlays.collaborators {
                        self.collaborators_overlay.next_wake_deadline()
                    } else {
                        None
                    }
                }),
            ),
            ("content", self.content.next_wake_deadline()),
            ("strip-switcher", self.strip_switcher_wake_deadline(now)),
            (
                "notification",
                self.chrome.as_ref().and_then(|chrome| {
                    self.notification_stack
                        .next_wake_deadline(&ChromeNotifications::new(chrome))
                }),
            ),
            (
                "osd",
                self.chrome
                    .as_ref()
                    .and_then(|chrome| chrome.borrow().osd)
                    .and_then(|osd| osd.next_wake_deadline_at(now)),
            ),
            (
                "acknowledgement",
                self.current_acknowledgement(now).and_then(|ack| {
                    ack.next_wake_at(now, self.acknowledgement_reduce_motion.get())
                }),
            ),
            ("now-playing", self.now_playing_wake_deadline(now)),
            (
                "quick-settings-scroll",
                self.chrome
                    .as_ref()
                    .is_some_and(|chrome| chrome.borrow().overlays.quick_settings)
                    .then(|| {
                        self.qs_root
                            .scroll_wake_deadline(&self.overview_layout_theme)
                    })
                    .flatten(),
            ),
            ("quick-settings-wifi-scan", self.qs_wifi_rescan_deadline()),
            ("window-previews", self.window_previews_wake_deadline(now)),
            (
                "strip-indicators",
                self.strip_indicators.next_wake_deadline_at(now),
            ),
            ("top-bar-reveal", self.top_bar_reveal.deadline()),
            (
                "launchpad-app",
                self.pending_launchpad_app
                    .as_ref()
                    .and_then(|_| now.checked_add(LAUNCHPAD_APP_RETRY)),
            ),
            (
                "launchpad-action",
                (!self.pending_launchpad_actions.is_empty())
                    .then(|| now.checked_add(LAUNCHPAD_ACTION_POLL))
                    .flatten(),
            ),
            ("shortcut-schedule", self.held_firings_wake_deadline(now)),
            ("desk", self.desk_wake_deadline(now)),
            ("arrival", self.arrival_line.deadline()),
        ]
    }

    /// Which of the host's sources hold a wake deadline at or before `due`;
    /// for the desktop's widget tree, the deepest widgets holding it.
    pub(crate) fn wake_deadline_owners(&self, due: Instant) -> Vec<String> {
        let limit = due
            .checked_add(crate::shell::animation_diagnostics::WAKE_MATCH_SLACK)
            .unwrap_or(due);
        let mut owners = Vec::new();
        for (name, deadline) in self.wake_deadline_sources(Instant::now()) {
            if deadline.is_none_or(|at| at > limit) {
                continue;
            }
            if name == "content" {
                owners.extend(
                    crate::shell::animation_diagnostics::wake_deadline_leaves(
                        self.content.as_ref(),
                        due,
                    )
                    .into_iter()
                    .map(|leaf| format!("content{leaf}")),
                );
            } else {
                owners.push(name.to_string());
            }
        }
        owners
    }

    /// [`Widget::needs_animation_frame`] against a caller-supplied clock, so a
    /// test can pin the same instant it started a transition at and assert
    /// about the animation rather than about how busy the machine was between
    /// two statements.
    pub(super) fn needs_animation_frame_at(&self, now: Instant) -> bool {
        // OOBE normally owns the screen, except when Capture Studio is
        // temporarily above it. Keep both animation clocks alive so opening
        // a screenshot picker during setup cannot freeze its entrance or
        // leave an opaque frame behind after Escape.
        if let Some(onboarding) = self.onboarding.as_ref() {
            return onboarding.needs_animation_frame()
                || self.capture_studio.needs_animation_frame();
        }
        // Continuous redraws while any of the overview crossfade, the
        // workspace slide-swap, quick-settings open/close, or any inner
        // widget's animation is in flight. `tick_*` clear their own flags once
        // the duration has elapsed, so at rest this returns false and the
        // event loop can go idle.
        let qs_in_flight = self.qs_motion.is_animating(now)
            || self.qs_root.is_animating(now)
            || self.qs_root_scroll_fading();
        let ws_settings_in_flight = self.workspace_settings_overlay.is_animating(now);
        let palette_in_flight = self.palette_transition.is_moving(now);
        let layers_in_flight = self.layers.is_animating(now);
        let room_activity_in_flight = self.room_activity.needs_animation_frame(now);
        let follow_in_flight = self.follow_transition.is_moving(now);
        let cursor_chat_in_flight =
            self.cursor_chat_animating(now) || self.presence_notice_moving(now);
        let room_replay_in_flight = self.room_activity.replay_is_playing();
        let capture_in_flight = self.capture_studio.needs_animation_frame();
        let projection_needs_frame = self.projection_overlay.needs_animation_frame(now)
            || self.display_identify.needs_frame(now);
        let switcher_frame = self.strip_switcher.needs_animation_frame(now);
        let mirror_frame = self
            .capture_mirrors
            .iter()
            .any(|(_, mirror)| mirror.needs_animation_frame());
        let markup_frame = self
            .screenshot_markup
            .as_ref()
            .is_some_and(Widget::needs_animation_frame);
        let notification_frame = self.chrome.as_ref().is_some_and(|chrome| {
            self.notification_stack
                .needs_animation_frame(&ChromeNotifications::new(chrome))
        });
        let osd_frame = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().osd)
            .is_some_and(|osd| osd.needs_animation_frame_at(now));
        let acknowledgement_frame = {
            let now = ui::time::now();
            self.current_acknowledgement(now)
                .is_some_and(|ack| ack.fading_at(now, self.acknowledgement_reduce_motion.get()))
        };
        self.overview_zoom_moving(now)
            || self.desk_animating(now)
            || self.strip_indicators.needs_animation_frame_at(now)
            || self.strip_ribbon_focus.is_moving(now)
            || self.strip_rail_focus.is_moving(now)
            || self.workspace_switch_holding_at(now)
            || qs_in_flight
            || ws_settings_in_flight
            || palette_in_flight
            || layers_in_flight
            || room_activity_in_flight
            || follow_in_flight
            || cursor_chat_in_flight
            || room_replay_in_flight
            || capture_in_flight
            // The projection component includes both its transition and the
            // bounded worker-response frame pump in this signal.
            || projection_needs_frame
            || self.content.needs_animation_frame()
            || switcher_frame
            || mirror_frame
            || markup_frame
            || notification_frame
            || (self.overlay_flags().collaborators && self.collaborators_overlay.needs_animation_frame())
            || osd_frame
            || acknowledgement_frame
            || self.window_previews.changed()
            || self.peer_sync_overlay.needs_animation_frame()
            || self.directory_sheet.needs_animation_frame()
            || self.arrival_line.needs_animation_frame()
    }
}
