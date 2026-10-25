//! The desktop's control hints on the native desktop: the same
//! [`ControlHints`] the page runs, fed what this frame shows (the shown
//! strip's row, the bar's own avatar, the search chord as configured) and
//! drawn above the windows. What the person learned is kept with their
//! personal settings, which roam to their other devices.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use settings_store::control_hints::LEARNED;
use settings_store::hardware::ShortcutAction;
use tilo_desktop::control_hints::{ControlHints, HintFrame};
use ui::control_hints::Quiet;

impl ShellOverlayHost {
    /// What this frame shows the hints.
    fn control_hint_frame(&mut self, now: Instant) -> HintFrame {
        let row = self
            .active_strip_id()
            .and_then(|id| self.group_row_ptrs.get(&id).copied())
            // SAFETY: see field doc on `group_row_ptrs`.
            .map(|ptr| unsafe { &*ptr }.hint_places())
            .unwrap_or_default();
        let area = if row.viewport.is_zero_area() {
            let display = self.overlay_area();
            Rect::new(
                display.x0,
                display.y0 + f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX),
                display.x1,
                display.y1,
            )
        } else {
            row.viewport
        };
        let search_keys = self
            .shortcut_config()
            .bindings
            .get(&ShortcutAction::Search)
            .map(|chord| {
                ui::keycaps::active()
                    .chord_keys(chord.modifiers.iter().map(String::as_str), &chord.key)
            })
            .unwrap_or_default();
        let avatar = self.main_bar_rect(tilo_desktop::topbar::TopBarChrome::own_avatar_rect);
        let colour = self
            .chrome
            .as_ref()
            .map(|chrome| crate::shell::topbar_chrome::own_rgb(&chrome.borrow()));
        HintFrame {
            row,
            area,
            search_keys,
            avatar: (!avatar.is_zero_area()).then_some(avatar),
            colour,
            quiet: self.control_hints_quiet(now),
        }
    }

    /// Why no hint may begin now. In a call or following someone, one that
    /// shows goes too.
    fn control_hints_quiet(&self, now: Instant) -> Option<Quiet> {
        if self.own_voice_call().is_some() {
            return Some(Quiet::Call);
        }
        if self.followed_peer.is_some() {
            return Some(Quiet::Following);
        }
        if self
            .chrome
            .as_ref()
            .is_some_and(|chrome| chrome.borrow().typing_at(now))
        {
            return Some(Quiet::Typing);
        }
        let flags = self.overlay_flags();
        let busy = self.palette_visible
            || flags.overview
            || flags.quick_settings
            || flags.ws_settings
            || flags.collaborators
            || flags.shortcut_help
            || flags.desktop_tour
            || flags.capture
            || flags.projection
            || self.bar_popovers.profile.is_open()
            || self.bar_popovers.workspace_menu.is_open()
            || self.onboarding_active()
            || self.modal_sheet_owns_keyboard()
            || self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.borrow().is_fullscreen());
        busy.then_some(Quiet::Busy)
    }

    /// Advance the hints to `now`, and keep what the person learned.
    pub(super) fn prepare_control_hints(&mut self, now: Instant) {
        if self.control_hints.reread_due(now) {
            self.control_hints.merge(&LEARNED.get(), now);
        }
        let frame = self.control_hint_frame(now);
        self.control_hints.update(&frame, now);
        self.control_hints_frame = frame;
        self.keep_control_hints();
    }

    /// Turn the hints on or off, as the profile card's switch asks.
    pub(super) fn set_control_hints_enabled(&mut self, on: bool) {
        self.control_hints.set_enabled(on, Instant::now());
        self.keep_control_hints();
    }

    pub(super) fn control_hints_enabled(&self) -> bool {
        self.control_hints.enabled()
    }

    fn keep_control_hints(&mut self) {
        if let Some(learned) = self.control_hints.take_changed()
            && let Err(error) = LEARNED.set(learned)
        {
            tracing::warn!(%error, "could not keep the control hints learned");
        }
    }

    pub(super) fn draw_control_hints(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
    ) {
        self.control_hints.draw(
            &self.control_hints_frame,
            scene,
            transform,
            text_renderer,
            theme,
            Instant::now(),
        );
    }

    pub(super) fn control_hints_animating(&self, now: Instant) -> bool {
        self.control_hints.needs_animation_frame(now)
    }

    pub(super) fn control_hints_wake_deadline(&self, now: Instant) -> Option<Instant> {
        self.control_hints.next_wake_deadline(now)
    }

    /// The hint on screen, for assistive technology.
    pub(super) fn control_hint_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.control_hints.accessibility()
    }
}

/// The hints a native desktop starts with: the person's record as kept.
pub(super) fn load() -> ControlHints {
    ControlHints::new(LEARNED.get())
}
