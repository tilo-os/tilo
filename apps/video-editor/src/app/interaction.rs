#[allow(
    clippy::wildcard_imports,
    reason = "this file is one part of the `VideoEditorApp` impl split out of `app.rs`; it shares the parent's whole scope"
)]
use super::*;

// ==========================================================================
// Event handling helpers
// ==========================================================================
impl VideoEditorApp {
    #[expect(
        clippy::too_many_lines,
        reason = "the keyboard map: one arm per binding, in the order the help sheet lists them"
    )]
    pub(super) fn handle_key(
        &mut self,
        key: &Key,
        modifiers: winit::event::Modifiers,
        state: &mut ui::widget::Edits<'_>,
        r: &Regions,
    ) -> bool {
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        let shift = modifiers.state().shift_key();
        // The unsaved-work question owns the keyboard: Esc stays put, Enter
        // takes the safe answer. Nothing else fires meanwhile, so a stray key
        // cannot edit the project that is being asked about.
        if self.pending_open.is_some() {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.resolve_pending_open(state, DiscardChoice::Cancel);
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.resolve_pending_open(state, DiscardChoice::Save);
                    return true;
                }
                _ => return false,
            }
        }
        if self.presentation.export_open {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                // Esc closes the dialog, not the render: the chip under the
                // Export button carries the job on.
                self.presentation.export_open = false;
                return true;
            }
            if matches!(key, Key::Named(NamedKey::Enter)) {
                if self.export_is_offered() {
                    self.request_export();
                }
                return true;
            }
            return false;
        }
        // A timecode being typed owns the keyboard: every key either edits it,
        // commits it, or abandons it. Nothing else fires meanwhile, so typing
        // "1:30" cannot also split a clip and add a marker on the way.
        if let Some(field) = self.goto_entry.as_ref() {
            match key {
                Key::Named(NamedKey::Escape) => {
                    self.goto_entry = None;
                }
                Key::Named(NamedKey::Backspace) => {
                    if let Some(field) = self.goto_entry.as_mut() {
                        let mut value = field.value().to_string();
                        ui::text_edit::delete_last_grapheme(&mut value);
                        field.set_text(&value);
                    }
                }
                Key::Named(NamedKey::Enter) => {
                    let typed = field.value().to_string();
                    match crate::timecode::parse(&typed, self.project.fps) {
                        Some(t) => {
                            self.playhead = t.clamp(0.0, self.project.duration().max(t));
                            self.playback.playing = false;
                            self.shuttle = 0.0;
                            self.playback.chase_playhead = true;
                            self.goto_entry = None;
                        }
                        // Keep the field open on a value that cannot be read,
                        // rather than closing it and silently going nowhere.
                        None => self.navigation.goto_notice = true,
                    }
                }
                // Every other key was the field's: it edits the timecode.
                _ => {}
            }
            return true;
        }
        // The shortcut sheet owns the keyboard like the dialogs above it: Esc
        // and ? close it, and nothing else fires meanwhile — Ctrl+E used to
        // open the export dialog underneath the sheet's scrim.
        if self.navigation.help_open {
            match key {
                Key::Named(NamedKey::Escape) => self.navigation.help_open = false,
                Key::Character(c) if c.as_str() == "?" || (shift && c.as_str() == "/") => {
                    self.navigation.help_open = false;
                }
                _ => {}
            }
            return true;
        }
        if self.bin_overlay_is_open() {
            // The narrow bin paints a full-window scrim, so it owns keyboard
            // input as well as pointer input. Keep the one useful pane walk
            // inside the bin and give both Escape and B the visible toggle's
            // close behaviour; no timeline edit may happen behind the veil.
            return match key {
                Key::Named(NamedKey::Escape) => {
                    self.close_bin_overlay();
                    true
                }
                Key::Named(NamedKey::Tab) => {
                    self.focus.pane = Pane::Bin;
                    self.clamp_pane_cursor();
                    true
                }
                Key::Named(NamedKey::ArrowUp) => self.nav_arrow(true, true, state),
                Key::Named(NamedKey::ArrowDown) => self.nav_arrow(false, true, state),
                Key::Named(NamedKey::Enter) => self.activate_focused(state),
                Key::Character(c) if c.eq_ignore_ascii_case("b") => {
                    self.close_bin_overlay();
                    true
                }
                _ => false,
            };
        }
        match key {
            Key::Named(NamedKey::Space) => {
                self.toggle_play();
                true
            }
            Key::Named(NamedKey::PageUp) if self.focus.pane == Pane::Timeline => {
                self.scroll_timeline_page(false, r)
            }
            Key::Named(NamedKey::PageDown) if self.focus.pane == Pane::Timeline => {
                self.scroll_timeline_page(true, r)
            }
            // Arrows nudge every selected clip a frame (Shift = one second);
            // with nothing selected they step the playhead instead.
            Key::Named(NamedKey::ArrowLeft) if self.focus.pane == Pane::Transport => {
                self.nav_arrow(true, false, state)
            }
            Key::Named(NamedKey::ArrowRight) if self.focus.pane == Pane::Transport => {
                self.nav_arrow(false, false, state)
            }
            Key::Named(NamedKey::ArrowLeft) => {
                let step = if shift {
                    1.0
                } else {
                    frame_step(self.project.fps)
                };
                if self.selection.is_empty() {
                    self.step_frame(if shift { -self.project.fps } else { -1.0 });
                } else {
                    self.nudge_selected(-step);
                }
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                let step = if shift {
                    1.0
                } else {
                    frame_step(self.project.fps)
                };
                if self.selection.is_empty() {
                    self.step_frame(if shift { self.project.fps } else { 1.0 });
                } else {
                    self.nudge_selected(step);
                }
                true
            }
            Key::Named(NamedKey::Home) => {
                self.playhead = 0.0;
                self.playback.playing = false;
                self.shuttle = 0.0;
                self.playback.chase_playhead = true;
                true
            }
            Key::Named(NamedKey::End) => {
                self.playhead = self.project.duration();
                self.playback.playing = false;
                self.shuttle = 0.0;
                self.playback.chase_playhead = true;
                true
            }
            // Shift+Delete ripples (closes the gap); plain Delete lifts.
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                if shift {
                    self.ripple_delete_selected();
                } else {
                    self.delete_selected();
                }
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("e") => {
                self.presentation.export_open = true;
                let _ = r;
                true
            }
            // Ctrl+O opens a project. It used to import media, from before the
            // editor had projects to open — and Ctrl+O means "open a document"
            // in every app a person has ever used, so leaving it on import
            // would make the one shortcut everybody already knows do the one
            // thing they did not ask for. Import keeps the same letter, one
            // modifier along, and both are in the shortcut sheet.
            Key::Character(c) if ctrl && shift && c.eq_ignore_ascii_case("o") => {
                self.trigger_import();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("o") => {
                self.request_open_project();
                true
            }
            Key::Character(c) if ctrl && shift && c.eq_ignore_ascii_case("s") => {
                self.request_save_as();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("s") => {
                self.queue_save_project();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                self.copy_selected();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("x") => {
                self.cut_selected();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                self.paste_clip();
                true
            }
            // Ctrl+D drops a copy of the selection immediately after it.
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("d") => {
                self.duplicate_selected();
                true
            }
            // Ctrl+A selects every clip on the timeline.
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => {
                self.select_all();
                true
            }
            // J / K / L shuttle: J reverse, K stop, L forward. Repeated J/L
            // doubles the rate, the standard NLE shuttle.
            Key::Character(c) if c.eq_ignore_ascii_case("j") && !ctrl => {
                self.shuttle_key(-1.0);
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("k") && !ctrl => {
                self.shuttle = 0.0;
                self.playback.playing = false;
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("l") && !ctrl => {
                self.shuttle_key(1.0);
                true
            }
            // Shift+I / Shift+O mark the timeline's render range; Shift+X
            // drops both marks. Plain I / O keep trimming the selected clip,
            // which is what they have always done and what every NLE does
            // with them — the range is a property of the timeline, the trim
            // one of a clip, and they are not the same decision.
            Key::Character(c) if shift && !ctrl && c.eq_ignore_ascii_case("i") => {
                self.mark_in_here();
                true
            }
            Key::Character(c) if shift && !ctrl && c.eq_ignore_ascii_case("o") => {
                self.mark_out_here();
                true
            }
            Key::Character(c) if shift && !ctrl && c.eq_ignore_ascii_case("x") => {
                self.clear_marks();
                true
            }
            // I / O set the selected clip's in / out point to the playhead.
            Key::Character(c) if c.eq_ignore_ascii_case("i") && !ctrl => {
                self.set_in_at_playhead();
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("o") && !ctrl => {
                self.set_out_at_playhead();
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("s") && !ctrl => {
                self.split_selected();
                true
            }
            // Toggle the media bin flap. Useful on narrow windows where the
            // bin is an overlay; harmless (collapses the docked bin) on wide.
            Key::Character(c) if c.eq_ignore_ascii_case("b") && !ctrl => {
                self.toggle_bin();
                true
            }
            // M drops a marker at the playhead and opens it for naming — an
            // unnamed tick is a marker you cannot find again.
            Key::Character(c) if c.eq_ignore_ascii_case("m") && !ctrl => {
                self.add_marker_here();
                true
            }
            // T makes a title, places it and puts the caret in its words.
            Key::Character(c) if c.eq_ignore_ascii_case("t") && !ctrl => {
                self.add_title();
                true
            }
            // , and . step between marked instants, the project's own and
            // every clip's alike.
            Key::Character(c) if (c.as_str() == "," || c.as_str() == "<") && !ctrl => {
                self.jump_to_marker(false);
                true
            }
            Key::Character(c) if (c.as_str() == "." || c.as_str() == ">") && !ctrl => {
                self.jump_to_marker(true);
                true
            }
            // C walks the places the mix went over, the same walk the red CLIP
            // chip does under the pointer. Without it the *only* route to the
            // answer was clicking a 60-pixel chip: the warning told a keyboard
            // user their mix was ruined and then gave them a whole timeline to
            // scrub. Returns false when nothing has clipped, so the key falls
            // through as unhandled rather than reporting a jump it did not
            // make.
            Key::Character(c) if c.eq_ignore_ascii_case("c") && !ctrl => self.go_to_next_clipping(),
            // Shift+Z fits the whole project to the window, the NLE standard.
            Key::Character(c) if c.eq_ignore_ascii_case("z") && !ctrl => {
                self.zoom_to_fit(r);
                true
            }
            Key::Character(c) if c.as_str() == "+" || c.as_str() == "=" => {
                self.zoom_by(1.25);
                true
            }
            Key::Character(c) if c.as_str() == "-" || c.as_str() == "_" => {
                self.zoom_by(1.0 / 1.25);
                true
            }
            // Tab moves between panes and the arrows move within the focused
            // one. Tab used to toggle the inspector, but an editor whose only
            // Tab stop is the whole application is one a keyboard-only user
            // cannot navigate at all; the inspector toggle moved to Ctrl+I,
            // which the shortcut sheet states.
            Key::Named(NamedKey::Tab) => {
                self.focus_pane(shift);
                true
            }
            Key::Named(NamedKey::ArrowUp) => self.nav_arrow(true, true, state),
            Key::Named(NamedKey::ArrowDown) => self.nav_arrow(false, true, state),
            Key::Named(NamedKey::Enter) => self.activate_focused(state),
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("i") => {
                self.toggle_inspector();
                true
            }
            // The shortcut sheet. Fifteen shortcuts existed and none of them
            // were written down anywhere in the product.
            Key::Character(c) if c.as_str() == "?" || (shift && c.as_str() == "/") => {
                self.navigation.help_open = !self.navigation.help_open;
                self.help_scroll = 0.0;
                true
            }
            // G opens a timecode field: type a time, press Enter, land there.
            Key::Character(c) if c.eq_ignore_ascii_case("g") && !ctrl => {
                self.goto_entry = Some(goto_field(""));
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Esc dismisses an open overlay bin first, then an open
                // overlay inspector, then clears the selection — matching the
                // flap kernel's Esc-to-close.
                if self.navigation.help_open {
                    self.navigation.help_open = false;
                    return true;
                }
                if self.bin_layout() == SplitLayout::Overlay
                    && self.bin_flap.is_open(SplitLayout::Overlay)
                {
                    self.close_bin_overlay();
                } else if self.inspector_split() == SplitLayout::Overlay
                    && self.inspector_flap.is_open(SplitLayout::Overlay)
                {
                    self.reconcile_flaps();
                    self.inspector_flap.close();
                } else {
                    self.selection.clear();
                }
                true
            }
            _ => false,
        }
    }

    /// The go-to-timecode card, for the painter and the hit test alike.
    ///
    /// Shared rather than derived twice: a click has to land on the card that
    /// was actually drawn, or dismiss-on-click-away fires while the pointer
    /// is over the field.
    pub(super) fn goto_layout(
        &self,
        r: &Regions,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) -> GotoLayout {
        let type_sizes = dialog_type(theme);
        let sp = &theme.tokens.spacing;
        let (s1, s2, s4) = (f64::from(sp.s1), f64::from(sp.s2), f64::from(sp.s4));
        let line_xs = line_height(tr, &type_sizes.xs);
        // The field is a text field set in the timecode's own role, as tall
        // as its line plus the field's inset.
        let field_h = line_height(tr, &type_sizes.xl) + s4;
        // Caption, field, hint — each one spacing step apart, inside the
        // card's inset; so the card grows with the text.
        let h = s4 + line_xs + s1 + field_h + s1 + line_xs + s4;
        let w = GOTO_CARD_W.min(self.rect.width() - 2.0 * s4);
        // Anchored just above the transport, whose playhead time it edits.
        let y1 = r.transport.y0 - s2;
        let cx = self.rect.center().x;
        let card = Rect::new(cx - w / 2.0, y1 - h, cx + w / 2.0, y1);
        let label_top = card.y0 + s4;
        let entry_top = label_top + line_xs + s1;
        let entry = Rect::new(card.x0 + s4, entry_top, card.x1 - s4, entry_top + field_h);
        let hint_top = entry.y1 + s1;
        GotoLayout {
            card,
            type_sizes,
            label_top,
            entry,
            hint_top,
        }
    }

    /// Whether a full-window scrim is up — the editor behind it is
    /// unavailable, and every click belongs to the surface on top.
    pub(super) fn is_modal(&self) -> bool {
        self.pending_open.is_some()
            || self.presentation.export_open
            || self.navigation.help_open
            || self.goto_entry.is_some()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one hit test per pressable region, in paint order"
    )]
    pub(super) fn handle_press(&mut self, state: &AutoCommit, r: &Regions, shift: bool) {
        let p = self.last_mouse;

        // Modals own all clicks. The unsaved-work question is asked over the
        // export dialog too (Ctrl+O works while it is up), so it is first.
        if self.pending_open.is_some() {
            // Laid out from the current state — see `chrome_theme`. The
            // question has no scrim dismissal and no close control: its way
            // out is the Cancel it shows.
            let hit = self
                .with_dialog_layout(super::VideoEditorApp::discard_layout)
                .and_then(|l| l.modal.hit(p));
            let choice = match hit {
                Some(ModalHit::Action(0)) => DiscardChoice::Cancel,
                Some(ModalHit::Action(1)) => DiscardChoice::Discard,
                Some(ModalHit::Action(2)) => DiscardChoice::Save,
                _ => return,
            };
            self.resolve_pending_open(state, choice);
            return;
        }
        if self.presentation.export_open {
            self.press_export(state, p);
            return;
        }
        // The shortcut sheet and the go-to-timecode field paint the same
        // full-window scrim as the dialogs above, so they swallow clicks the
        // same way. Before this, a click "on" the dimmed background reached
        // the clip, slider or Export button underneath — a real edit, made
        // invisibly, through a surface that said the editor was unavailable.
        // Clicking away dismisses, which is what a scrim invites; the card
        // itself keeps the click, as every modal card in the fleet does.
        if self.navigation.help_open {
            let dismisses = self
                .with_dialog_layout(|app, tr, theme| app.help_layout(tr, theme).modal.dismisses(p));
            if dismisses {
                self.navigation.help_open = false;
            }
            return;
        }
        if self.goto_entry.is_some() {
            let outside = self.with_dialog_layout(|app, tr, theme| {
                !app.goto_layout(r, tr, theme).card.contains(p)
            });
            if outside {
                self.goto_entry = None;
            }
            return;
        }
        // Scrollbars float over their panes, so they are hit-tested before any
        // content underneath them.
        if self.press_scrollbars(r, p) {
            return;
        }
        // The export chip floats over whatever pane is under it, so it is hit
        // first. It is the way back into the render it is reporting: the full
        // path, the failure's own words and Cancel all live in the dialog.
        if self.export_chip_is_shown() && r.export_chip.contains(p) {
            self.presentation.export_open = true;
            return;
        }
        // The media bin's show/hide toggle, in every layout.
        if r.menu_btn.contains(p) {
            self.toggle_bin();
            return;
        }
        // An open bin overlay owns every click outside its flap. This check
        // must precede the other top-bar controls: otherwise the scrim could
        // look modal while Import, Export, or the inspector still changed
        // behind it.
        let bin_visible = self.bin_is_visible(r);
        if r.bin_overlay && bin_visible && !r.bin.contains(p) && !r.menu_btn.contains(p) {
            self.close_bin_overlay();
            return;
        }
        // The inspector's show/hide toggle, in every layout.
        if r.inspector_btn.contains(p) {
            self.toggle_inspector();
            return;
        }
        // An open inspector overlay owns its own rect, before the preview and
        // timeline underneath it get a look.
        if r.inspector_overlay && self.inspector_is_visible(r) && r.inspector.contains(p) {
            self.press_inspector(r, p);
            return;
        }
        // Top bar buttons.
        if r.project_btn.width() > 0.0 && r.project_btn.contains(p) {
            if self.project_menu.is_open() {
                self.project_menu.close();
            } else {
                self.open_project_menu(Point::new(r.project_btn.x0, r.project_btn.y1 + 4.0));
            }
            return;
        }
        if r.export_btn.contains(p) {
            self.presentation.export_open = true;
            return;
        }
        if r.import_btn.contains(p) {
            self.trigger_import();
            return;
        }
        if r.split_btn.contains(p) {
            self.split_selected();
            return;
        }
        // Empty-bin "Import media" affordance.
        if let Some(btn) = self.empty_bin_import_rect(r)
            && btn.contains(p)
        {
            self.trigger_import();
            return;
        }
        // "+ Title" in the bin header.
        if self.bin_is_visible(r) && self.add_title_rect(r).contains(p) {
            self.add_title();
            return;
        }
        // Media bin (only when it's on-screen: docked, or overlay open).
        // A row scrolled out of the bin's list is not under the pointer, even
        // where its rect runs on past the pane.
        if bin_visible && VideoEditorApp::bin_viewport(r).contains(p) {
            for (mid, rect) in self.bin_item_rects(r) {
                if rect.contains(p) {
                    self.selected_media = Some(mid);
                    // How long ago this same item was last *clicked* (not
                    // programmatically selected) — `None` for a different item
                    // or a fresh one. Both the double-click and the rename are
                    // gated on it, so a freshly-imported, auto-selected item is
                    // never renamed on the first click.
                    let since = self.last_click_media.and_then(|(m, t)| {
                        (m == mid).then(|| ui::time::now().saturating_duration_since(t).as_millis())
                    });
                    let dbl = matches!(since, Some(ms) if ms < BIN_DOUBLE_CLICK_MS);
                    // The rename window is *bounded at both ends*: fast enough
                    // and it is a double-click, slow enough and the earlier
                    // click is stale and this is simply a new press. Without
                    // the upper bound an item, once clicked, could never be
                    // dragged to the timeline again — the name band covers
                    // most of the row, which is exactly where you grab it.
                    let rename_window = matches!(
                        since,
                        Some(ms) if (BIN_DOUBLE_CLICK_MS..BIN_RENAME_WINDOW_MS).contains(&ms)
                    );
                    if dbl {
                        self.add_media_to_timeline(mid);
                        self.last_click_media = None;
                        self.pending_rename = None;
                    } else {
                        // Always arm a potential bin→timeline drag. When the
                        // press also falls in the rename window, the rename is
                        // only an *intent*: it fires on release if the pointer
                        // never moved, and is cancelled the moment the press
                        // becomes a drag (see `update_bin_drag`). A drag
                        // therefore always wins over a rename.
                        self.bin_drag = Some(BinDrag {
                            media: mid,
                            origin: p,
                            active: false,
                        });
                        self.pending_rename = (rename_window
                            && VideoEditorApp::bin_name_rect(rect).contains(p))
                        .then_some(mid);
                        self.last_click_media = Some((mid, ui::time::now()));
                    }
                    return;
                }
            }
        }
        // Inspector controls.
        if self.inspector_is_visible(r) && r.inspector.contains(p) {
            self.press_inspector(r, p);
            return;
        }
        // Transport controls.
        for (action, rect) in self.transport_buttons(r) {
            if rect.contains(p) {
                self.transport_action(action);
                return;
            }
        }
        // History. `request` refuses when the verb is unavailable, which is
        // the same test the drawing makes — there is no second availability
        // rule here to drift from the first.
        for (which, rect) in self.history_buttons(r) {
            if rect.contains(p) {
                self.undo.request(which);
                return;
            }
        }
        let scrub = self.scrubber_rect(r);
        if self.has_scrubber(r)
            && (scrub.contains(p)
                || (p.x >= scrub.x0 && p.x <= scrub.x1 && (p.y - scrub.center().y).abs() < 14.0))
        {
            self.drag = Some(Drag::Scrubber);
            self.apply_scrubber(p, r);
            return;
        }
        // The standing clipping warning, in the master strip. Pressing it
        // walks the playhead through the places the mix went over, wrapping
        // round — which is the difference between a warning you can act on
        // and one you can only be annoyed by.
        if self.mix.clipped() && self.clip_flag_rect(r).contains(p) {
            self.go_to_next_clipping();
            return;
        }
        // Zoom readout / fit-to-window. **Before** the ruler scrub, because
        // it now sits at the ruler's right end: a chip inside a region that
        // scrubs has to be claimed before the region is, or pressing it just
        // moves the playhead to the far right.
        // The playhead's head keeps the ruler's right end even where the
        // chip sits: a press on the head scrubs, as it does anywhere else.
        if self.zoom_chip_rect(r).contains(p) && !self.playhead_head_rect(r).contains(p) {
            self.zoom_to_fit(r);
            return;
        }
        // Ruler scrub.
        if r.ruler.contains(p) {
            self.drag = Some(Drag::Playhead);
            self.playhead = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps)
                .clamp(0.0, self.project.duration());
            return;
        }
        // The measured add-track controls may extend from the gutter into
        // the lanes. Their actual painted bounds own the press there too.
        for (kind, rect) in self.add_track_buttons(r) {
            if rect.contains(p) {
                self.add_track(kind);
                return;
            }
        }
        // Timeline gutter: per-track mute, lock and solo.
        if r.gutter.contains(p) {
            for (tid, _kind, lane) in self.lane_rects(r) {
                let header = VideoEditorApp::track_head_rect(r, &lane);
                if header.contains(p) {
                    // The same bands the paint reads (`chrome_layout::track_head`),
                    // so the pill under the pointer is the pill that was drawn.
                    let head = self.track_head_layout(header);
                    if head.mute.contains(p) {
                        self.toggle_track_mute(tid);
                    } else if head.lock.contains(p) {
                        self.toggle_track_lock(tid);
                    } else if head.solo.contains(p) {
                        self.toggle_track_solo(tid);
                    }
                    return;
                }
            }
            return;
        }
        // Timeline lanes — clips, trim handles, rubber-band / scrub.
        if r.lanes.contains(p) {
            self.press_timeline(r, p, shift);
        }
    }

    /// Give every visible scrollbar first refusal on a press. Bars float over
    /// their pane's content, so they must be hit-tested before it. Returns
    /// `true` when one took the press.
    pub(super) fn press_scrollbars(&mut self, r: &Regions, p: Point) -> bool {
        for pane in Self::SCROLL_PANES {
            let Some((track, m)) = self.scroll_pane(pane, r) else {
                continue;
            };
            match self.scroll_bar_mut(pane).press(track, m, p) {
                ui::widget::PressOutcome::Thumb => {
                    self.drag = Some(Drag::Scrollbar(pane));
                    return true;
                }
                ui::widget::PressOutcome::Page(target) => {
                    self.set_scroll_offset(pane, target, r);
                    return true;
                }
                ui::widget::PressOutcome::Miss => {}
            }
        }
        false
    }

    /// Finish a rubber-band drag. If the band ever opened, select every clip
    /// it touches (extending the previous selection when `additive`).
    /// Otherwise the press was a plain click on empty space: clear the
    /// selection and scrub the playhead there, the behaviour a bare click on
    /// the lanes has always had.
    pub(super) fn release_marquee(&mut self, origin: Point, additive: bool, r: &Regions) {
        let Some(band) = self.marquee else {
            if !additive {
                self.selection.clear();
            }
            self.playhead = snap_to_frame(self.x_to_time(origin.x, &r.lanes), self.project.fps)
                .clamp(0.0, self.project.duration());
            return;
        };
        let mut hits: Vec<ClipId> = if additive {
            self.selection.ids().to_vec()
        } else {
            Vec::new()
        };
        for (tid, _kind, lane) in self.lane_rects(r) {
            if lane.y1 < band.y0 || lane.y0 > band.y1 {
                continue;
            }
            for c in self.project.clips_on(tid) {
                let cr = self.clip_rect(c, &lane, r);
                // Any horizontal overlap with the band counts — the lane test
                // above already established the vertical one.
                if cr.x1 >= band.x0 && cr.x0 <= band.x1 && !hits.contains(&c.id) {
                    hits.push(c.id);
                }
            }
        }
        self.selection.set(hits);
    }

    /// The clip under `p`, as `(clip id, its lane rect)`. `None` over empty
    /// lane space.
    pub(super) fn clip_at(&self, r: &Regions, p: Point) -> Option<(ClipId, Rect)> {
        for (tid, _kind, lane) in self.lane_rects(r) {
            if p.y < lane.y0 || p.y > lane.y1 {
                continue;
            }
            for c in self.project.clips_on(tid) {
                let cr = self.clip_rect(c, &lane, r);
                if p.x >= cr.x0 - 2.0 && p.x <= cr.x1 + 2.0 {
                    return Some((c.id, lane));
                }
            }
        }
        None
    }

    /// Press inside the lane area. On a clip: select (shift extends), then arm
    /// a trim or a move — unless its track is locked, in which case the clip
    /// still selects (so the inspector can show it) but no edit drag begins.
    /// On empty space: arm a rubber-band selection, which falls back to a
    /// playhead scrub if the press never moves.
    pub(super) fn press_timeline(&mut self, r: &Regions, p: Point, shift: bool) {
        if let Some((cid, lane)) = self.clip_at(r, p) {
            if shift {
                self.selection.toggle(cid);
            } else if !self.selection.contains(cid) {
                // Clicking a clip that is already part of a multi-selection
                // keeps that selection, so the whole block can be dragged.
                self.selection.select_only(cid);
            } else {
                self.selection.set_primary(cid);
            }
            // A narrow window keeps the inspector out of the way until there
            // is something to inspect; picking a clip is exactly that moment.
            self.reveal_inspector();
            let Some(clip) = self.project.clip(cid).cloned() else {
                return;
            };
            let cr = self.clip_rect(&clip, &lane, r);
            // A key diamond on the clip's keyframe lane takes the playhead to
            // that key. Checked before the trim handles and the move drag,
            // because it is the smaller, more specific target inside the same
            // body.
            if !shift
                && p.y >= cr.y1 - 13.0
                && let Some(t) = clip
                    .key_times()
                    .into_iter()
                    .find(|t| (self.time_to_x(*t, &r.lanes) - p.x).abs() <= 5.0)
            {
                self.playhead = t.clamp(0.0, self.project.duration());
                self.playback.playing = false;
                self.shuttle = 0.0;
                return;
            }
            if !self.link_group_editable(cid) || shift {
                // A locked group is inspectable but not draggable; a shift-click
                // is a selection gesture, not the start of a move.
                return;
            }
            let trim_hit = TRIM_HIT * self.chrome_scale();
            if (p.x - cr.x0).abs() <= trim_hit {
                self.drag = Some(Drag::TrimIn { id: cid });
            } else if (p.x - cr.x1).abs() <= trim_hit {
                self.drag = Some(Drag::TrimOut { id: cid });
            } else {
                if self.edit_targets().is_empty() {
                    return;
                }
                self.drag = Some(Drag::ClipMove {
                    id: cid,
                    grab_dt: self.x_to_time(p.x, &r.lanes) - clip.start,
                    track: clip.track,
                });
            }
            return;
        }
        // Empty lane area: a rubber band, resolved on release. A press that
        // never travels past MARQUEE_THRESHOLD scrubs the playhead instead,
        // so click-to-scrub still works exactly as it did.
        self.drag = Some(Drag::Marquee {
            origin: p,
            additive: shift,
        });
        self.marquee = None;
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the inspector hit test deliberately mirrors its ordered, scroll-clipped control layout"
    )]
    pub(super) fn press_inspector(&mut self, r: &Regions, p: Point) {
        // The fixed identity card is context, not a hidden control surface.
        // Do not let a translated row behind it receive a click, nor a row
        // the body leaves out because it is only part-way in view.
        let l = self.inspector_layout(r);
        if !self.inspector_shown_body(r, &l).contains(p) {
            return;
        }
        // The markers section and the section folds are project-level, so
        // they answer to a press whether or not a clip is selected. Every
        // control below this point edits the selection and needs one.
        for (s, rect) in &l.sections {
            if rect.contains(p) {
                if !self.collapsed.remove(s) {
                    self.collapsed.insert(*s);
                }
                return;
            }
        }
        for (m, row) in l.marker_rows.clone() {
            if !row.contains(p) {
                continue;
            }
            let (_, note_btn, rename, del) = marker_row_parts(row);
            if del.contains(p) {
                self.delete_marker(m);
            } else if note_btn.contains(p) {
                self.begin_marker_edit(m, MarkerField::Note);
            } else if rename.contains(p) {
                self.begin_marker_rename(m);
            } else {
                self.seek_to_marker(m);
            }
            return;
        }
        if let Some(rect) = l.add_marker
            && rect.contains(p)
        {
            self.add_marker_here();
            return;
        }
        // The project's format is project-level too, so it answers whether or
        // not a clip is selected -- above the `l.valid` gate, which is what
        // marks "a clip's controls were laid out".
        for ((w, h), rect) in l.project_frames.clone() {
            if rect.contains(p) {
                self.set_project_frame_size(w, h);
                return;
            }
        }
        for (rate, rect) in l.project_rates.clone() {
            if rect.contains(p) {
                self.set_project_fps(rate);
                return;
            }
        }
        if let Some(rect) = l.project_match
            && rect.contains(p)
        {
            self.match_project_format();
            return;
        }
        if !l.valid {
            return;
        }
        // The slider controls start a drag and preview the click position in
        // memory; the final value is dispatched as one action on mouse-up
        // (see `commit_drag`), so a slider interaction is one undo step.
        for (track, param, drag) in [
            (l.opacity, Param::Opacity, Drag::Opacity),
            (l.volume, Param::Volume, Drag::Volume),
            (l.brightness, Param::Brightness, Drag::Brightness),
            (l.contrast, Param::Contrast, Drag::Contrast),
            (l.saturation, Param::Saturation, Drag::Saturation),
        ] {
            if let Some(track) = track
                && hit_slider(track, p)
            {
                self.drag = Some(drag);
                let v = slider_value_at(track, p.x);
                self.preview_param(param, v);
                return;
            }
        }
        // Fade in / out sliders (value in seconds, 0..clip-duration).
        let max = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .map_or(0.0, crate::model::Clip::duration);
        if let Some(track) = l.fade_in
            && hit_slider(track, p)
        {
            self.drag = Some(Drag::FadeIn);
            let frac = slider_value_at(track, p.x);
            self.preview_edit(move |c| c.fade_in = (frac * max).clamp(0.0, c.duration()));
            return;
        }
        if let Some(track) = l.fade_out
            && hit_slider(track, p)
        {
            self.drag = Some(Drag::FadeOut);
            let frac = slider_value_at(track, p.x);
            self.preview_edit(move |c| c.fade_out = (frac * max).clamp(0.0, c.duration()));
            return;
        }
        if let Some(track) = l.transition_len
            && hit_slider(track, p)
        {
            self.drag = Some(Drag::TransitionLen);
            let frac = slider_value_at(track, p.x);
            let max = self.transition_len_max();
            self.preview_edit(move |c| c.transition_duration = (frac * max).max(0.01));
            return;
        }
        for (t, rect) in &l.transitions {
            if rect.contains(p) {
                self.set_transition(*t);
                return;
            }
        }
        if let Some(rect) = l.link
            && rect.contains(p)
        {
            self.toggle_link_selected();
            return;
        }
        for (pal, rect) in &l.colors {
            if rect.contains(p) {
                self.set_palette(*pal);
                return;
            }
        }
        // Effect rows: reorder / intensity slider / remove.
        for (i, row) in l.effect_rows.iter().enumerate() {
            let (_key, up, down, slider, remove) = Self::effect_row_parts(*row);
            if up.contains(p) {
                self.move_effect_selected(i, true);
                return;
            }
            if down.contains(p) {
                self.move_effect_selected(i, false);
                return;
            }
            if remove.contains(p) {
                self.remove_effect_from_selected(i);
                return;
            }
            if hit_slider(slider, p) {
                let Some(effect) = self
                    .selected()
                    .and_then(|id| self.project.clip(id))
                    .and_then(|clip| clip.effects.get(i))
                    .copied()
                else {
                    return;
                };
                self.drag = Some(Drag::EffectAmount {
                    idx: i,
                    effect: effect.id,
                });
                let v = slider_value_at(slider, p.x);
                self.preview_edit(move |c| {
                    if let Some(e) = c.effects.get_mut(i) {
                        e.amount = v;
                    }
                });
                return;
            }
        }
        if l.add_effect.width() > 0.0 && l.add_effect.contains(p) {
            self.open_effect_menu(p);
            return;
        }
        // Tone curves: the channel chooser, then the editor's box.
        for (ch, rect) in l.curve_channels.clone() {
            if rect.contains(p) {
                self.curve_channel = ch;
                self.collapsed.remove(&Section::Curves);
                return;
            }
        }
        if let Some(canvas) = l.curve_canvas
            && canvas.contains(p)
        {
            self.press_curve(canvas, p);
            return;
        }
        if let Some(rect) = l.curve_reset
            && rect.contains(p)
        {
            let ch = self.curve_channel;
            self.edit_selected(move |c| {
                *c.curves.spline_mut(ch) = crate::curve::Spline::identity();
            });
            return;
        }
        // Key controls. An unkeyed parameter takes one click to start
        // animating; a keyed one opens the menu that shapes, removes and
        // steps between its keys, because "what do I do with this key" has
        // more than one answer and a second click cannot express them all.
        for (param, rect) in &l.keys {
            if rect.contains(p) {
                if self.key_state(*param) == KeyState::Off {
                    self.key_here(*param);
                } else {
                    self.open_key_menu(*param, p);
                }
                return;
            }
        }
        // Ranged controls — geometry, rate, title size.
        for (f, track) in l.nums.clone() {
            if hit_slider(track, p) {
                self.drag = Some(Drag::Num(f));
                let frac = slider_value_at(track, p.x);
                self.set_num(f, f.at_frac(frac));
                return;
            }
        }
        // Picture-in-picture presets, and the way back to filling the frame.
        for (_, corner, rect) in l.pip.clone() {
            if rect.contains(p) {
                self.edit_selected(move |c| {
                    c.transform = crate::model::Transform::pip(PIP_SCALE, corner, PIP_INSET);
                });
                return;
            }
        }
        for (anchor, rect) in l.anchor.clone() {
            if rect.contains(p) {
                self.edit_selected(move |c| {
                    c.transform.anchor_x = anchor.0;
                    c.transform.anchor_y = anchor.1;
                });
                return;
            }
        }
        if let Some(rect) = l.reset_transform
            && rect.contains(p)
        {
            self.edit_selected(|c| c.transform = crate::model::Transform::default());
            return;
        }
        for (rate, rect) in l.speed_presets.clone() {
            if rect.contains(p) {
                let rev = self.selected_clip().map(|c| c.speed.reverse);
                if let Some(rev) = rev {
                    self.set_speed_selected(rate, rev);
                }
                return;
            }
        }
        if let Some(rect) = l.reverse
            && rect.contains(p)
        {
            let now = self
                .selected_clip()
                .map(|c| (c.speed.rate(), c.speed.reverse));
            if let Some((rate, rev)) = now {
                self.set_speed_selected(rate, !rev);
            }
            return;
        }
        // Colour key.
        if let Some(rect) = l.key_enable
            && rect.contains(p)
        {
            self.toggle_key_enabled();
            return;
        }
        for (col, rect) in l.key_colors.clone() {
            if rect.contains(p) {
                self.edit_selected(move |c| c.key.color = col);
                return;
            }
        }
        if let Some(rect) = l.key_pick
            && rect.contains(p)
        {
            // Only writes a colour when there really is a decoded frame
            // to read one from; with none the button is drawn disabled
            // and this is a no-op rather than a made-up sample.
            if let Some(col) = self.preview_pixel(0.5, 0.5) {
                self.edit_selected(move |c| c.key.color = col);
            }
            return;
        }
        // Mask.
        for (shape, rect) in l.mask_shapes.clone() {
            if rect.contains(p) {
                self.set_mask_shape(shape);
                return;
            }
        }
        if let Some(rect) = l.mask_fit
            && rect.contains(p)
        {
            let (src_aspect, frame_aspect) = self.selected_aspects();
            self.edit_selected(move |c| {
                let shape = c.mask.shape;
                let (feather, invert) = (c.mask.feather, c.mask.invert);
                c.mask = crate::model::Mask::over(shape, &c.transform, src_aspect, frame_aspect);
                c.mask.feather = feather;
                c.mask.invert = invert;
            });
            return;
        }
        if let Some(rect) = l.mask_invert
            && rect.contains(p)
        {
            self.edit_selected(|c| c.mask.invert = !c.mask.invert);
            return;
        }
        if let Some(rect) = l.mix_reset
            && rect.contains(p)
        {
            self.reset_selected_track_mix();
            return;
        }
        // Title properties.
        if let Some(rect) = l.title_text
            && rect.contains(p)
        {
            if let Some(m) = self.selected_media_item().map(|m| m.id) {
                self.begin_title_edit(m);
            }
            return;
        }
        for (align, rect) in l.title_align.clone() {
            if rect.contains(p) {
                self.edit_title(move |t| t.align = align);
                return;
            }
        }
        if let Some(rect) = l.title_font
            && rect.contains(p)
            && self.font_picker.is_none()
        {
            self.open_font_picker();
            return;
        }
        for (col, rect) in l.title_colors.clone() {
            if rect.contains(p) {
                self.edit_title(move |t| t.color = col);
                return;
            }
        }
        if let Some(rect) = l.title_bold
            && rect.contains(p)
        {
            self.edit_title(|t| t.bold = !t.bold);
            return;
        }
        if let Some(rect) = l.title_italic
            && rect.contains(p)
        {
            self.edit_title(|t| t.italic = !t.italic);
            return;
        }
        if l.split.contains(p) {
            self.split_selected();
            return;
        }
        if l.delete.contains(p) {
            self.delete_selected();
        }
    }

    /// A press inside the curve editor: grab the point under the pointer, or
    /// put a new one there.
    ///
    /// Adding on a plain click is what makes the curve editable at all — a
    /// separate "add point" button would need somewhere to add it, and the
    /// answer is always "where I clicked". A click too close to an existing
    /// point grabs that one instead, so points cannot be stacked.
    pub(super) fn press_curve(&mut self, canvas: Rect, p: Point) {
        let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else {
            return;
        };
        let ch = self.curve_channel;
        let (x, y) = curve_coords(canvas, p);
        let existing = clip.curves.spline(ch).point_near(x, y, POINT_GRAB);
        let idx = if let Some(i) = existing {
            i
        } else {
            // A new point is a discrete edit, so it commits now; the drag
            // that follows previews and commits again on release.
            let mut placed = None;
            self.edit_selected(|c| {
                placed = c.curves.spline_mut(ch).set_point(x, y);
            });
            if let Some(i) = placed {
                i
            } else {
                // Refused — too close to a neighbour for a point to mean
                // anything. Grab the neighbour rather than doing nothing.
                let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else {
                    return;
                };
                match clip.curves.spline(ch).point_near(x, y, POINT_GRAB * 2.0) {
                    Some(i) => i,
                    None => return,
                }
            }
        };
        self.drag = Some(Drag::CurvePoint { channel: ch, idx });
    }

    pub(super) fn press_export(&mut self, state: &AutoCommit, p: Point) {
        // Laid out from the current state, as the next frame will paint it —
        // see `chrome_theme`.
        let l = self.with_dialog_layout(super::VideoEditorApp::export_layout);
        match l.modal.hit(p) {
            // The close control, or a click outside the card, dismisses. A
            // render in flight is not abandoned by that: it keeps reporting
            // from the topbar chip and its own live region, and the chip is
            // the way back in here.
            Some(ModalHit::Close | ModalHit::Scrim) => {
                self.presentation.export_open = false;
            }
            Some(ModalHit::Action(0)) => {
                if self.export_running() {
                    // A render in flight: this is Cancel, not Close. Stop it
                    // and leave the dialog up so the outcome is visible here
                    // as well as in the chip.
                    if let Some(job) = self.export_job.as_ref() {
                        job.cancel();
                    }
                } else {
                    self.presentation.export_open = false;
                }
            }
            Some(ModalHit::Action(_)) => {
                // Matches what the button paints: a disabled Export refuses
                // rather than starting something that can only fail.
                if self.export_is_offered() {
                    self.request_export();
                }
                let _ = state;
            }
            Some(ModalHit::Card) => {
                // Only an option the body shows can be chosen: on a window too
                // short for even the compacted stack the groups run past the
                // body's clip, and a row nobody can see must not answer.
                let body = l.modal.body;
                let shown = |rect: &Rect| rect.intersect(body).contains(p);
                if let Some((res, _)) = l.resolution.iter().find(|(_, rect)| shown(rect)) {
                    self.export.resolution = *res;
                } else if let Some((c, _)) = l.container.iter().find(|(_, rect)| shown(rect)) {
                    self.export.container = *c;
                } else if let Some((f, _)) = l.fps.iter().find(|(_, rect)| shown(rect)) {
                    self.export.frame_rate = *f;
                }
            }
            None => {}
        }
    }
}

/// The go-to field: the toolkit's text field, set in the timecode's role and
/// admitting only what a timecode is written with, focused so the first key
/// lands in it.
pub(super) fn goto_field(initial: &str) -> ui::widget::TextInput {
    let mut field = ui::widget::TextInput::local()
        .with_char_filter(|c| c.is_ascii_digit() || c == ':' || c == '.')
        .with_kind(ui::widget::TextInputKind::Number)
        .with_text_role(TextRole::Title)
        .with_placeholder("0:00")
        .with_accessible_name("Timecode")
        .with_initial_value(initial);
    field.set_focus(true);
    field
}
