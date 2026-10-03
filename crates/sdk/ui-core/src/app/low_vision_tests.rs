use super::*;
use crate::a11y::contrast_ratio;
use crate::tokens::TextRole;

fn app() -> App {
    App::new(State::new(), Box::new(crate::widget::Text::new("t")))
}

#[test]
fn low_vision_setters_thread_onto_the_theme_and_survive_toggles() {
    // Case `defaults_are_off`.
    {
        let a = app();
        assert!(!a.high_contrast());
        assert!(!a.reduce_motion());
        assert_eq!(a.text_scale(), 1.0);
        assert!(!a.theme.reduce_motion);
        assert_eq!(a.theme.text_scale, 1.0);
    }

    // Case `toggle_high_contrast_swaps_the_theme`.
    {
        let mut a = app();
        let plain_ratio = contrast_ratio(a.theme.text_primary, a.theme.bg_app);

        let now_on = a.toggle_high_contrast();
        assert!(now_on);
        assert!(a.high_contrast());
        let hc_ratio = contrast_ratio(a.theme.text_primary, a.theme.bg_app);
        assert!(
            hc_ratio > plain_ratio,
            "high contrast must raise fg/bg contrast ({hc_ratio:.1} vs {plain_ratio:.1})",
        );
        assert_eq!(a.theme.name, "High contrast");

        let now_off = a.toggle_high_contrast();
        assert!(!now_off);
        assert!(!a.high_contrast());
        let restored = contrast_ratio(a.theme.text_primary, a.theme.bg_app);
        assert!(
            (restored - plain_ratio).abs() < 1e-6,
            "off restores base colours"
        );
    }

    // Case `text_scale_setter_scales_the_theme_type_size`.
    {
        let mut a = app();
        let base = a.theme.tokens.typography.size(TextRole::Body);
        a.set_text_scale(2.0);
        assert_eq!(a.text_scale(), 2.0);
        assert!((a.theme.tokens.typography.size(TextRole::Body) - base * 2.0).abs() < 1e-3);
        // A scale change must re-run layout so the larger text doesn't clip.
        assert!(a.layout.layout_dirty);
    }

    // Case `text_scale_survives_a_high_contrast_toggle`.
    {
        let mut a = app();
        a.set_text_scale(1.5);
        let hc_base = Theme::high_contrast()
            .tokens
            .typography
            .size(TextRole::Body);
        a.set_high_contrast(true);
        assert_eq!(
            a.text_scale(),
            1.5,
            "scale preference is preserved across the swap"
        );
        assert!((a.theme.tokens.typography.size(TextRole::Body) - hc_base * 1.5).abs() < 1e-3);
        assert_eq!(a.theme.name, "High contrast");
    }

    // Case `reduce_motion_setter_threads_onto_the_theme`.
    {
        let mut a = app();
        assert!(!a.theme.reduce_motion);
        a.set_reduce_motion(true);
        assert!(a.reduce_motion());
        assert!(
            a.theme.reduce_motion,
            "the flag reaches the theme widgets draw against"
        );
        a.set_reduce_motion(false);
        assert!(!a.theme.reduce_motion);
    }

    // Case `set_theme_keeps_low_vision_preferences`.
    {
        let mut a = app();
        a.set_text_scale(1.25);
        a.set_reduce_motion(true);
        a.set_high_contrast(true);
        // Swap the *base* theme — the a11y layer must re-apply on top.
        a.set_theme(Theme::light());
        assert!(a.theme.reduce_motion);
        assert_eq!(a.text_scale(), 1.25);
        // High contrast still wins over the freshly-set light base, in the
        // base's polarity.
        assert_eq!(a.theme.name, "High contrast light");
    }

    // Case `ui_scale_setter_grows_spacing_radii_and_type_together`.
    {
        let mut a = app();
        let base_gap = a.theme.tokens.spacing.s3;
        let base_radius = a.theme.tokens.radii.control;
        let base_type = a.theme.tokens.typography.size(TextRole::Body);
        a.set_ui_scale(1.5);
        assert_eq!(a.ui_scale(), 1.5);
        assert!(
            (a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-3,
            "spacing grows"
        );
        assert!(
            (a.theme.tokens.radii.control - base_radius * 1.5).abs() < 1e-3,
            "radii grow"
        );
        assert!(
            (a.theme.tokens.typography.size(TextRole::Body) - base_type * 1.5).abs() < 1e-3,
            "type grows"
        );
        // A UI-scale change must re-run layout so the whole interface re-flows.
        assert!(a.layout.layout_dirty);
    }

    // Case `ui_scale_and_text_scale_compose_on_the_live_theme`.
    {
        let mut a = app();
        let base_type = a.theme.tokens.typography.size(TextRole::Body);
        let base_gap = a.theme.tokens.spacing.s3;
        a.set_ui_scale(1.25);
        a.set_text_scale(1.2);
        assert_eq!(a.ui_scale(), 1.25);
        assert_eq!(a.text_scale(), 1.2);
        // Type picks up BOTH factors; spacing only the UI factor.
        assert!(
            (a.theme.tokens.typography.size(TextRole::Body) - base_type * 1.25 * 1.2).abs() < 1e-2
        );
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-2);
    }

    // Case `ui_scale_survives_a_high_contrast_toggle`.
    {
        let mut a = app();
        a.set_ui_scale(1.5);
        let hc_gap = Theme::high_contrast().tokens.spacing.s3;
        a.set_high_contrast(true);
        assert_eq!(
            a.ui_scale(),
            1.5,
            "UI-scale preference is preserved across the swap"
        );
        assert!((a.theme.tokens.spacing.s3 - hc_gap * 1.5).abs() < 1e-2);
        assert_eq!(a.theme.name, "High contrast");
    }
}

struct AppearanceRoot {
    intent: crate::theme::AppearanceIntent,
}

impl Widget for AppearanceRoot {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, _t: &TaffyTree<()>, _n: NodeId, _o: Point) {}
    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _tr: &mut TextRenderer,
        _theme: &Theme,
        _p: &crate::presence::PresenceCtx,
    ) {
    }
    fn appearance_intent(&self) -> Option<crate::theme::AppearanceIntent> {
        Some(self.intent.clone())
    }
    fn name(&self) -> &'static str {
        "AppearanceRoot"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
}

fn intent(
    theme: Theme,
    ui_scale: f32,
    text_scale: f32,
    high_contrast: bool,
) -> crate::theme::AppearanceIntent {
    crate::theme::AppearanceIntent {
        base_theme: theme,
        high_contrast,
        reduce_motion: false,
        text_scale,
        ui_scale,
    }
}

#[test]
fn appearance_reconcile_is_a_noop_when_off_and_applies_live_otherwise() {
    // Case `appearance_reconcile_off_is_a_noop`.
    {
        let root = AppearanceRoot {
            intent: intent(Theme::light(), 1.5, 1.3, true),
        };
        let mut a = App::new(State::new(), Box::new(root));
        assert!(!a.reconcile_appearance());
        assert_eq!(a.theme.name, "Dark");
        assert_eq!(a.ui_scale(), 1.0);
    }

    // Case `appearance_reconcile_applies_scale_theme_and_contrast_live`.
    {
        let base_gap = Theme::high_contrast().tokens.spacing.s3;
        let base_type = Theme::high_contrast()
            .tokens
            .typography
            .size(TextRole::Body);
        // Light base + high_contrast=true → high-contrast track wins; ui 1.5, text 1.2.
        let root = AppearanceRoot {
            intent: intent(Theme::light(), 1.5, 1.2, true),
        };
        let mut a = App::new(State::new(), Box::new(root));
        a.appearance_reconcile = true;
        assert!(
            a.reconcile_appearance(),
            "first reconcile applies the intent"
        );
        assert_eq!(a.ui_scale(), 1.5);
        assert_eq!(a.text_scale(), 1.2);
        assert!(a.high_contrast());
        assert_eq!(
            a.theme.name, "High contrast light",
            "high_contrast overrides the Light base with its own polarity"
        );
        // Spacing grew by the UI factor; type by UI × text.
        assert!((a.theme.tokens.spacing.s3 - base_gap * 1.5).abs() < 1e-1);
        assert!(
            (a.theme.tokens.typography.size(TextRole::Body) - base_type * 1.5 * 1.2).abs() < 1e-1
        );
        assert!(
            a.layout.layout_dirty,
            "a live appearance change re-runs layout"
        );
        assert!(
            !a.reconcile_appearance(),
            "unchanged intent does not re-theme"
        );
    }
}

#[test]
fn host_appearance_rethemes_through_the_appearance_intent() {
    use crate::host_appearance::{HostAppearance, Scheme};
    let mut a = app();
    let light = HostAppearance {
        scheme: Some(Scheme::Light),
        ..HostAppearance::default()
    };
    assert!(a.apply_host_appearance(light));
    assert_eq!(a.theme.name, "Light");
    assert_eq!(a.theme.bg_app, Theme::light().bg_app);
    assert!(
        !a.apply_host_appearance(light),
        "an unchanged host is a no-op"
    );

    let accented = HostAppearance {
        accent: Some([0xE0, 0x40, 0x80]),
        ..light
    };
    assert!(
        a.apply_host_appearance(accented),
        "an accent change alone re-themes"
    );
    assert_eq!(
        a.theme.tokens.colors.intent.you,
        Theme::light().tokens.colors.presence.hue(
            crate::tokens::PresenceHue::nearest(vello::peniko::Color::from_rgb8(0xE0, 0x40, 0x80))
                .expect("a pink has a hue")
        ),
        "the host accent is the person's own colour"
    );

    assert!(a.apply_host_appearance(HostAppearance {
        high_contrast: true,
        reduce_motion: true,
        ..accented
    }));
    assert!(a.high_contrast());
    assert!(a.reduce_motion());
    assert_eq!(a.theme.name, "High contrast light");
}
