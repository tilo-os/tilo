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
