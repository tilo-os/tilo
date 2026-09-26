#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::Theme;
use tilo_ui::tokens::{Metrics, Radii, Spacing, SpacingStep, Tokens};

#[test]
fn colour_and_radius_tokens_have_their_documented_shape() {
    // Case `tokens_dark_has_11_neutral_stops`.
    {
        let t = Tokens::dark();
        assert_eq!(t.colors.neutral.len(), 11);
        assert_ne!(t.colors.neutral[0], t.colors.neutral[10]);
    }

    // Case `radii_scale_is_ordered_4_8_12_24_pill`.
    // The radius steps are strictly ordered, and pinned to the exact scale
    // (Tailwind `rounded`/`-lg`/`-xl`/`-3xl`/`-full`), so a drift in one step is
    // a visible failure rather than a softer corner.
    {
        let r = Radii::default();
        assert!(r.sm < r.md);
        assert!(r.md < r.lg);
        assert!(r.lg < r.xl);
        assert!(r.xl < r.full);
        assert_eq!(r.sm, 4.0);
        assert_eq!(r.md, 8.0);
        assert_eq!(r.lg, 12.0);
        assert_eq!(r.xl, 24.0);
        assert_eq!(r.full, 9999.0);
    }
}

/// The spacing scale is the 4 px base, read by field or resolved by step name.
#[test]
fn spacing_scale_is_4px_base_by_field_and_by_step() {
    assert_eq!(Spacing::default(), Spacing::BASE);
    let s = Spacing::default();
    let cases = [
        ("s0", s.s0, SpacingStep::S0, 0.0),
        ("s1", s.s1, SpacingStep::S1, 4.0),
        ("s2", s.s2, SpacingStep::S2, 8.0),
        ("s3", s.s3, SpacingStep::S3, 12.0),
        ("s4", s.s4, SpacingStep::S4, 16.0),
        ("s6", s.s6, SpacingStep::S6, 24.0),
        ("s8", s.s8, SpacingStep::S8, 32.0),
        ("s12", s.s12, SpacingStep::S12, 48.0),
        ("s16", s.s16, SpacingStep::S16, 64.0),
    ];
    for (name, field, step, want) in cases {
        assert_eq!(field, want, "case {name}: field");
        assert_eq!(s.step(step), want, "case {name}: step");
    }
}

#[test]
fn theme_metrics_follow_the_hig_density_table_and_scale_only_with_ui_scale() {
    // Case `metrics_default_to_the_hig_density_table`.
    // HIG §9.6 density, stated once. Every theme constructor starts from this.
    {
        let m = Metrics::default();
        assert_eq!(m, Metrics::BASE);
        assert_eq!(m.bar_height, 40.0);
        assert_eq!(m.bar_control_height, 28.0);
        assert_eq!(m.panel_row_height, 44.0);
        assert_eq!(m.selector_height, 28.0);
        assert_eq!(m.min_pointer_target, 32.0);
        assert_eq!(m.panel_width_utility, 320.0);
        assert_eq!(m.panel_width_list, 360.0);
        assert_eq!(m.panel_width_sheet, 560.0);
        assert_eq!(m.icon_sm, 14.0);
        assert_eq!(m.icon_md, 20.0);
        assert_eq!(m.icon_lg, 24.0);
        assert_eq!(m.icon_label_gap, 6.0);
        assert_eq!(m.sidebar_width, 280.0);
        assert_eq!(m.drawer_width, 320.0);
        assert_eq!(m.overview_width, 340.0);
        assert_eq!(m.compact_breakpoint, 720.0);
        assert_eq!(
            m.drawer_width, m.panel_width_utility,
            "a slid-in drawer and a docked utility panel are one width"
        );
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            assert_eq!(theme.tokens.metrics, Metrics::BASE, "{}", theme.name);
        }
    }

    // Case `theme_exposes_tokens`.
    {
        let t = Theme::dark();
        assert_eq!(t.padding_standard, t.tokens.spacing.s3);
        assert_eq!(t.gap_standard, t.tokens.spacing.s3);
        // tokens are the source of truth
        assert_eq!(f64::from(t.tokens.radii.md), t.radius_standard);
    }

    // Case `ui_scale_scales_metrics_with_spacing`.
    // `ui_scale` is the one knob that resizes density; metrics follow spacing.
    {
        let scaled = Theme::dark().with_ui_scale(1.5);
        let m = scaled.tokens.metrics;
        assert!((m.bar_height - 60.0).abs() < 1e-4);
        assert!((m.min_pointer_target - 48.0).abs() < 1e-4);
        assert!((m.panel_width_utility - 480.0).abs() < 1e-4);
        assert!((m.compact_breakpoint - 1080.0).abs() < 1e-4);
        assert!(
            (m.bar_height / scaled.tokens.spacing.s4 - 40.0 / 16.0).abs() < 1e-4,
            "bar height keeps its ratio to the spacing scale"
        );
        let text_only = Theme::dark().with_text_scale(2.0);
        assert_eq!(
            text_only.tokens.metrics,
            Metrics::BASE,
            "text scale grows type, never targets or panel widths"
        );
    }

    // Case `text_growth_follows_larger_text_and_never_shrinks`.
    // HIG §9.6: the text-growth rule the responsive shells apply to a
    // text-sized width or breakpoint is `text_scale.max(1.0)`, stated once.
    {
        assert_eq!(Metrics::text_growth(1.0), 1.0);
        assert_eq!(Metrics::text_growth(2.0), 2.0);
        assert_eq!(Metrics::text_growth(1.25), 1.25);
        assert_eq!(Metrics::text_growth(0.75), 1.0, "text never shrinks a pane");
        assert_eq!(Metrics::text_growth(f32::NAN), 1.0, "a broken scale is 1×");
        assert_eq!(Theme::dark().with_text_scale(2.0).text_growth(), 2.0);
        assert_eq!(
            Theme::dark().with_text_scale(2.0).responsive_scale(),
            2.0,
            "the shells' factor is ui_scale × text_growth"
        );
    }
}

use tilo_ui::permissions::{PermissionScope, ResourceRef, Verb};

#[test]
fn permission_scope_constructs() {
    let s = PermissionScope::workspace(Verb::Write);
    assert_eq!(s.op, Verb::Write);
    assert_eq!(s.resource, ResourceRef::Workspace);
}
