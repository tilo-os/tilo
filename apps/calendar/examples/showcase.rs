//! Native-resolution Calendar captures and real event/task pointer interactions.
use calendar::{CalendarApp, CalendarShell};
use ui::framework::testing::TestApp;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

fn fonts() -> Result<(), Box<dyn std::error::Error>> {
    let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/sdk/ui-core/assets/fonts");
    let executable = std::env::current_exe()?;
    let parent = executable.parent().ok_or("binary directory")?;
    for dir in [
        parent.to_path_buf(),
        parent.join("../deps"),
        parent.join(".."),
    ] {
        std::fs::create_dir_all(&dir)?;
        let link = dir.join("fonts");
        if !link.exists() {
            std::os::unix::fs::symlink(&fonts, link)?;
        }
    }
    Ok(())
}
fn capture(
    scenario: &str,
    theme: Theme,
) -> Result<TestApp<CalendarShell>, Box<dyn std::error::Error>> {
    let mut app = TestApp::<CalendarShell>::capture_at(Some(scenario), (1440, 810))?;
    app.harness().set_theme(theme.with_ui_scale(1.5));
    app.harness().relayout();
    app.harness()
        .wait_for_visuals(std::time::Duration::from_secs(10))?;
    Ok(app)
}
fn find(node: &ui::a11y::AccessNode, name: &str) -> Option<Rect> {
    if node.name.as_deref() == Some(name) {
        return node.bounds.map(ui::a11y::AccessBounds::to_rect);
    }
    node.children.iter().find_map(|child| find(child, name))
}
fn target(
    app: &mut TestApp<CalendarShell>,
    name: &str,
) -> Result<Point, Box<dyn std::error::Error>> {
    let tree = app
        .surface_mut::<CalendarApp>()
        .accessibility()
        .ok_or("semantics")?;
    Ok(find(&tree, name)
        .ok_or_else(|| format!("missing control: {name}"))?
        .center())
}
fn min_jerk(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
}
fn reach(from: Point, to: Point, t: f64, bend: f64) -> Point {
    let s = min_jerk(t);
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length = dx.hypot(dy).max(1.0);
    let control = Point::new(
        from.x.midpoint(to.x) - dy / length * bend,
        from.y.midpoint(to.y) + dx / length * bend,
    );
    Point::new(
        (1.0 - s).powi(2) * from.x + 2.0 * (1.0 - s) * s * control.x + s * s * to.x,
        (1.0 - s).powi(2) * from.y + 2.0 * (1.0 - s) * s * control.y + s * s * to.y,
    )
}
fn pointer(scene: &mut vello::Scene, p: Point) {
    let mut path = BezPath::new();
    path.move_to((0.0, 0.0));
    for point in [
        (2.0, 23.0),
        (7.5, 17.5),
        (12.0, 27.0),
        (16.5, 24.5),
        (12.0, 15.0),
        (20.0, 14.5),
    ] {
        path.line_to(point);
    }
    path.close_path();
    let at = Affine::translate((p.x, p.y));
    scene.stroke(
        &Stroke::new(4.0),
        at,
        Color::from_rgb8(12, 16, 24),
        None,
        &path,
    );
    scene.fill(Fill::NonZero, at, Color::WHITE, None, &path);
}
fn movie(out: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let theme = Theme::dark().with_ui_scale(1.5);
    let mut app = capture("agenda", Theme::dark())?;
    let event = target(&mut app, "Open event Design review")?;
    app.harness().click_at(event.x, event.y);
    app.harness().relayout();
    if app.surface_mut::<CalendarApp>().selected_event().is_none() {
        return Err("event click did not open the inspector".into());
    }
    app.harness().render_png(&out.join("showcase.png"));
    let event = target(&mut app, "Open event Design review")?;
    let notes = target(&mut app, "Attached notes")?;
    let check = target(&mut app, "Mark task Prepare quarterly report complete")?;
    let task_id = app
        .state()
        .tasks
        .iter()
        .find(|task| task.title == "Prepare quarterly report")
        .ok_or("sample task")?
        .id;
    let frames = out.join("frames");
    std::fs::create_dir_all(&frames)?;
    let mut renderer = ui::offscreen::OffscreenRenderer::new(1440, 810);
    let quiet = Point::new(1250.0, 720.0);
    for i in 0..210 {
        let p = match i {
            0..=30 => reach(Point::new(1120.0, 460.0), event, f64::from(i) / 30.0, -65.0),
            31..=70 => reach(event, notes, (f64::from(i) - 31.0) / 39.0, 35.0),
            71..=100 => notes,
            101..=155 => reach(notes, check, (f64::from(i) - 101.0) / 54.0, -65.0),
            _ => reach(check, quiet, (f64::from(i) - 156.0) / 44.0, -35.0),
        };
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(p.x, p.y));
        if i == 155 {
            app.harness().click_at(check.x, check.y);
        }
        app.harness().relayout();
        let mut scene = app.harness().scene().clone();
        pointer(&mut scene, p);
        renderer.render_png(
            &scene,
            theme.tokens.colors.surfaces.content,
            frames.join(format!("{i:03}.png")),
        );
    }
    if !app
        .state()
        .tasks
        .iter()
        .any(|task| task.id == task_id && task.done)
    {
        return Err("checkbox did not durably complete the task".into());
    }
    if app.surface_mut::<CalendarApp>().selected_event().is_none() {
        return Err("task completion closed the event inspector".into());
    }
    std::fs::write(
        out.join("gesture.txt"),
        "210 native frames at 60fps: Design review and attached rich notes remain open while Prepare quarterly report is durably completed. Pointer uses min_jerk quadratic arcs.\n",
    )?;
    // Encode these native frames with the ffmpeg command in showcase.md.
    // Keeping encoding separate also supports captures on remote builders.
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    fonts()?;
    let out = std::path::PathBuf::from(std::env::args().nth(1).ok_or("absolute output directory")?);
    if !out.is_absolute() {
        return Err("output directory must be absolute".into());
    }
    std::fs::create_dir_all(&out)?;
    for scenario in [
        "week",
        "month",
        "agenda",
        "tasks",
        "event_editor",
        "task_detail",
    ] {
        let mut app = capture(scenario, Theme::dark())?;
        app.harness()
            .render_png(&out.join(format!("{scenario}.png")));
    }
    let mut app = capture("week", Theme::light())?;
    app.harness().render_png(&out.join("light.png"));
    let mut app = TestApp::<CalendarShell>::capture_at(Some("event_editor"), (720, 900))?;
    app.harness().set_theme(Theme::dark().with_ui_scale(1.5));
    app.harness().render_png(&out.join("narrow.png"));
    if std::env::args().nth(2).as_deref() == Some("video") {
        movie(&out)?;
    }
    Ok(())
}
