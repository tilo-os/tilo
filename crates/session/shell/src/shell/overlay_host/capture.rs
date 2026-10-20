//! Screen capture, the magnifier, screenshot markup and floating surfaces.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Whether a shortcut changed the magnifier since the last call.
    pub fn take_magnifier_changed(&mut self) -> bool {
        std::mem::take(&mut self.magnifier_changed)
    }

    /// Apply a magnifier shortcut to the session's settings and show the resulting zoom.
    pub(super) fn step_magnifier(&mut self, step: MagnifierStep) {
        match settings_store::magnifier::apply_step(step) {
            Ok(zoom) => {
                self.magnifier_changed = true;
                if let Some(chrome) = self.chrome.as_ref() {
                    chrome
                        .borrow_mut()
                        .show_osd(crate::shell::osd::Osd::magnifier(zoom));
                }
            }
            Err(error) => tracing::warn!(%error, "magnifier change could not be saved"),
        }
    }

    /// Drained by the compositor after it has built a clean scene (the picker
    /// closes before an intent is queued, so its chrome is never captured).
    pub fn take_capture_intents(&mut self) -> Vec<ui::widget::CaptureIntent> {
        let mut intents = std::mem::take(&mut self.instant_captures);
        intents.extend(self.capture_studio.take_intents());
        if !self.capture_studio.is_open()
            && let Some(chrome) = self.chrome.as_ref()
        {
            let mut state = chrome.borrow_mut();
            if state.overlays.capture {
                state.overlays.capture = false;
                state.bump();
            }
        }
        intents
    }

    /// Open the one trusted capture chooser from a shell shortcut or an app
    /// request. Apps can choose the starting mode, but never bypass this
    /// visible user-confirmation surface or supply their own target list.
    pub fn open_capture_studio(&mut self, mode: ui::widget::CaptureMode) -> bool {
        if self.capture_studio.is_recording_active() {
            return false;
        }
        match mode {
            ui::widget::CaptureMode::Screenshot => {
                self.capture_studio.open();
                self.capture_studio
                    .set_mode(ui::widget::CaptureMode::Screenshot);
            }
            ui::widget::CaptureMode::Recording => self.capture_studio.open_recording_setup(),
            ui::widget::CaptureMode::Pip => self.capture_studio.open_pip_setup(),
        }
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            state.overlays.capture = true;
            state.bump();
        }
        true
    }

    /// Open Capture Studio for a portal request with a trusted initial source.
    /// Apps only name a broad portal target; they never inject geometry,
    /// surface ids, or an approval outcome into chrome.
    pub fn open_portal_screenshot_selector(
        &mut self,
        target: crate::portal::desktop_services::ScreenshotTarget,
    ) -> bool {
        if !self.open_capture_studio(ui::widget::CaptureMode::Screenshot) {
            return false;
        }
        self.capture_studio.set_source_mode(match target {
            crate::portal::desktop_services::ScreenshotTarget::Area => {
                ui::widget::SourceMode::Region
            }
            crate::portal::desktop_services::ScreenshotTarget::Window
            | crate::portal::desktop_services::ScreenshotTarget::ActiveWindow => {
                ui::widget::SourceMode::Window
            }
            crate::portal::desktop_services::ScreenshotTarget::Screen => {
                ui::widget::SourceMode::Display
            }
        });
        true
    }

    pub fn capture_studio_open(&self) -> bool {
        self.capture_studio.is_open()
    }

    /// Whether the screenshot picker is up and nothing has been chosen yet;
    /// the compositor holds the screen still for exactly this span.
    pub fn capture_choosing_screenshot(&self) -> bool {
        self.capture_studio.is_choosing_screenshot()
    }

    /// Whether an inner shell surface currently owns the seat even though it
    /// is not represented by one of the ordinary chrome overlay flags.
    ///
    /// Capture Studio deliberately stops being modal after recording starts,
    /// so `capture_studio_open` is too broad for this boundary: the recording
    /// pill must leave applications interactive. Screenshot markup remains
    /// modal for its whole lifetime.
    ///
    /// Every shell-owned menu or flyout claims the seat while it is open,
    /// as Quick Settings does: the stacked popovers (the tab menu, the power
    /// menu) and the screenshot receipt's "Send…" picker. Each hangs over
    /// application windows; were the window under it given the pointer, the
    /// menu would show no hover and judge its next press against the last
    /// pointer position the shell saw — outside the menu — and close.
    pub(crate) fn inner_surface_captures_input(&self) -> bool {
        self.layers.popover_open()
            || self.screenshot_markup.is_some()
            || self.capture_studio.modal_capture().is_some()
            || self.capture_studio.send_menu_open()
            || self.agent_quick_composer.modal_capture().is_some()
            || self.person_quick_composer.modal_capture().is_some()
            || self.bar_popovers.any_open()
    }

    /// Whether the pointer is over the shell-owned bar a maximized window
    /// shows over its client when the person reaches for it. The client
    /// fills the window, so ordinary foreign hit-testing would otherwise
    /// consume Restore/Close presses before the `WindowGroup` which draws
    /// those controls can see them. Pointer only: the keyboard stays with the
    /// client.
    pub(crate) fn maximized_chrome_captures_pointer(&self) -> bool {
        // Local input controls are above window chrome and preserve its seat.
        if self
            .last_cursor
            .is_some_and(|point| self.dictation_pointer_at((point.x, point.y)))
        {
            return false;
        }
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        let Some(group_id) = chrome.borrow().maximized_group() else {
            return false;
        };
        self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: same stable boxed-row contract as the other
            // `group_row_ptrs` accessors; this is a read-only query.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups.iter().any(|group| {
                group.id == group_id
                    && self
                        .last_cursor
                        .is_some_and(|p| group.maximized_chrome_captures(p))
            })
        })
    }

    /// Run `f` on the maximized group, if there is one.
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the group is reached through a raw row pointer; `&mut self` is what makes that access unaliased"
    )]
    pub(super) fn with_maximized_group(&mut self, f: impl FnOnce(&mut ui::widget::WindowGroup)) {
        let Some(group_id) = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.borrow().maximized_group())
        else {
            return;
        };
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same stable boxed-row contract as the other
            // `group_row_ptrs` accessors; `&mut self` gives unaliased access.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            if let Some(group) = row.groups.iter_mut().find(|group| group.id == group_id) {
                f(group);
                return;
            }
        }
    }

    /// A press at the last pointer position, wherever it goes: one in the
    /// maximized window's reveal band while its bar hides belongs to the
    /// app and cancels the bar's reveal (`top_edge_reveal`).
    pub fn note_pointer_press(&mut self) {
        let Some(p) = self.last_cursor else {
            return;
        };
        // The line under the top bar goes with any press; one on it is
        // its own (its "Change" opens the avatar menu).
        let on_notice = self.presence_notices.rect.contains(p) || self.arrival_line.change_at(p);
        self.press_presence_notice(p);
        self.presence_notice_press = on_notice;
        if !self.notification_stack.rect().contains(p) {
            self.release_banner_keyboard();
        }
        self.with_maximized_group(|group| group.note_maximized_press(p));
        if let Some((display, bar)) = self.top_bar_reveal_geometry() {
            self.top_bar_reveal.on_press(p, display, bar);
        }
    }

    pub fn open_screenshot_markup(
        &mut self,
        rgba: Arc<[u8]>,
        width: u32,
        height: u32,
        spawn_from: Option<Rect>,
    ) {
        self.screenshot_markup = Some(
            ui::widget::ScreenshotMarkup::new(rgba, width, height).with_spawn_from(spawn_from),
        );
        self.screenshot_markup_node = None;
        self.capture_mirrors_structure_dirty = true;
    }

    pub fn take_screenshot_markup_commands(&mut self) -> Vec<ui::widget::ScreenshotMarkupCommand> {
        self.screenshot_markup
            .as_mut()
            .map_or_else(Vec::new, ui::widget::ScreenshotMarkup::take_commands)
    }

    pub fn close_screenshot_markup(&mut self) {
        if self.screenshot_markup.take().is_some() {
            self.screenshot_markup_node = None;
            self.capture_mirrors_structure_dirty = true;
        }
    }

    #[doc(hidden)]
    pub fn set_screenshot_markup_transition_progress(&mut self, permille: u16) {
        if let Some(markup) = self.screenshot_markup.as_mut() {
            markup.set_entrance_transition_progress(permille);
        }
    }

    pub fn set_capture_recording_active(&mut self, active: bool) {
        self.capture_studio.set_recording_active(active);
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            if state.overlays.capture {
                state.overlays.capture = false;
                state.bump();
            }
        }
    }

    /// The devices the screenshot receipt's "Send…" offers.
    pub fn set_capture_send_devices(&mut self, devices: Vec<ui::widget::ScreenshotSendDevice>) {
        self.capture_studio.set_screenshot_send_devices(devices);
    }

    /// Queue "Send…" requests from the action door into the capture studio,
    /// so they take the same path as a pick in the receipt's picker.
    pub(super) fn pump_screenshot_sends(&mut self) -> bool {
        let sends = self
            .chrome
            .as_ref()
            .map(|chrome| chrome.borrow_mut().take_screenshot_sends())
            .unwrap_or_default();
        let any = !sends.is_empty();
        for target in sends {
            self.capture_studio.send_last_screenshot(target);
        }
        any
    }

    pub fn set_capture_recording_transition_progress(&mut self, permille: u16) {
        self.capture_studio
            .set_recording_transition_progress(permille);
    }

    /// Freeze every newly-created capture `PiP` at a known source-handoff
    /// phase. This is used by deterministic visual shots only; live windows
    /// continue to derive progress from elapsed time.
    pub fn set_capture_pip_transition_progress(&mut self, permille: u16) {
        for (_, mirror) in &mut self.capture_mirrors {
            mirror.set_source_handoff_progress(permille);
            mirror.content_mut().set_handoff_progress(permille);
        }
    }

    pub fn set_capture_recording_paused(&mut self, paused: bool) {
        self.capture_studio.set_recording_paused(paused);
        self.bump_chrome();
    }

    pub fn set_capture_recording_state_transition_progress(&mut self, permille: u16) {
        self.capture_studio
            .set_recording_state_transition_progress(permille);
    }

    pub fn set_capture_recording_receipt_transition_progress(&mut self, permille: u16) {
        self.capture_studio
            .set_recording_receipt_transition_progress(permille);
    }

    pub fn set_capture_recording_finished(&mut self) {
        self.capture_studio.set_recording_finished();
        self.bump_chrome();
    }

    /// What the screenshot receipt shown says became of the screenshot:
    /// saved, copied, or both. `None` while no receipt shows.
    pub fn capture_screenshot_kept(&self) -> Option<ui::widget::ScreenshotKept> {
        self.capture_studio.screenshot_kept()
    }

    pub fn set_capture_screenshot_finished(
        &mut self,
        kept: ui::widget::ScreenshotKept,
        width: u32,
        height: u32,
        preview: ui::widget::CapturePreview,
    ) {
        self.capture_studio
            .set_screenshot_finished(kept, width, height, preview);
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            if state.overlays.capture {
                state.overlays.capture = false;
            }
            state.bump();
        }
    }

    pub fn set_capture_screenshot_transition_progress(&mut self, permille: u16) {
        self.capture_studio
            .set_screenshot_transition_progress(permille);
    }

    pub fn set_capture_screenshot_pin_pending(&mut self, pending: bool) {
        self.capture_studio.set_screenshot_pin_pending(pending);
        self.bump_chrome();
    }

    pub fn set_capture_screenshot_pinned(&mut self) {
        self.capture_studio.set_screenshot_pinned();
        self.bump_chrome();
    }

    pub fn set_capture_replay_active(&mut self, active: bool, audio: ui::widget::AudioCaptureMode) {
        self.capture_studio.set_replay_active(active, audio);
        self.bump_chrome();
    }

    pub fn set_capture_replay_buffered_seconds(&mut self, seconds: u8) {
        self.capture_studio.set_replay_buffered_seconds(seconds);
    }

    pub fn set_capture_replay_saved(&mut self) {
        self.capture_studio.set_replay_saved();
        self.bump_chrome();
    }

    pub fn set_capture_audio_available(&mut self, system: bool, microphone: bool) {
        self.capture_studio.set_audio_available(system, microphone);
        self.bump_chrome();
    }

    /// Render selected live apps in their virtual strip coordinates, without
    /// clipping them to the physical output. Used by the compositor's capture
    /// executor for `FullApp` and `WholeStrip`; the normal screen scene remains
    /// the source for visible-region/display captures.
    pub fn draw_capture_targets(
        &mut self,
        capture: &ui::widget::LiveCapture<'_>,
        ctx: &mut ui::paint::PaintCtx<'_>,
        presence: &PresenceCtx,
    ) -> Vec<Rect> {
        let rows: Vec<_> = self.group_row_ptrs.values().copied().collect();
        let mut rects = Vec::new();
        for row in rows {
            // SAFETY: the pointers reference boxed rows owned by `content`;
            // this exclusive host borrow and sequential traversal prevent
            // aliasing, matching the existing capture-target/read paths.
            unsafe {
                rects.extend((&mut *row).draw_capture_targets(capture, ctx, presence));
            }
        }
        rects
    }

    /// Create or refresh a capture-owned presentation surface. Live requests
    /// update only their pixels when the id already exists, preserving the
    /// user's window geometry; pinned requests use unique ids and remain
    /// immutable after insertion.
    pub(crate) fn upsert_capture_surface(&mut self, request: CaptureSurfaceRequest<'_>) {
        let PreparedCaptureSurface {
            id,
            title,
            image,
            pixel_aspect,
            preferred_size,
            spawn_from,
            handoff_layers,
            crossfade_preview,
            document_aspect,
        } = request.prepare();
        if let Some((_, window)) = self
            .capture_mirrors
            .iter_mut()
            .find(|(mirror_id, _)| mirror_id == id)
        {
            window.content_mut().set_image(image);
            return;
        }
        // On the display holding what was captured (the focused one for a
        // capture of nothing in particular), and kept on it: across the
        // whole desktop a corner can be on the other display, or on none.
        let display = self.surface_area(spawn_from.map(|source| source.center()));
        let initial_size = capture_mirror_initial_size(display, pixel_aspect, preferred_size);
        let body_w = initial_size.width;
        let body_h = initial_size.height;
        let visible_spawn = visible_capture_spawn(display, spawn_from);
        let content = match crossfade_preview {
            Some(preview) => CaptureMirrorImage::new_crossfade(image, preview, document_aspect),
            None => CaptureMirrorImage::new(image, handoff_layers),
        };
        let mut window = FloatingWindow::new(title, content)
            .open(true)
            .modal(false)
            .closable(true)
            .overlay_chrome(true)
            .with_initial_size(body_w, body_h)
            .spawn_from(visible_spawn);
        if document_aspect.is_none() {
            window = window.with_aspect_ratio(pixel_aspect);
        }

        let window_size = vello::kurbo::Size::new(
            body_w.max(ui::widget::MIN_WIDTH),
            body_h.max(ui::widget::MIN_HEIGHT),
        );
        let origin = capture_mirror_origin(display, visible_spawn, window_size);
        window.place_at_within(display, origin);
        self.capture_mirrors.push((id.to_string(), window));
        self.capture_mirrors_structure_dirty = true;
    }

    /// Paint capture-owned presentation surfaces as a distinct compositor
    /// pass. Keeping mirrors and status chrome out of the ordinary desktop
    /// scene prevents recursive `PiP` feedback and keeps the recording pill out
    /// of the encoded video.
    pub fn draw_capture_overlays(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The display the studio covers (`place_on_focused_display`), so
        // "Screen" captures that display and the recording indicator stays
        // on it.
        self.capture_studio.set_overlay_rect(self.overlay_area());
        // The screenshot picker is the one live thing over the desktop the
        // compositor holds still while it chooses (`CaptureExecutor`). Drawn
        // into the desktop scene, it was held still with it: frozen on its
        // first, fully transparent frame, with no hover, selection or
        // tooltip ever reaching the screen. Drawn here, after the hold, it
        // stays live and stays out of the screenshot.
        if self.capture_studio.is_choosing_screenshot() {
            self.capture_studio
                .draw(scene, transform, text_renderer, theme, presence);
        }
        for (_, mirror) in &mut self.capture_mirrors {
            mirror.draw(scene, transform, text_renderer, theme, presence);
        }
        self.capture_studio
            .draw_countdown_indicator(scene, transform, text_renderer, theme);
        self.capture_studio
            .draw_recording_indicator(scene, transform, text_renderer, theme);
        if let Some(markup) = self.screenshot_markup.as_mut() {
            markup.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    pub fn take_closed_capture_mirrors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.closed_capture_mirrors)
    }

    #[cfg(test)]
    pub(crate) fn capture_mirror_count_for_tests(&self) -> usize {
        self.capture_mirrors.len()
    }

    pub(super) fn route_capture_mirror_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let pointer = self.last_cursor.unwrap_or(Point::ZERO);
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            // Every PiP must observe pointer departure as well as arrival.
            // Routing only to the surface under the new point leaves the old
            // one's hover chrome permanently painted over its pixels.
            for (_, mirror) in &mut self.capture_mirrors {
                if mirror.wants_pointer_capture() {
                    mirror.update_pointer_presence(pointer);
                } else {
                    mirror.update_pointer_position(pointer);
                }
            }
        }
        let Some(index) = self.capture_mirrors.iter().rposition(|(_, mirror)| {
            mirror.wants_pointer_capture()
                || (mirror.is_open() && mirror.window_rect().contains(pointer))
        }) else {
            return false;
        };
        let Some(node) = self.capture_mirror_nodes.get(index).copied() else {
            return false;
        };
        let Some((_, mirror)) = self.capture_mirrors.get_mut(index) else {
            return false;
        };
        mirror.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        let consumed = *ctx.handled || mirror.wants_pointer_capture();
        if !mirror.is_open() {
            let (id, _) = self.capture_mirrors.remove(index);
            self.closed_capture_mirrors.push(id);
            self.capture_mirrors_structure_dirty = true;
        }
        consumed
    }

    /// Drained by the compositor's floating-surface host after app updates.
    /// The request keeps the app/region identity intact, so the shell can
    /// continue presenting the source rather than a copied panel.
    pub fn take_floating_surface_requests(
        &mut self,
    ) -> Vec<ui::floating_surface::FloatingSurfaceRequest> {
        std::mem::take(&mut self.pending_floating_surfaces)
    }

    pub fn take_floating_surface_source_updates(&mut self) -> Vec<(String, Rect)> {
        std::mem::take(&mut self.pending_floating_surface_updates)
    }

    pub fn active_floating_surface_ids(&self) -> Vec<String> {
        self.group_row_ptrs
            .values()
            .flat_map(|ptr| {
                // SAFETY: stable Box<GroupRow> contract; this is a shared
                // read and cannot race a workspace reconciliation sweep.
                let row: &GroupRow = unsafe { &**ptr };
                ui::widget::FloatingSurfaceWidgetExt::active_floating_surface_ids(row)
            })
            .collect()
    }

    pub fn on_floating_surface_host_message(
        &mut self,
        message: &ui::floating_surface::FloatingSurfaceHostMessage,
    ) {
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same stable Box<GroupRow> contract as the request
            // harvesters; the host owns exclusive access for dispatch.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            ui::widget::FloatingSurfaceWidgetExt::on_floating_surface_host_message(row, message);
        }
    }

    /// `(strip_id, group_id, rect)` triples from the live per-strip
    /// [`GroupRow`] pointers, in traversal order. Shared by the
    /// click-focus stamp and the file-drop router so both resolve a
    /// pointer coordinate through the same post-layout geometry.
    pub(super) fn group_hit_candidates(&self) -> Vec<(StripId, GroupId, Rect)> {
        let mut candidates: Vec<(StripId, GroupId, Rect)> = Vec::new();
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: same contract as every other `group_row_ptrs`
            // accessor — captured from a `Box<GroupRow>` owned through
            // the content tree, host outlives the content, and the `&self`
            // borrow keeps any reconcile sweep from aliasing the read.
            let row: &GroupRow = unsafe { &*ptr };
            for g in &row.groups {
                candidates.push((strip_id, g.id, g.get_rect()));
            }
        }
        candidates
    }

    /// The on-screen part of the focused window, which the magnifier follows when keyboard
    /// focus is inside an app rather than on shell chrome.
    pub fn focused_window_rect(&self) -> Option<Rect> {
        self.focused_capture_target()
            .map(|target| target.visible_rect)
    }

    /// The window Alt+Print photographs: the focused group, which
    /// [`Self::capture_targets`] lists first.
    pub(super) fn focused_capture_target(&self) -> Option<ui::widget::CaptureTarget> {
        self.capture_target_candidates()
            .into_iter()
            .find(|(focused, target)| *focused && target.visible_rect.area() > 0.0)
            .map(|(_, target)| target)
    }

    /// Live app surfaces offered by the capture studio. The full virtual rect
    /// is retained even when Tilo's continuous strip has moved most of the app
    /// past this monitor; only `visible_rect` is clipped to the output.
    pub(crate) fn capture_targets(&self) -> Vec<ui::widget::CaptureTarget> {
        let mut targets = self.capture_target_candidates();
        targets.sort_by_key(|(focused, _)| !*focused);
        targets.into_iter().map(|(_, target)| target).collect()
    }

    /// Every capturable window group, paired with whether it holds focus.
    pub(super) fn capture_target_candidates(&self) -> Vec<(bool, ui::widget::CaptureTarget)> {
        let viewport = self.get_rect();
        let mut targets = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same stable Box<GroupRow> contract as
            // `group_hit_candidates` above.
            let row: &GroupRow = unsafe { &*ptr };
            for group in &row.groups {
                let Some(_) = group.active_host() else {
                    continue;
                };
                // A selectable app is the complete window group, including
                // its tab decoration. This matches the exact scene painted by
                // `GroupRow::draw_capture_targets` for App/Strip export.
                let virtual_rect = group.get_rect();
                if virtual_rect.area() <= 0.0 {
                    continue;
                }
                let active = group.active_tab_id();
                let Some(tab) = group.tabs().iter().find(|tab| tab.id == active) else {
                    continue;
                };
                targets.push((
                    group.focused,
                    ui::widget::CaptureTarget {
                        id: format!("group:{}:tab:{}", group.id, tab.id),
                        title: tab.title.clone(),
                        app_id: tab
                            .app_kind
                            .map_or_else(|| "app".into(), |kind| format!("{kind:?}")),
                        visible_rect: virtual_rect.intersect(viewport),
                        virtual_rect,
                        complete_rect: group.complete_capture_rect(),
                    },
                ));
            }
        }
        targets
    }

    /// Pull at most one freshly-emitted request from every strip. Individual
    /// apps retain their own live call/navigation state; this queue only
    /// transfers presentation ownership to the shell compositor.
    pub(super) fn harvest_floating_surface_requests(&mut self) {
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same stable `Box<GroupRow>` contract as
            // `capture_targets`; update holds exclusive access to the host.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            if let Some(request) =
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_request(row)
            {
                let already_queued = self
                    .pending_floating_surfaces
                    .iter()
                    .any(|queued| queued.id == request.id);
                if !already_queued {
                    self.pending_floating_surfaces.push(request);
                }
            }
        }
    }

    pub(super) fn harvest_floating_surface_source_updates(&mut self) {
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same stable Box<GroupRow> contract as request harvest.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for update in
                ui::widget::FloatingSurfaceWidgetExt::take_floating_surface_source_updates(row)
            {
                if let Some(existing) = self
                    .pending_floating_surface_updates
                    .iter_mut()
                    .find(|(id, _)| id == &update.0)
                {
                    *existing = update;
                } else {
                    self.pending_floating_surface_updates.push(update);
                }
            }
        }
    }

    /// Pull app-originated requests for the shared shell chooser. This is a
    /// separate edge from a floating-surface handoff: selecting content is
    /// still pending and remains wholly shell/user owned.
    pub(super) fn harvest_capture_studio_requests(&mut self) {
        let mut requested = None;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same stable Box<GroupRow> contract as the floating
            // surface harvester immediately above.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            requested = requested
                .or_else(|| ui::widget::FloatingSurfaceWidgetExt::take_capture_studio_request(row));
        }
        if let Some(mode) = requested {
            let _ = self.open_capture_studio(mode);
        }
    }
}

/// The magnifier step a shortcut performs, if it is a magnifier shortcut.
pub(super) fn magnifier_step(
    action: settings_store::hardware::ShortcutAction,
) -> Option<MagnifierStep> {
    use settings_store::hardware::ShortcutAction as A;
    match action {
        A::ToggleMagnifier => Some(MagnifierStep::Toggle),
        A::MagnifierZoomIn => Some(MagnifierStep::ZoomIn),
        A::MagnifierZoomOut => Some(MagnifierStep::ZoomOut),
        _ => None,
    }
}

/// The Shift+Print capture: everything on `viewport`, saved and copied.
pub(super) fn instant_screen_capture(viewport: Rect) -> ui::widget::CaptureIntent {
    instant_capture(ui::widget::SourceMode::Display, vec![viewport], Vec::new())
}

/// The Alt+Print capture: the focused window as it is on screen.
pub(super) fn instant_window_capture(
    target: ui::widget::CaptureTarget,
) -> ui::widget::CaptureIntent {
    instant_capture(
        ui::widget::SourceMode::Window,
        vec![target.visible_rect],
        vec![target],
    )
}

pub(super) fn instant_capture(
    source: ui::widget::SourceMode,
    regions: Vec<Rect>,
    targets: Vec<ui::widget::CaptureTarget>,
) -> ui::widget::CaptureIntent {
    ui::widget::CaptureIntent {
        mode: ui::widget::CaptureMode::Screenshot,
        source,
        destination: ui::widget::CaptureDestination::FileAndClipboard,
        regions,
        include_pointer: false,
        audio: ui::widget::AudioCaptureMode::None,
        extent: ui::widget::CaptureExtent::Visible,
        pip_layout: ui::widget::PipLayout::Auto,
        targets,
        recording_command: None,
        replay_command: None,
        screenshot_command: None,
    }
}
