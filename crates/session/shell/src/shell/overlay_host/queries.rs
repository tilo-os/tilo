//! Geometry and state queries for hit testing, tests and shots.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Cached "+ Add workspace" tile rect, `Rect::ZERO` while the overview
    /// is hidden. Lets tests assert it's on screen and click its centre.
    pub fn add_workspace_rect_for_tests(&self) -> Rect {
        self.add_workspace_hit_rect()
    }

    /// The new-workspace row while the overview is open.
    ///
    /// The fade's first sample is still alpha 0, and that sample clears the
    /// painted hit cache so a closing overview cannot create a room. The
    /// row's geometry is the control: `list-targets` and a click both use
    /// it as soon as the overlay flag is up, including that first sample.
    pub(super) fn add_workspace_hit_rect(&self) -> Rect {
        if !self.overlay_flags().overview {
            return Rect::ZERO;
        }
        let content = self.overview_content_rect();
        let top = self.overview_list_top().max(content.y0);
        if top >= content.y1 {
            return Rect::ZERO;
        }
        let band = Rect::new(content.x0, top, content.x1, content.y1);
        let row = self
            .overview_add_workspace_rect_for(self.overview_cards().len())
            .intersect(band);
        if row.width() > 0.0 && row.height() > 0.0 {
            row
        } else {
            Rect::ZERO
        }
    }

    /// Test-only: a strip's `GroupRow` rect in screen-space. `None` if no
    /// row is registered for the strip, `Rect::ZERO` if not yet laid out.
    /// Used by cross-strip drag tests to pick a release point in another
    /// strip's band without re-walking the taffy tree. Mirrors the
    /// SAFETY contract on [`Self::groups_per_strip`].
    #[cfg(any(test, debug_assertions))]
    pub fn group_row_rect_for_tests(&self, strip_id: StripId) -> Option<Rect> {
        let &ptr = self.group_row_ptrs.get(&strip_id)?;
        // SAFETY: see field doc on `group_row_ptrs`.
        let row: &GroupRow = unsafe { &*ptr };
        Some(row.get_rect())
    }

    /// Screen-space rect of the content area (below the tab bar) of
    /// window group `group_id`, or `None` if no live row hosts it yet.
    /// The compositor's input router uses this to translate a cursor
    /// position into a foreign toplevel's surface-local coordinates,
    /// since the group's active tile rect *is* the surface's rect.
    ///
    /// SAFETY mirrors [`Self::groups_per_strip`].
    /// Where the open file chooser sits this frame: a sheet on the window
    /// that asked while that window is in front of its group and laid out,
    /// waiting out of sight otherwise; over the display in use when no
    /// window of the app asked.
    pub(crate) fn file_picker_place(&self) -> crate::shell::file_picker_overlay::PickerPlace {
        use crate::shell::file_picker_overlay::PickerPlace;
        let Some(tab) = self.file_picker_overlay.asking_tab() else {
            return PickerPlace::Display(self.overlay_area());
        };
        self.asking_window(tab)
            .map_or(PickerPlace::Hidden, |asking| PickerPlace::Window {
                window: asking.window,
                content: asking.content,
                display: self.surface_area(Some(asking.content.center())),
                focused: asking.focused,
            })
    }

    /// The laid-out window group showing `tab` in front.
    pub(super) fn asking_window(&self, tab: crate::shell::state::TabId) -> Option<AskingWindow> {
        self.group_row_ptrs.iter().find_map(|(&strip, &ptr)| {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            let group = row
                .groups
                .iter()
                .find(|group| group.active_tab_id() == tab)?;
            let window = group.get_rect();
            let content = group.active_host().map(ui::Widget::get_rect)?;
            (window.area() > 0.0 && content.area() > 0.0).then_some(AskingWindow {
                strip,
                group: group.id,
                window,
                content,
                focused: group.focused,
            })
        })
    }

    /// A press on the window an open file chooser waits on brings that
    /// window forward, so the keys that follow reach its sheet.
    pub(super) fn focus_asking_window_on_press(&mut self, event: &WindowEvent) {
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            return;
        }
        if let Some(tab) = self.file_picker_overlay.asking_tab()
            && let Some(asking) = self.asking_window(tab)
            && !asking.focused
            && self
                .last_cursor
                .is_some_and(|at| asking.window.contains(at))
        {
            self.focus_window_on_strip(asking.strip, asking.group, tab);
        }
    }

    pub fn active_tile_rect(&self, group_id: u64) -> Option<Rect> {
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            if let Some(group) = row.groups.iter().find(|g| g.id == group_id) {
                return group.active_host().map(ui::Widget::get_rect);
            }
        }
        None
    }

    /// The shared window grip overlays client content. Resolve it in laid-out
    /// screen coordinates, after strip panning, before forwarding client input.
    pub fn group_resize_hit(&self, group_id: u64, point: (f64, f64)) -> bool {
        self.group_resize_rects(group_id)
            .iter()
            .any(|rect| rect.contains(Point::new(point.0, point.1)))
    }

    /// Visible grip geometry (leading and trailing edge) shared by local
    /// routing and remote admission.
    pub fn group_resize_rects(&self, group_id: u64) -> Vec<Rect> {
        self.group_row_ptrs
            .values()
            .flat_map(|&ptr| {
                // SAFETY: see field doc on `group_row_ptrs`.
                let row: &GroupRow = unsafe { &*ptr };
                row.group_resize_rects(group_id)
            })
            .collect()
    }

    /// Every tile laid out on this device, with the tab it shows, in
    /// screen space: what peer pointers are anchored to and placed over
    /// (see [`crate::shell::peer_presence::PointerAnchor`]).
    ///
    /// SAFETY mirrors [`Self::groups_per_strip`].
    pub fn tab_tiles(&self) -> Vec<(u64, Rect)> {
        let mut tiles = Vec::new();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            for group in &row.groups {
                let Some(rect) = group.active_host().map(ui::Widget::get_rect) else {
                    continue;
                };
                if rect.width() > 0.0 && rect.height() > 0.0 {
                    tiles.push((group.active_tab_id(), rect));
                }
            }
        }
        tiles
    }

    /// Keep [`Self::tab_tiles`] and the pointer current in a handle a
    /// presence pump outside this tree can read — the hosted shell's frame
    /// tick, which is handed no host. Tiles refresh after every layout, the
    /// pointer on every move and when it leaves the window.
    pub fn share_desk(&mut self) -> crate::shell::peer_presence::SharedDesk {
        let desk = self.shared_desk.get_or_insert_default().clone();
        desk.set_tiles(self.tab_tiles());
        desk.set_pointer(self.last_cursor.map(|p| (p.x, p.y)));
        desk
    }

    /// SAFETY mirrors the rest of the `group_row_ptrs` accessors: pointers
    /// were captured from a `Box<GroupRow>` we own through the content
    /// tree, the host outlives the content, and `&self` here keeps the
    /// reconcile sweeps from racing the read.
    pub fn groups_per_strip(&self) -> GroupsPerStrip {
        let mut out = Vec::with_capacity(self.group_row_ptrs.len());
        for (&strip_id, &ptr) in &self.group_row_ptrs {
            // SAFETY: see field doc on `group_row_ptrs`.
            let row: &GroupRow = unsafe { &*ptr };
            let snapshot: Vec<(u64, Vec<u64>)> = row
                .groups
                .iter()
                .map(|g| {
                    let tab_ids: Vec<u64> = g.tabs().iter().map(|t| t.id).collect();
                    (g.id, tab_ids)
                })
                .collect();
            out.push((strip_id, snapshot));
        }
        out
    }

    /// Cached WS-settings tab-pill rect for `tab`, populated while the
    /// modal is visible. Every [`WsTab`] has a real pill, so every tab has
    /// a rect.
    pub fn ws_settings_tab_pill_rect(&self, tab: WsTab) -> Rect {
        self.workspace_settings_overlay.tab_pill_rect(tab)
    }

    /// The workspace-settings name field's rect, for harnesses that
    /// click it by name. Zero-area while the modal is closed.
    pub fn ws_settings_name_rect_for_tests(&self) -> Rect {
        if self.overlay_flags().ws_settings {
            self.ws_settings_name_rect()
        } else {
            Rect::ZERO
        }
    }

    /// One rect per workspace card in the overview, in tree order.
    /// Empty while the overview is closed.
    pub fn overview_row_rects_for_tests(&self) -> Vec<Rect> {
        if !self.overlay_flags().overview {
            return Vec::new();
        }
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        let count = chrome.borrow().tree().desktop.workspaces.len();
        (0..count).map(|i| self.overview_row_rect(i)).collect()
    }

    /// The peer-sync card's buttons, named `join` / `cancel` / `copy` /
    /// `paste`.
    pub fn peer_sync_prompt_rects_for_tests(&self) -> Vec<(&'static str, Rect)> {
        self.peer_sync_overlay.button_rects_for_tests()
    }

    pub fn peer_sync_snapshot(&self) -> PeerSyncSnapshot {
        self.peer_sync_overlay.snapshot()
    }

    pub fn peer_sync_card_visible(&self) -> bool {
        self.peer_sync_overlay.visible()
    }

    #[cfg(test)]
    pub fn set_peer_sync_snapshot_for_tests(&mut self, snapshot: PeerSyncSnapshot) {
        self.peer_sync_overlay.set_snapshot_for_tests(snapshot);
    }

    /// The roster's Invite button, as painted last frame (not recomputed) —
    /// `Rect::ZERO` when closed or not permitted, so a test click on a
    /// missing button fails instead of hitting the origin.
    pub fn collaborators_invite_rect_for_tests(&self) -> Rect {
        self.collaborators_overlay.invite_rect()
    }

    /// Revoke-button rects painted by `Self::draw_ws_settings` while
    /// the Permissions tab is active, each keyed by the capability id it
    /// revokes. Empty whenever the modal is closed or another tab is
    /// active.
    pub fn permissions_revoke_rects(&self) -> &[(String, Rect)] {
        self.workspace_settings_overlay.revoke_rects()
    }

    /// Grant-composer control rects painted while the Permissions tab is
    /// active. `None` when the composer isn't on screen.
    pub fn grant_composer_rects(&self) -> Option<&GrantComposerRects> {
        self.workspace_settings_overlay.grant_composer_rects()
    }

    /// Per-member control rects painted while the Members tab is active:
    /// `(member, role chip, Remove button)`.
    pub fn member_control_rects(&self) -> &[(crate::shell::state::UserId, Rect, Rect)] {
        self.workspace_settings_overlay.member_control_rects()
    }

    /// Bounds for the currently open member-role chooser or destructive
    /// confirmation sheet, in visible button order.
    pub fn member_sheet_option_rects_for_tests(&self) -> &[Rect] {
        self.workspace_settings_overlay.member_sheet_option_rects()
    }

    pub fn member_sheet_is_open_for_tests(&self) -> bool {
        self.workspace_settings_overlay.member_sheet_is_open()
    }

    /// The command-palette visibility flag, for tests that only hold the
    /// host through `Box<dyn Widget>`.
    pub fn palette_visible(&self) -> bool {
        self.palette_visible
    }

    /// Last pointer position this host knows about, or `None` before
    /// anything has moved a pointer through the session.
    pub fn pointer_position(&self) -> Option<Point> {
        self.last_cursor
    }

    /// Record pointer position for a motion this widget tree never sees:
    /// a `CursorMoved` over a foreign client goes straight to it, so
    /// `last_cursor` (which hit-tests the position-less `MouseInput`)
    /// would otherwise go stale inside that window.
    pub fn note_pointer(&mut self, p: Point) {
        self.last_cursor = Some(p);
        // Resting on the banner stack holds its banners; leaving it into the
        // window under it lets them go, though that window takes the motion.
        if let Some(chrome) = self.chrome.clone()
            && self
                .notification_stack
                .note_pointer(&ChromeNotifications::new(&chrome), Some(p))
        {
            chrome.borrow_mut().bump();
        }
        // The strip switcher reads the path toward the ribbon over the
        // windows it crosses, as the top-edge reveals do.
        self.strip_switcher_pointer_moved(p);
        // The maximized window's bar reads the pointer's path even while the
        // client under it takes the motion (`top_edge_reveal`).
        self.with_maximized_group(|group| {
            group.note_maximized_pointer(p);
        });
        // So does the top bar the person hides, by the same model.
        if let Some((display, bar)) = self.top_bar_reveal_geometry() {
            let now = Instant::now();
            if self.top_bar_reveal.on_motion(p, display, bar, now) {
                self.sync_top_bar_presence(now);
            }
        }
    }

    /// Retire raw direct-touch contacts at a compositor ownership boundary
    /// without interpreting the disappearance as a completed shell gesture.
    pub fn cancel_direct_touch_gestures(&mut self) {
        self.touch_gestures.cancel_all();
    }

    /// Whether the strip switcher takes the keyboard: opened by Super+S, or
    /// a strip name is being typed. Read alongside [`Self::palette_visible`]
    /// so keys are taken from the focused client only while the shell has
    /// something up that uses them.
    pub fn strip_switcher_takes_keys(&self) -> bool {
        self.strip_switcher.takes_keys()
    }

    /// Whether the pointer is on the open strip switcher, its ribbon or one
    /// of its labels: its presses, wheel and motion are the shell's, not the
    /// window's beneath it.
    pub fn strip_switcher_captures_pointer(&self) -> bool {
        self.strip_switcher.is_open()
            && self.last_cursor.is_some_and(|p| {
                self.strip_switcher_zone().contains(p) || self.strip_switcher.target_at(p).is_some()
            })
    }

    /// Whether the last draw found the layout it was handed isn't what
    /// it needs (the filter band growing a row is only measurable by
    /// painting). A peek, not a take, so a caller that paints once (a
    /// screenshot) can notice it must lay out and paint again.
    pub fn structure_dirty(&self) -> bool {
        self.palette_layout_dirty
    }

    /// The palette's query, the id Enter would activate, and the rows on
    /// offer — otherwise a keystroke-driven harness can't tell which row
    /// is selected without "press Enter and see what changed".
    pub fn palette_snapshot(&self) -> (bool, String, Option<String>, Vec<(String, String)>) {
        (
            self.palette_visible,
            self.palette.query().to_string(),
            self.palette
                .selected_id()
                .map(std::string::ToString::to_string),
            self.palette
                .visible_entries()
                .into_iter()
                .map(|e| (e.id.clone(), e.label.clone()))
                .collect(),
        )
    }

    /// Result kinds the palette is filtered to, by stable kind id. Empty
    /// means every kind is on offer — otherwise "filtered to apps" and
    /// "a query only apps matched" produce the same row list.
    pub fn palette_kind_filter(&self) -> Vec<String> {
        self.active_kind_filter
            .iter()
            .map(|k| k.id().to_string())
            .collect()
    }

    /// [`GroupRow::canvas_pad`] of `strip`'s row, in layout pixels.
    pub fn strip_canvas_pad(&self, strip: StripId) -> f64 {
        self.group_row_ptrs.get(&strip).map_or(0.0, |&ptr| {
            // SAFETY: same contract as every other `group_row_ptrs` accessor.
            let row: &GroupRow = unsafe { &*ptr };
            row.canvas_pad()
        })
    }

    /// Where on screen `strip` pans when it is drawn in canvas coordinates:
    /// its row with the shadows drawn past it.
    pub fn strip_pan_rect(&self, strip: StripId) -> Option<vello::kurbo::Rect> {
        Some(GroupRow::drawn_bounds(self.strip_row_rect(strip)?))
    }

    /// The step the presenter needs strip pans drawn on, in layout pixels
    /// (`1 / scale` for whole device pixels), or `None` for exact pans
    /// ([`GroupRow::set_scroll_quantum`]).
    pub fn set_strip_pan_quantum(&mut self, quantum: Option<f64>) {
        self.strip_pan_quantum = quantum;
    }

    /// The strips with a row on screen, top to bottom.
    pub(super) fn rows_on_screen(&self) -> Vec<(StripId, *mut GroupRow)> {
        let mut rows: Vec<(f64, StripId, *mut GroupRow)> = self
            .group_row_ptrs
            .iter()
            .filter_map(|(&strip, &row)| Some((self.strip_row_rect(strip)?.y0, strip, row)))
            .collect();
        rows.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.0.cmp(&b.1.0)));
        rows.into_iter()
            .map(|(_, strip, row)| (strip, row))
            .collect()
    }

    /// Every strip's horizontal pan, keyed by strip id, paired with its
    /// max — on a strip whose content fits the display the pan is pinned
    /// at zero, so "the viewport didn't move" needs the bound to mean
    /// anything.
    pub fn strip_scroll_offsets(&self) -> Vec<(u64, f64, f64)> {
        let mut out: Vec<(u64, f64, f64)> = self
            .group_row_ptrs
            .iter()
            .map(|(sid, &ptr)| {
                // SAFETY: same contract as every other `group_row_ptrs`
                // accessor — the pointer is captured from a `Box<GroupRow>`
                // owned through the content tree, and the host outlives it.
                let row: &GroupRow = unsafe { &*ptr };
                (sid.0, row.drawn_scroll_x(), row.max_scroll_x())
            })
            .collect();
        out.sort_by_key(|(sid, _, _)| *sid);
        out
    }

    /// Pan every strip to `scroll` layout pixels, as a direct pan does.
    #[cfg(test)]
    pub(crate) fn set_strip_scroll_for_test(&mut self, scroll: f64) {
        self.strip_pan_active = true;
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: as in `strip_row_rect`, and `&mut self` makes the
            // access exclusive.
            unsafe { (&mut *ptr).set_scroll_x_for_test(scroll) };
        }
    }

    /// Where `strip` is drawn on screen, as laid out last. `None` while the
    /// strip has no row yet or its row is collapsed (another strip's window
    /// is fullscreen).
    pub fn strip_row_rect(&self, strip: StripId) -> Option<vello::kurbo::Rect> {
        let &ptr = self.group_row_ptrs.get(&strip)?;
        // SAFETY: same contract as every other `group_row_ptrs` accessor —
        // the pointer is captured from a `Box<GroupRow>` owned through the
        // content tree, and the host outlives it.
        let row: &GroupRow = unsafe { &*ptr };
        let rect = ui::widget::Widget::get_rect(row);
        (rect.area() > 0.0).then_some(rect)
    }

    /// The rows on offer with their subtitles — [`Self::palette_snapshot`]
    /// carries labels only, not the badge/source-marker text a harness
    /// needs to tell "already have this" from "fetches this" apart.
    pub fn palette_rows_detailed(&self) -> Vec<(String, String, Option<String>)> {
        self.palette
            .visible_entries()
            .into_iter()
            .map(|e| (e.id.clone(), e.label.clone(), e.description.clone()))
            .collect()
    }

    /// Focus the palette's query field. `open_palette_with_scope` does
    /// this; harnesses that force `palette_visible` need it too or they
    /// capture the unfocused field.
    pub fn set_palette_focus(&mut self, focused: bool) {
        self.palette.set_focus(focused);
    }

    /// The palette owns text input while open, including when its host
    /// routes keys directly without changing the native widget focus.
    pub fn palette_ime_field(&self) -> Option<ui::ime::ImeWindowState> {
        (self.palette_visible && self.palette.is_focused())
            .then(|| ui::ime::ImeWindowState::from_widget(&self.palette))
            .flatten()
    }

    pub(crate) fn palette_ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.palette.ime_edit_fingerprint(key)
    }

    /// Force-set the palette visibility, so headless harnesses can drive
    /// the palette scene without a synthetic Ctrl+K.
    pub fn set_palette_visible(&mut self, v: bool) {
        self.palette_visible = v;
    }

    #[cfg(test)]
    pub(crate) fn fixture_provider_ids(&self) -> Vec<&str> {
        self.search_registry
            .providers_iter()
            .map(crate::shell::search::SearchProvider::id)
            .collect()
    }

    /// The room record index the records provider reads and writes.
    /// Shared, not copied — a caller referencing a record here changes
    /// what the next keystroke finds.
    pub fn room_records(
        &self,
    ) -> std::rc::Rc<std::cell::RefCell<crate::shell::room_records::RoomRecords>> {
        std::rc::Rc::clone(&self.room_records)
    }

    /// Runs the search fan-out directly, for `bin/shots` after forcing
    /// the palette open — scenes bypass the per-keystroke refresh.
    pub fn refresh_search_results_for_shots(
        &mut self,
        doc: &mut automerge::AutoCommit,
        root: &ObjId,
    ) {
        // Drop the no-op short-circuit, or an empty-query scene matches
        // the initially-empty `last_search_query` and runs no provider.
        self.last_search_query = "\u{0}not-yet-refreshed\u{0}".to_string();
        self.refresh_search_results(doc, root);
    }

    /// Builds a headless [`ActionCatalog`](crate::shell::search::action_api::ActionCatalog)
    /// over the same provider registry that feeds the Ctrl+K palette, so a
    /// script or LLM navigator can drive it with no scene. `visibility` is
    /// the permission seam — a restricted caller only sees what it may.
    pub fn action_catalog(
        &self,
        query: &str,
        chrome: &crate::shell::chrome_state::ChromeState,
        doc: &automerge::AutoCommit,
        root: &ObjId,
        visibility: &dyn crate::shell::search::action_api::ActionVisibility,
    ) -> crate::shell::search::action_api::ActionCatalog {
        // A caller that cannot see the screen cannot see the palette's scope
        // pills either: whichever pill a person last left selected must not
        // narrow what an agent or script is told exists. Visibility is the
        // capability gate's alone.
        crate::shell::search::action_api::ActionCatalog::build(
            &self.search_registry,
            query,
            crate::shell::chrome_state::SearchScope::All,
            None,
            chrome,
            doc,
            root,
            visibility,
        )
    }

    pub fn action_catalog_query(
        &self,
        args: &crate::shell::search::QueryArgs<'_>,
        visibility: &dyn crate::shell::search::action_api::ActionVisibility,
        include_programmatic: bool,
    ) -> crate::shell::search::action_api::ActionCatalog {
        crate::shell::search::action_api::ActionCatalog::build_query(
            &self.search_registry,
            args,
            visibility,
            include_programmatic,
        )
    }

    /// Resolve a previously enumerated action by its stable address. Dynamic
    /// providers can preserve their retained discovery page while
    /// revalidating the route used to invoke it.
    pub fn exact_action_catalog(
        &self,
        provider: &str,
        id: &str,
        chrome: &crate::shell::chrome_state::ChromeState,
        doc: &automerge::AutoCommit,
        root: &ObjId,
        visibility: &dyn crate::shell::search::action_api::ActionVisibility,
    ) -> crate::shell::search::action_api::ActionCatalog {
        // An exact provider/id address is already a deliberate scope. Do not
        // let the interactive palette's temporary "This app" filter hide a
        // stable capability such as session/logout from automation or agents.
        let scope = crate::shell::chrome_state::SearchScope::All;
        crate::shell::search::action_api::ActionCatalog::build_exact(
            &self.search_registry,
            &crate::shell::search::action_api::ResourceRef::new(provider, id),
            scope,
            None,
            chrome,
            doc,
            root,
            visibility,
        )
    }

    pub fn action_catalog_for_resource(
        &self,
        resource: &crate::shell::search::action_api::ResourceRef,
        chrome: &crate::shell::chrome_state::ChromeState,
        doc: &automerge::AutoCommit,
        root: &ObjId,
        visibility: &dyn crate::shell::search::action_api::ActionVisibility,
    ) -> crate::shell::search::action_api::ActionCatalog {
        // A resource address is already a deliberate scope. The interactive
        // palette's temporary "This app" filter must not hide a stable
        // capability such as session/logout from automation or agents.
        crate::shell::search::action_api::ActionCatalog::build_for_resource(
            &self.search_registry,
            resource,
            crate::shell::chrome_state::SearchScope::All,
            None,
            chrome,
            doc,
            root,
            visibility,
        )
    }

    /// The command-palette query buffer, for the `shots` harness to
    /// assert a synthetic `Type("…")` landed.
    pub fn palette_query(&self) -> &str {
        self.palette.query()
    }

    /// Force-set the query buffer, for `shots` to seed a query while the
    /// palette is closed without round-tripping synthetic key events.
    pub fn set_palette_query(&mut self, q: &str) {
        self.palette.set_query(q);
    }

    /// The rooms held through the web, which the overview offers to leave:
    /// the network's, or what a screenshot scene stood in.
    pub(super) fn site_rooms(&self) -> Vec<crate::shell::network::SiteRoom> {
        self.site_rooms_for_shots
            .clone()
            .unwrap_or_else(|| self.peer_sync.site_rooms())
    }

    /// What the switcher says under workspace `idx` when it is a site's:
    /// the site it comes from (named as the community's when it is this
    /// computer's community), then how many people are here when presence
    /// knows. A site's workspace keeps no roster here, so its access line
    /// would say no one has access.
    pub(super) fn overview_site_line(&self, idx: usize) -> Option<String> {
        let (room, here) = {
            let state = self.chrome.as_ref()?.borrow();
            let (workspace, room, mut here): (_, _, Vec<String>) = {
                let tree = state.tree();
                let workspace = tree.desktop.workspaces.get(idx)?;
                (
                    workspace.id,
                    workspace.room,
                    workspace
                        .collaborators
                        .iter()
                        .map(|person| person.id.clone())
                        .collect(),
                )
            };
            // A workspace still joining its site's room says so, and one
            // whose join failed says why.
            if let Some(join) = self
                .peer_sync
                .site_joins()
                .into_iter()
                .find(|join| join.workspace == Some(workspace))
            {
                return Some(match join.progress {
                    crate::shell::network::JoinProgress::Joining => t("directory-joining"),
                    crate::shell::network::JoinProgress::Failed(reason) => {
                        t_args("directory-join-failed-line", &[("error", reason.into())])
                    }
                });
            }
            here.retain(|id| *id != state.local_user_id);
            here.sort_unstable();
            here.dedup();
            (room, here.len())
        };
        let site = self
            .site_rooms()
            .into_iter()
            .find(|site| site.room == room)?;
        let host = site.site.unwrap_or_else(|| t("overview-site-web"));
        let origin = if site.community {
            t_args("overview-site-community", &[("site", host.into())])
        } else {
            host
        };
        Some(if here == 0 {
            origin
        } else {
            let here = t_args("overview-site-here", &[("count", here.into())]);
            format!("{origin} · {here}")
        })
    }

    /// Stand `rooms` in as the rooms held through the web, for a screenshot
    /// scene without a network.
    pub fn set_site_rooms_for_shots(&mut self, rooms: Vec<crate::shell::network::SiteRoom>) {
        self.site_rooms_for_shots = Some(rooms);
    }

    /// Open the join field with `typed` in it, as a person typing a site.
    pub fn prompt_join_for_shots(&mut self, typed: &str) {
        self.peer_sync_overlay.prompt_connect();
        let mut snapshot = self.peer_sync_overlay.snapshot();
        snapshot.connect_input = Some(typed.to_owned());
        snapshot.status = Some(crate::shell::i18n::t("peer-sync-paste-token"));
        self.peer_sync_overlay.set_snapshot_for_shots(snapshot);
    }

    /// Open "Pair a new device" showing `link`.
    pub fn show_pairing_for_shots(&mut self, link: &str) {
        self.peer_sync_overlay.prompt_pair();
        self.peer_sync_overlay.show_pairing(link.to_owned());
    }

    /// Fast-forwards the overview crossfade so a headless single-frame
    /// render sees the settled layer, not the alpha-zero start.
    pub fn force_overview_settled(&mut self, target: bool) {
        self.overview_zoom.snap(target);
        self.overview_naming = None;
        // Same seeding the animated open does, so a single-frame paint
        // matches what a real open leaves behind.
        self.reseed_overview_selection(target);
    }

    /// Start the keyboard on the current room each time the overview
    /// opens, and forget any press when it closes.
    pub(super) fn reseed_overview_selection(&mut self, open: bool) {
        self.overview_selection = if open {
            self.overview_here_index()
        } else {
            None
        };
        self.overview_window_selection = None;
        self.overview_window_interrupted = false;
        self.overview_press = None;
    }

    /// Single-frame settle for the palette's entry animation, so a
    /// one-frame headless scene doesn't land at alpha 0 — which is
    /// indistinguishable from no overlay at all. Test-only.
    pub fn force_palette_settled(&mut self) {
        self.palette_was_visible = self.palette_visible;
        self.palette_transition.snap(self.palette_visible);
    }

    /// Quick Settings reads nothing from the machine this host runs on: a
    /// capture shows the state it is given, never the renderer's networks,
    /// accessories or battery.
    pub fn disable_quick_settings_os_reads(&mut self) {
        self.qs_refresh.disable_os_reads();
    }

    /// See [`Self::force_palette_settled`].
    pub fn force_quick_settings_settled(&mut self) {
        if !self.overlay_flags().quick_settings {
            return;
        }
        // This hook replaces the normal draw-time rising edge. Mirror the
        // user-visible open contract completely: keyboard traversal starts
        // on Edit instead of inheriting stale focus from a previous open.
        if !self.qs_was_open {
            self.qs_root.reset_focus();
            self.refresh_quick_settings_summaries();
        }
        self.qs_was_open = true;
        self.qs_motion = qs_motion::QsMotion::opened();
        // An open section stands open too, not at the start of its motion.
        if let Some(chrome) = self.chrome.clone() {
            self.refresh_qs_hosted_rows();
            self.qs_root
                .settle_sections(&chrome.borrow().quick_settings);
        }
    }

    /// Fixes the quick-settings visual to an in-flight opening pose for a
    /// deterministic capture. Ordinary UI paths must use the live transition.
    #[doc(hidden)]
    pub fn set_quick_settings_capture_openness(&mut self, permille: u16) {
        if !self.overlay_flags().quick_settings {
            return;
        }
        self.qs_was_open = true;
        self.qs_motion = qs_motion::QsMotion::held_at(permille);
    }

    /// Screenshot-driver hook for the Quick Settings edit surface.
    pub fn force_quick_settings_edit_for_visual_test(&mut self, dragging: bool) {
        if !self.overlay_flags().quick_settings {
            return;
        }
        if dragging {
            let rect = self.quick_settings_rect();
            self.qs_root.start_edit_drag_for_visual_test(rect);
        } else {
            self.qs_root.set_edit_mode_for_visual_test(true);
        }
    }
}
