//! Overview layout, selection and hit testing.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// Full ordered list of scopes shown in the banner. Includes
    /// [`SearchScope::ActiveApp`] only when there's a currently focused
    /// tab — otherwise the pill would render an empty label.
    pub(super) fn search_scope_list(&self) -> Vec<crate::shell::chrome_state::SearchScope> {
        use crate::shell::chrome_state::SearchScope;
        let mut out = Vec::with_capacity(3);
        if self
            .chrome
            .as_ref()
            .and_then(|c| c.borrow().active_app_title())
            .is_some()
        {
            out.push(SearchScope::ActiveApp);
        }
        out.push(SearchScope::Workspace);
        out.push(SearchScope::All);
        out
    }

    /// Label for the i-th scope pill. [`SearchScope::ActiveApp`] names
    /// the focused app (`Notes app`), truncated to `MAX_APP_PILL_CHARS`
    /// — "app" is explicit because the kind-filter strip above uses the
    /// same names for a different axis.
    pub(crate) fn search_scope_pill_label(
        &self,
        scope: crate::shell::chrome_state::SearchScope,
    ) -> String {
        use crate::shell::chrome_state::SearchScope;
        const MAX_APP_PILL_CHARS: usize = 14;
        if let Some(resource_label) = self.chrome.as_ref().and_then(|chrome| {
            let state = chrome.borrow();
            (state.search_scope == scope)
                .then(|| {
                    state
                        .search_resource_scope
                        .as_ref()
                        .map(|resource| resource.label.clone())
                })
                .flatten()
        }) {
            const MAX_RESOURCE_PILL_CHARS: usize = 18;
            let label = if resource_label.chars().count() > MAX_RESOURCE_PILL_CHARS {
                let head: String = resource_label
                    .chars()
                    .take(MAX_RESOURCE_PILL_CHARS)
                    .collect();
                format!("{head}…")
            } else {
                resource_label
            };
            return format!("‹ {label}");
        }
        match scope {
            SearchScope::ActiveApp => {
                let Some(raw) = self
                    .chrome
                    .as_ref()
                    .and_then(|c| c.borrow().active_app_title())
                else {
                    return SearchScope::ActiveApp.localized_label();
                };
                let title = if raw.chars().count() > MAX_APP_PILL_CHARS {
                    let head: String = raw.chars().take(MAX_APP_PILL_CHARS).collect();
                    format!("{head}…")
                } else {
                    raw
                };
                // An app already called "… app" doesn't get told twice.
                if title.to_lowercase().ends_with(" app") {
                    title
                } else {
                    t_args("host-search-scope-app", &[("title", title.into())])
                }
            }
            other => other.localized_label(),
        }
    }

    /// The overview's model of every room, in tree order.
    ///
    /// Geometry and paint both read it, so the panel can never be sized
    /// for one set of rooms and painted with another.
    pub(super) fn overview_cards(&self) -> Vec<tilo_desktop::overview::RoomCard> {
        match self.chrome.as_ref() {
            Some(chrome) => {
                let state = chrome.borrow();
                let local_user = state.active_local_user();
                crate::shell::overview::room_cards(&state.tree(), &local_user)
            }
            None => Vec::new(),
        }
    }

    /// Index of the room this session is in, if the tree named one.
    pub(super) fn overview_here_index(&self) -> Option<usize> {
        self.overview_cards().iter().position(|c| c.is_current)
    }

    /// How many share rows the "shared out of here" list draws. None when
    /// the room shares nothing: the section is left out rather than
    /// standing in the overview as a permanent empty row.
    pub(super) fn overview_share_rows(card: &tilo_desktop::overview::RoomCard) -> usize {
        let shown = card.shares.len().min(OVERVIEW_SHARES_MAX_ROWS);
        let overflow = usize::from(card.shares.len() > shown);
        shown.saturating_add(overflow)
    }

    /// Height of the "shared out of here" section: its caption and rows,
    /// or nothing when the room shares nothing.
    pub(super) fn overview_shares_height(&self) -> f64 {
        let cards = self.overview_cards();
        let rows = self
            .overview_here_index()
            .and_then(|i| cards.get(i))
            .map_or(0, Self::overview_share_rows);
        if rows == 0 {
            return 0.0;
        }
        let m = self.overview_metrics;
        m.caption_h + f64_of_usize(rows) * m.share_row_h
    }

    /// Height of the "you are here" block, including its share list.
    pub(super) fn overview_here_block_height(&self) -> f64 {
        self.overview_metrics.here_h
            + self.overview_windows_row_height()
            + self.overview_shares_height()
    }

    /// Every window in the overview's row, drawn live for this frame: a
    /// client from its current buffer, a shell-drawn window from a live
    /// paint of it made now (the same path a screenshot of it takes).
    pub(super) fn overview_live_thumbnails(
        &mut self,
        windows: &[tilo_desktop::window_switcher::SwitcherWindow],
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) -> crate::shell::window_previews::LiveThumbnails {
        let mut live = crate::shell::window_previews::LiveThumbnails::default();
        let locked = self
            .chrome
            .as_ref()
            .and_then(|chrome| chrome.try_borrow().ok().map(|state| state.session_locked()))
            .unwrap_or(true);
        if locked {
            return live;
        }
        for window in windows {
            let tilo_desktop::window_switcher::PreviewSource::Painted { capture_id } =
                &window.preview
            else {
                continue;
            };
            let mut painted = Scene::new();
            let rects = {
                let _capture = crate::shell::capture_protection::CapturePaint::begin();
                self.draw_capture_targets(
                    &ui::widget::LiveCapture {
                        target_ids: std::slice::from_ref(capture_id),
                        extent: ui::widget::CaptureExtent::FullApp,
                        complete_semantic_content: false,
                    },
                    &mut ui::paint::PaintCtx::new(
                        &mut painted,
                        Affine::IDENTITY,
                        text_renderer,
                        theme,
                    ),
                    &PresenceCtx::default(),
                )
            };
            if let Some(rect) = rects.first().copied().filter(|rect| rect.area() > 0.0) {
                live.painted.insert(
                    window.address.tab,
                    crate::shell::window_previews::PaintedWindow {
                        scene: painted,
                        window: rect,
                    },
                );
            }
        }
        live
    }

    pub(super) fn overview_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let count = self.overview_cards().len();
        let windows = self.overview_row_windows().len();
        let create = count.saturating_add(usize::from(windows > 0));
        match path {
            [] => Some(self.overview_bounds()),
            [index] if *index < count => Some(self.overview_row_rect(*index)),
            [index] if *index == count && windows > 0 => self.overview_window_list_rect(),
            [index, window] if *index == count && *window < windows => {
                Some(self.overview_window_tile_rect(*window))
            }
            [index] if *index == create => Some(self.overview_add_workspace_rect_for(count)),
            _ => None,
        }
    }

    pub(super) fn focus_overview_accessibility(&mut self, path: &[usize]) -> bool {
        if !self.overlay_flags().overview || self.palette_visible {
            return false;
        }
        let count = self.overview_cards().len();
        let windows = self.overview_row_windows().len();
        let create = count.saturating_add(usize::from(windows > 0));
        match path {
            [index, window] if *index == count && *window < windows => {
                self.select_overview_window(*window);
                return true;
            }
            [index] if *index < count => self.overview_selection = Some(*index),
            [index] if *index == create => self.overview_selection = Some(count),
            _ => return false,
        }
        self.overview_window_selection = None;
        self.overview_window_interrupted = false;
        self.scroll_overview_selection_into_view();
        self.bump_chrome();
        true
    }

    /// Go to a window from the overview: focus it and close the overview.
    pub(super) fn activate_overview_window(&mut self, tab: u64) -> bool {
        let Some(address) = self
            .overview_row_windows()
            .into_iter()
            .find(|window| window.address.tab == tab)
            .map(|window| window.address)
        else {
            return false;
        };
        let focused = self.focus_switcher_window(address);
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            state.overlays.overview = false;
            state.bump();
        }
        focused
    }

    /// Close a window from the overview, through the one tab lifecycle so
    /// it can be reopened. The overview stays up.
    pub(super) fn close_overview_window(&mut self, tab: u64) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let closed = crate::shell::window_actions::close_tab(&mut chrome.borrow_mut(), tab).is_ok();
        self.overview_window_selection = None;
        self.overview_window_interrupted = false;
        closed
    }

    /// Move a window to room `room` (tree order). The overview stays up so
    /// the window can be seen leaving the row. A client window asks first
    /// what crosses, on the transfer sheet.
    pub(super) fn move_overview_window(&mut self, tab: u64, room: usize) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let mut state = chrome.borrow_mut();
        let Some(target) = state
            .tree()
            .desktop
            .workspaces
            .get(room)
            .map(|workspace| workspace.id)
        else {
            return false;
        };
        match crate::shell::window_actions::move_tab_to_workspace(&mut state, tab, target) {
            Ok(_) => {
                drop(state);
                self.overview_window_selection = None;
                self.overview_window_interrupted = false;
                true
            }
            Err(err) => {
                state.push_warning_notification(
                    crate::shell::i18n::t("overview-window-move-failed"),
                    err.user_message(),
                );
                false
            }
        }
    }

    /// How many rows the "go to" list holds: every room but the one you
    /// are in, plus the trailing "New room" row.
    pub(super) fn overview_list_slots(&self) -> usize {
        let n = self.overview_cards().len();
        n.saturating_sub(usize::from(self.overview_here_index().is_some()))
            .saturating_add(2)
    }

    /// Height the list would take if nothing clipped it.
    pub(super) fn overview_content_height(&self) -> f64 {
        let k = f64_of_usize(self.overview_list_slots());
        let m = self.overview_metrics;
        (k * (m.room_row_h + m.room_row_gap) - m.room_row_gap).max(0.0)
    }

    pub(super) fn overview_uses_full_window(&self) -> bool {
        ui::widget::modal_dialog::uses_full_window(self.overlay_area(), &self.overview_layout_theme)
    }

    pub(super) fn overview_panel_width(&self) -> f64 {
        let area = self.overlay_area();
        if self.overview_uses_full_window() {
            area.width().max(0.0)
        } else if self.overview_beside_picture() {
            overview_grid::COLUMN_WIDTH_PX
        } else {
            OVERVIEW_PANEL_WIDTH_PX
                .min(area.width() - self.overview_metrics.margin * 2.0)
                .max(0.0)
        }
    }

    pub(super) fn resolve_overview_layout(&mut self, theme: &Theme) {
        let metrics = OverviewMetrics::for_theme(theme);
        let viewport = self.overlay_area();
        if metrics == self.overview_metrics && viewport == self.overview_layout_viewport {
            return;
        }
        let had_layout = self.overview_layout_viewport.area() > 0.0;
        self.overview_layout_theme.clone_from(theme);
        self.overview_layout_viewport = viewport;
        self.overview_metrics = metrics;
        self.overview_scroll_y = self
            .overview_scroll_y
            .clamp(0.0, self.overview_max_scroll_y());
        if had_layout {
            self.scroll_overview_selection_into_view();
        }
    }

    /// On a constrained output the whole body scrolls. A pinned header must
    /// never consume the list's last reachable action row.
    pub(super) fn overview_scrolls_whole_panel(&self) -> bool {
        self.overview_uses_full_window()
            || self.overview_content_rect().height()
                - self.overview_here_block_height()
                - self.overview_metrics.section_gap
                - self.overview_metrics.caption_h
                < self.overview_metrics.room_row_h
    }

    pub(super) fn overview_content_top(&self) -> f64 {
        self.overview_content_rect().y0
            - if self.overview_scrolls_whole_panel() {
                self.overview_scroll_y
            } else {
                0.0
            }
    }

    /// The panel: a floating surface centred in the viewport, sized to what
    /// it has to say and capped so it never runs off screen. Same width and
    /// centring as the workspace-settings modal — the same kind of object.
    pub(super) fn overview_panel_rect(&self) -> Rect {
        let m = self.overview_metrics;
        let area = self.overlay_area();
        if self.overview_uses_full_window() {
            return area;
        }
        let w = self.overview_panel_width();
        let natural = m.pad * 2.0
            + self.overview_here_block_height()
            + m.section_gap
            + m.caption_h
            + self.overview_content_height();
        let max_h = area.height() - m.margin * 2.0;
        let h = natural.min(max_h).max(0.0);
        let cy = f64::midpoint(area.y0, area.y1);
        // Beside the picture of this workspace it keeps to the display's
        // trailing edge; alone, it is centred like any modal panel.
        let cx = if self.overview_beside_picture() {
            area.x1 - m.margin - w / 2.0
        } else {
            f64::midpoint(area.x0, area.x1)
        };
        Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
    }

    /// The band inside the panel that content is laid out in.
    pub(super) fn overview_content_rect(&self) -> Rect {
        self.overview_panel_rect().inset(-self.overview_metrics.pad)
    }

    /// The "you are here" block — the current room's identity, access
    /// and people, with what it shares beneath.
    pub(super) fn overview_here_rect(&self) -> Rect {
        let c = self.overview_content_rect();
        Rect::new(
            c.x0,
            self.overview_content_top(),
            c.x1,
            self.overview_content_top()
                + self.overview_metrics.here_h
                + self.overview_windows_row_height(),
        )
    }

    /// Y of the "Shared out of here" caption.
    pub(super) fn overview_shares_top(&self) -> f64 {
        self.overview_here_rect().y1
    }

    /// Y of the "Go to" caption above the room list.
    pub(super) fn overview_list_caption_top(&self) -> f64 {
        self.overview_content_top()
            + self.overview_here_block_height()
            + self.overview_metrics.section_gap
    }

    /// Y the first room row starts at, before scrolling.
    pub(super) fn overview_list_top(&self) -> f64 {
        self.overview_list_caption_top() + self.overview_metrics.caption_h
    }

    /// Height of the band the room list scrolls inside.
    pub(super) fn overview_viewport_height(&self) -> f64 {
        if self.overview_scrolls_whole_panel() {
            self.overview_content_rect().height().max(0.0)
        } else {
            (self.overview_content_rect().y1 - self.overview_list_top()).max(0.0)
        }
    }

    /// Maximum legal `overview_scroll_y` — the list's overflow below the
    /// visible band. Zero when every room fits.
    pub(super) fn overview_max_scroll_y(&self) -> f64 {
        let header = if self.overview_scrolls_whole_panel() {
            self.overview_here_block_height()
                + self.overview_metrics.section_gap
                + self.overview_metrics.caption_h
        } else {
            0.0
        };
        (header + self.overview_content_height() - self.overview_viewport_height()).max(0.0)
    }

    /// Rect of list slot `slot`, panned by `overview_scroll_y`.
    pub(super) fn overview_list_slot_rect(&self, slot: usize) -> Rect {
        let c = self.overview_content_rect();
        let m = self.overview_metrics;
        let scroll = if self.overview_scrolls_whole_panel() {
            0.0
        } else {
            self.overview_scroll_y
        };
        let y0 = self.overview_list_top() - scroll
            + f64_of_usize(slot) * (m.room_row_h + m.room_row_gap);
        Rect::new(c.x0, y0, c.x1, y0 + m.room_row_h)
    }

    /// The list slot a room occupies: the room you are in has none — it
    /// is the block at the top — and every other room keeps tree order.
    pub(super) fn overview_slot_of(&self, idx: usize) -> Option<usize> {
        match self.overview_here_index() {
            Some(here) if here == idx => None,
            Some(here) if idx > here => Some(idx.saturating_sub(1)),
            _ => Some(idx),
        }
    }

    /// Rect of workspace `idx` — the "you are here" block for the room
    /// you are in, its list row for any other. Indices are tree order,
    /// the contract the test-control socket's `overview_card[i]` names
    /// are built on.
    pub(super) fn overview_row_rect(&self, idx: usize) -> Rect {
        match self.overview_slot_of(idx) {
            None => self.overview_here_rect(),
            Some(slot) => self.overview_list_slot_rect(slot),
        }
    }

    /// Rect of the "New room" row — the slot after the last room.
    pub(super) fn overview_add_workspace_rect_for(&self, ws_count: usize) -> Rect {
        let slot = ws_count.saturating_sub(usize::from(self.overview_here_index().is_some()));
        self.overview_list_slot_rect(slot)
    }

    /// Rect of the "Join a workspace…" row, after "New workspace".
    pub(super) fn overview_join_workspace_rect_for(&self, ws_count: usize) -> Rect {
        let slot = ws_count.saturating_sub(usize::from(self.overview_here_index().is_some()));
        self.overview_list_slot_rect(slot.saturating_add(1))
    }

    /// The "Leave" control on the row of workspace `idx` when it is a
    /// site's workspace, at the row's trailing edge; zero otherwise.
    pub(super) fn overview_leave_rect(
        &self,
        idx: usize,
        site_rooms: &[crate::shell::network::SiteRoom],
    ) -> Rect {
        let leavable = self.chrome.as_ref().is_some_and(|chrome| {
            chrome
                .borrow()
                .tree()
                .desktop
                .workspaces
                .get(idx)
                .is_some_and(|workspace| site_rooms.iter().any(|site| site.room == workspace.room))
        });
        if !leavable {
            return Rect::ZERO;
        }
        let row = self.overview_row_rect(idx);
        // Wide enough for "Leave" in every language the shell speaks.
        let w = 112.0;
        let h = 28.0_f64;
        if self.overview_slot_of(idx).is_none() {
            // The room you are in: on its access line, at the panel's edge.
            let x1 = self.overview_content_rect().x1;
            return Rect::new(x1 - w, row.y1 - h, x1, row.y1);
        }
        let pad = self.overview_metrics.row_pad;
        let h = h.min(row.height() - 8.0).max(0.0);
        let cy = row.center().y;
        Rect::new(row.x1 - pad - w, cy - h / 2.0, row.x1 - pad, cy + h / 2.0)
    }

    /// The join row's hit area, from geometry as the "New workspace" row's.
    pub(super) fn join_workspace_hit_rect(&self) -> Rect {
        let add = self.add_workspace_hit_rect();
        if add.area() <= 0.0 {
            return Rect::ZERO;
        }
        let content = self.overview_content_rect();
        let top = self.overview_list_top().max(content.y0);
        let band = Rect::new(content.x0, top, content.x1, content.y1);
        let row = self
            .overview_join_workspace_rect_for(self.overview_cards().len())
            .intersect(band);
        if row.width() > 0.0 && row.height() > 0.0 {
            row
        } else {
            Rect::ZERO
        }
    }

    /// Open "Join a workspace…", the directory of public workspaces, in
    /// place of the overview.
    pub(super) fn open_join_sheet(&self) {
        if let Some(chrome) = self.chrome.as_ref() {
            let mut state = chrome.borrow_mut();
            state.overlays.overview = false;
            state.push_peer_sync_request(PeerSyncAction::BrowseDirectory);
            state.bump();
        }
    }

    /// Pan the room list so the keyboard's selection is on screen —
    /// otherwise arrowing past the last visible room moves a selection
    /// nobody can see.
    pub(super) fn scroll_overview_selection_into_view(&mut self) {
        let Some(idx) = self.overview_selection else {
            return;
        };
        let m = self.overview_metrics;
        let whole = self.overview_scrolls_whole_panel();
        let (top, height) = match self.overview_slot_of(idx) {
            Some(slot) => {
                let header = if whole {
                    self.overview_here_block_height() + m.section_gap + m.caption_h
                } else {
                    0.0
                };
                (
                    header + f64_of_usize(slot) * (m.room_row_h + m.room_row_gap),
                    m.room_row_h,
                )
            }
            None if whole => (0.0, m.here_h + self.overview_windows_row_height()),
            None => return,
        };
        let bottom = top + height;
        let view = self.overview_viewport_height();
        let max = self.overview_max_scroll_y();
        let mut y = self.overview_scroll_y;
        if top < y {
            y = top;
        } else if bottom > y + view {
            y = bottom - view;
        }
        self.overview_scroll_y = y.clamp(0.0, max);
    }

    /// Mirror of the geometry in `draw_overview`: return the index of the
    /// room at logical position `p`, if any. A row panned out of the
    /// list's band is not hittable, so a click never lands on a room the
    /// user cannot see.
    pub(super) fn hit_test_overview(&self, p: Point) -> Option<usize> {
        if !self.overview_content_rect().contains(p) {
            return None;
        }
        let chrome = self.chrome.as_ref()?;
        let count = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop.workspaces.len()
        };
        let band = Rect::new(
            self.overview_content_rect().x0,
            self.overview_list_top()
                .max(self.overview_content_rect().y0),
            self.overview_content_rect().x1,
            self.overview_content_rect().y1,
        );
        for i in 0..count {
            let r = self.overview_row_rect(i);
            let visible = self.overview_slot_of(i).is_none()
                || (r.y0 >= band.y0 - 0.5 && r.y1 <= band.y1 + 0.5);
            if visible && r.contains(p) {
                return Some(i);
            }
        }
        None
    }

    /// True if `p` is over any open overlay's hit rect, so
    /// [`Widget::handle_event`] absorbs every pointer event and not just
    /// `Pressed` — otherwise `Released`/`CursorMoved` fall through and
    /// light up affordances underneath.
    pub(crate) fn cursor_over_open_overlay(&self, p: Point) -> bool {
        (self.palette_visible && self.palette.get_rect().contains(p))
            || self.cursor_over_other_open_overlay(p)
    }

    pub(super) fn cursor_over_other_open_overlay(&self, p: Point) -> bool {
        let f = self.overlay_flags();
        self.dictation_pointer_at((p.x, p.y))
            || (f.collaborators && self.collaborators_overlay.contains(p))
            || (f.quick_settings && self.quick_settings_rect().contains(p))
            || (f.ws_settings && self.ws_settings_rect().contains(p))
            || (f.room_activity && self.room_activity.rect().contains(p))
            || (f.projection && ProjectionOverlay::contains(self.overlay_area(), p))
            || (self.layers.emoji_picker.is_open()
                && self.layers.emoji_picker.card_rect().contains(p))
    }
}
