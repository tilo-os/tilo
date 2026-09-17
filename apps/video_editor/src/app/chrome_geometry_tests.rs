//! The alignment the editor's chrome promises, read back from the production
//! layout functions rather than from a copy of their arithmetic: a control
//! cluster shares one centre line, the two pane headings share one band, a
//! sibling row keeps the clearance its neighbours have, and the topbar is
//! inset the same at both edges.
//!
//! A child module of `app` so it can reach the private geometry — `regions`,
//! `transport_buttons`, `inspector_layout` — which is exactly what the draw
//! pass and the hit-test consume.

use super::*;

/// A seeded project laid out at `w`×`h`, the way `--shots` composes one.
fn app_sized(w: f64, h: f64) -> VideoEditorApp {
    let mut doc = AutoCommit::new();
    VideoDoc::seed_sample(&mut doc);
    let mut app = VideoEditorApp::new();
    app.rect = Rect::new(0.0, 0.0, w, h);
    app.sync(&mut doc, &automerge::ROOT);
    app
}

fn assert_close(actual: f64, expected: f64, what: &str) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "{what}: {actual} != {expected}"
    );
}

#[test]
fn the_transport_cluster_shares_one_centre_line() {
    let app = app_sized(1280.0, 800.0);
    let r = app.regions();
    let cy = r.transport.y0 + TRANSPORT_CONTROL_CY;
    for (i, (_, rect)) in app.transport_buttons(&r).into_iter().enumerate() {
        assert_close(rect.center().y, cy, &format!("transport button {i} centre"));
    }
    let history = app.history_buttons(&r);
    assert!(
        !history.is_empty(),
        "a 1280px strip carries the history buttons"
    );
    for (verb, rect) in history {
        assert_close(rect.center().y, cy, &format!("{verb:?} button centre"));
    }

    // The readout, at the sizes the strip draws it: the playhead time centred
    // on the same line, every smaller part standing on its foot.
    let mut tr = TextRenderer::new();
    let heights = [
        f64::from(tr.measure_weighted("00:10:00", 18.0, None, TextWeight::Bold).1),
        f64::from(tr.measure("/  00:28:00", 13.0, None).1),
        f64::from(tr.measure("SHUTTLE ▶▶ 8×", 10.0, None).1),
        f64::from(tr.measure("1920×1080 · 30 fps", 11.5, None).1),
    ];
    let tops = readout_tops(r.transport, cy, heights);
    assert_close(tops[0] + heights[0] / 2.0, cy, "playhead time centre");
    let foot = tops[0] + heights[0];
    for (top, height) in tops.iter().zip(heights).skip(1) {
        assert_close(top + height, foot, "readout part foot");
        assert!(
            *top >= tops[0],
            "a smaller part never starts above the playhead time"
        );
    }
}

#[test]
fn a_tall_readout_is_held_inside_the_strip_rather_than_pushed_above_it() {
    // Line boxes at a 2× text scale: taller than the strip's upper half.
    let transport = Rect::new(0.0, 100.0, 700.0, 100.0 + TRANSPORT_H);
    let heights = [45.0, 32.5, 25.0, 28.75];
    let tops = readout_tops(transport, transport.y0 + TRANSPORT_CONTROL_CY, heights);
    assert!(
        tops[0] >= transport.y0 + 2.0,
        "the playhead time keeps its top inside the strip: {}",
        tops[0]
    );
    let foot = tops[0] + heights[0];
    for (top, height) in tops.iter().zip(heights).skip(1) {
        assert_close(top + height, foot, "readout part foot at large text");
    }
}

#[test]
fn the_bin_heading_chip_sits_on_the_heading_band_and_clear_of_the_list() {
    let app = app_sized(1280.0, 800.0);
    let r = app.regions();
    let add = app.add_title_rect(&r);
    assert_close(
        add.center().y,
        r.bin.y0 + PANE_HEADING_Y0 + PANE_HEADING_H / 2.0,
        "\"+ Title\" centre against the heading band",
    );
    assert_close(add.height(), PANE_HEADING_H, "\"+ Title\" height");
    assert!(
        add.y1 <= r.bin.y0 + VideoEditorApp::BIN_LIST_TOP,
        "the heading row ends before the first bin row starts"
    );
    // The export chip lives in the strip under the topbar, beside the
    // inspector's heading: same band, same centre line.
    assert_close(
        r.export_chip.center().y,
        r.bin.y0 + PANE_HEADING_Y0 + PANE_HEADING_H / 2.0,
        "export chip centre against the heading band",
    );
}

#[test]
fn the_transition_caption_keeps_the_clearance_every_other_row_has() {
    let app = app_sized(1280.0, 800.0);
    let r = app.regions();
    let l = app.inspector_layout(&r);
    let opacity = l.opacity.expect("a video clip is selected");
    let brightness = l.brightness.expect("a video clip has a grade");
    let fade = l.fade_in.expect("every clip has a fade row");
    // A slider label is drawn at `track.y0 - 20`, the transition caption at
    // `transition_y - 20`: equal advances from the row above mean equal
    // clearance over that row's knobs.
    assert_close(
        l.transition_y - fade.y1,
        brightness.y0 - opacity.y1,
        "transition caption clearance vs an ordinary slider row",
    );
}

#[test]
fn the_topbar_is_inset_equally_at_both_edges_and_its_commands_share_one_width() {
    let app = app_sized(1280.0, 800.0);
    let r = app.regions();
    assert!(!r.bin_overlay, "1280px docks the bin");
    assert_close(
        r.project_btn.x0 - r.topbar.x0,
        r.topbar.x1 - r.export_btn.x1,
        "leading inset vs trailing inset",
    );
    for (name, rect) in [
        ("import", r.import_btn),
        ("split", r.split_btn),
        ("export", r.export_btn),
    ] {
        assert_close(rect.width(), TOPBAR_BTN_W, &format!("{name} width"));
        assert_close(
            rect.center().y,
            r.topbar.y0 + TOPBAR_H / 2.0,
            &format!("{name} centre on the topbar's midline"),
        );
    }
}
