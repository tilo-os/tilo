//! UI zoom must reach the same geometry painting and input use.
#![allow(clippy::expect_used, reason = "tests fail on missing editor surfaces")]
use super::*;

#[test]
fn ui_zoom_scales_timeline_and_panels() {
    for (ui_scale, text_scale) in [(1.0, 1.0), (1.5, 1.0), (2.0, 1.0), (1.5, 1.25)] {
        let theme = Theme::dark()
            .with_ui_scale(ui_scale)
            .with_text_scale(text_scale);
        let mut app = VideoEditorApp::new();
        let mut taffy = TaffyTree::new();
        app.build_layout(&mut taffy, &theme, &mut HashMap::new());
        app.rect = Rect::new(0.0, 0.0, 1440.0, 810.0);
        let r = app.regions();
        let scale = f64::from(ui_scale);
        assert!((app.text_scale - theme.responsive_scale()).abs() < 1e-6);
        assert!((r.gutter.width() - GUTTER_W * app.chrome_scale()).abs() < 1e-6);
        assert!(app.bin_width() >= BIN_W * scale);
        assert!(app.inspector_width() >= INSPECTOR_W * scale);
        assert!((r.lanes.x0 - r.gutter.x1).abs() < 1e-6);
        let lane = Rect::new(
            r.lanes.x0,
            r.lanes.y0,
            r.lanes.x1,
            r.lanes.y0 + app.lane_height(TrackKind::Video),
        );
        let head = app.track_head_layout(VideoEditorApp::track_head_rect(&r, &lane));
        assert!(
            head.name.width() > 35.0 * scale,
            "track identity has room beside toggles"
        );
        assert!(head.solo.x0 >= head.name.x1 + S1 * scale - 1e-6);
        assert!(head.kind.y0 >= head.name.y1 + S1 * scale - 1e-6);
        assert!(head.meter.y0 >= head.kind.y1 + S1 * scale - 1e-6);
        assert!(head.mute.y1 <= lane.y1);
    }
}
