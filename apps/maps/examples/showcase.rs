//! Deterministic hosted Maps captures; no compositor, network or user data.
//!
//! ```sh
//! cargo run --locked -p maps --example showcase -- output.png transit
//! cargo run --locked -p maps --example showcase -- frames demo
//! ffmpeg -framerate 60 -i frames/%03d.png -c:v libx264 -crf 18 -pix_fmt yuv420p demo.mp4
//! ```
//! Link crates/sdk/ui-core/assets/fonts beside the capture binary.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "bounded capture coordinates and a fixed 210-frame timeline"
)]

use maps::workspace::workspace;
use maps::{MapsApp, MapsShell, TravelMode, Workspace, sample};
use ui::framework::testing::TestApp;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

type CaptureResult<T> = Result<T, Box<dyn std::error::Error>>;

fn capture(scenario: &str) -> CaptureResult<TestApp<MapsShell>> {
    let mut app = TestApp::<MapsShell>::capture_at(Some(scenario), (1440, 810))?;
    app.harness().set_theme(Theme::dark().with_ui_scale(1.5));
    Ok(app)
}

fn find(node: &ui::a11y::AccessNode, target: &str) -> Option<Rect> {
    if node.action_target.as_deref() == Some(target) {
        return node.bounds.map(ui::a11y::AccessBounds::to_rect);
    }
    node.children.iter().find_map(|child| find(child, target))
}

fn target(app: &mut TestApp<MapsShell>, id: &str) -> CaptureResult<Point> {
    let node = app
        .surface_mut::<MapsApp>()
        .accessibility()
        .ok_or("map accessibility")?;
    Ok(find(&node, id)
        .ok_or_else(|| format!("missing visible target: {id}"))?
        .center())
}

fn min_jerk(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
}

fn reach(from: Point, to: Point, t: f64, bend: f64) -> Point {
    let s = min_jerk(t);
    let delta = to - from;
    let length = delta.hypot().max(1.0);
    let control = Point::new(
        f64::midpoint(from.x, to.x) - delta.y / length * bend,
        f64::midpoint(from.y, to.y) + delta.x / length * bend,
    );
    Point::new(
        (1.0 - s).powi(2) * from.x + 2.0 * (1.0 - s) * s * control.x + s * s * to.x,
        (1.0 - s).powi(2) * from.y + 2.0 * (1.0 - s) * s * control.y + s * s * to.y,
    )
}

fn pointer(scene: &mut vello::Scene, tip: Point) {
    let mut arrow = BezPath::new();
    arrow.move_to((0.0, 0.0));
    for point in [
        (2.0, 23.0),
        (7.5, 17.5),
        (12.0, 27.0),
        (16.5, 24.5),
        (12.0, 15.0),
        (20.0, 14.5),
    ] {
        arrow.line_to(point);
    }
    arrow.close_path();
    let at = Affine::translate(tip.to_vec2());
    scene.stroke(&Stroke::new(4.0), at, Color::BLACK, None, &arrow);
    scene.fill(Fill::NonZero, at, Color::WHITE, None, &arrow);
}

/// Seed both stored plans through their ordinary result actions. Only this
/// offline capture supplies the planner response; the live app fetches it.
fn plans(app: &mut TestApp<MapsShell>) -> CaptureResult<()> {
    let model = sample::default_model();
    app.invoke(Workspace::ACCEPT_TRANSIT.with(workspace::AcceptTransit {
        transit: model.transit,
        origin: model.origin,
        origin_name: model.origin_name,
        dest: model.dest,
        dest_name: model.dest_name,
    }))?;
    app.invoke(ui::tilo_resource::Invoke::edit(
        &Workspace::paths().mode.set(TravelMode::Car),
    )?)?;
    app.harness().relayout();
    Ok(())
}

fn overview(app: &mut TestApp<MapsShell>) -> CaptureResult<()> {
    let zoom = target(app, "maps.control:zoom-out")?;
    app.harness().click_at(zoom.x, zoom.y);
    // Keep the Isar in the frame without moving the route under Directions.
    app.harness().drag_at((1100.0, 500.0), (1040.0, 500.0));
    Ok(())
}

fn movie(out: &std::path::Path) -> CaptureResult<()> {
    std::fs::create_dir_all(out)?;
    let mut app = capture("directions")?;
    plans(&mut app)?;
    overview(&mut app)?;
    let cycle = target(&mut app, "maps.mode:cycle")?;
    let transit = target(&mut app, "maps.mode:transit")?;
    let inspect = Point::new(290.0, 585.0);
    let pan = Point::new(1020.0, 450.0);
    let mut departure = inspect;
    let mut renderer = ui::offscreen::OffscreenRenderer::new(1440, 810);
    // Install the real host's backdrop before adding only the capture cursor.
    let _ = app.harness().render_rgba_with(&mut renderer, Color::BLACK);
    for frame in 0..210 {
        let p = match frame {
            0..=24 => reach(
                Point::new(1080.0, 610.0),
                cycle,
                f64::from(frame) / 24.0,
                -65.0,
            ),
            25..=62 => reach(cycle, transit, (f64::from(frame) - 25.0) / 37.0, 30.0),
            63..=92 => reach(transit, inspect, (f64::from(frame) - 63.0) / 29.0, -35.0),
            93..=125 => reach(inspect, departure, (f64::from(frame) - 93.0) / 32.0, 25.0),
            126..=157 => reach(departure, pan, (f64::from(frame) - 126.0) / 31.0, -55.0),
            _ => reach(
                pan,
                Point::new(850.0, 520.0),
                (f64::from(frame) - 158.0) / 38.0,
                25.0,
            ),
        };
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(p.x, p.y));
        if frame == 24 {
            app.harness().click_at(p.x, p.y);
            if app.state().mode != TravelMode::Bike {
                return Err("cycle click did not change mode".into());
            }
            overview(&mut app)?;
        }
        if frame == 62 {
            app.harness().click_at(p.x, p.y);
            if app.state().mode != TravelMode::Transit {
                return Err("transit click did not change mode".into());
            }
            overview(&mut app)?;
        }
        if frame == 92 {
            app.harness().scroll(0.0, -1.0);
            departure = target(&mut app, "maps.journey:1")?;
        }
        if frame == 125 {
            app.harness().click_at(p.x, p.y);
        }
        if frame == 158 {
            app.harness().send(&ui::synthetic_input::mouse_press());
        }
        if frame == 196 {
            app.harness().send(&ui::synthetic_input::mouse_release());
        }
        app.harness().relayout();
        let mut scene = app.harness().scene().clone();
        pointer(&mut scene, p);
        renderer.render_png(&scene, Color::BLACK, out.join(format!("{frame:03}.png")));
    }
    app.harness().close_components();
    Ok(())
}

fn main() -> CaptureResult<()> {
    ui::clock::freeze_for_capture();
    let mut args = std::env::args().skip(1);
    let output = std::path::PathBuf::from(args.next().ok_or("expected output path")?);
    let scenario = args.next().unwrap_or_else(|| "transit".into());
    if scenario == "demo" {
        return movie(&output);
    }
    let mut app = capture(&scenario)?;
    if args.next().as_deref() == Some("overview") {
        overview(&mut app)?;
    }
    app.harness().render_png(&output);
    app.harness().close_components();
    Ok(())
}
