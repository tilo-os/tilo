//! Reproducible Writer visual audit, using the production component and actions.
//! `cargo run --locked -p word-processor --example showcase -- --frames` writes
//! 1440×810 dark/light PNGs and 210 video frames under `target/writer-capture`.
//! Encode at 60fps with:
//! `ffmpeg -framerate 60 -i target/writer-capture/frames/%03d.png -c:v libx264
//! -crf 18 -pix_fmt yuv420p -movflags +faststart target/writer-capture/demo.mp4`.

use std::path::Path;
use ui::framework::{ResourceApp, ResourceSelection, testing::HeadlessApp};
use ui::permissions::UserId;
use ui::tilo_resource::Invoke;
use word_processor::{WpFormat, effects::WordProcessorEffect, framework::WordProcessorApp};

const DOCUMENT: &str = include_str!("showcase.md");

fn presence(app: &mut HeadlessApp<WordProcessorApp>, block: &str, offset: usize) {
    let key = ResourceSelection::Item(block.to_owned())
        .presence::<tilo_document::RichText>(WordProcessorApp::ID)
        .0;
    app.harness().update_presence(|p| {
        for (id, name, at, start) in [("ada", "Ada", offset, 15), ("alice", "Alice", 10, 0)] {
            let user = UserId(id.into());
            p.set_display_name(user.clone(), name);
            p.carets
                .entry(key.clone())
                .or_default()
                .retain(|(u, _)| u != &user);
            p.carets
                .entry(key.clone())
                .or_default()
                .push((user.clone(), at));
            p.selections
                .entry(key.clone())
                .or_default()
                .retain(|(u, _)| u != &user);
            p.selections
                .entry(key.clone())
                .or_default()
                .push((user, ui::presence::SelectionRange { start, end: at }));
        }
        true
    });
    app.harness().relayout();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut app = HeadlessApp::<WordProcessorApp>::for_person("You")?;
    app.harness().resize((1440, 810))?;
    app.harness()
        .set_theme(ui::Theme::dark().with_ui_scale(1.5));
    let edit =
        word_processor::import_action(DOCUMENT.as_bytes(), WpFormat::Markdown, &app.content())?;
    app.invoke(Invoke::edit(&word_processor::model::edit(&edit))?)?;
    let blocks = app.read_document(|doc| {
        word_processor::model::open(doc).map(|c| tilo_doc::model::materialize(doc, &c).blocks)
    })?;
    let intro = blocks
        .iter()
        .find(|b| b.text.starts_with("Good ideas"))
        .ok_or("missing introduction")?;
    app.effect(&WordProcessorEffect::CommentSelection {
        text: "A little breathing room goes a long way. Keep this thought.".into(),
        at: Some(ui::tilo_resource::Place::Range {
            item: intro.id.clone(),
            start: 0,
            end: 10,
        }),
    })?;
    let quote = blocks
        .iter()
        .find(|b| b.text.starts_with("Keep it simple"))
        .ok_or("missing quotation")?;
    app.effect(&WordProcessorEffect::SelectBlock {
        block: quote.id.clone(),
    })?;
    app.effect(&WordProcessorEffect::ApplyNamedStyle {
        style: "quote".into(),
        at: None,
    })?;
    app.effect(&WordProcessorEffect::MoveCaret {
        to: Some("document-start".into()),
        block: None,
        offset: None,
    })?;
    app.effect(&WordProcessorEffect::Ruler)?;
    app.effect(&WordProcessorEffect::Outline)?;
    if !app
        .harness()
        .settle_for_capture(ui::time::Duration::from_secs(2))
    {
        return Err("the writing surface did not settle".into());
    }
    presence(&mut app, &intro.id, 32);
    std::fs::create_dir_all("target/writer-capture")?;
    app.harness()
        .render_png(Path::new("target/writer-capture/showcase.png"));
    app.harness()
        .set_theme(ui::Theme::light().with_ui_scale(1.5));
    app.harness()
        .render_png(Path::new("target/writer-capture/light.png"));
    app.harness()
        .set_theme(ui::Theme::dark().with_ui_scale(1.5));
    if std::env::args().any(|arg| arg == "--frames") {
        record(&mut app, &intro.id, &blocks)?;
    }
    Ok(())
}

fn min_jerk(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
}

fn cursor(t: f64) -> (f64, f64) {
    let s = min_jerk(t);
    let a = 1.0 - s;
    // A cubic path with zero velocity and acceleration at both ends.
    (
        a.powi(3) * 1030.0 + 3.0 * a * a * s * 1010.0 + 3.0 * a * s * s * 720.0 + s.powi(3) * 895.0,
        a.powi(3) * 122.0 + 3.0 * a * a * s * 290.0 + 3.0 * a * s * s * 400.0 + s.powi(3) * 568.0,
    )
}

fn paint_cursor(scene: &mut vello::Scene, point: (f64, f64)) {
    use vello::{
        kurbo::{Affine, BezPath, Stroke},
        peniko::{Color, Fill},
    };
    let mut arrow = BezPath::new();
    arrow.move_to((0.0, 0.0));
    arrow.line_to((0.0, 24.0));
    arrow.line_to((6.0, 18.0));
    arrow.line_to((11.0, 28.0));
    arrow.line_to((15.0, 26.0));
    arrow.line_to((10.0, 16.0));
    arrow.line_to((19.0, 16.0));
    arrow.close_path();
    let transform = Affine::translate(point);
    scene.stroke(
        &Stroke::new(3.0).with_join(vello::kurbo::Join::Round),
        transform,
        Color::BLACK,
        None,
        &arrow,
    );
    scene.fill(Fill::NonZero, transform, Color::WHITE, None, &arrow);
}

fn record(
    app: &mut HeadlessApp<WordProcessorApp>,
    intro: &str,
    blocks: &[tilo_doc::snapshot::BlockView],
) -> Result<(), Box<dyn std::error::Error>> {
    let cell = blocks
        .iter()
        .find(|b| b.text == "Room for the work")
        .ok_or("missing table cell")?;
    std::fs::create_dir_all("target/writer-capture/frames")?;
    let mut renderer = ui::offscreen::OffscreenRenderer::new(1440, 810);
    let text: Vec<char> = "Room to think".chars().collect();
    for frame in 0_u32..210 {
        let point = cursor(f64::from(frame) / 65.0);
        app.harness()
            .send_host_event(&ui::synthetic_input::cursor_moved(point.0, point.1));
        if frame == 70 {
            app.harness().click_at(point.0, point.1);
            app.effect(&WordProcessorEffect::SelectRange {
                start_block: cell.id.clone(),
                start: 0,
                end_block: cell.id.clone(),
                end: cell.text.chars().count(),
            })?;
        }
        if let Some(typed) = frame
            .checked_sub(85)
            .filter(|n| *n < 39 && n.is_multiple_of(3))
            && let Some(ch) = text.get(usize::try_from(typed / 3)?)
        {
            app.harness().type_text(&ch.to_string());
        }
        presence(app, intro, usize::try_from(frame / 15)?.saturating_add(25));
        let mut scene = vello::Scene::new();
        scene.append(app.harness().scene(), None);
        paint_cursor(&mut scene, point);
        renderer.render_png(
            &scene,
            ui::Theme::dark().tokens.colors.surfaces.content,
            format!("target/writer-capture/frames/{frame:03}.png"),
        );
    }
    let edited = app.read_document(|doc| {
        word_processor::model::open(doc).map(|c| tilo_doc::model::block_text(doc, &c, &cell.id))
    })?;
    if edited.as_deref() != Some("Room to think") {
        return Err("the recording did not edit the real table cell".into());
    }
    Ok(())
}
