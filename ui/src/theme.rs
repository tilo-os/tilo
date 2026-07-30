use vello::peniko::Color;
use crate::tokens::Tokens;
use crate::tokens::colors::{ACCENT_HOVER_IDX, ACCENT_PRIMARY_IDX};

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub tokens: Tokens,
    // Legacy shortcuts — derived from tokens on construction, kept for compat during migration.
    pub bg_app: Color,
    pub bg_primary: Color,
    pub bg_hover: Color,
    pub bg_active: Color,
    pub border_standard: Color,
    pub border_focus: Color,
    pub text_primary: Color,
    pub text_dim: Color,
    /// Concept's dominant accent fill (indigo-500 `#6366f1`). Use for
    /// active states (slider active track, checkbox tick, toggle
    /// on-state, focused caret, currently-selected dot, etc.). For
    /// hover states use [`Theme::accent_hover`] instead.
    pub accent_primary: Color,
    /// Concept's hover-state accent (indigo-600 `#4f46e5`). One step
    /// darker than [`Theme::accent_primary`].
    pub accent_hover: Color,
    pub status_success: Color,
    pub radius_standard: f64,
    pub padding_standard: f32,
    pub gap_standard: f32,
    /// When set, widgets that animate (spinners, indeterminate progress,
    /// hover/press transitions) snap straight to their resting state instead
    /// of easing. Carried on the theme so every widget can react to the
    /// user's reduced-motion preference without new plumbing through `draw`.
    /// Default `false`.
    pub reduce_motion: bool,
    /// Multiplier applied to every text size the theme exposes, so the whole
    /// UI scales together for low-vision users. `1.0` is the default size.
    /// Already baked into `tokens.typography.scale` and `font_size_base` by
    /// [`Theme::with_text_scale`], so widgets that read their font size from
    /// the theme scale automatically — and layout, which measures the same
    /// sizes, grows to fit so text never clips. Kept here too so a widget can
    /// scale a size it computes itself (a hard-coded glyph metric, say).
    pub text_scale: f32,
    /// Global UI-scale multiplier applied to the WHOLE design system — spacing,
    /// radii, and the type scale all grow together — so the entire interface
    /// gets larger or smaller as one unit (the "make everything bigger" knob,
    /// distinct from [`Theme::text_scale`] which only touches type). `1.0` is
    /// the default. Baked into `tokens.spacing`, `tokens.radii`,
    /// `tokens.typography.scale`, and `font_size_base` by
    /// [`Theme::with_ui_scale`], and reflected in the legacy
    /// `padding`/`gap`/`radius` shortcuts, so every widget that sources its
    /// metrics from the theme scales with no per-widget plumbing.
    ///
    /// This is the **user** scale; it composes on top of the per-output
    /// **device** scale the compositor reports (widgets lay out in logical
    /// px and the render transform rasterises at the device's physical
    /// resolution). Effective physical size = logical × `ui_scale` ×
    /// device-scale, and text additionally × `text_scale`.
    pub ui_scale: f32,
}

impl Default for Theme { fn default() -> Self { Self::dark() } }

/// A widget's desired global appearance, polled by [`crate::app::App`] once per
/// frame (when [`crate::app::App::appearance_reconcile`] is enabled) and
/// reconciled against the live theme. Mirrors the level-triggered
/// `fullscreen_intent` contract: returning a *steady* value is correct and
/// idempotent — the App only re-themes when a field actually differs from what
/// is currently applied, and a settings pane surfaces the user's live choice by
/// returning the same intent every frame while it is open.
///
/// `base_theme` is the colour theme **only** (no high-contrast track, no
/// scale folded); the App applies the high-contrast override, the UI scale, and
/// the text scale on top so the a11y and scale bookkeeping stays in one place.
#[derive(Clone, Debug)]
pub struct AppearanceIntent {
    /// Base colour theme, unscaled and without the a11y track folded in — e.g.
    /// [`Theme::nord`]. The App layers high-contrast / scale on top itself.
    pub base_theme: Theme,
    /// Force the high-contrast accessibility palette.
    pub high_contrast: bool,
    /// Snap animations to their resting state.
    pub reduce_motion: bool,
    /// Text-size multiplier (type only). `1.0` = default.
    pub text_scale: f32,
    /// Global UI-scale multiplier (spacing + radii + type together). `1.0` =
    /// default.
    pub ui_scale: f32,
}

impl Theme {
    fn from_tokens(name: &str, tokens: Tokens) -> Self {
        let c = &tokens.colors;
        Self {
            name: name.into(),
            bg_app: c.surfaces.base,
            bg_primary: c.surfaces.raised,
            bg_hover: c.neutral[4],
            bg_active: c.neutral[2],
            border_standard: c.border_standard,
            border_focus: c.border_focus,
            text_primary: c.text_primary,
            text_dim: c.text_dim,
            accent_primary: c.accent[ACCENT_PRIMARY_IDX],
            accent_hover: c.accent[ACCENT_HOVER_IDX],
            status_success: c.semantic.success,
            radius_standard: tokens.radii.md as f64,
            padding_standard: tokens.spacing.s3,
            gap_standard: tokens.spacing.s3,
            reduce_motion: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            tokens,
        }
    }
    pub fn dark() -> Self { Self::from_tokens("Dark", Tokens::dark()) }
    pub fn light() -> Self { Self::from_tokens("Light", Tokens::light()) }
    pub fn dracula() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::from_rgb8(40, 42, 54);
        t.colors.surfaces.raised = Color::from_rgb8(68, 71, 90);
        t.colors.text_primary = Color::from_rgb8(248, 248, 242);
        t.colors.accent.stops[4] = Color::from_rgb8(255, 121, 198);
        t.colors.border_focus = Color::from_rgb8(189, 147, 249);
        Self::from_tokens("Dracula", t)
    }
    pub fn nord() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::from_rgb8(46, 52, 64);
        t.colors.surfaces.raised = Color::from_rgb8(59, 66, 82);
        t.colors.text_primary = Color::from_rgb8(216, 222, 233);
        t.colors.accent.stops[4] = Color::from_rgb8(129, 161, 193);
        t.colors.border_focus = Color::from_rgb8(136, 192, 208);
        Self::from_tokens("Nord", t)
    }
    pub fn gruvbox() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::from_rgb8(40, 40, 40);
        t.colors.surfaces.raised = Color::from_rgb8(60, 56, 54);
        t.colors.text_primary = Color::from_rgb8(235, 219, 178);
        t.colors.accent.stops[4] = Color::from_rgb8(184, 187, 38);
        t.colors.border_focus = Color::from_rgb8(254, 128, 25);
        t.radii.md = 2.0;
        Self::from_tokens("Gruvbox", t)
    }
    pub fn solarized() -> Self {
        let mut t = Tokens::dark();
        t.colors.surfaces.base = Color::from_rgb8(0, 43, 54);
        t.colors.surfaces.raised = Color::from_rgb8(7, 54, 66);
        t.colors.text_primary = Color::from_rgb8(131, 148, 150);
        t.colors.accent.stops[4] = Color::from_rgb8(42, 161, 152);
        t.colors.border_focus = Color::from_rgb8(38, 139, 210);
        t.radii.md = 0.0;
        Self::from_tokens("Solarized", t)
    }

    /// High-contrast accessibility preset (workstream W2 of the a11y design).
    ///
    /// Maximises foreground/background contrast for low-vision users: pure
    /// black solid surfaces under pure-white text (WCAG AAA, 21:1 — the
    /// maximum possible), crisp white borders, and a high-visibility **yellow
    /// focus ring** so keyboard focus is unmistakable. Because every widget
    /// reads its colours from the passed-in [`Theme`], selecting this preset
    /// makes the whole UI high-contrast with no per-widget changes.
    ///
    /// Selected when [`A11ySettings::high_contrast`](crate::a11y::A11ySettings)
    /// is set; the shell owns theming and broadcasts the swap.
    pub fn high_contrast() -> Self {
        let mut t = Tokens::dark();
        let c = &mut t.colors;

        // Solid (non-translucent) surfaces — pure black so text/borders pop.
        c.surfaces.base = Color::BLACK;
        c.surfaces.raised = Color::BLACK;
        c.surfaces.overlay = Color::from_rgba8(0, 0, 0, 255);
        c.surfaces.glass = Color::from_rgba8(0, 0, 0, 255);
        c.surfaces.glass_blur_px = 0.0;
        c.surfaces.window_card = Color::BLACK;
        c.surfaces.window_card_border = Color::WHITE;

        // Maximal text contrast. `text_dim` stays near-white so even secondary
        // copy clears AAA against black instead of fading to a low-contrast grey.
        c.text_primary = Color::WHITE;
        c.text_dim = Color::from_rgb8(230, 230, 230);

        // Crisp white structure + an unmistakable yellow focus ring.
        c.border_standard = Color::WHITE;
        c.border_focus = Color::from_rgb8(255, 255, 0); // high-visibility yellow

        // A bright, legible accent fill. Stays distinct from the yellow focus
        // ring and keeps the white checkmark/knob it backs readable on top.
        c.accent.stops[ACCENT_PRIMARY_IDX] = Color::from_rgb8(0, 153, 255);
        c.accent.stops[ACCENT_HOVER_IDX] = Color::from_rgb8(64, 176, 255);

        // Keep semantic colours bright enough to read on black.
        c.semantic.success = Color::from_rgb8(0, 230, 118);
        c.semantic.warning = Color::from_rgb8(255, 214, 0);
        c.semantic.danger = Color::from_rgb8(255, 82, 82);
        c.semantic.info = Color::from_rgb8(64, 196, 255);

        Self::from_tokens("High Contrast", t)
    }

    /// Return this theme with reduced motion turned on or off. Widgets that
    /// animate consult [`Theme::reduce_motion`] and skip their motion when it
    /// is set, so the resting state is shown immediately.
    pub fn with_reduce_motion(mut self, on: bool) -> Self {
        self.reduce_motion = on;
        self
    }

    /// Return this theme with every text size scaled by `factor` (`1.0` =
    /// unchanged). The factor multiplies the whole type scale, the base body
    /// size, and the line heights, so widgets that read their font size from
    /// the theme grow together — and because layout measures the same scaled
    /// sizes, the boxes around the text grow to fit and nothing clips.
    ///
    /// Clamped to a sane range so a stray setting can't make text vanish or
    /// blow up the layout. Idempotent on top of an already-scaled theme is
    /// not assumed — call this once on a fresh theme.
    pub fn with_text_scale(mut self, factor: f32) -> Self {
        let factor = factor.clamp(0.5, 4.0);
        self.text_scale = factor;

        let scale = &mut self.tokens.typography.scale;
        scale.xs *= factor;
        scale.sm *= factor;
        scale.base *= factor;
        scale.lg *= factor;
        scale.xl *= factor;
        scale.x2l *= factor;
        scale.x3l *= factor;
        scale.x4l *= factor;

        self.tokens.font_size_base *= factor;

        self
    }

    /// Return this theme scaled uniformly by a global **UI scale** factor
    /// (`1.0` = unchanged): spacing, radii, the type scale, and the base body
    /// size are all multiplied together, so the entire interface grows or
    /// shrinks as one unit. This is the user-facing "make everything bigger /
    /// smaller" knob, distinct from [`Theme::with_text_scale`] which only
    /// resizes type.
    ///
    /// The legacy `padding_standard` / `gap_standard` / `radius_standard`
    /// shortcuts are re-derived from the freshly scaled tokens so widgets that
    /// still read those keep in step. `radii.full` (the pill sentinel) is left
    /// alone — a fully-round corner stays fully round at any scale.
    ///
    /// Clamped to a sane range so a stray setting can't collapse the layout or
    /// blow it up. Call once on a fresh theme; it is not assumed idempotent on
    /// top of an already-scaled theme.
    pub fn with_ui_scale(mut self, factor: f32) -> Self {
        let factor = factor.clamp(0.5, 3.0);
        self.ui_scale = factor;

        let sp = &mut self.tokens.spacing;
        sp.s0 *= factor;
        sp.s1 *= factor;
        sp.s2 *= factor;
        sp.s3 *= factor;
        sp.s4 *= factor;
        sp.s6 *= factor;
        sp.s8 *= factor;
        sp.s12 *= factor;
        sp.s16 *= factor;

        let r = &mut self.tokens.radii;
        r.sm *= factor;
        r.md *= factor;
        r.lg *= factor;
        r.xl *= factor;
        // `full` is a "make it a pill" sentinel, not a real length — leave it.

        let scale = &mut self.tokens.typography.scale;
        scale.xs *= factor;
        scale.sm *= factor;
        scale.base *= factor;
        scale.lg *= factor;
        scale.xl *= factor;
        scale.x2l *= factor;
        scale.x3l *= factor;
        scale.x4l *= factor;

        self.tokens.font_size_base *= factor;

        // Keep the legacy shortcuts consistent with the scaled tokens.
        self.radius_standard = self.tokens.radii.md as f64;
        self.padding_standard = self.tokens.spacing.s3;
        self.gap_standard = self.tokens.spacing.s3;

        self
    }

    /// Apply the user's low-vision preferences to this theme: pick the
    /// high-contrast colour track when requested, fold in the global UI-scale
    /// and text-scale factors (UI scale first so it grows the whole system,
    /// then text scale layers additional type growth on top), and carry the
    /// reduced-motion flag. One call turns a base theme into the themed surface
    /// every widget should draw against.
    pub fn with_a11y(
        self,
        settings: &crate::a11y::A11ySettings,
        text_scale: f32,
        ui_scale: f32,
    ) -> Self {
        let base = if settings.high_contrast {
            Self::high_contrast()
        } else {
            self
        };
        base.with_ui_scale(ui_scale)
            .with_text_scale(text_scale)
            .with_reduce_motion(settings.reduce_motion)
    }
}

#[cfg(test)]
mod tests {
    //! Pin the dark theme's accent shortcuts so a swap of the indigo
    //! ramp or the [`ACCENT_PRIMARY_IDX`] / [`ACCENT_HOVER_IDX`] indices
    //! becomes a visible test failure rather than a silent visual
    //! regression. The concept (`nirios-concept/src/components/`) uses
    //! indigo-500 (`#6366f1`) 14× as the dominant accent fill and
    //! indigo-600 (`#4f46e5`) only 4× as the hover shade — pin both.
    use super::*;
    use crate::color_compat::ColorCompat;
    use crate::tokens::colors::{ACCENT_HOVER_IDX, ACCENT_PRIMARY_IDX};

    fn rgb(c: Color) -> [u8; 3] { [c.r(), c.g(), c.b()] }

    #[test]
    fn dark_accent_primary_maps_to_indigo_500() {
        let theme = Theme::dark();
        assert_eq!(rgb(theme.accent_primary), [0x63, 0x66, 0xf1], "indigo-500 #6366f1");
    }

    #[test]
    fn dark_accent_hover_maps_to_indigo_600() {
        let theme = Theme::dark();
        assert_eq!(rgb(theme.accent_hover), [0x4f, 0x46, 0xe5], "indigo-600 #4f46e5");
    }

    #[test]
    fn accent_indices_pin_to_concept_palette_positions() {
        // If these constants ever drift the rest of the assertions in
        // this module are rendered meaningless — pin them here so a
        // ramp re-ordering trips this test first.
        assert_eq!(ACCENT_PRIMARY_IDX, 5);
        assert_eq!(ACCENT_HOVER_IDX, 4);
    }

    #[test]
    fn high_contrast_beats_dark_on_fg_bg_contrast() {
        use crate::a11y::contrast_ratio;
        let dark = Theme::dark();
        let hc = Theme::high_contrast();

        let dark_ratio = contrast_ratio(dark.text_primary, dark.bg_app);
        let hc_ratio = contrast_ratio(hc.text_primary, hc.bg_app);

        assert!(
            hc_ratio > dark_ratio,
            "high_contrast fg/bg ({hc_ratio:.2}:1) must exceed dark ({dark_ratio:.2}:1)",
        );
        // It should also clear WCAG AAA (7:1) — and in fact hit the 21:1 max
        // for pure white-on-black.
        assert!(hc_ratio >= 7.0, "high_contrast must clear WCAG AAA, got {hc_ratio:.2}:1");
        assert!((hc_ratio - 21.0).abs() < 0.01, "white-on-black is the 21:1 maximum");
    }

    #[test]
    fn text_scale_multiplies_every_type_size() {
        let base = Theme::dark();
        let base_sm = base.tokens.typography.scale.sm;
        let base_base = base.tokens.typography.scale.base;
        let base_x4l = base.tokens.typography.scale.x4l;
        let base_font = base.tokens.font_size_base;

        let scaled = Theme::dark().with_text_scale(1.5);
        assert_eq!(scaled.text_scale, 1.5);
        assert!((scaled.tokens.typography.scale.sm - base_sm * 1.5).abs() < 1e-4);
        assert!((scaled.tokens.typography.scale.base - base_base * 1.5).abs() < 1e-4);
        assert!((scaled.tokens.typography.scale.x4l - base_x4l * 1.5).abs() < 1e-4);
        assert!((scaled.tokens.font_size_base - base_font * 1.5).abs() < 1e-4);
    }

    #[test]
    fn text_scale_of_one_is_identity() {
        let base = Theme::dark();
        let same = Theme::dark().with_text_scale(1.0);
        assert_eq!(same.text_scale, 1.0);
        assert_eq!(same.tokens.typography.scale.base, base.tokens.typography.scale.base);
    }

    #[test]
    fn text_scale_is_clamped_to_a_sane_range() {
        // Absurd factors can't make text vanish or explode the layout.
        let tiny = Theme::dark().with_text_scale(0.0);
        assert!(tiny.text_scale >= 0.5);
        let huge = Theme::dark().with_text_scale(100.0);
        assert!(huge.text_scale <= 4.0);
    }

    #[test]
    fn reduce_motion_flag_threads_onto_the_theme() {
        assert!(!Theme::dark().reduce_motion, "default is motion on");
        assert!(Theme::dark().with_reduce_motion(true).reduce_motion);
        assert!(!Theme::dark().with_reduce_motion(false).reduce_motion);
    }

    #[test]
    fn with_a11y_picks_high_contrast_track_and_folds_in_scale_and_motion() {
        use crate::a11y::{A11ySettings, contrast_ratio};
        let settings = A11ySettings {
            high_contrast: true,
            reduce_motion: true,
            screen_reader: false,
        };
        let theme = Theme::dark().with_a11y(&settings, 2.0, 1.0);
        // High-contrast colours.
        assert_eq!(theme.name, "High Contrast");
        assert!(contrast_ratio(theme.text_primary, theme.bg_app) >= 7.0);
        // Reduced motion + text scale folded in.
        assert!(theme.reduce_motion);
        assert_eq!(theme.text_scale, 2.0);
        let base_sm = Theme::high_contrast().tokens.typography.scale.sm;
        assert!((theme.tokens.typography.scale.sm - base_sm * 2.0).abs() < 1e-4);
    }

    #[test]
    fn with_a11y_all_off_keeps_base_colours() {
        use crate::a11y::A11ySettings;
        let off = A11ySettings::default();
        let theme = Theme::dark().with_a11y(&off, 1.0, 1.0);
        assert_eq!(theme.name, "Dark", "no high contrast → keep the base theme");
        assert!(!theme.reduce_motion);
        assert_eq!(theme.text_scale, 1.0);
        assert_eq!(theme.ui_scale, 1.0);
    }

    #[test]
    fn ui_scale_multiplies_spacing_radii_and_type_together() {
        let base = Theme::dark();
        let bs = base.tokens.spacing.s3;
        let br = base.tokens.radii.md;
        let bt = base.tokens.typography.scale.base;
        let bfull = base.tokens.radii.full;

        let scaled = Theme::dark().with_ui_scale(1.5);
        assert_eq!(scaled.ui_scale, 1.5);
        assert!((scaled.tokens.spacing.s3 - bs * 1.5).abs() < 1e-4, "spacing grows");
        assert!((scaled.tokens.radii.md - br * 1.5).abs() < 1e-4, "radii grow");
        assert!((scaled.tokens.typography.scale.base - bt * 1.5).abs() < 1e-4, "type grows");
        // The pill sentinel is left untouched — still fully round.
        assert_eq!(scaled.tokens.radii.full, bfull, "radii.full is a sentinel, not scaled");
        // Legacy shortcuts track the scaled tokens.
        assert!((scaled.padding_standard - bs * 1.5).abs() < 1e-4, "padding shortcut tracks spacing");
        assert!((scaled.radius_standard - (br as f64) * 1.5).abs() < 1e-4, "radius shortcut tracks radii");
    }

    #[test]
    fn ui_scale_of_one_is_identity() {
        let base = Theme::dark();
        let same = Theme::dark().with_ui_scale(1.0);
        assert_eq!(same.ui_scale, 1.0);
        assert_eq!(same.tokens.spacing.s3, base.tokens.spacing.s3);
        assert_eq!(same.tokens.typography.scale.base, base.tokens.typography.scale.base);
    }

    #[test]
    fn ui_scale_is_clamped_to_a_sane_range() {
        assert!(Theme::dark().with_ui_scale(0.0).ui_scale >= 0.5);
        assert!(Theme::dark().with_ui_scale(100.0).ui_scale <= 3.0);
    }

    #[test]
    fn ui_scale_and_text_scale_compose_on_type() {
        // UI scale grows the whole system; text scale layers extra type growth
        // on top — so a heading ends up at base × ui × text, while spacing only
        // picks up the UI factor (not the text factor).
        let base = Theme::dark();
        let base_type = base.tokens.typography.scale.base;
        let base_gap = base.tokens.spacing.s3;

        let composed = Theme::dark().with_ui_scale(1.25).with_text_scale(1.2);
        assert_eq!(composed.ui_scale, 1.25);
        assert_eq!(composed.text_scale, 1.2);
        assert!(
            (composed.tokens.typography.scale.base - base_type * 1.25 * 1.2).abs() < 1e-3,
            "type = base × ui × text",
        );
        assert!(
            (composed.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-3,
            "spacing only picks up the UI factor, not text",
        );
    }

    #[test]
    fn high_contrast_uses_a_strong_distinct_focus_ring() {
        use crate::a11y::contrast_ratio;
        let hc = Theme::high_contrast();
        // The focus ring must itself be highly visible against the background.
        assert!(
            contrast_ratio(hc.border_focus, hc.bg_app) >= 7.0,
            "focus ring must clear AAA against the background",
        );
        // …and be a different colour from the standard border so focus reads
        // as a distinct state, not just "a border".
        assert_ne!(
            (hc.border_focus.r(), hc.border_focus.g(), hc.border_focus.b()),
            (hc.border_standard.r(), hc.border_standard.g(), hc.border_standard.b()),
        );
    }
}
