//! Four execution modes for the `shots` binary:
//!   - `offscreen`  — wgpu adapter, no window; captures PNGs.
//!   - `headless`   — no GPU; dumps state JSON.
//!   - `recording`  — wgpu adapter, no window; encodes a scenario to
//!                    a raw H.264 NALU stream (Annex-B), suitable for
//!                    ffplay / mpv / vlc.
//!   - `window`     — real winit window; pre-rolls a scenario then
//!                    yields control to the live event loop.

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
            self.renderer.render_png(&scene, Color::TRANSPARENT, &path);
            if !self.first_paint_seen {
                // Match the mark name the ui `App` emits at the same
                // moment. Downstream analysis tools can look up
                // `first-paint` regardless of whether the process was
                // the live compositor or the shots harness. Mark AFTER
                // `render_png` returns so the metric captures the
                // vello pipeline compile too (they only fire on the
                // first render_to_texture call).
                qos_startup_metrics::mark("first-paint");
                self.first_paint_seen = true;
            }
            println!("  wrote {}", path.display());
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
        let (w, h) = driver.viewport();
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
    /// Returns an error if the output directory cannot be created.
    ///
    /// # Panics
    ///
    /// Panics if a scene named in `scenes` fails its own setup.
    pub fn run_scenes(scenes: &[String], dir: &Path, viewport: (u32, u32)) -> Result<(), String> {
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
            let mut driver = ShellDriver::new(viewport);
            driver
                .scene_setup(name)
                .unwrap_or_else(|e| panic!("scene_setup({name}) failed: {e}"));
            let scene = driver.paint_settled();
            let path = dir.join(format!("{name}.png"));
            renderer.render_png(&scene, Color::TRANSPARENT, &path);
            if !first_paint_seen {
                // Mark AFTER render_png returns so the metric captures
                // vello's ~30-pipeline cold compile, not just the
                // CPU-side scene build.
                qos_startup_metrics::mark("first-paint");
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

    use std::fs::File;
    use std::io::Write;

    use shell::network::encoder::H264Encoder;

    /// Run a scenario and append one or more encoded H.264 NALU payloads
    /// per scenario step to the file at `out`.
    ///
    /// `fps` controls both the encoder's rate-control hint AND the number
    /// of frames rendered per step (`fps * step_duration_secs`) so the
    /// resulting video plays back at scenario-step pace rather than
    /// galloping through at one frame per step.
    ///
    /// Output is a raw Annex-B byte stream (no MP4 container). ffplay /
    /// mpv / vlc all play `.h264` directly.
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
    ) -> Result<(), String> {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let (w, h) = driver.viewport();
        let (w, h) = (dim_to_u32(w), dim_to_u32(h));
        let mut renderer = OffscreenRenderer::new(w, h);
        let mut encoder = H264Encoder::new(w, h, fps)
            .map_err(|e| format!("H264Encoder::new for recording mode: {e}"))?;
        let mut file = File::create(out)
            .map_err(|e| format!("create recording output file '{}': {e}", out.display()))?;
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
        Ok(())
    }

    /// `SnapshotSink` adapter for recording mode. Encodes `frames_per_step`
    /// frames per scenario step into the open `file`. Each captured frame
    /// re-renders the *same* scene (`paint`) because the scenario engine's
    /// step is itself the granularity of state change.
    pub struct RecordingSink<'a> {
        pub renderer: &'a mut OffscreenRenderer,
        pub encoder: &'a mut H264Encoder,
        pub file: &'a mut File,
        pub frames_per_step: u32,
        /// Set by [`Self::encode_current`] on failure when called through
        /// [`SnapshotSink::capture`], whose signature can't return `Result`;
        /// the driving loop checks this after every capture.
        error: Option<String>,
    }

    impl RecordingSink<'_> {
        fn encode_current(&mut self, driver: &mut ShellDriver) -> Result<(), String> {
            let scene = driver.paint();
            let rgba = self.renderer.render_rgba_resilient(&scene);
            for _ in 0..self.frames_per_step {
                let nalus = self
                    .encoder
                    .encode(&rgba)
                    .map_err(|e| format!("H264Encoder::encode in recording mode: {e}"))?;
                if !nalus.is_empty() {
                    self.file
                        .write_all(&nalus)
                        .map_err(|e| format!("write encoded NALU bytes: {e}"))?;
                }
            }
            Ok(())
        }
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

    use shell::shell::{build_nirios_root, persist, sync_tab_presence_from_tree};
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
            if let Err(e) = persist::save_tree_to(&chrome.tree(), &path) {
                eprintln!("shots: pre-roll persist failed: {e}");
            }
        }

        // Live binary boilerplate — mirror of `shell/src/main.rs`.
        let path = persist::state_json_path();
        let (tree, notice) = persist::load_or_default(&path);
        if let Some(n) = &notice {
            eprintln!("shots: {}", n.one_line());
        }
        let state = State::new();
        let root = build_nirios_root(&tree);
        let mut app = App::new(state, root);
        sync_tab_presence_from_tree(&tree, app.presence_ctx_mut());
        // The room, not the local workspace id: the id is a per-machine
        // handle, so a pointer published under it is addressed to a scope
        // no other machine has. Same key the overlay reads peers by.
        if let Some(room) = tree
            .workspaces
            .iter()
            .find(|w| w.id == tree.active)
            .map(|w| w.room.to_string())
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

    use shell::network::decoder::H264Decoder;

    use super::super::scenario::lookup;

    /// Recording mode must produce a non-empty .h264 file whose first
    /// bytes are a valid Annex-B NAL unit start prefix.
    ///
    /// Round-trips the encoded bytes through `H264Decoder` to confirm
    /// they decode to a real (non-uniform, non-black) frame — the
    /// negative regression to guard against is an all-zero RGBA buffer
    /// being fed to the encoder because of a BGRA-vs-RGBA swizzle.
    #[test]
    fn recording_mode_writes_non_empty_h264_for_scenario() {
        let tmp = std::env::temp_dir().join("shots-recording-test.h264");
        // Idempotent — remove any leftover from a previous run.
        let _ = fs::remove_file(&tmp);

        let scenario = lookup("palette_typing").expect("palette_typing in catalogue");
        // 128×96 is the encoder's smallest legal 16-pixel-aligned size
        // with enough resolution to render real content. The recording
        // mode honours the driver's viewport, so we set a small one.
        let mut driver = ShellDriver::new((128, 96));
        recording::run_scenario(&mut driver, &scenario, &tmp, 30, 1.0).expect("run_scenario");

        let bytes = fs::read(&tmp).expect("read recorded file");
        assert!(
            !bytes.is_empty(),
            "recording mode produced empty file at {}",
            tmp.display()
        );

        // Annex-B start code: 0x00 0x00 0x00 0x01 OR 0x00 0x00 0x01.
        let four = bytes.starts_with(&[0x00, 0x00, 0x00, 0x01]);
        let three = bytes.starts_with(&[0x00, 0x00, 0x01]);
        assert!(
            four || three,
            "recorded file must start with an Annex-B NALU prefix; first 8 bytes: {:02x?}",
            &bytes[..bytes.len().min(8)]
        );

        // Sanity: decode the first frame and confirm it carries content
        // (non-uniform pixels). All-zero RGBA would indicate the
        // offscreen renderer's output never reached the encoder.
        let mut dec = H264Decoder::new();
        let frame = dec
            .decode(&bytes)
            .expect("decoder yields at least one frame");
        assert_eq!(frame.width, 128);
        assert_eq!(frame.height, 96);
        let pixels = match &frame.pixels {
            shell::network::DecodedPixels::Rgba8(bytes) => bytes,
            other => panic!("SDR H.264 decoder should yield RGBA pixels, got {other:?}"),
        };
        let first_r = pixels[0];
        // If every pixel's R channel equals the first byte, the frame is
        // uniformly coloured. The shell paints a backdrop + chrome, so a
        // legitimate render is never uniform across 128×96.
        let mut nonuniform = false;
        for chunk in pixels.chunks(4).step_by(64) {
            if chunk[0] != first_r {
                nonuniform = true;
                break;
            }
        }
        assert!(
            nonuniform,
            "decoded recording is uniformly coloured (R={first_r}) — \
             likely a black frame, meaning the offscreen renderer's \
             output isn't reaching the encoder"
        );

        // Cleanup: don't leak the file across runs.
        let _ = fs::remove_file(&tmp);
    }
}
