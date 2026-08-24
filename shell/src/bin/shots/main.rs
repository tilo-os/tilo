//! `shots` — four-mode driver for the nirios shell, plus a `check`
//! subcommand for baseline image diffs.
//!
//! Modes:
//!   - `window`    — real winit window + GPU swapchain. Optional
//!                    `--scenario NAME` pre-rolls before going
//!                    interactive.
//!   - `offscreen` — wgpu offscreen renderer, no window. Captures one
//!                    PNG per `Snapshot` action under `--out`.
//!   - `headless`  — no GPU, no display. Dumps chrome state JSON per
//!                    `Snapshot` action under `--out`.
//!   - `recording` — wgpu offscreen + H.264 encoder. Writes a raw
//!                    Annex-B NALU stream to `--out`.
//!
//! Subcommands:
//!   - `shots check --baseline <dir>` — capture current outputs and
//!     pixel-diff them against PNGs already in `<dir>`. Exit 1 on any
//!     mismatch, 0 otherwise.
//!
//! Multi-resolution sweep (`--sweep`) and per-frame capture
//! (`--per-frame N`) are documented in `--help`.
//!
//! Backwards-compat: invoked without `--mode` / `--scenario`, behaves
//! like the original `shots` binary — renders the legacy scene set
//! (`desktop`, `overview`, `palette`, …) to PNG via offscreen mode.

mod diff;
mod driver;
mod mode;
mod scenario;

use std::path::PathBuf;

use driver::ShellDriver;
use scenario::{Scenario, catalogue, lookup};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Window,
    Offscreen,
    Headless,
    /// H.264 video recording. Renders every scenario step as one or more
    /// encoded frames into a single `.h264` file (raw Annex-B NALU stream).
    Recording,
}

/// Default sweep set used when `--sweep` is passed without an explicit
/// list. Covers the four most common 16:9-ish target sizes; `800x600`
/// surfaces the worst-case narrow viewport for sidebar overflow tests.
const DEFAULT_SWEEP: &[(u32, u32)] = &[(800, 600), (1280, 720), (1600, 900), (1920, 1080)];

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
    /// Optional multi-resolution sweep. When `Some`, the offscreen /
    /// headless paths run once per resolution and write to
    /// `<out>/<WxH>/...` subdirectories. `None` means "use the single
    /// `--width`/`--height` size".
    sweep: Option<Vec<(u32, u32)>>,
    /// When `Some(n)` and a scenario is active, every `Wait(k)` step
    /// gets expanded into `n` intermediate snapshots so animation
    /// curves are inspectable per frame. See `scenario::run_with`.
    per_frame: Option<u32>,
    /// Subcommand-style verb. `Some("check")` triggers the baseline-
    /// diff path; the rest of the flags configure the capture step.
    subcommand: Option<String>,
    /// Directory of PNGs to compare against in `check` mode.
    baseline: Option<PathBuf>,
    /// Per-channel delta below which a pixel still counts as "match" in
    /// `check` mode. Tunable for noisy GPUs.
    diff_threshold: u8,
    /// Maximum number of differing pixels per image before a file is
    /// reported as DIFF in `check` mode. Lets CI tolerate the small
    /// number of pixels that flicker between renders because of live
    /// data (the topbar clock reads `chrono::Local::now()`, the system
    /// monitor reads CPU usage, etc.). Default 0 — strict.
    max_diff_pixels: u64,
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
        let mut sweep: Option<Vec<(u32, u32)>> = None;
        let mut per_frame: Option<u32> = None;
        let mut subcommand: Option<String> = None;
        let mut baseline: Option<PathBuf> = None;
        let mut diff_threshold: u8 = 4;
        let mut max_diff_pixels: u64 = 0;

        let mut argv: Vec<String> = std::env::args().skip(1).collect();
        // Subcommand detection — `shots check ...` runs the baseline
        // diff path. Anything else falls through to the modes.
        if let Some(first) = argv.first() {
            if first == "check" {
                subcommand = Some("check".to_string());
                argv.remove(0);
            }
        }
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
                        other => {
                            return Err(format!(
                                "--mode: unknown value '{other}' \
                             (expected window|offscreen|headless|recording)"
                            ));
                        }
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
                "--sweep" => {
                    // Optional value. If the next arg looks like a flag
                    // (or there isn't one) we use the default set;
                    // otherwise parse a comma-separated WxH list.
                    let next = argv.first().cloned();
                    let resolutions: Vec<(u32, u32)> = match next {
                        Some(v) if !v.starts_with("--") => {
                            argv.remove(0);
                            parse_sweep_list(&v)?
                        }
                        _ => DEFAULT_SWEEP.to_vec(),
                    };
                    if resolutions.is_empty() {
                        return Err("--sweep: at least one resolution required".into());
                    }
                    sweep = Some(resolutions);
                }
                "--per-frame" => {
                    let v = argv
                        .first()
                        .ok_or("--per-frame needs a value")?
                        .parse::<u32>()
                        .map_err(|e| format!("--per-frame: {e}"))?;
                    argv.remove(0);
                    if v == 0 {
                        return Err("--per-frame must be > 0".into());
                    }
                    per_frame = Some(v);
                }
                "--baseline" => {
                    let v = argv.first().ok_or("--baseline needs a value")?.clone();
                    argv.remove(0);
                    baseline = Some(PathBuf::from(v));
                }
                "--diff-threshold" => {
                    let v = argv
                        .first()
                        .ok_or("--diff-threshold needs a value")?
                        .parse::<u8>()
                        .map_err(|e| format!("--diff-threshold: {e}"))?;
                    argv.remove(0);
                    diff_threshold = v;
                }
                "--max-diff-pixels" => {
                    let v = argv
                        .first()
                        .ok_or("--max-diff-pixels needs a value")?
                        .parse::<u64>()
                        .map_err(|e| format!("--max-diff-pixels: {e}"))?;
                    argv.remove(0);
                    max_diff_pixels = v;
                }
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
        if subcommand.as_deref() == Some("check") {
            if baseline.is_none() {
                return Err("shots check: --baseline DIR is required".into());
            }
            // `check` always runs through the offscreen path — it needs
            // PNGs to diff. Reject mode mismatches explicitly so the
            // failure mode is obvious.
            if mode != Mode::Offscreen {
                return Err(format!(
                    "shots check: --mode {:?} is not supported (offscreen only)",
                    mode
                ));
            }
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
            if sweep.is_some() {
                return Err("--sweep is not supported in recording mode".into());
            }
            if per_frame.is_some() {
                return Err("--per-frame is not supported in recording mode".into());
            }
            // Default `out` is a directory; in recording mode we write a
            // single file. Switch the default to a `.h264` path if the
            // caller didn't supply one.
            if out == PathBuf::from("target/nirios-shots") {
                out = PathBuf::from("target/nirios-shots.h264");
            }
        }
        if mode == Mode::Window && (sweep.is_some() || per_frame.is_some()) {
            return Err("--sweep / --per-frame are not supported in window mode".into());
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
            sweep,
            per_frame,
            subcommand,
            baseline,
            diff_threshold,
            max_diff_pixels,
        })
    }
}

/// Parse a comma-separated `WxH,WxH,...` resolution list. Rejects empty
/// entries and zero dimensions. Pure function for unit-test coverage.
pub fn parse_sweep_list(raw: &str) -> Result<Vec<(u32, u32)>, String> {
    let mut out = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (w, h) = entry
            .split_once(|c: char| c == 'x' || c == 'X')
            .ok_or_else(|| format!("--sweep: '{entry}' is not WxH"))?;
        let w: u32 = w
            .trim()
            .parse()
            .map_err(|e| format!("--sweep: '{entry}' width: {e}"))?;
        let h: u32 = h
            .trim()
            .parse()
            .map_err(|e| format!("--sweep: '{entry}' height: {e}"))?;
        if w == 0 || h == 0 {
            return Err(format!("--sweep: '{entry}' must have non-zero dims"));
        }
        out.push((w, h));
    }
    if out.is_empty() {
        return Err("--sweep: at least one resolution required".into());
    }
    Ok(out)
}

fn print_help() {
    let scene_list = ShellDriver::ALL_SCENES.join(", ");
    let scenario_list: Vec<String> = catalogue()
        .into_iter()
        .map(|s| format!("    {:18} — {}", s.name, s.description))
        .collect();
    let default_sweep: Vec<String> = DEFAULT_SWEEP
        .iter()
        .map(|(w, h)| format!("{w}x{h}"))
        .collect();
    println!(
        "shots — four-mode driver for the nirios shell\n\
\n\
Usage:\n\
  shots [MODE-FLAGS]                        capture mode (default)\n\
  shots check --baseline DIR [MODE-FLAGS]   capture + diff against DIR\n\
\n\
Modes:\n\
  --mode window     real winit window (interactive)\n\
  --mode offscreen  no window, captures PNGs       [default]\n\
  --mode headless   no GPU, dumps state JSON\n\
  --mode recording  no window, writes one .h264 file (raw NALUs)\n\
\n\
Common flags:\n\
  --scenario NAME       replay a built-in scenario\n\
  --scenes A,B,C        [offscreen, no scenario] pick scenes from the legacy catalogue\n\
  --out PATH            output path. Directory for offscreen/headless/check;\n\
                        file for recording (default ./target/nirios-shots for the\n\
                        former, ./target/nirios-shots.h264 for the latter)\n\
  --width N             viewport width  (default 1600)\n\
  --height N            viewport height (default 900)\n\
  --fps N               [recording] frame rate (default 30)\n\
  --step-duration S     [recording] seconds of video per scenario step (default 1.0)\n\
  --sweep [LIST]        run at multiple resolutions and write to\n\
                        <out>/<WxH>/...; LIST is a comma-separated WxH set\n\
                        like 720x480,1366x768. Omit to use the default set:\n\
                        {default_sweep}\n\
  --per-frame N         expand every Wait(k) into N intermediate snapshots so\n\
                        animation curves can be inspected frame-by-frame.\n\
                        Output: <out>/<scenario>/<step>-frame00.png …\n\
  --baseline DIR        [check] directory of PNGs to compare against\n\
  --diff-threshold N    [check] per-channel delta below which a pixel still\n\
                        matches (default 4)\n\
  --max-diff-pixels N   [check] tolerate up to N differing pixels per image\n\
                        before flagging it as DIFF (default 0). Useful for\n\
                        the topbar clock + system monitor which update from\n\
                        live data and produce a small per-frame delta\n\
  --list-scenarios      print the built-in catalogue and exit\n\
\n\
Scene catalogue (legacy):\n  {scene_list}\n\
\n\
Built-in scenarios:\n{scenarios}\n\
\n\
Examples:\n\
  shots                                                 # offscreen, all scenes\n\
  shots --scenes desktop,palette --out /tmp/s\n\
  shots --mode offscreen --scenario palette_typing\n\
  shots --mode offscreen --scenes desktop --sweep --out /tmp/sweep\n\
  shots --mode offscreen --scenario overview_transition --per-frame 4\n\
  shots --mode headless  --scenario palette_typing --out target/headless\n\
  shots --mode recording --scenario palette_typing --out /tmp/out.h264\n\
  shots --mode window    --scenario workspace_switch\n\
  shots check --baseline target/baseline --out target/current\n",
        default_sweep = default_sweep.join(","),
        scene_list = scene_list,
        scenarios = scenario_list.join("\n"),
    );
}

fn main() {
    // A home of the harness's own, before anything can read the real one.
    //
    // First statement in `main` for two reasons. It pins environment
    // variables, which is only sound while this process is still
    // single-threaded — the font warmup below spawns the first thread.
    // And every home-reading surface downstream (Files, the palette's
    // file provider, the portal's picker scope) resolves its root once,
    // early, so a pin applied later would arrive after the answer.
    driver::stage_demo_home();

    // One language on screen, and the same one every time.
    //
    // Every other surface that photographs this compositor pins its
    // locale; `shots` did not, so it rendered in whatever the machine
    // filming it happened to use. That machine is German, and only some
    // strings have a German catalogue — so a shot came back with an
    // English strip of tabs above a German dialog, which is what broken
    // software looks like. Worse for a harness: nothing in a shot says
    // which language produced it, so a reviewer measuring a label's
    // width or a panel's fit was measuring a string the film will never
    // show.
    //
    // `QOS_SHOTS_LANG` overrides it, because shooting the German
    // catalogue on purpose is how you find out whether a longer word
    // still fits — the question this pin would otherwise hide.
    // Every catalogue this process renders, not just the shell's. `shots`
    // links the apps it photographs and draws them in-process, and an app
    // that ships its own catalogue resolves it against its own active
    // locale — which, left alone, it reads from the environment. So the
    // shell pill above a word-processor tile said "Document" in English
    // while the toolbar inside it read "Neu / Tabelle / Stile / Lineal",
    // in a shot taken *after* this pin was added: the pin was real and
    // covered one of the two catalogues on screen.
    //
    // `set_locale` is per-crate because the catalogues are; keep this
    // list in step with the crates that have one. `shots_pins_every_-
    // catalogue` fails when a new one is added without a line here.
    let shots_lang = std::env::var("QOS_SHOTS_LANG")
        .unwrap_or_else(|_| "en_US.UTF-8".to_string());
    shell::shell::i18n::set_locale(&shots_lang);
    word_processor::i18n::set_locale(&shots_lang);

    // Startup timing capture. Same env-var contract as `shell` — a
    // process launched with `QOS_STARTUP_METRICS_PATH=/tmp/x.json` gets
    // a JSON dump on exit; without it, every `mark` call is a single
    // atomic-load no-op. `shots` is the harness that measures the win
    // from the cold-start warmup, so instrumenting it is the only way
    // to A/B the two paths without a real display.
    qos_startup_metrics::init();
    let _startup_dump = qos_startup_metrics::ScopedDumper;
    qos_startup_metrics::mark("main-start");

    // Kick off cosmic-text + Vello warmup on a background thread. See
    // `ui::warmup::spawn_warmup` for what runs; the point of doing it
    // this early is that the FontSystem build and the scratch Vello
    // render both overlap with argv parsing / driver bring-up. Disabled
    // with QOS_STARTUP_WARM=off.
    // FontSystem-only warmup by default (~90 ms cold-start win vs no
    // warmup, measured on the shots harness). Vello scratch-render
    // opt-in via QOS_STARTUP_WARM_VELLO=1 — see the compositor's main
    // for the rationale.
    let want_vello = std::env::var("QOS_STARTUP_WARM_VELLO")
        .ok()
        .is_some_and(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "off" | "false" | "no"
            )
        });
    let _warmup_handle = ui::warmup::spawn_warmup(!want_vello);

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

    if args.subcommand.as_deref() == Some("check") {
        let baseline = args
            .baseline
            .as_ref()
            .expect("Args::parse enforces --baseline for `check`");
        let exit_code = run_check(&args, baseline);
        std::process::exit(exit_code);
    }

    run_capture(&args);
}

/// Capture the configured scenes / scenarios to disk. Honours `--sweep`
/// (multi-resolution) and `--per-frame` (animation step expansion).
fn run_capture(args: &Args) {
    let viewports: Vec<(u32, u32)> = match &args.sweep {
        Some(list) => list.clone(),
        None => vec![(args.width, args.height)],
    };
    for vp in &viewports {
        let out = if args.sweep.is_some() {
            args.out.join(format!("{}x{}", vp.0, vp.1))
        } else {
            args.out.clone()
        };
        run_capture_one(args, *vp, &out);
    }
}

fn run_capture_one(args: &Args, viewport: (u32, u32), out: &std::path::Path) {
    match args.mode {
        Mode::Offscreen => match args.scenario.as_deref() {
            Some(name) => {
                let scenario = require_scenario(name);
                let mut driver = ShellDriver::new(viewport);
                mode::offscreen::run_scenario(&mut driver, &scenario, out, args.per_frame);
            }
            None => mode::offscreen::run_scenes(&args.scenes, out, viewport),
        },
        Mode::Headless => {
            let scenario = require_scenario(
                args.scenario
                    .as_deref()
                    .expect("Args::parse enforces --scenario for headless"),
            );
            let mut driver = ShellDriver::new(viewport);
            mode::headless::run_scenario(&mut driver, &scenario, out, args.per_frame);
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
                out,
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

/// Drive the `check` subcommand: capture current outputs (under a temp
/// directory, or under `--out` if set) and diff every PNG present in
/// `baseline` against the captured copy.
///
/// Returns the process exit code (0 = all match, 1 = at least one
/// diff or missing file).
fn run_check(args: &Args, baseline: &std::path::Path) -> i32 {
    // Capture current state under args.out (default
    // `target/nirios-shots`). The diff walker then compares every PNG
    // in `baseline` against the corresponding file under `current`.
    let current = args.out.clone();
    if let Err(e) = std::fs::create_dir_all(&current) {
        eprintln!("shots check: failed to create '{}': {e}", current.display());
        return 2;
    }
    println!(
        "shots check: capturing current state to {}",
        current.display(),
    );
    run_capture(args);
    println!(
        "shots check: comparing against baseline {}",
        baseline.display(),
    );
    match diff::compare_tree(baseline, &current, args.diff_threshold) {
        Ok(report) => {
            let mut any_diff = false;
            let mut ok_count = 0u64;
            let mut diff_count = 0u64;
            for entry in &report.entries {
                match entry.kind {
                    diff::DiffKind::Ok => {
                        ok_count += 1;
                        println!("OK   {}", entry.path.display())
                    }
                    diff::DiffKind::Diff(px) => {
                        // Tolerate `max_diff_pixels` per-image — the
                        // topbar clock and the system monitor read live
                        // data, so a small per-frame delta is the
                        // expected steady state.
                        if px <= args.max_diff_pixels {
                            ok_count += 1;
                            println!("OK   {} ({} px within tolerance)", entry.path.display(), px,);
                        } else {
                            any_diff = true;
                            diff_count += 1;
                            println!("DIFF {} {} pixels differ", entry.path.display(), px,);
                        }
                    }
                    diff::DiffKind::Missing => {
                        any_diff = true;
                        diff_count += 1;
                        println!("MISS {} (no current capture)", entry.path.display());
                    }
                    diff::DiffKind::SizeMismatch { baseline, current } => {
                        any_diff = true;
                        diff_count += 1;
                        println!(
                            "SIZE {} baseline={}x{} current={}x{}",
                            entry.path.display(),
                            baseline.0,
                            baseline.1,
                            current.0,
                            current.1,
                        );
                    }
                    diff::DiffKind::ReadError(ref msg) => {
                        any_diff = true;
                        diff_count += 1;
                        println!("READ {} {}", entry.path.display(), msg);
                    }
                }
            }
            println!("shots check: {} OK, {} diff(s)", ok_count, diff_count,);
            if any_diff { 1 } else { 0 }
        }
        Err(e) => {
            eprintln!("shots check: {e}");
            2
        }
    }
}

fn require_scenario(name: &str) -> Scenario {
    lookup(name).unwrap_or_else(|| {
        eprintln!("shots: unknown scenario '{name}'. Try --list-scenarios to see the catalogue.");
        std::process::exit(2);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_flag_parses_multiple_resolutions() {
        let res = parse_sweep_list("720x480,1366x768").expect("parse ok");
        assert_eq!(res, vec![(720, 480), (1366, 768)]);
        // Case-insensitive separator + whitespace tolerance.
        let res = parse_sweep_list(" 800X600 , 1280x720 ").expect("parse ok");
        assert_eq!(res, vec![(800, 600), (1280, 720)]);
    }

    #[test]
    fn sweep_flag_rejects_bad_entries() {
        assert!(parse_sweep_list("garbage").is_err());
        assert!(parse_sweep_list("").is_err());
        assert!(parse_sweep_list("0x600").is_err());
        assert!(parse_sweep_list("800x0").is_err());
        assert!(parse_sweep_list("800,600").is_err());
    }

    #[test]
    fn default_sweep_covers_expected_resolutions() {
        // Pinned so a future edit that drops 800x600 (the worst-case
        // narrow viewport regression catcher) is caught by CI.
        assert!(DEFAULT_SWEEP.contains(&(800, 600)));
        assert!(DEFAULT_SWEEP.contains(&(1920, 1080)));
    }

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("shell/ has a parent")
            .to_path_buf()
    }

    /// The crate a source file belongs to, as the identifier a `use`
    /// path would name it by.
    fn crate_ident_for(file: &std::path::Path) -> Option<String> {
        let mut dir = file.parent()?;
        loop {
            if dir.join("Cargo.toml").is_file() {
                return Some(dir.file_name()?.to_string_lossy().replace('-', "_"));
            }
            dir = dir.parent()?;
        }
    }

    /// Every crate that chooses its catalogue from the environment,
    /// found by looking for the function that does the choosing.
    ///
    /// Deliberately not a hand-kept list. A crate that grows one of
    /// these later is picked up without anyone remembering to come back
    /// here, which is the entire point of the test below.
    fn env_reading_catalogue_crates() -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        let mut stack = vec![repo_root()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if path.is_dir() {
                    // Build output and third-party sources are not ours
                    // to pin, and walking them costs seconds.
                    if matches!(
                        name.as_str(),
                        "target" | "vendor" | ".git" | ".build" | ".claude" | "node_modules"
                    ) {
                        continue;
                    }
                    stack.push(path);
                } else if name == "i18n.rs" {
                    let Ok(src) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    if !src.contains("fn locale_from_env") {
                        continue;
                    }
                    if let Some(krate) = crate_ident_for(&path) {
                        if !found.contains(&krate) {
                            found.push(krate);
                        }
                    }
                }
            }
        }
        found.sort();
        found
    }

    /// `shots` draws the apps it photographs in-process, and an app with
    /// its own catalogue resolves against its own active locale. Pinning
    /// the shell's and stopping there is how a word-processor tile came
    /// back entirely German — "Neu / Tabelle / Stile / Lineal" — under an
    /// English shell pill reading "Document", in a shot taken *after* the
    /// shell pin was added. Nothing in a PNG says which language produced
    /// it, so that shipped.
    #[test]
    fn shots_pins_every_catalogue_that_reads_the_environment() {
        let crates = env_reading_catalogue_crates();
        // A check that cannot see anything has to fail rather than pass.
        // If this scan stops finding the two known catalogues — a move, a
        // rename, a repo root resolved wrong — then the invariant is
        // unverified, which is not the same as satisfied.
        assert!(
            crates.len() >= 2,
            "found only {crates:?}, expected at least the shell's and the \
             word processor's — the scan is not looking where it thinks, \
             so it is proving nothing"
        );
        let src = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/shots/main.rs"),
        )
        .expect("read shots' own source");
        let missing: Vec<&String> = crates
            .iter()
            .filter(|k| !src.contains(&format!("{k}::i18n::set_locale")))
            .collect();
        assert!(
            missing.is_empty(),
            "{missing:?} choose their catalogue from the environment, and \
             `shots` never pins them — so they render in whatever language \
             the machine happens to use, underneath a shell pinned to \
             another. Add a `set_locale` call beside the others in `main`."
        );
    }
}
