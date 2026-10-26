//! The top bar's reveal and what captures the pointer near it.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    /// The display whose top edge the full bar stands on, and the strip the
    /// bar takes there. `None` without a desktop surface.
    pub(super) fn top_bar_reveal_geometry(&self) -> Option<(Rect, Rect)> {
        let display = self
            .primary_output()
            .map_or(self.rect, |output| output.rect());
        let bar = self
            .content
            .as_any()?
            .downcast_ref::<tilo_desktop::composition::DesktopSurface>()?
            .topbar()?
            .get_rect();
        let bar = if bar.is_zero_area() {
            Rect::new(
                display.x0,
                display.y0,
                display.x1,
                display.y0 + f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX),
            )
        } else {
            bar
        };
        Some((display, bar))
    }

    /// Whether a maximized window stands on the display the top bar stands
    /// on: a window there fills that display, so the bar hides on it.
    pub(super) fn maximized_on_top_bar_display(&self) -> bool {
        let Some((display, _)) = self.top_bar_reveal_geometry() else {
            return false;
        };
        self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: same stable boxed-row contract as the other
            // `group_row_ptrs` accessors; this is a read-only query.
            let row: &GroupRow = unsafe { &*ptr };
            // Only a strip on screen: one the person moved away from keeps
            // its windows' last places.
            row.get_rect().intersect(display).area() > 0.0
                && row
                    .groups
                    .iter()
                    .any(|group| group.maximized && display.contains(group.get_rect().center()))
        })
    }

    /// Whether something the bar opened holds it shown: one of its menus,
    /// the workspace name being edited, or the bar's own keyboard focus.
    pub(super) fn top_bar_held_open(&self) -> bool {
        let chrome_holds = self.chrome.as_ref().is_some_and(|chrome| {
            let state = chrome.borrow();
            state.overlays.quick_settings
                || state.overlays.collaborators
                || state.overlays.ws_settings
                || state.overlays.clock
                || state.editing_workspace_name.is_some()
        });
        chrome_holds
            || self
                .content
                .as_any()
                .and_then(|content| {
                    content.downcast_ref::<tilo_desktop::composition::DesktopSurface>()
                })
                .and_then(tilo_desktop::composition::DesktopSurface::topbar)
                .is_some_and(crate::shell::topbar_chrome::TopBarChrome::holds_itself_shown)
    }

    /// Bring the bar's presence up to date at `now`: the person's choice,
    /// the pointer's requests, and what holds it open.
    pub fn sync_top_bar_presence(&mut self, now: Instant) {
        // The person's choice hides it everywhere; a maximized window hides
        // it on the display the window fills, so the window is all app (and
        // its client can be scanned out) until the bar is reached for.
        let chosen = self.desktop_controls.config().hide_top_bar;
        let maximized = self.maximized_on_top_bar_display();
        // The bar leaves with a window maximizing over it and comes back
        // with its restore, moved by what moved the window.
        let (motion, source) = (self.overview_layout_theme.motion(), self.input_source);
        if let Some(bar) = self
            .desktop_surface_mut()
            .and_then(tilo_desktop::composition::DesktopSurface::topbar_mut)
        {
            bar.move_with_window(!maximized, motion, source);
        }
        self.top_bar_reveal.set_hide(chosen || maximized);
        self.top_bar_reveal.settle(now);
        let held_open = self.top_bar_held_open();
        let presence = self.top_bar_reveal.presence(held_open);
        if let Some(chrome) = self.chrome.as_ref() {
            let changed = chrome.borrow().top_bar_presence() != presence;
            if changed {
                tracing::debug!(
                    target: "tilo::top_bar",
                    ?presence,
                    chosen,
                    maximized,
                    held_open,
                    reveal = ?self.top_bar_reveal,
                    "top bar presence changed"
                );
            }
            chrome.borrow_mut().set_top_bar_presence(presence);
        }
        // While the bar shows over the windows' top edge, a maximized
        // window's own bar stands below it rather than under it: the two
        // read top to bottom as one stack.
        let inset = if presence == tilo_desktop::chrome::TopBarPresence::Shown {
            self.top_bar_reveal_geometry()
                .map_or(0.0, |(_, bar)| bar.height())
        } else {
            0.0
        };
        // A fullscreen window's bar shows over it while reached for.
        let fullscreen = self.chrome.as_ref().and_then(|chrome| {
            let state = chrome.borrow();
            state
                .is_fullscreen()
                .then(|| state.fullscreen_group())
                .flatten()
        });
        // Entered by a key or an action, its bar lands with it; by a click,
        // it fades in.
        let entered_by = self
            .desktop_surface()
            .and_then(tilo_desktop::composition::DesktopSurface::transition_source)
            .unwrap_or(self.input_source);
        self.fullscreen_bar_reveal
            .follow(fullscreen, entered_by, now);
        let in_use = self.fullscreen_bar_in_use();
        self.fullscreen_bar_reveal.settle(now, in_use);
        let bar_over = self
            .fullscreen_bar_reveal
            .shown()
            .then(|| self.fullscreen_bar_reveal.group())
            .flatten();
        let bar_source = self.fullscreen_bar_reveal.source();
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same stable boxed-row contract as the other
            // `group_row_ptrs` accessors; `&mut self` gives unaliased access.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                group.set_top_overlay_inset(inset);
                group.set_fills_display(presence != tilo_desktop::chrome::TopBarPresence::Docked);
                group.set_fullscreen_bar_revealed(bar_over == Some(group.id), bar_source);
            }
        }
    }

    /// Whether the fullscreen window's bar is being used, which keeps it
    /// shown past its idle time.
    fn fullscreen_bar_in_use(&self) -> bool {
        let Some(group_id) = self.fullscreen_bar_reveal.group() else {
            return false;
        };
        self.group_row_ptrs.values().any(|&ptr| {
            // SAFETY: same stable boxed-row contract as the other
            // `group_row_ptrs` accessors; this is a read-only query.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .any(|group| group.id == group_id && group.bar_in_use())
        })
    }

    /// The key that asks for a fullscreen window's bar (Super) went down:
    /// it shows for its idle time, over the window.
    pub fn reveal_fullscreen_bar_by_key(&mut self) {
        let now = Instant::now();
        if self.fullscreen_bar_reveal.reveal_by_key(now) {
            self.sync_top_bar_presence(now);
        }
    }

    /// Where the fullscreen window stands and where its bar shows over it,
    /// while a window is fullscreen.
    pub(super) fn fullscreen_bar_geometry(&self) -> Option<(Rect, Rect)> {
        let group_id = self.fullscreen_bar_reveal.group()?;
        let window = self.group_row_ptrs.values().find_map(|&ptr| {
            // SAFETY: same stable boxed-row contract as the other
            // `group_row_ptrs` accessors; this is a read-only query.
            let row: &GroupRow = unsafe { &*ptr };
            row.groups
                .iter()
                .find(|group| group.id == group_id)
                .map(|group| (group.get_rect(), group.bar_rect()))
        })?;
        let (display, bar) = window;
        let bar = if bar.area() > 0.0 {
            bar
        } else {
            Rect::new(
                display.x0,
                display.y0,
                display.x1,
                display.y0 + ui::widget::tabs::TAB_BAR_HEIGHT,
            )
        };
        Some((display, bar))
    }

    /// Whether the pointer is on a fullscreen window's bar while it shows
    /// over the window: the bar, not the client under it, takes it.
    pub(crate) fn fullscreen_bar_captures_pointer_at(&self, p: Point) -> bool {
        self.fullscreen_bar_reveal.shown()
            && self
                .fullscreen_bar_geometry()
                .is_some_and(|(_, bar)| bar.contains(p))
    }

    /// Whether a finger landing at `p` is on a display's top edge while
    /// something hides there (the top bar the person hides, a fullscreen
    /// window's bar): a swipe down from it is the shell's, and no client
    /// sees the contact.
    pub fn takes_top_edge_touch(&self, p: Point) -> bool {
        let band = ui::widget::top_edge_reveal::TOUCH_EDGE_PX;
        let on_edge = |display: Rect| {
            p.x >= display.x0 && p.x < display.x1 && p.y >= display.y0 && p.y < display.y0 + band
        };
        let bar_hidden = self.top_bar_reveal.hidden()
            && self
                .top_bar_reveal_geometry()
                .is_some_and(|(display, _)| on_edge(display));
        let window_bar_hidden = !self.fullscreen_bar_reveal.shown()
            && self
                .fullscreen_bar_geometry()
                .is_some_and(|(display, _)| on_edge(display));
        bar_hidden || window_bar_hidden
    }

    /// One raw contact, read for a swipe down from a display's top edge,
    /// which reveals the hidden top bar or a fullscreen window's bar there,
    /// as the pointer pushing into the edge does. Returns whether the
    /// contact is the swipe's.
    pub(super) fn top_edge_swipe(
        &mut self,
        id: u64,
        p: Point,
        phase: winit::event::TouchPhase,
    ) -> bool {
        use winit::event::TouchPhase;
        match phase {
            TouchPhase::Started => {
                if self.top_edge_swipe.is_some() || !self.takes_top_edge_touch(p) {
                    return false;
                }
                self.top_edge_swipe = Some((id, p));
                true
            }
            TouchPhase::Moved => {
                let Some((swiping, from)) = self.top_edge_swipe else {
                    return false;
                };
                if swiping != id {
                    return false;
                }
                if p.y - from.y >= ui::widget::top_edge_reveal::TOUCH_SWIPE_PX {
                    self.top_edge_swipe = None;
                    self.reveal_top_edge_from(from);
                }
                true
            }
            TouchPhase::Ended | TouchPhase::Cancelled => self
                .top_edge_swipe
                .take_if(|(swiping, _)| *swiping == id)
                .is_some(),
        }
    }

    /// Reveal what hides at the top edge a swipe began on.
    fn reveal_top_edge_from(&mut self, from: Point) {
        let now = Instant::now();
        let near = |display: Rect| from.x >= display.x0 && from.x < display.x1;
        if self
            .top_bar_reveal_geometry()
            .is_some_and(|(display, _)| near(display))
        {
            self.top_bar_reveal.reveal_by_touch();
        }
        if self
            .fullscreen_bar_geometry()
            .is_some_and(|(display, _)| near(display))
        {
            self.fullscreen_bar_reveal.reveal_by_touch(now);
        }
        self.sync_top_bar_presence(now);
    }

    /// Whether the pointer is on the top bar the person hides while it shows
    /// over the windows: it takes the pointer from the window under it, and
    /// only the pointer.
    pub(crate) fn top_bar_captures_pointer_at(&self, p: Point) -> bool {
        let shown = self.chrome.as_ref().is_some_and(|chrome| {
            chrome.borrow().top_bar_presence() == tilo_desktop::chrome::TopBarPresence::Shown
        });
        shown
            && self
                .top_bar_reveal_geometry()
                .is_some_and(|(_, bar)| bar.contains(p))
    }

    /// Whether a notification banner has the pointer where it is.
    #[cfg(test)]
    pub(crate) fn notification_banner_captures_pointer(&self) -> bool {
        self.notification_banner_captures_pointer_at(self.last_cursor)
    }

    /// Whether the pointer is on a notification banner, or a click begun on
    /// one still owns its release. Banners hang over application windows;
    /// were the window under one given the pointer, a click on the banner
    /// would land in the app and the banner could be neither opened nor
    /// dismissed.
    fn notification_banner_captures_pointer_at(&self, p: Option<Point>) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        if chrome.borrow().notifications().is_empty() {
            return false;
        }
        self.notification_stack.wants_pointer_capture()
            || p.is_some_and(|p| self.notification_stack.rect().contains(p))
    }

    /// Whether the shell takes pointer input at `p` (the pointer's place, or
    /// where a finger lands) though no modal holds input: the bar a maximized
    /// window shows over its client, the open strip switcher, the top bar
    /// shown over the windows, the capture tool's receipt and recording bar,
    /// a notification banner. A press there is the shell's, never the
    /// window's beneath.
    pub(crate) fn chrome_band_captures_pointer_at(&self, p: Option<Point>) -> bool {
        p.is_some_and(|p| {
            self.maximized_chrome_captures_pointer_at(p)
                || self.strip_switcher_captures_pointer_at(p)
                || self.top_bar_captures_pointer_at(p)
                || self.fullscreen_bar_captures_pointer_at(p)
                || self.capture_studio.takes_pointer_at(p)
        }) || self.notification_banner_captures_pointer_at(p)
    }

    /// [`Self::chrome_band_captures_pointer_at`] where the pointer is.
    pub(crate) fn chrome_band_captures_pointer(&self) -> bool {
        self.chrome_band_captures_pointer_at(self.last_cursor)
    }

    /// Where the notification banners were last drawn.
    pub fn notification_banners_rect(&self) -> Rect {
        self.notification_stack.rect()
    }

    /// Test hook: as if the person had turned "hide the top bar" on or off.
    pub fn set_hide_top_bar_for_tests(&mut self, hide: bool) {
        let mut config = self.desktop_controls.config();
        config.hide_top_bar = hide;
        self.desktop_controls = DesktopControlsSource::Explicit(config);
        self.sync_top_bar_presence(Instant::now());
    }

    /// When the top bar's reveal next has something to settle.
    pub fn top_bar_reveal_deadline(&self) -> Option<Instant> {
        self.top_bar_reveal.deadline()
    }

    /// When a fullscreen window's bar next has something to settle.
    pub fn fullscreen_bar_reveal_deadline(&self) -> Option<Instant> {
        self.fullscreen_bar_reveal.deadline()
    }
}
