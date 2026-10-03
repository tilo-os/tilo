#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::dnd::DragPayload;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::widget::{AppWidget, Button, Widget};

#[test]
fn widget_trait_defaults_presence_draw_and_token_radius() {
    // Case `widget_defaults_for_collab_methods`.
    {
        let b = Button::new("b", "label", None);
        assert!(tilo_ui::presence::capability::presence_key(&b).is_none());
        assert!(tilo_ui::permissions::capability::permission_scope(&b).is_none());
        assert!(!b.accepts_drop(&DragPayload::Text { value: "x".into() }));
        assert!(!b.wants_pointer_capture());
    }

    // Case `draw_takes_presence_ctx`.
    {
        fn _assert_draw_sig() {
            fn _call<W: Widget>(
                w: &mut W,
                scene: &mut vello::Scene,
                transform: vello::kurbo::Affine,
                tr: &mut tilo_ui::text_render::TextRenderer,
                theme: &tilo_ui::Theme,
                presence: &PresenceCtx,
            ) {
                w.draw(scene, transform, tr, theme, presence);
            }
        }
        fn _is_appwidget<W: AppWidget>() {}
    }

    // Case `button_uses_token_radius`.
    {
        use tilo_ui::tokens::Tokens;
        let mut t = Tokens::dark();
        t.radii.control = 99.0;
        let theme = tilo_ui::Theme {
            name: "T".into(),
            border_standard: t.colors.border_standard,
            border_focus: t.colors.border_focus,
            text_primary: t.colors.text_primary,
            text_dim: t.colors.text_dim,
            text_tertiary: t.colors.text_tertiary,
            status_success: t.colors.semantic.success,
            radius_standard: f64::from(t.radii.control),
            padding_standard: t.spacing.s3,
            gap_standard: t.spacing.s3,
            reduce_motion: false,
            reduce_transparency: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            caret_width: 2.0,
            forced_colors: false,
            tokens: t,
        };
        assert_eq!(theme.radius_standard, 99.0);
        assert_eq!(theme.tokens.radii.control, 99.0);
    }
}
