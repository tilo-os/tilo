//! `shots` — three-mode driver for the nirios shell.
//!
//! Modes:
//!   - `window`    — real winit window + GPU swapchain. Optional
//!                    `--scenario NAME` pre-rolls before going
//!                    interactive.
//!   - `offscreen` — wgpu offscreen renderer, no window. Captures one
//!                    PNG per `Snapshot` action under `--out`.
//!   - `headless`  — no GPU, no display. Dumps chrome state JSON per
//!                    `Snapshot` action under `--out`.
//!
//! Backwards-compat: invoked without `--mode` / `--scenario`, behaves
//! like the original `shots` binary — renders the legacy scene set
//! (`desktop`, `overview`, `palette`, …) to PNG via offscreen mode.
//!
//! See `--help` for the full surface and `--list-scenarios` for the
//! built-in catalogue.

mod driver;
mod mode;
mod scenario;

use std::path::PathBuf;

use driver::ShellDriver;
use scenario::{catalogue, lookup, Scenario};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Window,
    Offscreen,
    Headless,
    /// H.264 video recording. Renders every scenario step as one or more
    /// encoded frames into a single `.h264` file (raw Annex-B NALU stream).
    Recording,
}

#[derive(Debug)]
struct Args {
    mode: Mode,
    scenario: Option<String>,
    /// Legacy scene set — only consulted when `mode == Offscreen` and
    /// `scenario` is `None`.
    scenes: Vec<String>,
    out: PathBuf,
    width: u32,
    height: u32,
    /// Frame rate hint for the H.264 encoder AND the number of frames
    /// emitted per scenario step (× `step_duration`). Only consulted in
    /// recording mode.
    fps: u32,
    /// Seconds of video to emit per scenario step. Recording mode only.
    step_duration: f32,
    list_scenarios: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut mode = Mode::Offscreen;
        let mut scenario: Option<String> = None;
        let mut scenes: Vec<String> = ShellDriver::ALL_SCENES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let mut scenes_explicit = false;
        let mut out = PathBuf::from("target/nirios-shots");
        let mut width = 1600u32;
        let mut height = 900u32;
        let mut fps = 30u32;
        let mut step_duration = 1.0f32;
        let mut list_scenarios = false;

        let mut argv: Vec<String> = std::env::args().skip(1).collect();
        while let Some(arg) = argv.first().cloned() {
            argv.remove(0);
            match arg.as_str() {
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                "--mode" => {
                    let v = argv.first().ok_or("--mode needs a value")?.clone();
                    argv.remove(0);
                    mode = match v.as_str() {
                        "window" => Mode::Window,
                        "offscreen" => Mode::Offscreen,
                        "headless" => Mode::Headless,
                        "recording" => Mode::Recording,
                        other => return Err(format!(
                            "--mode: unknown value '{other}' \
                             (expected window|offscreen|headless|recording)"
                        )),
                    };
                }
                "--scenario" => {
                    let v = argv.first().ok_or("--scenario needs a value")?.clone();
                    argv.remove(0);
                    scenario = Some(v);
                }
                "--scenes" => {
                    let v = argv.first().ok_or("--scenes needs a value")?.clone();
                    argv.remove(0);
                    scenes = v.split(',').map(|s| s.trim().to_string()).collect();
                    for s in &scenes {
                        if !ShellDriver::ALL_SCENES.contains(&s.as_str()) {
                            return Err(format!(
                                "unknown scene '{s}'. Valid: {}",
                                ShellDriver::ALL_SCENES.join(", ")
                            ));
                        }
                    }
                    scenes_explicit = true;
                }
                "--out" => {
                    let v = argv.first().ok_or("--out needs a value")?.clone();
                    argv.remove(0);
                    out = PathBuf::from(v);
                }
                "--width" => {
                    let v = argv
                        .first()
                        .ok_or("--width needs a value")?
                        .parse::<u32>()
                        .map_err(|e| format!("--width: {e}"))?;
                    argv.remove(0);
                    width = v;
                }
                "--height" => {
                    let v = argv
                        .first()
                        .ok_or("--height needs a value")?
                        .parse::<u32>()
                        .map_err(|e| format!("--height: {e}"))?;
                    argv.remove(0);
                    height = v;
                }
                "--fps" => {
                    let v = argv
                        .first()
                        .ok_or("--fps needs a value")?
                        .parse::<u32>()
                        .map_err(|e| format!("--fps: {e}"))?;
                    argv.remove(0);
                    fps = v;
                }
                "--step-duration" => {
                    let v = argv
                        .first()
                        .ok_or("--step-duration needs a value")?
                        .parse::<f32>()
                        .map_err(|e| format!("--step-duration: {e}"))?;
                    argv.remove(0);
                    step_duration = v;
                }
                "--list-scenarios" => list_scenarios = true,
                other => return Err(format!("unknown arg '{other}' (try --help)")),
            }
        }

        if width == 0 || height == 0 {
            return Err("width and height must be > 0".into());
        }
        if fps == 0 {
            return Err("--fps must be > 0".into());
        }
        if !(step_duration > 0.0) {
            return Err("--step-duration must be > 0".into());
        }
        // Headless rejects --scenes (no rendering); require --scenario.
        if mode == Mode::Headless {
            if scenes_explicit {
                eprintln!("shots: --scenes is ignored in headless mode (no rendering)");
            }
            if scenario.is_none() {
                return Err("--mode headless requires --scenario NAME".into());
            }
        }
        if mode == Mode::Recording {
            if scenario.is_none() {
                return Err("--mode recording requires --scenario NAME".into());
            }
            // Default `out` is a directory; in recording mode we write a
            // single file. Switch the default to a `.h264` path if the
            // caller didn't supply one.
            if out == PathBuf::from("target/nirios-shots") {
                out = PathBuf::from("target/nirios-shots.h264");
            }
        }
        Ok(Args {
            mode,
            scenario,
            scenes,
            out,
            width,
            height,
            fps,
            step_duration,
            list_scenarios,
        })
    }
}

fn print_help() {
    let scene_list = ShellDriver::ALL_SCENES.join(", ");
    let scenario_list: Vec<String> = catalogue()
        .into_iter()
        .map(|s| format!("    {:18} — {}", s.name, s.description))
        .collect();
    println!(
        "shots — four-mode driver for the nirios shell\n\
\n\
Modes:\n\
  --mode window     real winit window (interactive)\n\
  --mode offscreen  no window, captures PNGs       [default]\n\
  --mode headless   no GPU, dumps state JSON\n\
  --mode recording  no window, writes one .h264 file (raw NALUs)\n\
\n\
Common flags:\n\
  --scenario NAME   replay a built-in scenario\n\
  --scenes A,B,C    [offscreen, no scenario] pick scenes from the legacy catalogue\n\
  --out PATH        output path. Directory for offscreen/headless; file for recording\n\
                    (default ./target/nirios-shots for the former,\n\
                     ./target/nirios-shots.h264 for the latter)\n\
  --width N         viewport width  (default 1600)\n\
  --height N        viewport height (default 900)\n\
  --fps N           [recording] frame rate (default 30)\n\
  --step-duration S [recording] seconds of video per scenario step (default 1.0)\n\
  --list-scenarios  print the built-in catalogue and exit\n\
\n\
Scene catalogue (legacy):\n  {}\n\
\n\
Built-in scenarios:\n{}\n\
\n\
Examples:\n\
  shots                                                 # offscreen, all scenes\n\
  shots --scenes desktop,palette --out /tmp/s\n\
  shots --mode offscreen --scenario palette_typing\n\
  shots --mode headless  --scenario palette_typing --out target/headless\n\
  shots --mode recording --scenario palette_typing --out /tmp/out.h264\n\
  shots --mode window    --scenario workspace_switch\n",
        scene_list,
        scenario_list.join("\n"),
    );
}

fn main() {
    let args = match Args::parse() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("shots: {e}");
            std::process::exit(2);
        }
    };

    if args.list_scenarios {
        for s in catalogue() {
            println!("{:18} {}", s.name, s.description);
        }
        return;
    }

    let viewport = (args.width, args.height);
    match args.mode {
        Mode::Offscreen => match args.scenario.as_deref() {
            Some(name) => {
                let scenario = require_scenario(name);
                let mut driver = ShellDriver::new(viewport);
                mode::offscreen::run_scenario(&mut driver, &scenario, &args.out);
            }
            None => mode::offscreen::run_scenes(&args.scenes, &args.out, viewport),
        },
        Mode::Headless => {
            let scenario = require_scenario(
                args.scenario
                    .as_deref()
                    .expect("Args::parse enforces --scenario for headless"),
            );
            let mut driver = ShellDriver::new(viewport);
            mode::headless::run_scenario(&mut driver, &scenario, &args.out);
        }
        Mode::Recording => {
            let scenario = require_scenario(
                args.scenario
                    .as_deref()
                    .expect("Args::parse enforces --scenario for recording"),
            );
            let mut driver = ShellDriver::new(viewport);
            mode::recording::run_scenario(
                &mut driver,
                &scenario,
                &args.out,
                args.fps,
                args.step_duration,
            );
        }
        Mode::Window => {
            let pre_roll = args.scenario.as_deref().map(require_scenario);
            mode::window::run(pre_roll.as_ref());
        }
    }
}

fn require_scenario(name: &str) -> Scenario {
    lookup(name).unwrap_or_else(|| {
        eprintln!(
            "shots: unknown scenario '{name}'. Try --list-scenarios to see the catalogue."
        );
        std::process::exit(2);
    })
}
