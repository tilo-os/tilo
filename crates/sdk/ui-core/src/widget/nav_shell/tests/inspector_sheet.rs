use super::*;
use crate::widget::SheetDetent;

const W: u32 = 420;
const H: u32 = 800;

/// A narrow window with its inspector open as a sheet, settled at half
/// height; and the content's input counter.
fn sheet_open() -> (FrameHarness, SharedRc<Cell<usize>>) {
    let (content, events) = Probe::boxed("content");
    let shell = NavShell::new()
        .content(content)
        .inspector(Probe::boxed("inspector").0);
    let mut h = harness(shell, W, H, Theme::dark());
    settle(&mut h, W, H);
    h.root_mut::<NavShell>().open_inspector();
    settle(&mut h, W, H);
    (h, events)
}

fn detent(h: &FrameHarness) -> Option<SheetDetent> {
    h.root::<NavShell>().inspector_detent()
}

fn grip(h: &FrameHarness) -> Point {
    let panel = geometry(h).inspector_column;
    Point::new(panel.center().x, panel.y0 + 4.0)
}

/// Drag the grip to `y` at a hand's pace, many reports on the way, and let
/// go: it settles where it was put, not where a throw would carry it.
fn drag_grip_to(h: &mut FrameHarness, y: f64) {
    let from = grip(h);
    let moved = |h: &mut FrameHarness, y: f64| {
        h.dispatch_to_root(&crate::synthetic_input::cursor_moved(from.x, y));
        h.relayout_at((W, H)).expect("layout");
    };
    moved(h, from.y);
    h.dispatch_to_root(&crate::synthetic_input::mouse_press());
    let steps = 80;
    for step in 1..=steps {
        let t = f64::from(step) / f64::from(steps);
        moved(h, from.y + (y - from.y) * t);
    }
    h.dispatch_to_root(&crate::synthetic_input::mouse_release());
    settle(h, W, H);
}

/// The top of the sheet at `detent`, in this window.
fn top_at(h: &FrameHarness, detent: SheetDetent) -> f64 {
    let frame = geometry(h).sheet;
    let [peek, half, full] = [SheetDetent::Peek, SheetDetent::Half, SheetDetent::Full].map(|d| {
        let mut sheet = crate::widget::BottomSheet::new();
        sheet.snap_to(d);
        sheet.geometry(frame, 1.0, true).panel.y0
    });
    match detent {
        SheetDetent::Peek => peek,
        SheetDetent::Half => half,
        SheetDetent::Full => full,
    }
}

#[test]
fn a_narrow_inspector_is_a_sheet_at_half_height_with_the_content_live_above_it() {
    let _settled = crate::motion::testing::settled();
    let (mut h, events) = sheet_open();
    let g = geometry(&h);
    assert_eq!(g.inspector, InspectorPlacement::Sheet);
    assert_eq!(detent(&h), Some(SheetDetent::Half));
    let panel = g.inspector_column;
    assert!(
        (panel.x0, panel.x1, panel.y1) == (0.0, f64::from(W), f64::from(H)),
        "the sheet spans the window's foot: {panel:?}"
    );
    assert!(
        (panel.height() - f64::from(H) / 2.0).abs() < f64::from(H) / 8.0,
        "about half the window: {panel:?}"
    );
    let content = child_rect(&h, "content").expect("the content stands");
    assert!(
        content.y1 <= panel.y0 + 0.5,
        "the content keeps the foot under the sheet clear: {content:?} {panel:?}"
    );
    let before = events.get();
    h.click_at(f64::from(W) / 2.0, panel.y0 / 2.0);
    settle(&mut h, W, H);
    assert!(
        events.get() > before,
        "a press above the sheet is the content's"
    );
    assert!(
        h.root::<NavShell>().is_inspector_open(),
        "and it puts nothing away"
    );
}

/// A surface drawn on works as narrow as a grid of tiles: at 640 wide its
/// inspector docks beside it and the surface stays in view, where a
/// document's needs the reading column and rises as a sheet.
#[test]
fn a_canvas_keeps_its_inspector_beside_it_where_a_document_takes_a_sheet() {
    let _settled = crate::motion::testing::settled();
    for (kind, placement) in [
        (ContentKind::Canvas, InspectorPlacement::Docked),
        (ContentKind::Document, InspectorPlacement::Sheet),
    ] {
        let shell = NavShell::new()
            .content_kind(kind)
            .content(Probe::boxed("content").0)
            .inspector(Probe::boxed("inspector").0);
        let mut h = harness(shell, 640, 480, Theme::dark());
        settle(&mut h, 640, 480);
        h.root_mut::<NavShell>().open_inspector();
        settle(&mut h, 640, 480);
        assert_eq!(geometry(&h).inspector, placement, "{kind:?} at 640x480");
        if kind == ContentKind::Canvas {
            let g = geometry(&h);
            let content = child_rect(&h, "content").expect("the content stands");
            assert!(
                content.width() > 0.0 && content.x1 <= g.inspector_column.x0 + 0.5,
                "the surface stands beside the inspector: {content:?} {:?}",
                g.inspector_column
            );
        }
    }
}

/// A window too short for a half-height sheet to hold the inspector at its
/// docked width opens it at the whole height, the content clear above it.
#[test]
fn a_short_window_opens_its_inspector_sheet_at_the_whole_height() {
    let _settled = crate::motion::testing::settled();
    let (w, h_px) = (W, 480);
    let shell = NavShell::new()
        .content(Probe::boxed("content").0)
        .inspector(Probe::boxed("inspector").0);
    let mut h = harness(shell, w, h_px, Theme::dark());
    settle(&mut h, w, h_px);
    h.root_mut::<NavShell>().open_inspector();
    settle(&mut h, w, h_px);
    let g = geometry(&h);
    assert_eq!(g.inspector, InspectorPlacement::Sheet);
    assert_eq!(detent(&h), Some(SheetDetent::Half));
    let panel = g.inspector_column;
    assert!(
        (panel.y0 - g.sheet.area.y0).abs() < 0.5 && (panel.y1 - f64::from(h_px)).abs() < 0.5,
        "half height is the whole height here: {panel:?} in {:?}",
        g.sheet.area
    );
    assert!(
        panel.height() / 2.0 < f64::from(crate::tokens::Metrics::BASE.inspector_width),
        "the window is short for a half sheet: {panel:?}"
    );
}

#[test]
fn dragging_the_grip_moves_the_sheet_between_its_detents() {
    let _settled = crate::motion::testing::settled();
    let (mut h, _) = sheet_open();
    for (to, expected) in [
        (SheetDetent::Peek, SheetDetent::Peek),
        (SheetDetent::Half, SheetDetent::Half),
        (SheetDetent::Full, SheetDetent::Full),
        (SheetDetent::Half, SheetDetent::Half),
        (SheetDetent::Peek, SheetDetent::Peek),
    ] {
        let y = top_at(&h, to) + 4.0;
        drag_grip_to(&mut h, y);
        assert_eq!(detent(&h), Some(expected), "dragged to {to:?}");
        let top = geometry(&h).inspector_column.y0;
        assert!(
            (top - top_at(&h, expected)).abs() < 0.5,
            "the sheet rests at {expected:?}: {top}"
        );
    }
    // A finger drags it as the pointer does.
    let from = grip(&h);
    let to = top_at(&h, SheetDetent::Half) + 4.0;
    h.touch(7, winit::event::TouchPhase::Started, from.x, from.y);
    for step in 1..=80 {
        let y = from.y + (to - from.y) * f64::from(step) / 80.0;
        h.touch(7, winit::event::TouchPhase::Moved, from.x, y);
        h.relayout_at((W, H)).expect("layout");
    }
    h.touch(7, winit::event::TouchPhase::Ended, from.x, to);
    settle(&mut h, W, H);
    assert_eq!(
        detent(&h),
        Some(SheetDetent::Half),
        "a finger drags the grip"
    );
}

fn grip_node(h: &FrameHarness) -> (crate::accesskit::NodeId, crate::accesskit::Node) {
    h.a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.role() == crate::accesskit::Role::Slider)
        .expect("the sheet's grip is a slider")
}

#[test]
fn the_keyboard_steps_the_sheet_between_its_detents() {
    let _settled = crate::motion::testing::settled();
    let (mut h, _) = sheet_open();
    let (grip, _) = grip_node(&h);
    h.perform_a11y_action(grip, &crate::a11y::Action::Focus)
        .expect("the grip takes the keyboard");
    for (key, expected) in [
        ("ArrowDown", SheetDetent::Peek),
        ("ArrowUp", SheetDetent::Half),
        ("ArrowUp", SheetDetent::Full),
        ("ArrowDown", SheetDetent::Half),
        ("ArrowDown", SheetDetent::Peek),
    ] {
        h.key(key);
        settle(&mut h, W, H);
        assert_eq!(detent(&h), Some(expected), "after {key}");
    }
}

#[test]
fn the_sheet_is_a_named_region_whose_grip_steps_its_detent() {
    let _settled = crate::motion::testing::settled();
    let (mut h, _) = sheet_open();
    let region = h
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.label() == Some("Inspector"))
        .expect("the sheet is a region named for the inspector");
    let (grip, node) = grip_node(&h);
    assert!(
        region.1.children().contains(&grip),
        "the grip stands in the region"
    );
    assert_eq!(node.value(), Some("Half height"));
    assert!(node.supports_action(crate::accesskit::Action::Increment));
    assert!(node.supports_action(crate::accesskit::Action::Decrement));
    h.perform_a11y_action(grip, &crate::a11y::Action::Increment)
        .expect("the grip steps up");
    settle(&mut h, W, H);
    assert_eq!(detent(&h), Some(SheetDetent::Full));
    assert_eq!(grip_node(&h).1.value(), Some("Full height"));
    let (grip, _) = grip_node(&h);
    h.perform_a11y_action(grip, &crate::a11y::Action::Decrement)
        .expect("the grip steps down");
    settle(&mut h, W, H);
    assert_eq!(detent(&h), Some(SheetDetent::Half));
}

/// The window's pixels as the CPU rasterizes the frame's scene, RGBA rows.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn pixels(h: &FrameHarness, ground: vello::peniko::Color) -> Vec<u8> {
    let mut canvas = crate::render_thread::cpu_raster::CpuCanvas::new();
    canvas
        .rasterize(&crate::retained_raster::FrameContent {
            scene: h.scene(),
            width: W,
            height: H,
            base_color: ground,
            damage_regions: &[],
            scroll_copies: &[],
            full_redraw: true,
            backdrop: &crate::backdrop::BackdropFrame::default(),
        })
        .expect("the frame rasterizes");
    canvas.pixels().to_vec()
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[test]
fn the_sheet_stands_off_the_content_above_it_in_both_themes() {
    let _settled = crate::motion::testing::settled();
    for theme in [Theme::light(), Theme::dark()] {
        let (content, _) = Probe::boxed("content");
        let shell = NavShell::new()
            .content(content)
            .inspector(Probe::boxed("inspector").0);
        let mut h = harness(shell, W, H, theme.clone());
        settle(&mut h, W, H);
        h.root_mut::<NavShell>().open_inspector();
        settle(&mut h, W, H);
        let ground = theme.tokens.colors.surfaces.content;
        let px = pixels(&h, ground);
        let at = |x: f64, y: f64| -> [i32; 3] {
            let i = ((y as usize) * W as usize + x as usize) * 4;
            [0, 1, 2].map(|c| i32::from(px[i + c]))
        };
        let lum = |p: [i32; 3]| p[0] + p[1] + p[2];
        let top = geometry(&h).inspector_column.y0.round();
        let x = f64::from(W) / 4.0;
        let window = at(x, top - 120.0);
        let face = at(x, top + 12.0);
        assert_ne!(
            face, window,
            "the sheet's face is its own level, not the window's ({})",
            theme.name
        );
        // Its top edge is drawn: the floating hairline (and, on a light
        // ground, the shadow it casts) parts it from the content above.
        let stands_off =
            |p: [i32; 3]| (lum(p) - lum(face)).abs() > 3 && (lum(p) - lum(window)).abs() > 3;
        let edge = (-3..4)
            .map(|d| at(x, top + f64::from(d)))
            .collect::<Vec<_>>();
        assert!(
            edge.iter().any(|p| stands_off(*p)),
            "the sheet's top edge parts it from the content ({}): {edge:?} between {window:?} and {face:?}",
            theme.name
        );
    }
}

/// What takes Back's step: the window bar's Back, Escape, or the system's
/// back gesture.
#[derive(Clone, Copy, Debug)]
enum BackInput {
    Bar,
    Escape,
    Gesture,
}

fn go_back(h: &mut FrameHarness, input: BackInput) {
    match input {
        BackInput::Bar => {
            let event = crate::window_toolbar::ToolbarEvent::Action {
                id: super::super::window_toolbar::WINDOW_BACK.into(),
            };
            assert!(h.root_mut::<NavShell>().window_toolbar_event(&event));
        }
        BackInput::Escape => h.key("Escape"),
        BackInput::Gesture => assert!(
            h.nav_gesture(crate::gesture::NavDirection::Back),
            "the sheet takes the back gesture"
        ),
    }
    settle(h, W, H);
}

#[test]
fn back_lowers_or_puts_the_sheet_away_before_it_goes_anywhere() {
    let _settled = crate::motion::testing::settled();
    for input in [BackInput::Bar, BackInput::Escape, BackInput::Gesture] {
        for (from, after) in [
            (SheetDetent::Full, Some(SheetDetent::Half)),
            (SheetDetent::Half, None),
            (SheetDetent::Peek, None),
        ] {
            let shell = NavShell::new()
                .sidebar(Probe::boxed("sidebar").0)
                .content(Probe::boxed("content").0)
                .inspector(Probe::boxed("inspector").0)
                .with_window_toolbar();
            let mut h = harness(shell, W, H, Theme::dark());
            settle(&mut h, W, H);
            h.root_mut::<NavShell>().show(NavPane::Content);
            settle(&mut h, W, H);
            h.root_mut::<NavShell>().open_inspector();
            h.root_mut::<NavShell>().set_inspector_detent(from);
            settle(&mut h, W, H);
            assert_eq!(detent(&h), Some(from));
            assert!(
                h.root::<NavShell>()
                    .window_toolbar()
                    .leading
                    .iter()
                    .any(|action| action.id == super::super::window_toolbar::WINDOW_BACK),
                "the bar offers Back while the sheet shows"
            );
            go_back(&mut h, input);
            let shell = h.root::<NavShell>();
            let shown = shell.is_inspector_open().then(|| detent(&h)).flatten();
            assert_eq!(shown, after, "{input:?} from {from:?}");
            assert_eq!(
                h.root::<NavShell>().front_pane(),
                NavPane::Content,
                "{input:?} from {from:?} takes the sheet's step, not a screen's"
            );
        }
    }
}

/// At each detent the inspector's frame, as the shell reports it and as a
/// link reads it, is where the body is laid out, and what it shows of the
/// body is the sheet under its grip down to the window's foot.
#[test]
fn the_inspector_frame_is_what_the_sheet_shows_at_each_detent() {
    let _settled = crate::motion::testing::settled();
    let (mut h, _) = sheet_open();
    let link = h.root::<NavShell>().inspector_link();
    for detent in [SheetDetent::Peek, SheetDetent::Half, SheetDetent::Full] {
        h.root_mut::<NavShell>().set_inspector_detent(detent);
        settle(&mut h, W, H);
        let frame = h.root::<NavShell>().inspector_frame();
        assert_eq!(
            link.frame(),
            frame,
            "{detent:?}: the link reads the same frame"
        );
        assert_eq!(frame.placement, InspectorPlacement::Sheet);
        assert!(frame.open);
        assert_eq!(frame.detent, Some(detent));
        assert_eq!(
            Some(frame.body),
            child_rect(&h, "inspector"),
            "{detent:?}: the body is laid out in the frame"
        );
        let panel = geometry(&h).inspector_column;
        assert!(
            (panel.y0 - top_at(&h, detent)).abs() < 0.5,
            "{detent:?}: the sheet rests at its detent: {panel:?}"
        );
        let under_grip = Rect::new(panel.x0, frame.body.y0, panel.x1, f64::from(H));
        assert!(
            frame.body.y0 > panel.y0,
            "{detent:?}: the body starts under the grip"
        );
        assert_eq!(
            frame.shown, under_grip,
            "{detent:?}: the frame shows the sheet under its grip down to the foot"
        );
    }
    // The link moves the sheet and puts it away, as the shell's own calls do.
    link.set_detent(SheetDetent::Full);
    settle(&mut h, W, H);
    assert_eq!(detent(&h), Some(SheetDetent::Full));
    link.close();
    settle(&mut h, W, H);
    assert!(!h.root::<NavShell>().is_inspector_open());
    assert!(!link.frame().open);
}
