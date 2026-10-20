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
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: same stable boxed-row contract as the other
            // `group_row_ptrs` accessors; `&mut self` gives unaliased access.
            let row: &mut GroupRow = unsafe { &mut *ptr };
            for group in &mut row.groups {
                group.set_top_overlay_inset(inset);
                group.set_fills_display(presence != tilo_desktop::chrome::TopBarPresence::Docked);
            }
        }
    }

    /// Whether the pointer is on the top bar the person hides while it shows
    /// over the windows: it takes the pointer from the window under it, and
    /// only the pointer.
    pub(crate) fn top_bar_captures_pointer(&self) -> bool {
        let shown = self.chrome.as_ref().is_some_and(|chrome| {
            chrome.borrow().top_bar_presence() == tilo_desktop::chrome::TopBarPresence::Shown
        });
        shown
            && self.last_cursor.is_some_and(|p| {
                self.top_bar_reveal_geometry()
                    .is_some_and(|(_, bar)| bar.contains(p))
            })
    }

    /// Whether the pointer is on a notification banner, or a click begun on
    /// one still owns its release. Banners hang over application windows;
    /// were the window under one given the pointer, a click on the banner
    /// would land in the app and the banner could be neither opened nor
    /// dismissed.
    /// The pointer is on the line under the top bar: its press is the
    /// line's, not the window's beneath.
    pub(crate) fn presence_notice_captures_pointer(&self) -> bool {
        self.presence_notice_press
            || self.last_cursor.is_some_and(|p| {
                self.presence_notices.rect.contains(p) || self.arrival_line.change_at(p)
            })
    }

    pub(crate) fn notification_banner_captures_pointer(&self) -> bool {
        let Some(chrome) = self.chrome.as_ref() else {
            return false;
        };
        if chrome.borrow().notifications().is_empty() {
            return false;
        }
        self.notification_stack.wants_pointer_capture()
            || self
                .last_cursor
                .is_some_and(|p| self.notification_stack.rect().contains(p))
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
}
