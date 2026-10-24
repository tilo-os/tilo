//! Four execution modes for the `shots` binary:
//!   - `offscreen` — wgpu adapter, no window; captures PNGs.
//!   - `headless` — no GPU; dumps state JSON.
//!   - `recording` — wgpu adapter, no window; encodes a scenario to a raw
//!     H.264 NALU stream (Annex-B), suitable for ffplay / mpv / vlc.
//!   - `window` — real winit window; pre-rolls a scenario then yields
//!     control to the live event loop.

use std::path::Path;

use vello::peniko::Color;

use ui::offscreen::OffscreenRenderer;

use super::driver::ShellDriver;
use super::scenario::{self, JsonDirSink, NullSink, Scenario, SnapshotSink};

/// Round a logical-pixel viewport dimension (from [`ShellDriver::viewport`]) to the
/// nearest `u32`. Viewport sizes are always small and non-negative in practice — the
/// clamp documents that rather than letting the cast wrap on a hypothetical
/// out-of-range value.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=f64::from(u32::MAX) immediately above, so it always fits a non-negative u32"
)]
fn dim_to_u32(v: f64) -> u32 {
    v.round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// Frames to encode per scenario step in recording mode: `fps * step_duration_secs`,
/// rounded and floored at 1 so a step never contributes zero frames to the video.
fn frames_per_step_for(fps: u32, step_duration_secs: f32) -> u32 {
    #[allow(
        clippy::cast_precision_loss,
        reason = "fps is a frame rate (tens to low hundreds); f32 precision loss is not reachable"
    )]
    let frames = (fps as f32) * step_duration_secs;
    let frames = frames.round().max(1.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded and clamped to >= 1.0 immediately above, so it always fits a non-negative u32"
    )]
    let frames = frames as u32;
    frames
}

// =====================================================================
// Offscreen mode
// =====================================================================

pub mod offscreen {
    use super::{
        Color, OffscreenRenderer, Path, Scenario, ShellDriver, SnapshotSink, dim_to_u32, scenario,
    };

    /// `SnapshotSink` that renders into a PNG file per snapshot under
    /// `dir`, using the supplied `OffscreenRenderer`.
    pub struct PngDirSink<'a> {
        pub dir: &'a Path,
        pub renderer: &'a mut OffscreenRenderer,
        /// Set once the first `capture` completes. Only the FIRST paint
        /// matters for cold-start measurement — subsequent paints hit
        /// warm caches unconditionally.
        pub first_paint_seen: bool,
    }

    impl SnapshotSink for PngDirSink<'_> {
        fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
            let scene = driver.paint();
            let path = self.dir.join(format!("{name}.png"));
            self.renderer.set_backdrop(driver.take_backdrop());
            self.renderer.render_png(&scene, Color::TRANSPARENT, &path);
            if !self.first_paint_seen {
                // Match the mark name the ui `App` emits at the same
                // moment. Downstream analysis tools can look up
                // `first-paint` regardless of whether the process was
                // the live compositor or the shots harness. Mark AFTER
                // `render_png` returns so the metric captures the
                // vello pipeline compile too (they only fire on the
                // first render_to_texture call).
                tilo_startup_metrics::mark("first-paint");
                self.first_paint_seen = true;
            }
            println!("  wrote {}", path.display());
        }

        fn film(
            &mut self,
            driver: &mut ShellDriver,
            prefix: &str,
            frames: Vec<scenario::film::FilmFrame>,
        ) {
            // Frames render on the ground the live compositor clears to, so a
            // film reads as the screen did.
            let ground = driver.ground();
            let mut described = Vec::with_capacity(frames.len());
            for frame in frames {
                let name = frame
                    .index
                    .map_or_else(|| "before".to_owned(), |i| format!("{i:03}"));
                let path = self.dir.join(format!("{prefix}-{name}.png"));
                self.renderer.set_backdrop(frame.backdrop);
                self.renderer.render_png(&frame.scene, ground, &path);
                described.push(serde_json::json!({
                    "index": frame.index,
                    "ms": frame.index.map(|_| frame.at.as_secs_f64() * 1000.0),
                    "late_ms": frame.late.as_secs_f64() * 1000.0,
                    "animation_owners": frame.owners,
                }));
            }
            let count = described.len();
            let path = self.dir.join(format!("{prefix}.json"));
            let json = serde_json::json!({ "hz": 60, "frames": described });
            match serde_json::to_vec_pretty(&json) {
                Ok(bytes) => match std::fs::write(&path, bytes) {
                    Ok(()) => println!("  wrote {} ({count} frames)", path.display()),
                    Err(error) => eprintln!("shots: write {}: {error}", path.display()),
                },
                Err(error) => eprintln!("shots: film {prefix}: {error}"),
            }
        }
    }

    /// Run a scenario through the offscreen renderer. When `per_frame`
    /// is `Some(n)`, every `Wait(k)` step in the scenario is expanded
    /// into `n` intermediate snapshots so animation curves are inspectable
    /// frame-by-frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the output directory cannot be created.
    pub fn run_scenario(
        driver: &mut ShellDriver,
        scenario: &Scenario,
        dir: &Path,
        per_frame: Option<u32>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("create offscreen output dir '{}': {e}", dir.display()))?;
        let (w, h) = driver.scene_size();
        let (w, h) = (dim_to_u32(w), dim_to_u32(h));
        let mut renderer = OffscreenRenderer::new(w, h);
        println!(
            "shots: offscreen scenario '{}' at {w}x{h} → {}",
            scenario.name,
            dir.display()
        );
        let mut sink = PngDirSink {
            dir,
            renderer: &mut renderer,
            first_paint_seen: false,
        };
        scenario::run_with(driver, scenario, &mut sink, per_frame);
        Ok(())
    }

    /// Backwards-compat scene-set path: render each named scene with a
    /// fresh driver (so flag mutations don't bleed between scenes).
    ///
    /// # Errors
    ///
    /// Returns an error if the output directory cannot be created, or if a
    /// scene named in `scenes` fails its own setup.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the shell fixture owns long-lived permits by design and lives for the whole run"
    )]
    pub fn run_scenes(
        scenes: &[String],
        dir: &Path,
        viewport: (u32, u32),
        outputs: Option<&[shell::compositor::backend::Output]>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("create offscreen output dir '{}': {e}", dir.display()))?;
        let mut renderer = OffscreenRenderer::new(viewport.0, viewport.1);
        println!(
            "shots: offscreen scene-set ({} scenes) at {}x{} → {}",
            scenes.len(),
            viewport.0,
            viewport.1,
            dir.display()
        );
        let mut first_paint_seen = false;
        for name in scenes {
            let mut driver = ShellDriver::for_scene(name, viewport);
            if let Some(outputs) = outputs {
                driver.set_outputs(outputs);
            }
            driver
                .scene_setup(name)
                .map_err(|e| format!("scene_setup({name}) failed: {e}"))?;
            let scene = driver.paint_settled();
            let path = dir.join(format!("{name}.png"));
            renderer.set_backdrop(driver.take_backdrop());
            renderer.render_png(&scene, Color::TRANSPARENT, &path);
            if !first_paint_seen {
                // Mark AFTER render_png returns so the metric captures
                // vello's ~30-pipeline cold compile, not just the
                // CPU-side scene build.
                tilo_startup_metrics::mark("first-paint");
                first_paint_seen = true;
            }
            println!("  wrote {}", path.display());
        }
        Ok(())
    }
}

// =====================================================================
// Headless mode
// =====================================================================

pub mod headless {
    use super::{JsonDirSink, Path, Scenario, ShellDriver, dim_to_u32, scenario};

    /// Run a scenario through the headless state dumper.
    ///
    /// # Errors
    ///
    /// Returns an error if the output directory cannot be created.
    pub fn run_scenario(
        driver: &mut ShellDriver,
        scenario: &Scenario,
        dir: &Path,
        per_frame: Option<u32>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("create headless output dir '{}': {e}", dir.display()))?;
        let (w, h) = driver.viewport();
        println!(
            "shots: headless scenario '{}' at {}x{} → {}",
            scenario.name,
            dim_to_u32(w),
            dim_to_u32(h),
            dir.display()
        );
        let mut sink = JsonDirSink::new(dir);
        scenario::run_with(driver, scenario, &mut sink, per_frame);
        Ok(())
    }
}

// =====================================================================
// Recording mode (H.264)
// =====================================================================

pub mod recording {
    use super::{
        OffscreenRenderer, Path, Scenario, ShellDriver, SnapshotSink, dim_to_u32,
        frames_per_step_for, scenario,
    };

    use shell::network::encoder::VideoEncoder;
    use tilo_callstream::mp4_recording::{Mp4Recording, recording_encoder};

    /// What a recording produced: the codec and its first access unit (a
    /// keyframe), so a caller can check the content decodes.
    pub struct RecordingSummary {
        pub codec: media::StreamCodec,
        pub first_unit: Option<Vec<u8>>,
    }

    /// Run a scenario and record one or more encoded frames per scenario
    /// step into an MP4 file at `out`.
    ///
    /// `fps` controls both the encoder's rate-control hint AND the number
    /// of frames rendered per step (`fps * step_duration_secs`) so the
    /// resulting video plays back at scenario-step pace rather than
    /// galloping through at one frame per step.
    ///
    /// The codec is the most compatible one this machine encodes (H.264
    /// where available, else AV1 in hardware, else VP9 in software).
    ///
    /// # Errors
    ///
    /// Returns an error if the output file cannot be created, the encoder cannot be
    /// constructed, or a frame fails to encode or write.
    pub fn run_scenario(
        driver: &mut ShellDriver,
        scenario: &Scenario,
        out: &Path,
        fps: u32,
        step_duration_secs: f32,
    ) -> Result<RecordingSummary, String> {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let (w, h) = driver.scene_size();
        // 4:2:0 encoders take even dimensions.
        let (w, h) = (dim_to_u32(w) & !1, dim_to_u32(h) & !1);
        let mut renderer = OffscreenRenderer::new(w, h);
        let mut encoder = recording_encoder(w, h, fps, 4_000_000, 0, true)?;
        let codec = encoder.codec();
        let mut file = Mp4Recording::new(out.to_path_buf(), codec, w, h, fps, Vec::new());
        let frames_per_step = frames_per_step_for(fps, step_duration_secs);
        println!(
            "shots: recording scenario '{}' at {}x{} {}fps, {} frame(s)/step → {}",
            scenario.name,
            w,
            h,
            fps,
            frames_per_step,
            out.display()
        );
        let mut sink = RecordingSink {
            renderer: &mut renderer,
            encoder: &mut encoder,
            file: &mut file,
            frames_per_step,
            first_unit: None,
            error: None,
        };
        // Snapshot the start state too (sink runs only on `Snapshot`
        // actions). For recording we want **every step** to contribute a
        // chunk to the video, not just `Snapshot` markers. So we use a
        // step-driven loop that wraps `scenario::apply` with our own
        // capture pass.
        for action in &scenario.actions {
            scenario_apply_then_capture(driver, action, &mut sink)?;
        }
        let mut first_unit = sink.first_unit.take();
        let tail = encoder
            .finish()
            .map_err(|e| format!("finish encoding in recording mode: {e}"))?;
        write_units(&mut file, &mut first_unit, tail)?;
        file.finish()?;
        Ok(RecordingSummary { codec, first_unit })
    }

    /// `SnapshotSink` adapter for recording mode. Encodes `frames_per_step`
    /// frames per scenario step into the open `file`. Each captured frame
    /// re-renders the *same* scene (`paint`) because the scenario engine's
    /// step is itself the granularity of state change.
    pub struct RecordingSink<'a> {
        pub renderer: &'a mut OffscreenRenderer,
        pub encoder: &'a mut VideoEncoder,
        pub file: &'a mut Mp4Recording,
        pub frames_per_step: u32,
        first_unit: Option<Vec<u8>>,
        /// Set by [`Self::encode_current`] on failure when called through
        /// [`SnapshotSink::capture`], whose signature can't return `Result`;
        /// the driving loop checks this after every capture.
        error: Option<String>,
    }

    impl RecordingSink<'_> {
        fn encode_current(&mut self, driver: &mut ShellDriver) -> Result<(), String> {
            let scene = driver.paint();
            self.renderer.set_backdrop(driver.take_backdrop());
            let rgba = self.renderer.render_rgba_resilient(&scene);
            for _ in 0..self.frames_per_step {
                let units = self
                    .encoder
                    .encode(&rgba)
                    .map_err(|e| format!("encode in recording mode: {e}"))?;
                write_units(self.file, &mut self.first_unit, units)?;
            }
            Ok(())
        }
    }

    fn write_units(
        file: &mut Mp4Recording,
        first_unit: &mut Option<Vec<u8>>,
        units: Vec<media::EncodedUnit>,
    ) -> Result<(), String> {
        for unit in units.into_iter().filter(|unit| !unit.data.is_empty()) {
            file.write_access_unit(&unit.data)?;
            if first_unit.is_none() {
                *first_unit = Some(unit.data);
            }
        }
        Ok(())
    }

    impl SnapshotSink for RecordingSink<'_> {
        fn capture(&mut self, driver: &mut ShellDriver, _name: &str) {
            // Encode the frame as offered. The step-driven outer loop
            // encodes every step too, so a plain `Snapshot` contributes
            // its frame twice — harmless in a video, and the price of
            // the case that matters: an action that samples an
            // animation curve emits its whole curve *through this sink*
            // and is a single step to the outer loop. Skipping here left
            // those videos jumping from the curve's first state to its
            // last with nothing in between.
            if let Err(e) = self.encode_current(driver) {
                self.error.get_or_insert(e);
            }
        }
    }

    /// Replay one scenario step through the engine, then encode the
    /// post-step scene as a video chunk. Mirrors `scenario::run` but
    /// interleaves the capture so non-`Snapshot` actions also land in
    /// the video.
    fn scenario_apply_then_capture(
        driver: &mut ShellDriver,
        action: &scenario::Action,
        sink: &mut RecordingSink<'_>,
    ) -> Result<(), String> {
        // Runs through the recording sink itself (see `RecordingSink::capture`
        // for why) rather than a side `NullSink`. `scenario::apply` is
        // private to the module, so this uses `run` with a one-action
        // scenario instead.
        let one_off = Scenario {
            name: "_recording_step",
            description: "",
            actions: vec![action.clone()],
        };
        scenario::run(driver, &one_off, sink);
        if let Some(e) = sink.error.take() {
            return Err(e);
        }
        // Always settle layout between steps so the encoded frame
        // matches what the user would see at this point in the
        // scenario, not a half-rebuilt tree.
        driver.relayout();
        sink.encode_current(driver)
    }
}

// =====================================================================
// Window mode
// =====================================================================

pub mod window {
    use super::{NullSink, Scenario, ShellDriver, scenario};

    use shell::shell::{build_tilo_root, persist, sync_tab_presence_from_tree};
    use ui::presence::PresenceKey;
    use ui::{App, State};

    /// Run the live shell in a real winit window. If `pre_roll` is
    /// `Some(_)`, the scenario runs against a throwaway driver first
    /// (so structural mutations to the workspace tree land on disk)
    /// before control hands off to `App::run`.
    ///
    /// Window mode treats `Snapshot` actions as no-ops because the user
    /// is watching the screen — the goal of window-mode-with-scenario
    /// is "drive the shell into a state, then leave it open".
    ///
    /// Caveat: chrome state (overlay flags, palette query) is **not**
    /// persisted — only the `WorkspaceTree` itself is. So a pre-rolled
    /// `OpenPalette` will not be visible on the live binary's first
    /// frame; a pre-rolled `SwitchWorkspace(1)` will. Useful for
    /// scripted "open the live binary on workspace 2 with these tabs".
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the shell fixture owns long-lived permits by design and lives for the whole run"
    )]
    pub fn run(pre_roll: Option<&Scenario>) {
        if let Some(scenario) = pre_roll {
            println!("shots: window-mode pre-roll scenario '{}'", scenario.name);
            let path = persist::state_json_path();
            // Printed rather than shown: this pre-roll has no chrome yet, and a
            // screenshot run that silently started from a bootstrapped tree
            // would produce images of the wrong desktop.
            let (tree, notice) = persist::load_or_default(&path);
            if let Some(n) = &notice {
                eprintln!("shots: {}", n.one_line());
            }
            let mut driver = ShellDriver::with_tree(tree, (1600, 900));
            let mut sink = NullSink;
            scenario::run(&mut driver, scenario, &mut sink);
            let chrome = driver.chrome();
            let saved = persist::save_shared_tree_to(&chrome.tree_handle(), &path);
            drop(chrome);
            if let Err(e) = saved {
                eprintln!("shots: pre-roll persist failed: {e}");
            }
        }

        // Live binary boilerplate — mirror of `crates/session/shell/src/main.rs`.
        let path = persist::state_json_path();
        let (tree, notice) = persist::load_or_default(&path);
        if let Some(n) = &notice {
            eprintln!("shots: {}", n.one_line());
        }
        let state = State::new();
        let root = build_tilo_root(&tree);
        let mut app = App::new(state, root);
        sync_tab_presence_from_tree(&tree, app.presence_ctx_mut());
        // The room, not the local workspace id: the id is a per-machine
        // handle, so a pointer published under it is addressed to a scope
        // no other machine has. Same key the overlay reads peers by.
        if let Some(room) = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.id == tree.desktop.active)
            .map(|w| w.room().to_string())
        {
            app.set_presence_broadcast_scope(Some(PresenceKey::Workspace { id: room }));
        }
        app.run();
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use tilo_callstream::surface_decoder::VideoDecoder;

    use super::super::scenario::lookup;

    /// Recording mode must produce a non-empty MP4 whose content decodes
    /// to a real (non-uniform, non-black) frame — the negative regression
    /// to guard against is an all-zero RGBA buffer being fed to the encoder
    /// because of a BGRA-vs-RGBA swizzle.
    #[test]
    fn recording_mode_writes_a_decodable_mp4_for_scenario() {
        let tmp = std::env::temp_dir().join("shots-recording-test.mp4");
        let _ = fs::remove_file(&tmp);

        let scenario = lookup("palette_typing").expect("palette_typing in catalogue");
        let mut driver = ShellDriver::new((128, 96));
        let summary =
            recording::run_scenario(&mut driver, &scenario, &tmp, 30, 1.0).expect("run_scenario");
        drop(driver);

        let bytes = fs::read(&tmp).expect("read recorded file");
        assert!(bytes.len() > 8, "recording mode produced an empty file");
        assert_eq!(&bytes[4..8], b"ftyp", "the recording is an MP4");

        let first = summary.first_unit.expect("at least one encoded frame");
        let mut dec = VideoDecoder::new(summary.codec, false);
        let frame = dec.decode(&first).expect("decoder yields a frame");
        assert_eq!(frame.width, 128);
        assert_eq!(frame.height, 96);
        let pixels = match &frame.pixels {
            tilo_callstream::surface_decoder::DecodedPixels::Rgba8(bytes) => bytes,
            other @ tilo_callstream::surface_decoder::DecodedPixels::P010 { .. } => {
                panic!("an SDR decoder should yield RGBA pixels, got {other:?}")
            }
        };
        let first_r = pixels[0];
        let nonuniform = pixels
            .chunks(4)
            .step_by(64)
            .any(|chunk| chunk[0] != first_r);
        assert!(
            nonuniform,
            "decoded recording is uniformly coloured (R={first_r}) — \
             likely a black frame, meaning the offscreen renderer's \
             output isn't reaching the encoder"
        );
        let _ = fs::remove_file(&tmp);
    }

    /// Shots render at a laptop's 2560x1600 at scale 2, alone and beside a
    /// 2560x1440 monitor at scale 1, with Quick Settings open over its
    /// blurred backdrop. The backdrop's render used to hold the renderer's
    /// one counter readback, so the frame over it was never measured: it
    /// overflowed unseen on every attempt and the run panicked ("Vello did
    /// not produce two consecutive overflow-free screenshot renders").
    #[test]
    fn a_2560x1600_at_scale_2_render_succeeds_with_quick_settings_open() {
        for arrangement in ["2560x1600@2", "2560x1600@2,2560x1440@1"] {
            let outputs =
                shell::compositor::backend::headless::resolve_output_arrangement(arrangement)
                    .expect("arrangement");
            let viewport = shell::compositor::backend::combined_geometry(&outputs);
            let dir = std::env::temp_dir()
                .join(format!("shots-hidpi-{}-{arrangement}", std::process::id()));
            offscreen::run_scenes(
                &["qs-wifi".to_owned(), "desktop".to_owned()],
                &dir,
                viewport,
                Some(&outputs),
            )
            .expect("the scenes render");
            for scene in ["qs-wifi", "desktop"] {
                let png = fs::read(dir.join(format!("{scene}.png"))).expect("the shot is written");
                assert!(!png.is_empty(), "{arrangement}: {scene} is a picture");
            }
            let _ = fs::remove_dir_all(&dir);
        }
    }

    /// The desktop a scenario boots opens its apps as their own components:
    /// its Notes tab is the Notes app, still there after the scenario
    /// clicks into it and types, instead of a tile saying it is unavailable.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the shell fixture owns long-lived permits by design and lives for the whole test"
    )]
    fn booted_notes_tab_is_the_notes_app_and_takes_typing() {
        let mut driver = ShellDriver::new((1600, 900));
        let (strip, notes) = {
            let chrome = driver.chrome();
            let tree = chrome.tree();
            let strip = &tree.desktop.workspaces[0].strips[0];
            (strip.id, strip.window_groups[0].tabs[0].id)
        };
        let scenario = lookup("notes_click_and_type").expect("notes_click_and_type in catalogue");
        scenario::run(&mut driver, &scenario, &mut NullSink);

        let host = driver
            .host_mut()
            .group_at_for_tests(strip, 0)
            .and_then(|group| group.host_for_tab_mut(notes))
            .expect("the booted Notes tab");
        match host.content() {
            ui::widget::TileContent::App(app) => assert_eq!(app.app_title(), "Notes"),
            _ => panic!("the booted Notes tab is not the Notes app"),
        }
    }

    /// The edge-overlay scenario addresses the top bar's chips by name, so
    /// it reaches the same chip at every viewport of the sweep; each name
    /// must resolve when the scenario reaches it, or the click is dropped
    /// and the snapshot shows the wrong state.
    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the shell fixture owns long-lived permits by design and lives for the whole test"
    )]
    fn overlay_edges_reaches_every_named_chip() {
        let scenario = lookup("overlay_edges").expect("overlay_edges in catalogue");
        for size in [(1280, 720), (1920, 1080)] {
            let mut driver = ShellDriver::new(size);
            for action in &scenario.actions {
                if let scenario::Action::ClickNamed(name)
                | scenario::Action::MoveCursorNamed(name) = action
                {
                    assert!(
                        driver.resolve_named_centre(name).is_some(),
                        "{size:?}: '{name}' does not resolve when overlay_edges reaches it"
                    );
                }
                let step = Scenario {
                    name: "_overlay_edges_step",
                    description: "",
                    actions: vec![action.clone()],
                };
                scenario::run(&mut driver, &step, &mut NullSink);
            }
        }
    }
}
