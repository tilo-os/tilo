//! Capture the real editor at 150%, using a caller-supplied MP4.
//!
//! Run with `cargo run -p video-editor --example showcase -- VIDEO OUTDIR [demo]`.
//! The optional demo records 210 frames at 60 fps through real pointer/key input.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "fixed-size capture fixture uses bounded frame arithmetic and three known tracks"
)]
use std::path::{Path, PathBuf};

use ui::prelude::TestApp;
use ui::theme::Theme;
use ui::tilo_resource::Invoke;
use video_editor::model::{MediaItem, MediaKind, Project, TextSource, Track, TrackKind};
use video_editor::{VideoEditor, resource::load_project};

fn project(source: &Path) -> anyhow::Result<Project> {
    let mut p = Project::new("A walk outside");
    p.set_frame_size(1920, 1080);
    p.set_fps(30.0);
    let footage = MediaItem {
        id: uuid::Uuid::new_v4(),
        name: "Park · original.mp4".into(),
        kind: MediaKind::Video,
        path: source.canonicalize()?.to_string_lossy().into_owned(),
        duration: 5.0,
        width: 1920,
        height: 1080,
        fps: 30.0,
        has_audio: true,
        ..Default::default()
    };
    let sound = MediaItem {
        id: uuid::Uuid::new_v4(),
        name: "Location sound".into(),
        kind: MediaKind::Audio,
        palette: 5,
        ..footage.clone()
    };
    let title = MediaItem {
        id: uuid::Uuid::new_v4(),
        name: "A WALK OUTSIDE".into(),
        kind: MediaKind::Text,
        duration: 5.0,
        palette: 3,
        text: Some(TextSource::new("A WALK OUTSIDE")),
        ..Default::default()
    };
    let tracks = [
        Track {
            id: uuid::Uuid::new_v4(),
            name: "Titles".into(),
            kind: TrackKind::Video,
            index: 2,
            ..Default::default()
        },
        Track {
            id: uuid::Uuid::new_v4(),
            name: "Video".into(),
            kind: TrackKind::Video,
            index: 1,
            ..Default::default()
        },
        Track {
            id: uuid::Uuid::new_v4(),
            name: "Audio".into(),
            kind: TrackKind::Audio,
            index: 1,
            ..Default::default()
        },
    ];
    p.media = vec![footage.clone(), sound.clone(), title.clone()].into();
    p.tracks = tracks.to_vec().into();
    p.add_clip(tracks[1].id, footage.id, 0.0);
    p.add_clip(tracks[2].id, sound.id, 0.0);
    let id = p
        .add_clip(tracks[0].id, title.id, 0.0)
        .ok_or_else(|| anyhow::anyhow!("title source missing"))?;
    if let Some(c) = p.clip_mut(id) {
        c.transform.scale_x = 0.65;
        c.transform.scale_y = 0.65;
        c.transform.pos_y = 0.32;
    }
    Ok(p)
}

#[cfg(feature = "gst")]
fn demo(app: &mut TestApp<VideoEditor>, out: &Path) -> anyhow::Result<()> {
    use gst::prelude::*;
    use gstreamer as gst;
    gst::init()?;
    let pipeline = gst::parse::launch("appsrc name=frames format=time ! videoconvert ! openh264enc bitrate=6000000 ! h264parse ! mp4mux ! filesink name=output")?
        .downcast::<gst::Pipeline>().map_err(|_| anyhow::anyhow!("capture pipeline"))?;
    let sink = pipeline
        .by_name("output")
        .ok_or_else(|| anyhow::anyhow!("output missing"))?;
    sink.set_property(
        "location",
        out.join("video-editor-demo.mp4").to_string_lossy().as_ref(),
    );
    let src = pipeline
        .by_name("frames")
        .ok_or_else(|| anyhow::anyhow!("frames missing"))?
        .downcast::<gstreamer_app::AppSrc>()
        .map_err(|_| anyhow::anyhow!("appsrc"))?;
    src.set_caps(Some(
        &gst::Caps::builder("video/x-raw")
            .field("format", "RGBA")
            .field("width", 1440i32)
            .field("height", 810i32)
            .field("framerate", gst::Fraction::new(60, 1))
            .build(),
    ));
    pipeline.set_state(gst::State::Playing)?;
    let mut renderer = ui::offscreen::OffscreenRenderer::new(1440, 810);
    // Input coordinates belong to this fixed 1440×810 capture recipe.
    // The document assertion below fails if a layout change misses the cut.
    let ruler_y = 484.0;
    // Fit is a real editor command, not a capture-only timeline transform.
    app.harness().key("z");
    for frame in 0u64..210 {
        if frame < 90 {
            let t = frame as f64 / 89.0;
            app.harness().send(&ui::synthetic_input::cursor_moved(
                240.0 + t * 430.0,
                ruler_y,
            ));
            if frame == 0 {
                app.harness().send(&ui::synthetic_input::mouse_press());
            }
            if frame == 89 {
                app.harness().send(&ui::synthetic_input::mouse_release());
            }
        } else if frame == 90 {
            app.harness().click_at(480.0, ruler_y + 140.0);
            app.harness().key("s");
            anyhow::ensure!(
                app.state().clips.len() == 4,
                "the pointer/key gesture did not split the video clip"
            );
        } else if frame > 125 && frame % 2 == 0 {
            // The shared frame-forward transport control previews at 30 fps.
            // Arrow keys would instead nudge the selected clip on its track.
            app.harness().click_at(627.0, 405.0);
        }
        app.harness().relayout();
        let bg = app.harness().theme().tokens.colors.surfaces.content;
        let rgba = app.harness().render_rgba_with(&mut renderer, bg);
        if [0, 89, 100, 209].contains(&frame) {
            ui::png::write_rgba8(out.join(format!("demo-{frame:03}.png")), 1440, 810, &rgba)?;
        }
        let mut buffer = gst::Buffer::from_mut_slice(rgba);
        let b = buffer
            .get_mut()
            .ok_or_else(|| anyhow::anyhow!("capture buffer shared"))?;
        b.set_pts(gst::ClockTime::from_nseconds(frame * 1_000_000_000 / 60));
        b.set_duration(gst::ClockTime::from_nseconds(1_000_000_000 / 60));
        src.push_buffer(buffer)?;
    }
    src.end_of_stream()?;
    let bus = pipeline
        .bus()
        .ok_or_else(|| anyhow::anyhow!("capture bus missing"))?;
    let result = loop {
        let msg = bus
            .timed_pop(gst::ClockTime::from_seconds(60))
            .ok_or_else(|| anyhow::anyhow!("capture timed out"))?;
        match msg.view() {
            gst::MessageView::Eos(_) => break Ok(()),
            gst::MessageView::Error(e) => break Err(anyhow::anyhow!("{}", e.error())),
            _ => {}
        }
    };
    pipeline.set_state(gst::State::Null)?;
    result
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let source = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("MP4 path required"))?,
    );
    let out = PathBuf::from(
        args.next()
            .ok_or_else(|| anyhow::anyhow!("output directory required"))?,
    );
    std::fs::create_dir_all(&out)?;
    let source = source.canonicalize()?;
    let mut app = TestApp::<VideoEditor>::new()?;
    app.harness().resize((1440, 810))?;
    app.invoke(Invoke::edit(&load_project(&project(&source)?))?)?;
    app.harness().set_theme(Theme::dark().with_ui_scale(1.5));
    app.harness().key("Home");
    app.harness().key("z");
    let deadline = ui::time::now() + ui::time::Duration::from_secs(30);
    while app.with_widget::<video_editor::VideoEditorApp, _>(|e| e.has_preview_frame())
        != Some(true)
        && ui::time::now() < deadline
    {
        app.harness().relayout();
    }
    app.harness().click_at(500.0, 650.0);
    app.harness()
        .render_png(&out.join("video-editor-showcase.png"));
    anyhow::ensure!(
        app.with_widget::<video_editor::VideoEditorApp, _>(|e| e.has_preview_frame()) == Some(true),
        "capture has no decoded video frame"
    );
    #[cfg(feature = "gst")]
    if args.next().is_some() {
        demo(&mut app, &out)?;
    }
    app.harness().set_theme(Theme::light().with_ui_scale(1.5));
    app.harness()
        .render_png(&out.join("video-editor-light.png"));
    app.harness().set_theme(Theme::dark().with_ui_scale(1.5));
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("e");
    app.harness().set_modifiers(&[]);
    app.harness()
        .render_png(&out.join("video-editor-export.png"));
    Ok(())
}
