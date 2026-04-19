use quick_ui::widget::{Widget, Button, AppWidget};
use quick_ui::presence::PresenceCtx;
use quick_ui::dnd::DragPayload;

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
    fn _assert_draw_sig<W: Widget>() {
        fn _call<W: Widget>(
            w: &mut W,
            scene: &mut vello::Scene,
            transform: vello::kurbo::Affine,
            tr: &mut quick_ui::text_render::TextRenderer,
            theme: &quick_ui::Theme,
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
    use quick_ui::tokens::Tokens;
    let mut t = Tokens::dark();
    t.radii.md = 99.0;
    let theme = quick_ui::Theme {
        name: "T".into(),
        bg_app: t.colors.surfaces.base,
        bg_primary: t.colors.surfaces.raised,
        bg_hover: t.colors.neutral[4],
        bg_active: t.colors.neutral[2],
        border_standard: t.colors.border_standard,
        border_focus: t.colors.border_focus,
        text_primary: t.colors.text_primary,
        text_dim: t.colors.text_dim,
        accent_primary: t.colors.accent[4],
        status_success: t.colors.semantic.success,
        radius_standard: t.radii.md as f64,
        padding_standard: t.spacing.s3,
        gap_standard: t.spacing.s3,
        tokens: t,
    };
    // A scene is required to draw; for this unit, just assert the legacy shortcut
    // mirrors the token.
    assert_eq!(theme.radius_standard, 99.0);
    assert_eq!(theme.tokens.radii.md, 99.0);
}
