//! Shell chrome at each display's own scale.
//!
//! The shell lays out once, in the logical desktop, and each display draws
//! its part at its own density: on a 2x laptop panel beside a 1x monitor,
//! the same top bar is twice as many physical rows on the panel. Input comes
//! back from scene pixels through the same mapping, so a click lands on the
//! control drawn under it on either display.
//!
//! Each case lays the production `FrameBuilder` out across one of the named
//! headless fixtures (`TILO_HEADLESS_OUTPUTS`), reads control rectangles
//! from the accessibility tree the native shell publishes, and routes
//! pointer input through `backend::layout::event_to_logical`, the
//! conversion the compositor applies to every backend event.

use automerge::{AutoCommit, ROOT};
use shell::compositor::backend::headless::output_fixture;
use shell::compositor::backend::layout::{event_to_logical, placements};
use shell::compositor::backend::{
    BackendEvent, Output, combined_geometry, logical_geometry, output_geometry,
};
use shell::compositor::frame::FrameBuilder;
use vello::kurbo::{Point, Rect};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

/// A frame builder laid out across `fixture`, painted once so the chrome's
/// hit rectangles (filled in while drawing) are current.
fn frame_across(fixture: &str) -> (FrameBuilder, Vec<Output>) {
    let outputs = output_fixture(fixture).unwrap_or_else(|| panic!("fixture {fixture}"));
    let mut fb = FrameBuilder::new(combined_geometry(&outputs));
    fb.set_output_geometry(output_geometry(&outputs));
    fb.relayout();
    let _ = fb.paint();
    fb.relayout();
    let _ = fb.paint();
    (fb, outputs)
}

/// The control labelled `label` on the display with output id `display`:
/// the primary carries the full top bar, every other display a compact one
/// repeating its workspace controls.
fn control_on(fb: &FrameBuilder, outputs: &[Output], label: &str, display: u32) -> Rect {
    let on = outputs
        .iter()
        .find(|o| o.id.0 == display)
        .map(Output::logical_rect)
        .expect("display");
    let update = fb.build_a11y_tree_update(None, 1.0);
    update
        .nodes
        .iter()
        .filter(|(_, node)| node.label() == Some(label))
        .filter_map(|(_, node)| node.bounds())
        .map(|b| Rect::new(b.x0, b.y0, b.x1, b.y1))
        .find(|r| on.contains(r.center()))
        .unwrap_or_else(|| panic!("no {label:?} on display {display}"))
}

fn output_under(outputs: &[Output], p: Point) -> &Output {
    outputs
        .iter()
        .find(|o| o.logical_rect().contains(p))
        .unwrap_or_else(|| panic!("{p:?} is on no output"))
}

#[allow(
    clippy::float_cmp,
    reason = "fixture scales and extents are exactly representable"
)]
#[test]
fn the_top_bar_is_drawn_at_each_displays_own_scale() {
    let (mut fb, outputs) = frame_across("docked-laptop");
    assert_eq!(fb.viewport(), {
        let (w, h) = logical_geometry(&outputs);
        (f64::from(w), f64::from(h))
    });
    assert_eq!(fb.scene_size(), {
        let (w, h) = combined_geometry(&outputs);
        (f64::from(w), f64::from(h))
    });

    // The panel's full bar and the monitor's compact bar both carry the
    // overview control: one on the 2x panel, one on the 1x monitor.
    let overview = control_on(&fb, &outputs, "Open overview", 0);
    let on_monitor = control_on(&fb, &outputs, "Open overview", 1);
    let panel = output_under(&outputs, overview.center());
    let monitor = output_under(&outputs, on_monitor.center());
    assert_eq!(panel.scale, 2.0, "overview should be on the laptop panel");
    assert_eq!(
        monitor.scale, 1.0,
        "the compact bar's control should be on the monitor"
    );

    // The same logical band drawn on each display: its physical height is
    // the display's scale times the logical one.
    let physical_height = |rect: Rect| {
        fb.layout_rect_to_scene(rect)
            .expect("the control is on a display")
            .height()
    };
    assert_eq!(physical_height(overview), overview.height() * 2.0);
    assert_eq!(physical_height(on_monitor), on_monitor.height());
    let band = |x: f64| Rect::new(x, 0.0, x + 1.0, overview.y1.max(on_monitor.y1));
    assert_eq!(
        physical_height(band(overview.center().x)) / physical_height(band(on_monitor.center().x)),
        2.0,
        "the top bar is twice as tall, physically, on the 2x panel"
    );

    // And the painted scene carries that magnification.
    let scene = fb.paint();
    assert!(
        scene
            .encoding()
            .transforms
            .iter()
            .any(|t| t.matrix[0] == 2.0 && t.matrix[3] == 2.0),
        "the panel's part of the desktop is painted magnified 2x"
    );
}

/// Click at a scene pixel the way a backend reports it: through the
/// compositor's scene → logical conversion into the shell.
fn click_scene(fb: &mut FrameBuilder, outputs: &[Output], at: Point) {
    let placements = placements(outputs);
    let moved = event_to_logical(
        &placements,
        BackendEvent::Input(WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(at.x, at.y),
        }),
    );
    let BackendEvent::Input(moved) = moved else {
        panic!("a cursor event stays a cursor event");
    };
    let mut doc = AutoCommit::new();
    let mut focus = None;
    let mods = Modifiers::default();
    fb.dispatch_event(&moved, &mods, &mut doc, &ROOT, &mut focus);
    for state in [ElementState::Pressed, ElementState::Released] {
        fb.dispatch_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            },
            &mods,
            &mut doc,
            &ROOT,
            &mut focus,
        );
    }
}

#[test]
fn a_click_lands_on_the_control_drawn_under_it_on_either_display() {
    let (mut fb, outputs) = frame_across("docked-laptop");

    // On the 2x panel.
    let overview = control_on(&fb, &outputs, "Open overview", 0);
    let drawn_at = fb.layout_point_to_scene(overview.center());
    assert!(
        drawn_at.x > overview.center().x,
        "on a 2x panel a control is drawn further from the origin than its logical position"
    );
    click_scene(&mut fb, &outputs, drawn_at);
    assert!(
        fb.chrome().overlays.overview,
        "clicking where overview is drawn on the panel opens it"
    );
    fb.with_chrome(|chrome| chrome.overlays.overview = false);
    fb.relayout();
    let _ = fb.paint();

    // On the 1x monitor's compact bar, whose scene rectangle starts after
    // the panel's 2560 physical pixels rather than at its logical 1280.
    let on_monitor = control_on(&fb, &outputs, "Open overview", 1);
    let drawn_at = fb.layout_point_to_scene(on_monitor.center());
    click_scene(&mut fb, &outputs, drawn_at);
    assert!(
        fb.chrome().overlays.overview,
        "clicking where overview is drawn on the monitor opens it"
    );
}

#[allow(
    clippy::float_cmp,
    reason = "fixture scales and extents are exactly representable"
)]
#[test]
fn an_offset_monitor_draws_its_part_at_its_own_scale_and_position() {
    let (fb, outputs) = frame_across("mixed-scale-offset");
    let geometry = fb.output_geometry();
    assert_eq!(geometry.len(), 2);
    let monitor = &outputs[1];
    let logical = monitor.logical_rect();
    // A point 10 logical pixels into the monitor, below its offset top edge,
    // is drawn 10 physical pixels into the monitor's scene rectangle.
    let inside = Point::new(logical.x0 + 10.0, logical.y0 + 10.0);
    let scene = monitor.scene_rect();
    assert_eq!(
        fb.layout_point_to_scene(inside),
        Point::new(scene.x0 + 10.0, scene.y0 + 10.0)
    );
    // The overview control on the 2x panel is drawn at twice its size.
    let overview = control_on(&fb, &outputs, "Open overview", 0);
    let drawn = fb.layout_rect_to_scene(overview).expect("on the panel");
    assert_eq!(drawn.height(), overview.height() * 2.0);
    assert_eq!(drawn.width(), overview.width() * 2.0);
}
