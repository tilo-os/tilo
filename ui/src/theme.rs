use vello::peniko::Color;
use crate::tokens::{PaletteSpec, Tokens};
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
    /// Secondary text — the second tier of the type hierarchy (captions,
    /// sub-labels). Mirrors `tokens.colors.text_dim`.
    pub text_dim: Color,
    /// Tertiary text — the quietest legible tier (timestamps, hints). Mirrors
    /// `tokens.colors.text_tertiary`; sits below [`Theme::text_dim`].
    pub text_tertiary: Color,
    /// Dominant accent fill. Use for
    /// active states (slider active track, checkbox tick, toggle
    /// on-state, focused caret, currently-selected dot, etc.). For
    /// hover states use [`Theme::accent_hover`] instead.
    pub accent_primary: Color,
    /// Hover-state accent. One step darker than [`Theme::accent_primary`].
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
    fn from_tokens(name: &str, mut tokens: Tokens) -> Self {
        // Keep the terminal palette coherent with whatever colour set this theme
        // ended up with. Presets replace `tokens.colors` wholesale (via
        // `Colors::themed`) after the base tokens are built, so deriving the
        // terminal palette here — the single funnel every constructor passes
        // through — is what stops the terminal from carrying its own hard-coded
        // scheme and makes it track the OS theme instead.
        tokens.terminal_palette = crate::tokens::TerminalPalette::from_colors(&tokens.colors);
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
            text_tertiary: c.text_tertiary,
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
    /// The default: a calm neutral **graphite** dark theme with a restrained
    /// blue accent. See [`crate::tokens::Colors::dark`].
    pub fn dark() -> Self { Self::from_tokens("Dark", Tokens::dark()) }
    /// A clean, faintly-warm neutral light theme.
    pub fn light() -> Self { Self::from_tokens("Light", Tokens::light()) }

    /// Build a theme from a compact [`PaletteSpec`]. The default typography /
    /// spacing / motion tokens come from the matching (dark or light) base;
    /// the whole colour set is expanded by [`Colors::themed`].
    fn from_palette(name: &str, spec: PaletteSpec) -> Self {
        let mut t = if spec.dark { Tokens::dark() } else { Tokens::light() };
        t.colors = crate::tokens::Colors::themed(spec);
        Self::from_tokens(name, t)
    }

    /// The original qOS look — slate + indigo — kept verbatim as a preset so
    /// nothing from the first design is lost. See [`Colors::qos_blue`].
    pub fn qos_blue() -> Self {
        let mut t = Tokens::dark();
        t.colors = crate::tokens::Colors::qos_blue();
        Self::from_tokens("qOS Blue", t)
    }

    /// Dracula — the canonical palette. Its signature **purple** (`#bd93f9`) is
    /// the interactive accent *and* the focus ring (pink `#ff79c6` is reserved,
    /// not the primary accent — using it as the accent drenched the UI pink),
    /// over the canonical green / cyan / red / yellow on a charcoal-violet
    /// ground. Every colour below is a canonical Dracula value.
    pub fn dracula() -> Self {
        Self::from_palette("Dracula", PaletteSpec {
            dark: true,
            base:          Color::from_rgb8(0x28, 0x2A, 0x36), // background
            raised:        Color::from_rgb8(0x34, 0x37, 0x46),
            popover:       Color::from_rgb8(0x21, 0x22, 0x2C), // "darker"
            text:          Color::from_rgb8(0xF8, 0xF8, 0xF2), // foreground
            text_dim:      Color::from_rgb8(0xC9, 0xCB, 0xE0),
            text_tertiary: Color::from_rgb8(0x8A, 0x90, 0xBE), // lifted comment
            border:        Color::from_rgb8(0x44, 0x47, 0x5A), // current line
            focus:         Color::from_rgb8(0xBD, 0x93, 0xF9), // purple
            accent:        Color::from_rgb8(0xBD, 0x93, 0xF9), // purple (signature accent)
            accent_hover:  Color::from_rgb8(0xA6, 0x81, 0xDB), // purple, one step darker
            success:       Color::from_rgb8(0x50, 0xFA, 0x7B), // green
            warning:       Color::from_rgb8(0xF1, 0xFA, 0x8C), // yellow
            danger:        Color::from_rgb8(0xFF, 0x55, 0x55), // red
            info:          Color::from_rgb8(0x8B, 0xE9, 0xFD), // cyan
            window_card:   None,
        })
    }

    /// Nord — Polar Night ground, Snow Storm text, Frost accent, Aurora
    /// semantics. The canonical Nord palette.
    pub fn nord() -> Self {
        Self::from_palette("Nord", PaletteSpec {
            dark: true,
            base:          Color::from_rgb8(0x2E, 0x34, 0x40), // nord0
            raised:        Color::from_rgb8(0x3B, 0x42, 0x52), // nord1
            popover:       Color::from_rgb8(0x27, 0x2C, 0x36),
            text:          Color::from_rgb8(0xEC, 0xEF, 0xF4), // nord6
            text_dim:      Color::from_rgb8(0xB8, 0xC0, 0xCE),
            text_tertiary: Color::from_rgb8(0x7B, 0x84, 0x94),
            border:        Color::from_rgb8(0x43, 0x4C, 0x5E), // nord2
            focus:         Color::from_rgb8(0x81, 0xA1, 0xC1), // nord9
            accent:        Color::from_rgb8(0x88, 0xC0, 0xD0), // nord8 frost
            accent_hover:  Color::from_rgb8(0x6F, 0xAF, 0xC4),
            success:       Color::from_rgb8(0xA3, 0xBE, 0x8C), // nord14
            warning:       Color::from_rgb8(0xEB, 0xCB, 0x8B), // nord13
            danger:        Color::from_rgb8(0xBF, 0x61, 0x6A), // nord11
            info:          Color::from_rgb8(0x81, 0xA1, 0xC1), // nord9
            window_card:   None,
        })
    }

    /// Gruvbox (dark) — the canonical warm retro ramp, near-square corners.
    pub fn gruvbox() -> Self {
        let mut t = Self::from_palette("Gruvbox", PaletteSpec {
            dark: true,
            base:          Color::from_rgb8(0x28, 0x28, 0x28), // bg0
            raised:        Color::from_rgb8(0x3C, 0x38, 0x36), // bg1
            popover:       Color::from_rgb8(0x1D, 0x20, 0x21), // bg0_hard
            text:          Color::from_rgb8(0xEB, 0xDB, 0xB2), // fg1
            text_dim:      Color::from_rgb8(0xBD, 0xAE, 0x93), // fg3
            text_tertiary: Color::from_rgb8(0x92, 0x83, 0x74), // gray
            border:        Color::from_rgb8(0x50, 0x49, 0x45), // bg2
            focus:         Color::from_rgb8(0xFE, 0x80, 0x19), // orange
            accent:        Color::from_rgb8(0xFA, 0xBD, 0x2F), // yellow
            accent_hover:  Color::from_rgb8(0xE0, 0xA5, 0x26),
            success:       Color::from_rgb8(0xB8, 0xBB, 0x26), // green
            warning:       Color::from_rgb8(0xFA, 0xBD, 0x2F), // yellow
            danger:        Color::from_rgb8(0xFB, 0x49, 0x34), // red
            info:          Color::from_rgb8(0x83, 0xA5, 0x98), // blue
            window_card:   None,
        });
        // Gruvbox reads as square-edged; keep the near-flat corner shape.
        t.tokens.radii.sm = 1.0;
        t.tokens.radii.md = 2.0;
        t.tokens.radii.lg = 3.0;
        t.tokens.radii.xl = 4.0;
        t.radius_standard = 2.0;
        t
    }

    /// Gruvbox Light — the canonical light variant on warm cream paper.
    pub fn gruvbox_light() -> Self {
        let mut t = Self::from_palette("Gruvbox Light", PaletteSpec {
            dark: false,
            base:          Color::from_rgb8(0xFB, 0xF1, 0xC7), // bg0
            raised:        Color::from_rgb8(0xF9, 0xF5, 0xD7), // bg0_hard
            popover:       Color::from_rgb8(0xF9, 0xF5, 0xD7),
            text:          Color::from_rgb8(0x3C, 0x38, 0x36), // fg1
            text_dim:      Color::from_rgb8(0x66, 0x5C, 0x54), // fg3
            text_tertiary: Color::from_rgb8(0x7C, 0x6F, 0x64), // fg4
            border:        Color::from_rgb8(0xD5, 0xC4, 0xA1), // bg2
            focus:         Color::from_rgb8(0xAF, 0x3A, 0x03), // orange (dark)
            accent:        Color::from_rgb8(0xD7, 0x99, 0x21), // yellow (dark)
            accent_hover:  Color::from_rgb8(0xB5, 0x76, 0x14),
            success:       Color::from_rgb8(0x98, 0x97, 0x1A), // green (dark)
            warning:       Color::from_rgb8(0xD7, 0x99, 0x21), // yellow (dark)
            danger:        Color::from_rgb8(0xCC, 0x24, 0x1D), // red (dark)
            info:          Color::from_rgb8(0x45, 0x85, 0x88), // blue (dark)
            window_card:   None,
        });
        t.tokens.radii.sm = 1.0;
        t.tokens.radii.md = 2.0;
        t.tokens.radii.lg = 3.0;
        t.tokens.radii.xl = 4.0;
        t.radius_standard = 2.0;
        t
    }

    /// Catppuccin Mocha — the canonical dark flavour (blue accent, mauve /
    /// green / yellow / red / sky on a soft violet-charcoal ground).
    pub fn catppuccin_mocha() -> Self {
        Self::from_palette("Catppuccin Mocha", PaletteSpec {
            dark: true,
            base:          Color::from_rgb8(0x1E, 0x1E, 0x2E), // base
            raised:        Color::from_rgb8(0x31, 0x32, 0x44), // surface0
            popover:       Color::from_rgb8(0x18, 0x18, 0x25), // mantle
            text:          Color::from_rgb8(0xCD, 0xD6, 0xF4), // text
            text_dim:      Color::from_rgb8(0xA6, 0xAD, 0xC8), // subtext0
            text_tertiary: Color::from_rgb8(0x7F, 0x84, 0x9C), // overlay1
            border:        Color::from_rgb8(0x45, 0x47, 0x5A), // surface1
            focus:         Color::from_rgb8(0x89, 0xB4, 0xFA), // blue
            accent:        Color::from_rgb8(0x89, 0xB4, 0xFA), // blue
            accent_hover:  Color::from_rgb8(0x6E, 0x9D, 0xF0),
            success:       Color::from_rgb8(0xA6, 0xE3, 0xA1), // green
            warning:       Color::from_rgb8(0xF9, 0xE2, 0xAF), // yellow
            danger:        Color::from_rgb8(0xF3, 0x8B, 0xA8), // red
            info:          Color::from_rgb8(0x89, 0xDC, 0xEB), // sky
            window_card:   None,
        })
    }

    /// Catppuccin Latte — the canonical light flavour.
    pub fn catppuccin_latte() -> Self {
        Self::from_palette("Catppuccin Latte", PaletteSpec {
            dark: false,
            base:          Color::from_rgb8(0xEF, 0xF1, 0xF5), // base
            raised:        Color::from_rgb8(0xFF, 0xFF, 0xFF),
            popover:       Color::from_rgb8(0xFF, 0xFF, 0xFF),
            text:          Color::from_rgb8(0x4C, 0x4F, 0x69), // text
            text_dim:      Color::from_rgb8(0x6C, 0x6F, 0x85), // subtext0
            text_tertiary: Color::from_rgb8(0x8C, 0x8F, 0xA1), // overlay1
            border:        Color::from_rgb8(0xCC, 0xD0, 0xDA), // surface0
            focus:         Color::from_rgb8(0x1E, 0x66, 0xF5), // blue
            accent:        Color::from_rgb8(0x1E, 0x66, 0xF5), // blue
            accent_hover:  Color::from_rgb8(0x15, 0x52, 0xCC),
            success:       Color::from_rgb8(0x40, 0xA0, 0x2B), // green
            warning:       Color::from_rgb8(0xDF, 0x8E, 0x1D), // yellow
            danger:        Color::from_rgb8(0xD2, 0x0F, 0x39), // red
            info:          Color::from_rgb8(0x04, 0xA5, 0xE5), // sky
            window_card:   None,
        })
    }

    /// Solarized (dark) — Ethan Schoonover's canonical palette.
    pub fn solarized() -> Self {
        let mut t = Self::from_palette("Solarized", PaletteSpec {
            dark: true,
            base:          Color::from_rgb8(0x00, 0x2B, 0x36), // base03
            raised:        Color::from_rgb8(0x07, 0x36, 0x42), // base02
            popover:       Color::from_rgb8(0x00, 0x21, 0x2B),
            text:          Color::from_rgb8(0x93, 0xA1, 0xA1), // base1
            text_dim:      Color::from_rgb8(0x83, 0x94, 0x96), // base0
            text_tertiary: Color::from_rgb8(0x58, 0x6E, 0x75), // base01
            border:        Color::from_rgb8(0x0F, 0x4A, 0x59),
            focus:         Color::from_rgb8(0x26, 0x8B, 0xD2), // blue
            accent:        Color::from_rgb8(0x2A, 0xA1, 0x98), // cyan
            accent_hover:  Color::from_rgb8(0x1E, 0x8C, 0x84),
            success:       Color::from_rgb8(0x85, 0x99, 0x00), // green
            warning:       Color::from_rgb8(0xB5, 0x89, 0x00), // yellow
            danger:        Color::from_rgb8(0xDC, 0x32, 0x2F), // red
            info:          Color::from_rgb8(0x26, 0x8B, 0xD2), // blue
            window_card:   None,
        });
        t.tokens.radii.sm = 0.0;
        t.tokens.radii.md = 0.0;
        t.tokens.radii.lg = 2.0;
        t.tokens.radii.xl = 4.0;
        t.radius_standard = 0.0;
        t
    }

    /// OLED Dark — a true-black (`#000000`) ground for OLED panels. Every
    /// surface tier is pure black so lit pixels are reserved for content, and
    /// cards / panels read by a **near-white outline** (like the high-contrast
    /// preset's borders) rather than an elevation tint — the only way a card
    /// separates from the ground when both are true black. White text and an
    /// accent tuned to pop on black complete it.
    pub fn oled() -> Self {
        Self::from_palette("OLED", PaletteSpec {
            dark: true,
            base:          Color::from_rgb8(0x00, 0x00, 0x00), // true black
            raised:        Color::from_rgb8(0x00, 0x00, 0x00), // true-black cards
            popover:       Color::from_rgb8(0x00, 0x00, 0x00),
            text:          Color::from_rgb8(0xFF, 0xFF, 0xFF),
            text_dim:      Color::from_rgb8(0xB4, 0xB7, 0xBE),
            text_tertiary: Color::from_rgb8(0x7A, 0x7D, 0x85),
            // Near-white card / panel outline so a true-black card reads
            // against the true-black ground (cf. high_contrast's white border).
            border:        Color::from_rgb8(0xE6, 0xE6, 0xEA),
            focus:         Color::from_rgb8(0x4C, 0x9A, 0xFF),
            accent:        Color::from_rgb8(0x4C, 0x9A, 0xFF), // pops on black
            accent_hover:  Color::from_rgb8(0x3D, 0x86, 0xE6),
            success:       Color::from_rgb8(0x37, 0xD6, 0x7A),
            warning:       Color::from_rgb8(0xFF, 0xB0, 0x20),
            danger:        Color::from_rgb8(0xFF, 0x5C, 0x5C),
            info:          Color::from_rgb8(0x4C, 0xC2, 0xFF),
            // Keep window framing true black so it disappears behind app
            // content rather than flashing a card on a black desktop.
            window_card:   Some(Color::from_rgb8(0x00, 0x00, 0x00)),
        })
    }

    /// Beige — a warm paper-white light theme (not a cold clinical white),
    /// with legible warm-neutral typography and a soft terracotta accent.
    pub fn warm_light() -> Self {
        Self::from_palette("Beige", PaletteSpec {
            dark: false,
            base:          Color::from_rgb8(0xF3, 0xEE, 0xE3), // warm paper
            raised:        Color::from_rgb8(0xFB, 0xF7, 0xEF),
            popover:       Color::from_rgb8(0xFF, 0xFF, 0xFF),
            text:          Color::from_rgb8(0x33, 0x30, 0x2A), // warm near-black
            text_dim:      Color::from_rgb8(0x6B, 0x65, 0x5A),
            text_tertiary: Color::from_rgb8(0x97, 0x90, 0x7F),
            border:        Color::from_rgb8(0xDD, 0xD3, 0xC2),
            focus:         Color::from_rgb8(0xBC, 0x6C, 0x3C), // terracotta
            accent:        Color::from_rgb8(0xBC, 0x6C, 0x3C), // terracotta
            accent_hover:  Color::from_rgb8(0xA1, 0x5A, 0x2E),
            success:       Color::from_rgb8(0x5B, 0x8C, 0x4E), // olive
            warning:       Color::from_rgb8(0xC0, 0x8A, 0x2E),
            danger:        Color::from_rgb8(0xB4, 0x46, 0x2F), // brick
            info:          Color::from_rgb8(0x3E, 0x7C, 0xA8),
            window_card:   None,
        })
    }

    /// Rosé Pine — the canonical "main" (dark) flavour.
    pub fn rose_pine() -> Self {
        Self::from_palette("Rosé Pine", PaletteSpec {
            dark: true,
            base:          Color::from_rgb8(0x19, 0x17, 0x24), // base
            raised:        Color::from_rgb8(0x1F, 0x1D, 0x2E), // surface
            popover:       Color::from_rgb8(0x16, 0x14, 0x1F),
            text:          Color::from_rgb8(0xE0, 0xDE, 0xF4), // text
            text_dim:      Color::from_rgb8(0x90, 0x8C, 0xAA), // subtle
            text_tertiary: Color::from_rgb8(0x6E, 0x6A, 0x86), // muted
            border:        Color::from_rgb8(0x40, 0x3D, 0x52), // highlight med
            focus:         Color::from_rgb8(0xC4, 0xA7, 0xE7), // iris
            accent:        Color::from_rgb8(0xEB, 0xBC, 0xBA), // rose
            accent_hover:  Color::from_rgb8(0xD9, 0xA6, 0xA4),
            success:       Color::from_rgb8(0x9C, 0xCF, 0xD8), // foam
            warning:       Color::from_rgb8(0xF6, 0xC1, 0x77), // gold
            danger:        Color::from_rgb8(0xEB, 0x6F, 0x92), // love
            info:          Color::from_rgb8(0xC4, 0xA7, 0xE7), // iris
            window_card:   None,
        })
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
        c.text_tertiary = Color::from_rgb8(210, 210, 210);

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

    /// Resolve a theme by name. Accepts the exact display name (e.g.
    /// `"Catppuccin Mocha"`) as well as loose forms — case-insensitive, with
    /// spaces / hyphens / underscores ignored (`"gruvbox-light"`,
    /// `"catppuccinmocha"`) — so the same lookup serves the settings picker,
    /// the persisted preference, and the `--shots` theme override. Returns
    /// `None` for an unknown name; callers pick their own fallback.
    pub fn named(name: &str) -> Option<Self> {
        let key: String = name
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect();
        Some(match key.as_str() {
            "dark" | "qosdark" => Self::dark(),
            "light" => Self::light(),
            "qosblue" | "blue" | "midnight" => Self::qos_blue(),
            "catppuccinmocha" | "mocha" => Self::catppuccin_mocha(),
            "catppuccinlatte" | "latte" => Self::catppuccin_latte(),
            "dracula" => Self::dracula(),
            "nord" => Self::nord(),
            "gruvbox" | "gruvboxdark" => Self::gruvbox(),
            "gruvboxlight" => Self::gruvbox_light(),
            "solarized" | "solarizeddark" => Self::solarized(),
            "oled" | "oleddark" | "black" => Self::oled(),
            "beige" | "warmlight" | "paper" => Self::warm_light(),
            "rosépine" | "rosepine" | "rosé" | "rose" => Self::rose_pine(),
            "highcontrast" | "contrast" => Self::high_contrast(),
            _ => return None,
        })
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
    //! Pin each theme's accent shortcuts and identity so a ramp swap or an
    //! [`ACCENT_PRIMARY_IDX`] / [`ACCENT_HOVER_IDX`] index change becomes a
    //! visible failure. The default is the neutral graphite theme (restrained
    //! blue accent); the preserved concept look lives in `qos_blue`.
    use super::*;
    use crate::color_compat::ColorCompat;
    use crate::tokens::colors::{ACCENT_HOVER_IDX, ACCENT_PRIMARY_IDX};

    fn rgb(c: Color) -> [u8; 3] { [c.r(), c.g(), c.b()] }

    #[test]
    fn dark_default_accent_shortcuts_are_the_restrained_blue() {
        let theme = Theme::dark();
        assert_eq!(rgb(theme.accent_primary), [0x58, 0x91, 0xE8], "restrained blue");
        assert_eq!(rgb(theme.accent_hover), [0x48, 0x7B, 0xD1], "hover one step darker");
        // The default theme carries three distinct text tiers.
        assert_ne!(rgb(theme.text_primary), rgb(theme.text_dim));
        assert_ne!(rgb(theme.text_dim), rgb(theme.text_tertiary));
    }

    #[test]
    fn qos_blue_preserves_the_indigo_concept_accent() {
        let theme = Theme::qos_blue();
        assert_eq!(theme.name, "qOS Blue");
        assert_eq!(rgb(theme.accent_primary), [0x63, 0x66, 0xf1], "indigo-500 #6366f1");
        assert_eq!(rgb(theme.accent_hover), [0x4f, 0x46, 0xe5], "indigo-600 #4f46e5");
    }

    #[test]
    fn accent_indices_pin_to_palette_positions() {
        // If these constants ever drift the rest of the assertions in
        // this module are rendered meaningless — pin them here so a
        // ramp re-ordering trips this test first.
        assert_eq!(ACCENT_PRIMARY_IDX, 5);
        assert_eq!(ACCENT_HOVER_IDX, 4);
    }

    #[test]
    fn preset_palettes_read_as_their_real_canonical_colours() {
        // A per-theme identity pin: each named preset must expose its
        // signature ground + accent, so a preset can't silently drift into
        // the default. These are the canonical upstream hex values.
        let cases: &[(Theme, [u8; 3], [u8; 3])] = &[
            (Theme::dracula(),          [0x28, 0x2A, 0x36], [0xBD, 0x93, 0xF9]),
            (Theme::nord(),             [0x2E, 0x34, 0x40], [0x88, 0xC0, 0xD0]),
            (Theme::gruvbox(),          [0x28, 0x28, 0x28], [0xFA, 0xBD, 0x2F]),
            (Theme::gruvbox_light(),    [0xFB, 0xF1, 0xC7], [0xD7, 0x99, 0x21]),
            (Theme::catppuccin_mocha(), [0x1E, 0x1E, 0x2E], [0x89, 0xB4, 0xFA]),
            (Theme::catppuccin_latte(), [0xEF, 0xF1, 0xF5], [0x1E, 0x66, 0xF5]),
            (Theme::solarized(),        [0x00, 0x2B, 0x36], [0x2A, 0xA1, 0x98]),
            (Theme::oled(),             [0x00, 0x00, 0x00], [0x4C, 0x9A, 0xFF]),
            (Theme::rose_pine(),        [0x19, 0x17, 0x24], [0xEB, 0xBC, 0xBA]),
        ];
        for (theme, base, accent) in cases {
            assert_eq!(rgb(theme.bg_app), *base, "{} ground", theme.name);
            assert_eq!(rgb(theme.accent_primary), *accent, "{} accent", theme.name);
            // The primary/hover accent stops are always pinned exactly.
            assert_eq!(rgb(theme.tokens.colors.accent[ACCENT_PRIMARY_IDX]), *accent);
        }
    }

    #[test]
    fn dracula_roles_map_to_canonical_hexes() {
        // The signature Dracula purple is the accent + focus (NOT pink — using
        // pink as the accent read "too pink"), over the canonical ground /
        // foreground / current-line and the green / yellow / red / cyan
        // semantics. Every value here is a documented canonical Dracula hex.
        let t = Theme::dracula();
        let c = &t.tokens.colors;
        assert_eq!(rgb(t.bg_app), [0x28, 0x2A, 0x36], "background #282a36");
        assert_eq!(rgb(t.text_primary), [0xF8, 0xF8, 0xF2], "foreground #f8f8f2");
        assert_eq!(rgb(c.border_standard), [0x44, 0x47, 0x5A], "current line #44475a");
        assert_eq!(rgb(t.accent_primary), [0xBD, 0x93, 0xF9], "accent = purple #bd93f9");
        assert_eq!(rgb(c.border_focus), [0xBD, 0x93, 0xF9], "focus = purple #bd93f9");
        assert_ne!(rgb(t.accent_primary), [0xFF, 0x79, 0xC6], "accent must NOT be pink");
        assert_eq!(rgb(c.semantic.success), [0x50, 0xFA, 0x7B], "green #50fa7b");
        assert_eq!(rgb(c.semantic.warning), [0xF1, 0xFA, 0x8C], "yellow #f1fa8c");
        assert_eq!(rgb(c.semantic.danger), [0xFF, 0x55, 0x55], "red #ff5555");
        assert_eq!(rgb(c.semantic.info), [0x8B, 0xE9, 0xFD], "cyan #8be9fd");
    }

    #[test]
    fn oled_ground_is_true_black() {
        let t = Theme::oled();
        assert_eq!(rgb(t.bg_app), [0, 0, 0], "OLED must be pure #000000");
        // The card / panel surface is true black too, so cards sit ON true
        // black rather than reading as a near-black tint against it.
        assert_eq!(rgb(t.bg_primary), [0, 0, 0], "OLED cards are true black");
    }

    #[test]
    fn oled_card_outline_is_near_white() {
        // With every surface at true black, a card separates from the ground
        // only by its outline — which must be close to pure white (like the
        // high-contrast preset's borders), not a dark near-black hairline.
        let t = Theme::oled();
        let b = rgb(t.border_standard);
        assert!(
            b.iter().all(|&ch| ch >= 0xC8),
            "OLED card/panel outline must be near-white, got {b:?}",
        );
    }

    #[test]
    fn named_resolves_every_preset_to_its_own_theme() {
        // Every display name round-trips to a theme whose own name matches,
        // and loose spellings resolve to the same theme.
        for name in [
            "Dark", "Light", "qOS Blue", "Catppuccin Mocha", "Catppuccin Latte",
            "Dracula", "Nord", "Gruvbox", "Gruvbox Light", "Solarized", "OLED",
            "Beige", "Rosé Pine", "High Contrast",
        ] {
            let t = Theme::named(name).unwrap_or_else(|| panic!("named({name}) resolves"));
            assert_eq!(t.name, name, "named({name:?}) returns its own theme");
        }
        assert_eq!(Theme::named("gruvbox-light").map(|t| t.name), Some("Gruvbox Light".into()));
        assert_eq!(Theme::named("catppuccinmocha").map(|t| t.name), Some("Catppuccin Mocha".into()));
        assert!(Theme::named("not-a-theme").is_none());
    }

    #[test]
    fn preset_default_body_text_clears_aa_on_its_ground() {
        // Every shipped preset must keep primary body text legible on its
        // own ground — a palette that reads as itself but can't be read is a
        // regression.
        use crate::a11y::contrast_ratio;
        for t in [
            Theme::dark(), Theme::light(), Theme::qos_blue(), Theme::dracula(),
            Theme::nord(), Theme::gruvbox(), Theme::gruvbox_light(),
            Theme::catppuccin_mocha(), Theme::catppuccin_latte(), Theme::solarized(),
            Theme::oled(), Theme::warm_light(), Theme::rose_pine(),
        ] {
            let r = contrast_ratio(t.text_primary, t.bg_app);
            assert!(r >= 4.5, "{} primary text on ground = {r:.2}:1 (< AA)", t.name);
        }
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
