//! Capture the real spreadsheet at 150%, with optional 210-frame video.
//!
//! `cargo run --locked -p sheets --example showcase -- OUTDIR [demo]`
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "bounded frame arithmetic in a fixed-size capture recipe"
)]
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use sheets::{SheetsApp, SheetsShell, Workbook, sample};
use ui::prelude::TestApp;
use ui::theme::Theme;
use ui::tilo_resource::{Id, Invoke};
use vello::kurbo::{Affine, BezPath, Point, Stroke};
use vello::peniko::Fill;

fn min_jerk(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
}

/// Smooth acceleration along a curved, rather than ruler-straight, path.
fn glide(a: Point, b: Point, t: f64) -> Point {
    let u = min_jerk(t);
    let bend = (std::f64::consts::PI * u).sin() * 40.0;
    Point::new(a.x + (b.x - a.x) * u, a.y + (b.y - a.y) * u - bend)
}

fn arrow(scene: &mut vello::Scene, at: Point, theme: &Theme) {
    let mut path = BezPath::new();
    path.move_to((0.0, 0.0));
    path.line_to((0.0, 29.0));
    path.quad_to((0.0, 31.0), (2.0, 29.0));
    path.line_to((9.0, 22.0));
    path.line_to((15.0, 35.0));
    path.quad_to((16.0, 37.0), (18.0, 35.0));
    path.line_to((21.0, 33.0));
    path.line_to((14.0, 20.0));
    path.line_to((25.0, 20.0));
    path.quad_to((27.0, 20.0), (25.0, 18.0));
    path.close_path();
    let transform = Affine::translate((at.x, at.y));
    scene.stroke(
        &Stroke::new(3.0),
        transform,
        theme.tokens.colors.surfaces.content,
        None,
        &path,
    );
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.text_primary,
        None,
        &path,
    );
}

fn demo(app: &mut TestApp<SheetsShell>, out: &Path) -> anyhow::Result<()> {
    let mut encoder = Command::new("ffmpeg")
        .args([
            "-y",
            "-v",
            "error",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgba",
            "-video_size",
            "1440x810",
            "-framerate",
            "60",
            "-i",
            "pipe:0",
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(out.join("sheets-demo.mp4"))
        .stdin(Stdio::piped())
        .spawn()?;
    let mut input = encoder
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("video pipe missing"))?;
    let mut renderer = ui::offscreen::OffscreenRenderer::new(1440, 810);
    // These are the cells in this bounded recipe after content autofit.
    let formula = Point::new(405.0, 114.0);
    let groceries = Point::new(350.0, 294.0);
    let total = Point::new(850.0, 251.0);
    for frame in 0u32..210 {
        let at = match frame {
            0..60 => glide(Point::new(650.0, 370.0), formula, f64::from(frame) / 59.0),
            60..90 => formula,
            90..140 => glide(formula, groceries, f64::from(frame - 90) / 49.0),
            140..170 => groceries,
            _ => glide(groceries, total, f64::from(frame - 170) / 39.0),
        };
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(at.x, at.y));
        match frame {
            60 | 209 => app.harness().click_at(at.x, at.y),
            85 | 155 => app.harness().key("Enter"),
            140 => {
                app.harness().click_at(at.x, at.y);
                app.harness().type_text("$680");
            }
            _ => {}
        }
        app.harness().relayout();
        let theme = app.harness().theme().clone();
        let mut scene = app.harness().scene().clone();
        arrow(&mut scene, at, &theme);
        // Each changed scene may grow Vello's atlas/bump buffers. Use the
        // capture renderer's confirmed submissions so transition frames do
        // not publish partially populated glyphs.
        let rgba = renderer.render_resilient(&scene, theme.tokens.colors.surfaces.content);
        input.write_all(&rgba)?;
    }
    drop(input);
    anyhow::ensure!(encoder.wait()?.success(), "video encoder failed");
    let state = app.state();
    let sheet = state
        .sheet(&Id::from_raw(sample::BUDGET_SHEET_ID))
        .ok_or_else(|| anyhow::anyhow!("budget missing"))?;
    anyhow::ensure!(
        sheet
            .cell_at(&sheet.layout(), (1, 2))
            .is_some_and(|cell| cell.raw.0 == "680"),
        "pointer gesture did not update groceries"
    );
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let out = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("output directory required"))?;
    let out = Path::new(&out);
    std::fs::create_dir_all(out)?;
    let out = out.canonicalize()?;
    let out = out.as_path();
    let mut app = TestApp::<SheetsShell>::new()?;
    app.harness().resize((1440, 810))?;
    for invoke in sample::workbook() {
        app.invoke(invoke)?;
    }
    // Emphasise authored headers/totals without adding data to the live app.
    let state = app.state();
    let id = Id::from_raw(sample::BUDGET_SHEET_ID);
    let sheet = state
        .sheet(&id)
        .ok_or_else(|| anyhow::anyhow!("budget missing"))?;
    let layout = sheet.layout();
    let cells: Vec<_> = (0..7)
        .flat_map(|col| [0, 6].map(|row| (col, row)))
        .filter_map(|addr| {
            let mut cell = sheet.cell_at(&layout, addr)?.clone();
            cell.bold = true;
            Some((addr, cell))
        })
        .collect();
    for edit in sheets::workbook::cell_edits(&state, &id, &cells) {
        app.invoke(Invoke::edit(&edit)?)?;
    }
    for (title, kind) in [
        ("Quarterly spend", sheets::workbook::ChartKind::Column),
        ("Monthly trend", sheets::workbook::ChartKind::Line),
    ] {
        app.invoke(Workbook::ADD_CHART.with(sheets::resource::verbs::AddChart {
            sheet: id.clone(),
            range: "B1:D6".into(),
            kind,
            title: title.into(),
        }))?;
    }
    app.harness().set_theme(Theme::dark().with_ui_scale(1.5));
    // Ada selects a real cell scope, resolved in the receiver's own grid.
    app.harness().click_at(1020.0, 294.0);
    let (key, _) = app
        .with_widget::<SheetsApp, _>(|widget| ui::presence::capability::presence_payload(widget))
        .flatten()
        .ok_or_else(|| anyhow::anyhow!("cell presence missing"))?;
    let presence = app.harness().presence_mut();
    let ada = ui::permissions::UserId("ada".into());
    presence.register_presence(&key, ada.clone());
    presence.set_display_name(ada, "Ada");
    app.harness().click_at(850.0, 251.0);
    app.harness().relayout();
    app.harness().render_png(&out.join("sheets-showcase.png"));
    if args.next().is_some() {
        demo(&mut app, out)?;
    }
    app.harness().set_theme(Theme::light().with_ui_scale(1.5));
    app.harness().render_png(&out.join("sheets-light.png"));
    Ok(())
}
