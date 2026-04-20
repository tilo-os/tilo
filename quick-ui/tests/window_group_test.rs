use quick_ui::widget::{WindowGroup, WindowWidth, TileContent, AppKind, TabDescriptor};

#[test]
fn window_width_presets_are_clamped() {
    assert_eq!(WindowWidth::Narrow.pixels(), 400);
    assert_eq!(WindowWidth::Normal.pixels(), 600);
    assert_eq!(WindowWidth::Wide.pixels(), 800);
    assert_eq!(WindowWidth::Custom(100).pixels(), 300);
    assert_eq!(WindowWidth::Custom(5000).pixels(), 1200);
    assert_eq!(WindowWidth::Custom(777).pixels(), 777);
}

#[test]
fn window_group_drag_right_edge_resizes() {
    let mut g = WindowGroup::new(1, vec![(TabDescriptor::new(1, "T1"), TileContent::Stub { kind: AppKind::Terminal, label: "Terminal".into() })], 1, WindowWidth::Normal);
    g.begin_resize(600.0);
    g.update_resize(750.0);
    g.end_resize();
    assert_eq!(g.width().pixels(), 750);
    g.begin_resize(750.0);
    g.update_resize(50.0);
    g.end_resize();
    assert_eq!(g.width().pixels(), 300);
}
