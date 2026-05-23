use vello::peniko::Color;

/// Index into the accent ramp that maps to the concept's dominant
/// indigo-500 fill (`#6366f1`). Used wherever a widget needs the "primary
/// accent" colour (active dot, slider active track, checkbox tick,
/// toggle on-state, etc.). The concept uses indigo-500 14× across
/// `nirios-concept/src/components/` versus indigo-600 (4 hits) which is
/// reserved for the *hover* state.
pub const ACCENT_PRIMARY_IDX: usize = 5;

/// Index into the accent ramp that maps to the concept's hover-state
/// indigo-600 (`#4f46e5`). Bound to a const so future ramp swaps stay
/// coherent with [`ACCENT_PRIMARY_IDX`].
pub const ACCENT_HOVER_IDX: usize = 4;

#[derive(Clone, Debug)]
pub struct ColorRamp { pub stops: [Color; 11] }

impl ColorRamp {
    pub fn len(&self) -> usize { self.stops.len() }
}

impl std::ops::Index<usize> for ColorRamp {
    type Output = Color;
    fn index(&self, i: usize) -> &Color { &self.stops[i] }
}

#[derive(Clone, Debug)]
pub struct Surfaces {
    pub base: Color,
    pub raised: Color,
    pub overlay: Color,
    pub glass: Color,
    pub glass_blur_px: f32,
    /// White rounded-3xl card that frames each window tile in the concept
    /// (`bg-white` in the React mock). Lives on top of the dark shell bg.
    pub window_card: Color,
    /// Subtle border around the card (`border-white/20` in the concept).
    pub window_card_border: Color,
}

#[derive(Clone, Debug)]
pub struct SemanticColors { pub success: Color, pub warning: Color, pub danger: Color, pub info: Color }

#[derive(Clone, Debug)]
pub struct Colors {
    pub neutral: ColorRamp,
    pub accent: ColorRamp,
    pub semantic: SemanticColors,
    pub surfaces: Surfaces,
    pub text_primary: Color,
    pub text_dim: Color,
    pub border_standard: Color,
    pub border_focus: Color,
    /// Translucent surface ≈ Tailwind `bg-white/5` (alpha 13 ≈ 0.05·255).
    /// Use for "barely there" hover veneers and faint dividers.
    pub surface_white_5: Color,
    /// Translucent surface ≈ Tailwind `bg-white/10` (alpha 26 ≈ 0.10·255).
    pub surface_white_10: Color,
    /// Translucent surface ≈ Tailwind `bg-white/20` (alpha 51 ≈ 0.20·255).
    pub surface_white_20: Color,
    /// Translucent border ≈ Tailwind `border-white/10` (alpha 26).
    pub border_white_10: Color,
    /// Translucent border ≈ Tailwind `border-white/20` (alpha 51).
    pub border_white_20: Color,
}

impl Colors {
    pub fn dark() -> Self {
        // Slate-950 → slate-50, one Tailwind step per ramp slot.
        Self {
            neutral: ColorRamp { stops: [
                Color::rgb8(  2,   6,  23), // slate-950
                Color::rgb8( 15,  23,  42), // slate-900
                Color::rgb8( 30,  41,  59), // slate-800
                Color::rgb8( 51,  65,  85), // slate-700
                Color::rgb8( 71,  85, 105), // slate-600
                Color::rgb8(100, 116, 139), // slate-500
                Color::rgb8(148, 163, 184), // slate-400
                Color::rgb8(203, 213, 225), // slate-300
                Color::rgb8(226, 232, 240), // slate-200
                Color::rgb8(241, 245, 249), // slate-100
                Color::rgb8(248, 250, 252), // slate-50
            ]},
            // Indigo-950 → indigo-50; index 5 is the concept's `indigo-500`
            // accent primary (#6366f1).
            accent: ColorRamp { stops: [
                Color::rgb8( 30,  27,  75), // indigo-950
                Color::rgb8( 49,  46, 129), // indigo-900
                Color::rgb8( 55,  48, 163), // indigo-800
                Color::rgb8( 67,  56, 202), // indigo-700
                Color::rgb8( 79,  70, 229), // indigo-600
                Color::rgb8( 99, 102, 241), // indigo-500 ← accent primary
                Color::rgb8(129, 140, 248), // indigo-400
                Color::rgb8(165, 180, 252), // indigo-300
                Color::rgb8(199, 210, 254), // indigo-200
                Color::rgb8(224, 231, 255), // indigo-100
                Color::rgb8(238, 242, 255), // indigo-50
            ]},
            semantic: SemanticColors {
                success: Color::rgb8( 16, 185, 129), // emerald-500
                warning: Color::rgb8(245, 158,  11), // amber-500
                danger:  Color::rgb8(239,  68,  68), // red-500
                info:    Color::rgb8( 96, 165, 250), // blue-400
            },
            surfaces: Surfaces {
                base:   Color::rgb8(15, 23, 42),       // slate-900
                raised: Color::rgb8(30, 41, 59),       // slate-800
                // Top-bar backdrop — concept uses near-opaque black under the
                // bar chrome. `bg-black/80` = alpha 204 (0.8 · 255 ≈ 204);
                // previous 205 was a one-bit rounding artefact.
                overlay: Color::rgba8(0, 0, 0, 204),   // black/80
                // Overlay-panel "glass" — concept's QuickSettings and
                // Collaborators popover use `bg-slate-900/80` = alpha 204.
                // (GlobalSearch + WorkspaceSettings use /70; we pick the
                // higher-visibility /80 here because it's the more prominent
                // panel treatment in the mockup.)
                glass:   Color::rgba8(15, 23, 42, 204),// slate-900/80
                glass_blur_px: 24.0,
                window_card: Color::WHITE,
                window_card_border: Color::rgba8(255, 255, 255, 51), // white/20
            },
            text_primary: Color::WHITE,
            text_dim: Color::rgb8(148, 163, 184), // slate-400
            border_standard: Color::rgb8(51, 65, 85), // slate-700
            border_focus: Color::rgb8(99, 102, 241),  // indigo-500
            surface_white_5:  Color::rgba8(255, 255, 255, 13),
            surface_white_10: Color::rgba8(255, 255, 255, 26),
            surface_white_20: Color::rgba8(255, 255, 255, 51),
            border_white_10:  Color::rgba8(255, 255, 255, 26),
            border_white_20:  Color::rgba8(255, 255, 255, 51),
        }
    }

    pub fn light() -> Self {
        Self {
            neutral: ColorRamp { stops: [
                Color::rgb8(255, 255, 255), Color::rgb8(245, 245, 248), Color::rgb8(235, 235, 240),
                Color::rgb8(220, 220, 225), Color::rgb8(200, 200, 205), Color::rgb8(160, 160, 165),
                Color::rgb8(120, 120, 125), Color::rgb8(90, 90, 95), Color::rgb8(60, 60, 65),
                Color::rgb8(30, 30, 35), Color::rgb8(10, 10, 12),
            ]},
            accent: ColorRamp { stops: [
                Color::rgb8(240, 250, 255), Color::rgb8(220, 240, 255), Color::rgb8(180, 220, 250),
                Color::rgb8(130, 200, 250), Color::rgb8(80, 170, 240), Color::rgb8(40, 150, 230),
                Color::rgb8(0, 120, 215), Color::rgb8(0, 100, 200), Color::rgb8(0, 75, 170),
                Color::rgb8(0, 50, 120), Color::rgb8(0, 25, 60),
            ]},
            semantic: SemanticColors {
                success: Color::rgb8(0, 150, 70),
                warning: Color::rgb8(200, 130, 0),
                danger: Color::rgb8(200, 50, 50),
                info: Color::rgb8(0, 120, 200),
            },
            surfaces: Surfaces {
                base: Color::rgb8(240, 240, 245),
                raised: Color::WHITE,
                overlay: Color::rgba8(255, 255, 255, 230),
                glass: Color::rgba8(245, 245, 250, 180),
                glass_blur_px: 24.0,
                window_card: Color::WHITE,
                window_card_border: Color::rgba8(0, 0, 0, 30),
            },
            text_primary: Color::BLACK,
            text_dim: Color::rgb8(100, 100, 105),
            border_standard: Color::rgb8(200, 200, 205),
            border_focus: Color::rgb8(0, 120, 215),
            // Light theme keeps the same translucent-white tokens —
            // they're meant to layer over arbitrary darker fills (e.g.
            // window cards), not over the page background, so the
            // alpha-on-white semantics are preserved across themes.
            surface_white_5:  Color::rgba8(255, 255, 255, 13),
            surface_white_10: Color::rgba8(255, 255, 255, 26),
            surface_white_20: Color::rgba8(255, 255, 255, 51),
            border_white_10:  Color::rgba8(255, 255, 255, 26),
            border_white_20:  Color::rgba8(255, 255, 255, 51),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Pin the dark theme's palette to Tailwind ground truth so that
    //! drift away from the nirios-concept palette becomes a visible
    //! test failure rather than a silent visual regression. Each
    //! assertion below references the Tailwind class the value is
    //! sourced from; see https://tailwindcss.com/docs/customizing-colors
    //! for the canonical hex codes.
    //!
    //! These tests do not rely on cross-crate state: they exercise
    //! `Colors::dark()` directly, so they are cheap to run via
    //! `cargo test -p quick-ui --lib tokens`.
    use super::*;

    fn rgb(c: Color) -> [u8; 3] {
        // peniko 0.1.x exposes straight (non-premultiplied) u8
        // components directly on `Color`.
        [c.r, c.g, c.b]
    }

    fn rgba(c: Color) -> [u8; 4] {
        [c.r, c.g, c.b, c.a]
    }

    #[test]
    fn dark_neutral_ramp_is_tailwind_slate_950_through_50() {
        let n = &Colors::dark().neutral;
        assert_eq!(rgb(n[0]),  [  2,   6,  23], "slate-950 #020617");
        assert_eq!(rgb(n[1]),  [ 15,  23,  42], "slate-900 #0f172a");
        assert_eq!(rgb(n[2]),  [ 30,  41,  59], "slate-800 #1e293b");
        assert_eq!(rgb(n[3]),  [ 51,  65,  85], "slate-700 #334155");
        assert_eq!(rgb(n[4]),  [ 71,  85, 105], "slate-600 #475569");
        assert_eq!(rgb(n[5]),  [100, 116, 139], "slate-500 #64748b");
        assert_eq!(rgb(n[6]),  [148, 163, 184], "slate-400 #94a3b8");
        assert_eq!(rgb(n[7]),  [203, 213, 225], "slate-300 #cbd5e1");
        assert_eq!(rgb(n[8]),  [226, 232, 240], "slate-200 #e2e8f0");
        assert_eq!(rgb(n[9]),  [241, 245, 249], "slate-100 #f1f5f9");
        assert_eq!(rgb(n[10]), [248, 250, 252], "slate-50  #f8fafc");
    }

    #[test]
    fn dark_accent_ramp_is_tailwind_indigo_950_through_50() {
        let a = &Colors::dark().accent;
        assert_eq!(rgb(a[0]),  [ 30,  27,  75], "indigo-950 #1e1b4b");
        assert_eq!(rgb(a[1]),  [ 49,  46, 129], "indigo-900 #312e81");
        assert_eq!(rgb(a[2]),  [ 55,  48, 163], "indigo-800 #3730a3");
        assert_eq!(rgb(a[3]),  [ 67,  56, 202], "indigo-700 #4338ca");
        assert_eq!(rgb(a[4]),  [ 79,  70, 229], "indigo-600 #4f46e5");
        // accent[5] is the concept's primary `bg-indigo-500` (most-used
        // accent fill in the concept — 14 occurrences vs 4 for indigo-600).
        assert_eq!(rgb(a[5]),  [ 99, 102, 241], "indigo-500 #6366f1");
        assert_eq!(rgb(a[6]),  [129, 140, 248], "indigo-400 #818cf8");
        assert_eq!(rgb(a[7]),  [165, 180, 252], "indigo-300 #a5b4fc");
        assert_eq!(rgb(a[8]),  [199, 210, 254], "indigo-200 #c7d2fe");
        assert_eq!(rgb(a[9]),  [224, 231, 255], "indigo-100 #e0e7ff");
        assert_eq!(rgb(a[10]), [238, 242, 255], "indigo-50  #eef2ff");
    }

    #[test]
    fn dark_semantic_colors_match_tailwind() {
        let s = &Colors::dark().semantic;
        assert_eq!(rgb(s.success), [ 16, 185, 129], "emerald-500 #10b981");
        assert_eq!(rgb(s.warning), [245, 158,  11], "amber-500   #f59e0b");
        assert_eq!(rgb(s.danger),  [239,  68,  68], "red-500     #ef4444");
        assert_eq!(rgb(s.info),    [ 96, 165, 250], "blue-400    #60a5fa");
    }

    #[test]
    fn dark_surfaces_match_concept_layered_palette() {
        let s = &Colors::dark().surfaces;
        // base: solid slate-900 — concept's gradient bottom anchor
        // (`from-slate-900 to-slate-950`, DesktopView.tsx:210).
        assert_eq!(rgb(s.base),   [ 15,  23,  42], "slate-900 #0f172a");
        // raised: slate-800 — concept's `bg-slate-800` card fill
        // (e.g. WorkspaceSettingsModal avatar tile, OverviewView pill).
        assert_eq!(rgb(s.raised), [ 30,  41,  59], "slate-800 #1e293b");
        // overlay: black/80 — concept's top-bar `bg-black/80`.
        // alpha 204 ≈ 0.80 × 255 (rounds to 204, not 205).
        assert_eq!(rgba(s.overlay), [0, 0, 0, 204], "black/80");
        // glass: slate-900/80 — concept's QuickSettings + Collaborators
        // popover `bg-slate-900/80 backdrop-blur-md`.
        assert_eq!(rgba(s.glass), [ 15,  23,  42, 204], "slate-900/80");
        // window_card: WHITE — concept's `bg-white shadow-2xl` window frame.
        assert_eq!(rgba(s.window_card), [255, 255, 255, 255], "white");
        // window_card_border: white/20 — concept's `border-white/20`.
        // alpha 51 ≈ 0.20 × 255.
        assert_eq!(rgba(s.window_card_border), [255, 255, 255, 51], "white/20");
    }

    #[test]
    fn dark_translucent_white_tokens_match_tailwind_alpha_bytes() {
        // Tailwind's `/N` alpha utilities round to 0.N · 255:
        //   /5  → 12.75 → 13
        //   /10 → 25.5  → 26
        //   /20 → 51.0  → 51
        let c = Colors::dark();
        assert_eq!(rgba(c.surface_white_5),  [255, 255, 255, 13], "white/5");
        assert_eq!(rgba(c.surface_white_10), [255, 255, 255, 26], "white/10");
        assert_eq!(rgba(c.surface_white_20), [255, 255, 255, 51], "white/20");
        assert_eq!(rgba(c.border_white_10),  [255, 255, 255, 26], "border-white/10");
        assert_eq!(rgba(c.border_white_20),  [255, 255, 255, 51], "border-white/20");
    }

    #[test]
    fn dark_text_and_border_tokens_match_concept() {
        let c = Colors::dark();
        assert_eq!(rgba(c.text_primary), [255, 255, 255, 255], "white");
        // text_dim → slate-400 (most common dim text in the concept;
        // the smaller-caption `text-slate-500` use-case is intentionally
        // not represented as its own token — see audit notes).
        assert_eq!(rgb(c.text_dim), [148, 163, 184], "slate-400 #94a3b8");
        // border_standard → slate-700, the explicit border colour
        // QuickSettings and the OverviewView mini-window cards use under
        // their slate-900 fills (`border border-slate-700`).
        assert_eq!(rgb(c.border_standard), [ 51,  65,  85], "slate-700 #334155");
        // border_focus → indigo-500, the concept's focus-ring colour.
        assert_eq!(rgb(c.border_focus), [ 99, 102, 241], "indigo-500 #6366f1");
    }
}
