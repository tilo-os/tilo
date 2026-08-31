//! `shots` — four-mode driver for the nirios shell (`window`, `offscreen`,
//! `headless`, `recording`), plus a `check` subcommand that captures and
//! pixel-diffs against baseline PNGs. Full flag reference in `--help`.
//!
//! Invoked without `--mode` / `--scenario`, behaves like the original
//! `shots` binary — renders the legacy scene set to PNG via offscreen mode.

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

/// Consume the next argv value for `flag`, erroring if there isn't one.
fn take_value(argv: &mut Vec<String>, flag: &str) -> Result<String, String> {
    let v = argv
        .first()
        .ok_or_else(|| format!("{flag} needs a value"))?
        .clone();
    argv.remove(0);
    Ok(v)
}

/// Parse `--mode`'s value. Split out of `Args::apply_flag` to keep it short.
fn parse_mode_flag(v: &str) -> Result<Mode, String> {
    match v {
        "window" => Ok(Mode::Window),
        "offscreen" => Ok(Mode::Offscreen),
        "headless" => Ok(Mode::Headless),
        "recording" => Ok(Mode::Recording),
        other => Err(format!(
            "--mode: unknown value '{other}' (expected window|offscreen|headless|recording)"
        )),
    }
}

/// Parse `--scenes`' comma-separated value into `args.scenes`, rejecting any
/// name outside [`ShellDriver::ALL_SCENES`]. Split out of `Args::apply_flag`
/// to keep it short.
fn apply_scenes_flag(
    args: &mut Args,
    argv: &mut Vec<String>,
    scenes_explicit: &mut bool,
) -> Result<(), String> {
    let v = take_value(argv, "--scenes")?;
    args.scenes = v.split(',').map(|s| s.trim().to_string()).collect();
    for s in &args.scenes {
        if !ShellDriver::ALL_SCENES.contains(&s.as_str()) {
            return Err(format!(
                "unknown scene '{s}'. Valid: {}",
                ShellDriver::ALL_SCENES.join(", ")
            ));
        }
    }
    *scenes_explicit = true;
    Ok(())
}

/// Parse `--sweep`'s optional value: a flag-like or missing next token keeps
/// [`DEFAULT_SWEEP`], otherwise it's a comma-separated `WxH` list. Split out
/// of `Args::apply_flag` to keep it short.
fn apply_sweep_flag(argv: &mut Vec<String>) -> Result<Vec<(u32, u32)>, String> {
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
    Ok(resolutions)
}

/// Parse `--per-frame`'s value, rejecting `0`. Split out of
/// `Args::apply_flag` to keep it short.
fn apply_per_frame_flag(argv: &mut Vec<String>) -> Result<u32, String> {
    let v: u32 = take_value(argv, "--per-frame")?
        .parse()
        .map_err(|e| format!("--per-frame: {e}"))?;
    if v == 0 {
        return Err("--per-frame must be > 0".into());
    }
    Ok(v)
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Args {
            mode: Mode::Offscreen,
            scenario: None,
            scenes: ShellDriver::ALL_SCENES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            out: PathBuf::from("target/nirios-shots"),
            width: 1600,
            height: 900,
            fps: 30,
            step_duration: 1.0,
            list_scenarios: false,
            sweep: None,
            per_frame: None,
            subcommand: None,
            baseline: None,
            diff_threshold: 4,
            max_diff_pixels: 0,
        };
        let mut scenes_explicit = false;

        let mut argv: Vec<String> = std::env::args().skip(1).collect();
        // Subcommand detection — `shots check ...` runs the baseline
        // diff path. Anything else falls through to the modes.
        if let Some(first) = argv.first()
            && first == "check"
        {
            args.subcommand = Some("check".to_string());
            argv.remove(0);
        }
        while let Some(arg) = argv.first().cloned() {
            argv.remove(0);
            Self::apply_flag(&mut args, &arg, &mut argv, &mut scenes_explicit)?;
        }
        Self::validate(&mut args, scenes_explicit)?;
        Ok(args)
    }

    /// Handle one `--flag [value]` argument, consuming its value(s) from
    /// `argv` and updating `args` in place. Split out of `parse` to keep
    /// the top-level loop short.
    fn apply_flag(
        args: &mut Args,
        arg: &str,
        argv: &mut Vec<String>,
        scenes_explicit: &mut bool,
    ) -> Result<(), String> {
        match arg {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--mode" => args.mode = parse_mode_flag(&take_value(argv, "--mode")?)?,
            "--scenario" => args.scenario = Some(take_value(argv, "--scenario")?),
            "--scenes" => apply_scenes_flag(args, argv, scenes_explicit)?,
            "--out" => args.out = PathBuf::from(take_value(argv, "--out")?),
            "--width" => {
                args.width = take_value(argv, "--width")?
                    .parse()
                    .map_err(|e| format!("--width: {e}"))?;
            }
            "--height" => {
                args.height = take_value(argv, "--height")?
                    .parse()
                    .map_err(|e| format!("--height: {e}"))?;
            }
            "--fps" => {
                args.fps = take_value(argv, "--fps")?
                    .parse()
                    .map_err(|e| format!("--fps: {e}"))?;
            }
            "--step-duration" => {
                args.step_duration = take_value(argv, "--step-duration")?
                    .parse()
                    .map_err(|e| format!("--step-duration: {e}"))?;
            }
            "--list-scenarios" => args.list_scenarios = true,
            "--sweep" => args.sweep = Some(apply_sweep_flag(argv)?),
            "--per-frame" => args.per_frame = Some(apply_per_frame_flag(argv)?),
            "--baseline" => args.baseline = Some(PathBuf::from(take_value(argv, "--baseline")?)),
            "--diff-threshold" => {
                args.diff_threshold = take_value(argv, "--diff-threshold")?
                    .parse()
                    .map_err(|e| format!("--diff-threshold: {e}"))?;
            }
            "--max-diff-pixels" => {
                args.max_diff_pixels = take_value(argv, "--max-diff-pixels")?
                    .parse()
                    .map_err(|e| format!("--max-diff-pixels: {e}"))?;
            }
            other => return Err(format!("unknown arg '{other}' (try --help)")),
        }
        Ok(())
    }

    /// Cross-flag validation once the whole argv has been consumed, plus
    /// the mode-specific `out` default (recording mode writes a single
    /// file, not a directory). Split out of `parse` to keep it short.
    fn validate(args: &mut Args, scenes_explicit: bool) -> Result<(), String> {
        if args.width == 0 || args.height == 0 {
            return Err("width and height must be > 0".into());
        }
        if args.fps == 0 {
            return Err("--fps must be > 0".into());
        }
        if !(args.step_duration > 0.0) {
            return Err("--step-duration must be > 0".into());
        }
        if args.subcommand.as_deref() == Some("check") {
            if args.baseline.is_none() {
                return Err("shots check: --baseline DIR is required".into());
            }
            // `check` always runs through the offscreen path — it needs
            // PNGs to diff. Reject mode mismatches explicitly so the
            // failure mode is obvious.
            if args.mode != Mode::Offscreen {
                return Err(format!(
                    "shots check: --mode {:?} is not supported (offscreen only)",
                    args.mode
                ));
            }
        }
        // Headless rejects --scenes (no rendering); require --scenario.
        if args.mode == Mode::Headless {
            if scenes_explicit {
                eprintln!("shots: --scenes is ignored in headless mode (no rendering)");
            }
            if args.scenario.is_none() {
                return Err("--mode headless requires --scenario NAME".into());
            }
        }
        if args.mode == Mode::Recording {
            if args.scenario.is_none() {
                return Err("--mode recording requires --scenario NAME".into());
            }
            if args.sweep.is_some() {
                return Err("--sweep is not supported in recording mode".into());
            }
            if args.per_frame.is_some() {
                return Err("--per-frame is not supported in recording mode".into());
            }
            // Default `out` is a directory; in recording mode we write a
            // single file. Switch the default to a `.h264` path if the
            // caller didn't supply one.
            if args.out == *"target/nirios-shots" {
                args.out = PathBuf::from("target/nirios-shots.h264");
            }
        }
        if args.mode == Mode::Window && (args.sweep.is_some() || args.per_frame.is_some()) {
            return Err("--sweep / --per-frame are not supported in window mode".into());
        }
        Ok(())
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
            .split_once(['x', 'X'])
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

fn main() -> Result<(), String> {
    // Same env-var contract as `shell`: QOS_STARTUP_METRICS_PATH=/tmp/x.json
    // dumps JSON timings on exit; without it every `mark` is a no-op.
    // `shots` is the harness that measures the cold-start warmup win.
    qos_startup_metrics::init();
    let _startup_dump = qos_startup_metrics::ScopedDumper;
    qos_startup_metrics::mark("main-start");

    // cosmic-text + Vello warmup on a background thread, overlapping argv
    // parsing / driver bring-up. FontSystem-only by default (~90 ms
    // cold-start win, measured on this harness); Vello scratch-render
    // opt-in via QOS_STARTUP_WARM_VELLO=1. Disabled with QOS_STARTUP_WARM=off.
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
        return Ok(());
    }

    if args.subcommand.as_deref() == Some("check") {
        let Some(baseline) = args.baseline.as_ref() else {
            // `Args::validate` rejects the `check` subcommand without `--baseline`.
            return Ok(());
        };
        let exit_code = run_check(&args, baseline);
        std::process::exit(exit_code);
    }

    run_capture(&args)
}

/// Capture the configured scenes / scenarios to disk. Honours `--sweep`
/// (multi-resolution) and `--per-frame` (animation step expansion).
fn run_capture(args: &Args) -> Result<(), String> {
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
        run_capture_one(args, *vp, &out)?;
    }
    Ok(())
}

fn run_capture_one(args: &Args, viewport: (u32, u32), out: &std::path::Path) -> Result<(), String> {
    match args.mode {
        Mode::Offscreen => match args.scenario.as_deref() {
            Some(name) => {
                let scenario = require_scenario(name);
                let mut driver = ShellDriver::new(viewport);
                mode::offscreen::run_scenario(&mut driver, &scenario, out, args.per_frame)?;
            }
            None => mode::offscreen::run_scenes(&args.scenes, out, viewport)?,
        },
        Mode::Headless => {
            let Some(name) = args.scenario.as_deref() else {
                // `Args::validate` requires `--scenario` for headless mode.
                return Ok(());
            };
            let scenario = require_scenario(name);
            let mut driver = ShellDriver::new(viewport);
            mode::headless::run_scenario(&mut driver, &scenario, out, args.per_frame)?;
        }
        Mode::Recording => {
            let Some(name) = args.scenario.as_deref() else {
                // `Args::validate` requires `--scenario` for recording mode.
                return Ok(());
            };
            let scenario = require_scenario(name);
            let mut driver = ShellDriver::new(viewport);
            mode::recording::run_scenario(
                &mut driver,
                &scenario,
                out,
                args.fps,
                args.step_duration,
            )?;
        }
        Mode::Window => {
            let pre_roll = args.scenario.as_deref().map(require_scenario);
            mode::window::run(pre_roll.as_ref());
        }
    }
    Ok(())
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
    if let Err(e) = run_capture(args) {
        eprintln!("shots check: capture failed: {e}");
        return 2;
    }
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
                        ok_count = ok_count.saturating_add(1);
                        println!("OK   {}", entry.path.display());
                    }
                    diff::DiffKind::Diff(px) => {
                        // Tolerate `max_diff_pixels` per-image — the
                        // topbar clock and the system monitor read live
                        // data, so a small per-frame delta is the
                        // expected steady state.
                        if px <= args.max_diff_pixels {
                            ok_count = ok_count.saturating_add(1);
                            println!("OK   {} ({} px within tolerance)", entry.path.display(), px);
                        } else {
                            any_diff = true;
                            diff_count = diff_count.saturating_add(1);
                            println!("DIFF {} {} pixels differ", entry.path.display(), px);
                        }
                    }
                    diff::DiffKind::Missing => {
                        any_diff = true;
                        diff_count = diff_count.saturating_add(1);
                        println!("MISS {} (no current capture)", entry.path.display());
                    }
                    diff::DiffKind::SizeMismatch { baseline, current } => {
                        any_diff = true;
                        diff_count = diff_count.saturating_add(1);
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
                        diff_count = diff_count.saturating_add(1);
                        println!("READ {} {}", entry.path.display(), msg);
                    }
                }
            }
            println!("shots check: {ok_count} OK, {diff_count} diff(s)");
            i32::from(any_diff)
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
}
