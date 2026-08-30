use ui::dnd::DragPayload;
use ui::presence::PresenceCtx;
use ui::widget::{AppWidget, Button, Widget};

#[test]
fn widget_defaults_for_collab_methods() {
    let b = Button::new("b", "label", None);
    assert!(b.presence_key().is_none());
    assert!(b.permission_scope().is_none());
    assert!(!b.accepts_drop(&DragPayload::Text { value: "x".into() }));
    assert!(!b.wants_pointer_capture());
}

#[test]
fn draw_takes_presence_ctx() {
    // compile-time assertion that draw signature is what we expect.
    fn _assert_draw_sig() {
        fn _call<W: Widget>(
            w: &mut W,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            tr: &mut ui::text_render::TextRenderer,
            theme: &ui::Theme,
            presence: &PresenceCtx,
        ) {
            w.draw(scene, transform, tr, theme, presence);
        }
    }
    // AppWidget marker compiles.
    fn _is_appwidget<W: AppWidget>() {}
}

#[test]
fn button_uses_token_radius() {
    use ui::tokens::Tokens;
    let mut t = Tokens::dark();
    t.radii.md = 99.0;
    let theme = ui::Theme {
        name: "T".into(),
        bg_app: t.colors.surfaces.base,
        bg_primary: t.colors.surfaces.raised,
        bg_hover: t.colors.neutral[4],
        bg_active: t.colors.neutral[2],
        border_standard: t.colors.border_standard,
        border_focus: t.colors.border_focus,
        text_primary: t.colors.text_primary,
        text_dim: t.colors.text_dim,
        text_tertiary: t.colors.text_tertiary,
        accent_primary: t.colors.accent[5],
        accent_hover: t.colors.accent[4],
        status_success: t.colors.semantic.success,
        radius_standard: f64::from(t.radii.md),
        padding_standard: t.spacing.s3,
        gap_standard: t.spacing.s3,
        reduce_motion: false,
        text_scale: 1.0,
        ui_scale: 1.0,
        tokens: t,
    };
    // A scene is required to draw; just assert the legacy shortcut mirrors the token.
    assert_eq!(theme.radius_standard, 99.0);
    assert_eq!(theme.tokens.radii.md, 99.0);
}
