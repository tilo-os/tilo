//! The shortcut guide's button and the input the guide explains.

#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;

impl ShellOverlayHost {
    #[cfg(test)]
    pub(super) fn draw_shortcut_help_button(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.shortcut_help_button_is_suppressed() {
            return;
        }
        let style = theme.text(TextRole::Label).strong();
        let label = t("help-pill");
        let label = label.as_str();
        let (w, h) = text_renderer.measure_style(label, None, &style);
        // Label padding `s6` across / `s3` down, `s4` off the display
        // corner (the same pixels as before, now the theme's steps). The
        // pill is at least the §9.6 pointer floor plus `s1` tall (36 at
        // 100 %), and as wide as its measured label needs — a 140 px width
        // floor added nothing the label did not.
        let spacing = &theme.tokens.spacing;
        let (pad_x, pad_y, inset) = (
            f64::from(spacing.s6),
            f64::from(spacing.s3),
            f64::from(spacing.s4),
        );
        let min_h = f64::from(theme.tokens.metrics.min_pointer_target + spacing.s1);
        // The focused display's corner: the desktop's bottom-right corner
        // is on no display at all when the displays are stacked offset.
        let area = self.overlay_area();
        self.shortcut_help_drawn_rect = Rect::new(
            area.x1 - (f64::from(w) + pad_x) - inset,
            area.y1 - (f64::from(h) + pad_y).max(min_h) - inset,
            area.x1 - inset,
            area.y1 - inset,
        );
        let rect = self.shortcut_help_button_rect();
        let colors = &theme.tokens.colors;
        // A floating pill over the desktop is Panel material (HIG §9.1–9.2):
        // the panel ground with its derived edge, never a ramp slot
        // (`neutral[1]` until 2026-09-17, S1-27), and ink chosen against
        // that ground rather than assumed.
        let radius = rect.height() / 2.0;
        let surface = ui::chrome::ChromeRole::Panel.surface(theme);
        ui::paint::fill_round(scene, transform, rect, radius, surface);
        ui::paint::stroke_round(
            scene,
            transform,
            rect,
            radius,
            ui::chrome::shell::panel_border(theme),
            1.0,
        );
        text_renderer.draw_style(
            scene,
            label,
            colors.ink_on(
                surface,
                colors.text_primary,
                ui::tokens::colors::ON_FILL_MIN_CONTRAST,
            ),
            transform.pre_translate(
                (
                    rect.center().x - f64::from(w) / 2.0,
                    rect.center().y - f64::from(h) / 2.0,
                )
                    .into(),
            ),
            None,
            &style,
        );
    }

    /// The input devices and last-used modality the chrome adapts to.
    pub fn interaction_environment(&self) -> ui::InteractionEnvironment {
        self.interaction
    }

    /// Input language the help overlay is written in.
    pub fn shortcut_help_modality(&self) -> ui::InputModality {
        self.interaction.preferred_hint_modality()
    }

    /// The shortcut-guide affordance is persistent chrome, not modal
    /// content. Keep it behind every temporary shell-owned surface: drawing
    /// it later would let its label overlap a popover at small sizes or large
    /// text, and would make it appear actionable while that surface owns
    /// input.
    pub(super) fn shortcut_help_button_is_suppressed(&self) -> bool {
        let flags = self.overlay_flags();
        let has_app_content = self.chrome.as_ref().is_some_and(|chrome| {
            chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .is_some_and(tilo_desktop_model::Workspace::has_windows)
        });
        self.onboarding.is_some()
            || has_app_content
            || flags.shortcut_help
            || flags.desktop_tour
            || flags.overview
            || flags.quick_settings
            || flags.ws_settings
            || flags.collaborators
            || flags.room_activity
            || flags.projection
            || flags.capture
            || self.palette_visible
            || self.palette_transition.is_visible(ui::time::now())
            || self.qs_motion.is_visible(ui::time::now())
            || self.layers.power_menu.is_drawn()
            || self.workspace_settings_overlay.member_sheet_is_open()
            || self.file_picker_overlay.widget().is_some()
            || self.peer_sync_overlay.visible()
            || self.directory_sheet.is_open()
            || self.portal_dialogs.view().is_some()
    }
}
