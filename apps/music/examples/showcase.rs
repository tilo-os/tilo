//! Native-resolution visual review and a real pointer-driven playback demo.
//! Run on a headless audio/GPU host; pass an output directory and `video`.
use music::{MusicApp, MusicShell};
use ui::framework::testing::TestApp;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn fonts() -> Result<()> {
    let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/sdk/ui-core/assets/fonts");
    let exe = std::env::current_exe()?;
    let parent = exe.parent().ok_or("executable directory")?;
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
fn capture(scenario: &str, theme: Theme, size: (u32, u32)) -> Result<TestApp<MusicShell>> {
    let mut app = TestApp::<MusicShell>::capture_at(Some(scenario), size)?;
    app.harness().set_theme(theme.with_ui_scale(1.5));
    app.harness().relayout();
    app.harness()
        .wait_for_visuals(std::time::Duration::from_secs(10))?;
    Ok(app)
}
fn named(node: &ui::a11y::AccessNode, name: &str) -> Option<Rect> {
    if node.name.as_deref() == Some(name)
        && let Some(bounds) = node.bounds
    {
        return Some(bounds.to_rect());
    }
    node.children.iter().find_map(|node| named(node, name))
}
fn control(widget: &dyn Widget, name: &str) -> Option<Rect> {
    if widget.test_id() == Some(name) && widget.get_rect().area() > 0.0 {
        return Some(widget.get_rect());
    }
    widget
        .accessibility()
        .and_then(|node| named(&node, name))
        .or_else(|| {
            widget
                .children()
                .into_iter()
                .find_map(|child| control(child, name))
        })
}
fn target(app: &mut TestApp<MusicShell>, name: &str) -> Result<Point> {
    control(app.surface_mut::<MusicApp>(), name)
        .map(|rect| rect.center())
        .ok_or_else(|| format!("missing visible control: {name}").into())
}
fn min_jerk(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
}
fn reach(from: Point, to: Point, t: f64, bend: f64) -> Point {
    let s = min_jerk(t);
    let delta = vello::kurbo::Vec2::new(to.x - from.x, to.y - from.y);
    let length = delta.hypot().max(1.0);
    let c = Point::new(
        from.x.midpoint(to.x) - delta.y / length * bend,
        from.y.midpoint(to.y) + delta.x / length * bend,
    );
    Point::new(
        (1.0 - s).powi(2) * from.x + 2.0 * (1.0 - s) * s * c.x + s * s * to.x,
        (1.0 - s).powi(2) * from.y + 2.0 * (1.0 - s) * s * c.y + s * s * to.y,
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
// Original quiet tones, with the catalogue's full duration. They are installed
// only in the caller-owned scratch cache; no network or pretend playback.
#[allow(
    clippy::cast_possible_truncation,
    reason = "PCM quantization intentionally rounds a bounded +/-1200 waveform to signed 16-bit samples"
)]
fn wave(seconds: u32, frequency: f64) -> Result<Vec<u8>> {
    const RATE: u32 = 8000;
    let samples = seconds
        .checked_mul(RATE)
        .ok_or("WAV sample count overflow")?;
    let bytes = samples.checked_mul(2).ok_or("WAV byte count overflow")?;
    let riff = bytes.checked_add(36).ok_or("WAV RIFF size overflow")?;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(b"\x02\0\x10\0data");
    wav.extend_from_slice(&bytes.to_le_bytes());
    for i in 0..samples {
        let t = f64::from(i) / f64::from(RATE);
        let envelope = (t / 0.2).min(1.0) * ((f64::from(seconds) - t) / 0.3).min(1.0);
        let sample = (envelope * 1200.0 * (t * frequency * std::f64::consts::TAU).sin()) as i16;
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    Ok(wav)
}
fn find<'a>(node: &'a ui::a11y::AccessNode, name: &str) -> Option<&'a ui::a11y::AccessNode> {
    if node.name.as_deref() == Some(name) {
        return Some(node);
    }
    node.children.iter().find_map(|node| find(node, name))
}

fn movie(out: &std::path::Path) -> Result<()> {
    let mut app = capture("sidebar", Theme::dark(), (1440, 810))?;
    let cache = tempfile::tempdir_in(out)?;
    let tracks: Vec<_> = [("tk_a1", 196.0), ("tk_h1", 262.0)]
        .into_iter()
        .map(|(id, frequency)| {
            let seconds = app.state().track(id).ok_or("sample track")?.duration_s;
            Ok((id.to_owned(), wave(seconds, frequency)?))
        })
        .collect::<Result<_>>()?;
    app.surface_mut::<MusicApp>()
        .snap_cached_audio(cache.path().to_path_buf(), tracks)?;
    let playlist = target(&mut app, "music.card.pl_drive")?;
    let mut play = Point::ZERO;
    let mut song = Point::ZERO;
    let mut queue = Point::ZERO;
    let frames = out.join("frames");
    std::fs::create_dir_all(&frames)?;
    let mut renderer = ui::offscreen::OffscreenRenderer::new(1440, 810);
    for i in 0..210 {
        let p = match i {
            0..=40 => reach(
                Point::new(1130.0, 620.0),
                playlist,
                f64::from(i) / 40.0,
                -55.0,
            ),
            41..=82 => reach(playlist, play, (f64::from(i) - 41.0) / 41.0, -50.0),
            83..=132 => reach(play, song, (f64::from(i) - 83.0) / 49.0, 60.0),
            133..=180 => reach(song, queue, (f64::from(i) - 133.0) / 47.0, -65.0),
            _ => reach(
                queue,
                Point::new(1080.0, 580.0),
                (f64::from(i) - 181.0) / 28.0,
                30.0,
            ),
        };
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(p.x, p.y));
        if [40, 82, 132, 180].contains(&i) {
            app.harness().click_at(p.x, p.y);
        }
        app.harness().relayout();
        if i == 40 {
            play = target(&mut app, "Play")?;
            song = target(&mut app, "Harbor Lights, Neon Harbor")?;
        }
        if i == 105 {
            app.harness().render_png(&out.join("music-showcase.png"));
        }
        if i == 132 {
            queue = target(&mut app, "Open queue")?;
        }
        let mut scene = app.harness().scene().clone();
        pointer(&mut scene, p);
        renderer.render_png(
            &scene,
            Theme::dark().tokens.colors.surfaces.content,
            frames.join(format!("{i:03}.png")),
        );
    }
    // Capture mode intentionally does not publish device effects. Its normal
    // accessibility status still reports decoder confirmation, not merely the
    // requested transport state; require that and the selected queue contents.
    let tree = app
        .surface_mut::<MusicApp>()
        .accessibility()
        .ok_or("Music semantics")?;
    let player = find(&tree, "Music player").ok_or("player status")?;
    let play = find(&tree, "Play or pause").ok_or("playing state")?;
    let queue = find(&tree, "Queue items").ok_or("queue view did not open")?;
    if !player
        .value
        .as_deref()
        .is_some_and(|value| value.starts_with("Harbor Lights;"))
        || player.description.as_deref() != Some("Playback controls")
        || play.state.checked != Some(true)
        || !queue
            .children
            .iter()
            .any(|row| row.name.as_deref() == Some("Last Exit"))
    {
        return Err(
            format!("pointer playback did not reach the decoder or queue: {player:?}").into(),
        );
    }
    let status = serde_json::json!({
        "viewport": [1440, 810], "ui_scale": 1.5, "frames": 210, "fps": 60,
        "player": player.value, "decoder_status": player.description,
        "playing": play.state.checked,
        "queue": queue.children.iter().map(|row| &row.name).collect::<Vec<_>>(),
        "pointer": "min_jerk quadratic arcs; native pointer presses and releases",
    });
    std::fs::write(
        out.join("verification.json"),
        serde_json::to_vec_pretty(&status)?,
    )?;
    Ok(())
}
fn main() -> Result<()> {
    fonts()?;
    let out = std::path::PathBuf::from(std::env::args().nth(1).ok_or("output directory")?);
    std::fs::create_dir_all(&out)?;
    for scenario in [
        "sidebar",
        "nowplaying",
        "queue",
        "lyrics",
        "album-page",
        "membership-open",
        "offline-cached",
    ] {
        let mut app = capture(scenario, Theme::dark(), (1440, 810))?;
        app.harness()
            .render_png(&out.join(format!("{scenario}.png")));
    }
    let mut light = capture("nowplaying", Theme::light(), (1440, 810))?;
    light.harness().render_png(&out.join("light.png"));
    let mut narrow = capture("nowplaying", Theme::dark(), (720, 900))?;
    narrow.harness().render_png(&out.join("narrow.png"));
    if std::env::args().nth(2).as_deref() == Some("video") {
        movie(&out)?;
    }
    Ok(())
}
