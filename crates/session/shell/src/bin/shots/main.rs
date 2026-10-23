//! `shots` — four-mode driver for the tilo shell (`window`, `offscreen`,
//! `headless`, `recording`), plus a `check` subcommand that captures and
//! pixel-diffs against baseline PNGs. Full flag reference in `--help`.
//!
//! Invoked without `--mode` / `--scenario`, behaves like the original
//! `shots` binary — renders the legacy scene set to PNG via offscreen mode.

mod diff;
mod driver;
mod mode;
mod own_device;
#[cfg(feature = "test-support")]
mod own_process;
mod scenario;

use std::path::PathBuf;

use driver::ShellDriver;
use scenario::{Scenario, catalogue, lookup};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Window,
    Offscreen,
    Headless,
    /// Video recording. Renders every scenario step as one or more
    /// encoded frames into a single `.mp4` file.
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
    /// Frame rate hint for the video encoder AND the number of frames
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
    /// Multi-output arrangement from `--outputs`. When set, the viewport
    /// is the arrangement's combined extent and strips resolve window
    /// widths against each output, exactly as the live compositor does.
    outputs: Option<Vec<shell::compositor::backend::Output>>,
    /// Maximum number of differing pixels per image before a file is
    /// reported as DIFF in `check` mode. Lets CI tolerate the small
    /// number of pixels that flicker between renders because of live
    /// data (the topbar clock reads `chrono::Local::now()`, the system
    /// monitor reads CPU usage, etc.). Default 0 — strict.
    max_diff_pixels: u64,
}

/// Consume the next value for `flag` from the remaining arguments, erroring
/// if there isn't one.
fn take_value(rest: &mut Vec<String>, flag: &str) -> Result<String, String> {
    let v = rest
        .first()
        .ok_or_else(|| format!("{flag} needs a value"))?
        .clone();
    rest.remove(0);
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
    rest: &mut Vec<String>,
    scenes_explicit: &mut bool,
) -> Result<(), String> {
    let v = take_value(rest, "--scenes")?;
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
fn apply_sweep_flag(rest: &mut Vec<String>) -> Result<Vec<(u32, u32)>, String> {
    let next = rest.first().cloned();
    let resolutions: Vec<(u32, u32)> = match next {
        Some(v) if !v.starts_with("--") => {
            rest.remove(0);
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
fn apply_per_frame_flag(rest: &mut Vec<String>) -> Result<u32, String> {
    let v: u32 = take_value(rest, "--per-frame")?
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
            out: PathBuf::from("target/tilo-shots"),
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
            outputs: None,
        };
        let mut scenes_explicit = false;

        let mut rest: Vec<String> = std::env::args().skip(1).collect();
        // Subcommand detection — `shots check ...` runs the baseline
        // diff path. Anything else falls through to the modes.
        if let Some(first) = rest.first()
            && first == "check"
        {
            args.subcommand = Some("check".to_string());
            rest.remove(0);
        }
        while let Some(arg) = rest.first().cloned() {
            rest.remove(0);
            Self::apply_flag(&mut args, &arg, &mut rest, &mut scenes_explicit)?;
        }
        Self::validate(&mut args, scenes_explicit)?;
        Ok(args)
    }

    /// Handle one `--flag [value]` argument, consuming its value(s) from
    /// `rest` and updating `args` in place. Split out of `parse` to keep
    /// the top-level loop short.
    fn apply_flag(
        args: &mut Args,
        arg: &str,
        rest: &mut Vec<String>,
        scenes_explicit: &mut bool,
    ) -> Result<(), String> {
        match arg {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--mode" => args.mode = parse_mode_flag(&take_value(rest, "--mode")?)?,
            "--scenario" => args.scenario = Some(take_value(rest, "--scenario")?),
            "--scenes" => apply_scenes_flag(args, rest, scenes_explicit)?,
            "--out" => args.out = PathBuf::from(take_value(rest, "--out")?),
            "--width" => {
                args.width = take_value(rest, "--width")?
                    .parse()
                    .map_err(|e| format!("--width: {e}"))?;
            }
            "--height" => {
                args.height = take_value(rest, "--height")?
                    .parse()
                    .map_err(|e| format!("--height: {e}"))?;
            }
            "--fps" => {
                args.fps = take_value(rest, "--fps")?
                    .parse()
                    .map_err(|e| format!("--fps: {e}"))?;
            }
            "--step-duration" => {
                args.step_duration = take_value(rest, "--step-duration")?
                    .parse()
                    .map_err(|e| format!("--step-duration: {e}"))?;
            }
            "--outputs" => {
                let value = take_value(rest, "--outputs")?;
                let outputs =
                    shell::compositor::backend::headless::resolve_output_arrangement(&value)
                        .map_err(|e| format!("--outputs: {e}"))?;
                (args.width, args.height) = shell::compositor::backend::combined_geometry(&outputs);
                args.outputs = Some(outputs);
            }
            "--list-scenarios" => args.list_scenarios = true,
            "--theme" => {
                shell::shell::frame::request_shots_theme(take_value(rest, "--theme")?);
            }
            "--sweep" => args.sweep = Some(apply_sweep_flag(rest)?),
            "--per-frame" => args.per_frame = Some(apply_per_frame_flag(rest)?),
            "--baseline" => args.baseline = Some(PathBuf::from(take_value(rest, "--baseline")?)),
            "--diff-threshold" => {
                args.diff_threshold = take_value(rest, "--diff-threshold")?
                    .parse()
                    .map_err(|e| format!("--diff-threshold: {e}"))?;
            }
            "--max-diff-pixels" => {
                args.max_diff_pixels = take_value(rest, "--max-diff-pixels")?
                    .parse()
                    .map_err(|e| format!("--max-diff-pixels: {e}"))?;
            }
            other => return Err(format!("unknown arg '{other}' (try --help)")),
        }
        Ok(())
    }

    /// Cross-flag validation once every argument has been consumed, plus
    /// the mode-specific `out` default (recording mode writes a single
    /// file, not a directory). Split out of `parse` to keep it short.
    fn validate(args: &mut Args, scenes_explicit: bool) -> Result<(), String> {
        if args.width == 0 || args.height == 0 {
            return Err("width and height must be > 0".into());
        }
        if args.fps == 0 {
            return Err("--fps must be > 0".into());
        }
        // Not `<= 0.0`: comparisons against NaN are always false, so that
        // rewrite would silently accept a NaN step duration.
        if args.step_duration.is_nan() || args.step_duration <= 0.0 {
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
            // single file. Switch the default to a `.mp4` path if the
            // caller didn't supply one.
            if args.out == *"target/tilo-shots" {
                args.out = PathBuf::from("target/tilo-shots.mp4");
            }
        }
        if args.outputs.is_some() && args.sweep.is_some() {
            return Err(
                "--outputs fixes the viewport to the arrangement's extent; drop --sweep".into(),
            );
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
        "shots — four-mode driver for the tilo shell\n\
\n\
Usage:\n\
  shots [MODE-FLAGS]                        capture mode (default)\n\
  shots check --baseline DIR [MODE-FLAGS]   capture + diff against DIR\n\
\n\
Modes:\n\
  --mode window     real winit window (interactive)\n\
  --mode offscreen  no window, captures PNGs       [default]\n\
  --mode headless   no GPU, dumps state JSON\n\
  --mode recording  no window, writes one .mp4 file\n\
\n\
Common flags:\n\
  --scenario NAME       replay a built-in scenario\n\
  --scenes A,B,C        [offscreen, no scenario] pick scenes from the legacy catalogue\n\
  --out PATH            output path. Directory for offscreen/headless/check;\n\
                        file for recording (default ./target/tilo-shots for the\n\
                        former, ./target/tilo-shots.mp4 for the latter)\n\
  --width N             viewport width  (default 1600)\n\
  --height N            viewport height (default 900)\n\
  --outputs ARRANGEMENT lay the shell out across several displays instead of\n\
                        one; ARRANGEMENT is a fixture ({fixtures})\n\
                        or a spec like 2560x1600@2,1920x1080@1+2560+0,1920x1080r90.\n\
                        Overrides --width/--height with the combined extent\n\
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
  --theme NAME          dark (default), light or high-contrast; the same as\n\
                        TILO_SHOTS_THEME=NAME\n\
\n\
Scene catalogue (legacy):\n  {scene_list}\n\
\n\
Built-in scenarios:\n{scenarios}\n\
\n\
Examples:\n\
  shots                                                 # offscreen, all scenes\n\
  shots --scenes desktop,palette --out /tmp/s\n\
  TILO_SHOTS_THEME=high-contrast TILO_SHOTS_TEXT_SCALE=1.3 shots \\\n+    --scenes quick-settings,qs-compact,qs-wifi-long-labels,qs-bt-long-labels,qs-vpn-long-labels \\\n+    --sweep 1280x720,960x540 --out /tmp/quick-settings-a11y\n\
  shots --mode offscreen --scenario palette_typing\n\
  shots --mode offscreen --scenes desktop --sweep --out /tmp/sweep\n\
  shots --mode offscreen --scenario overview_transition --per-frame 4\n\
  shots --mode headless  --scenario palette_typing --out target/headless\n\
  shots --mode recording --scenario palette_typing --out /tmp/out.mp4\n\
  shots --mode window    --scenario workspace_switch\n\
  shots check --baseline target/baseline --out target/current\n",
        default_sweep = default_sweep.join(","),
        fixtures = shell::compositor::backend::headless::OUTPUT_FIXTURES
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(", "),
        scene_list = scene_list,
        scenarios = scenario_list.join("\n"),
    );
}

/// This run's own home, config, data, cache, state and runtime directories,
/// removed when the run ends.
struct PrivateHome(std::path::PathBuf);

impl PrivateHome {
    #[allow(
        clippy::disallowed_methods,
        reason = "the whole process is the isolated run; called first in `main`, before any thread starts"
    )]
    fn enter() -> Result<Self, String> {
        use std::os::unix::fs::PermissionsExt as _;
        // A run that ended through `process::exit` left its home behind;
        // take away those whose process is gone.
        if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(pid) = name
                    .to_str()
                    .and_then(|name| name.strip_prefix("tilo-shots-home-"))
                else {
                    continue;
                };
                if !std::path::Path::new("/proc").join(pid).exists() {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
        let root = std::env::temp_dir().join(format!("tilo-shots-home-{}", std::process::id()));
        let runtime = root.join("run");
        for dir in [&root, &runtime] {
            std::fs::create_dir_all(dir)
                .map_err(|error| format!("private home {}: {error}", dir.display()))?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("private home {}: {error}", dir.display()))?;
        }
        for (key, path) in [
            ("HOME", root.clone()),
            ("XDG_CONFIG_HOME", root.join(".config")),
            ("XDG_DATA_HOME", root.join(".local/share")),
            ("XDG_STATE_HOME", root.join(".local/state")),
            ("XDG_CACHE_HOME", root.join(".cache")),
            ("XDG_RUNTIME_DIR", runtime),
        ] {
            // SAFETY: called first thing in `main`, before this process
            // starts any thread, so no other thread reads the environment.
            unsafe {
                std::env::set_var(key, path);
            }
        }
        Ok(Self(root))
    }
}

impl Drop for PrivateHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn main() -> Result<(), String> {
    // Room documents are admitted in automerge isolation workers that
    // re-enter this binary, as they re-enter the shell; serve them first.
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    // A capture shows the state its scenes seed, never the rendering
    // machine's: its recent files, settings, staged display changes, or the
    // live session's services behind `XDG_RUNTIME_DIR` (a projector, a
    // published display snapshot). Isolate them while this process is still
    // single-threaded, before any worker or provider starts.
    let _private_home = PrivateHome::enter()?;
    // Same env-var contract as `shell`: TILO_STARTUP_METRICS_PATH=/tmp/x.json
    // dumps JSON timings on exit; without it every `mark` is a no-op.
    // `shots` is the harness that measures the cold-start warmup win.
    tilo_startup_metrics::init();
    let _startup_dump = tilo_startup_metrics::ScopedDumper;
    tilo_startup_metrics::mark("main-start");

    // cosmic-text + Vello warmup on a background thread, overlapping
    // argument parsing / driver bring-up. FontSystem-only by default (~90 ms
    // cold-start win, measured on this harness); Vello scratch-render
    // opt-in via TILO_STARTUP_WARM_VELLO=1. Disabled with TILO_STARTUP_WARM=off.
    let want_vello = std::env::var("TILO_STARTUP_WARM_VELLO")
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
    let outputs = args.outputs.as_deref();
    let new_driver = || {
        let mut driver = ShellDriver::new(viewport);
        if let Some(outputs) = outputs {
            driver.set_outputs(outputs);
        }
        driver
    };
    match args.mode {
        Mode::Offscreen => match args.scenario.as_deref() {
            Some(name) => {
                let scenario = require_scenario(name);
                let mut driver = new_driver();
                mode::offscreen::run_scenario(&mut driver, &scenario, out, args.per_frame)?;
            }
            None => mode::offscreen::run_scenes(&args.scenes, out, viewport, outputs)?,
        },
        Mode::Headless => {
            let Some(name) = args.scenario.as_deref() else {
                // `Args::validate` requires `--scenario` for headless mode.
                return Ok(());
            };
            let scenario = require_scenario(name);
            let mut driver = new_driver();
            mode::headless::run_scenario(&mut driver, &scenario, out, args.per_frame)?;
        }
        Mode::Recording => {
            let Some(name) = args.scenario.as_deref() else {
                // `Args::validate` requires `--scenario` for recording mode.
                return Ok(());
            };
            let scenario = require_scenario(name);
            let mut driver = new_driver();
            let summary = mode::recording::run_scenario(
                &mut driver,
                &scenario,
                out,
                args.fps,
                args.step_duration,
            )?;
            drop(driver);
            if summary.first_unit.is_none() {
                return Err(format!("recording {} contains no frames", out.display()));
            }
            println!(
                "shots: recorded {} as {}",
                out.display(),
                summary.codec.wire_label()
            );
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
    // `target/tilo-shots`). The diff walker then compares every PNG
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

#[allow(
    clippy::exit,
    reason = "matches Args::parse's own exit(2) on a bad CLI argument in main; \
              propagating this as a Result instead would fall through to main's \
              generic Result-error exit code (1), silently changing the exit code \
              a script checks for an unknown --scenario name"
)]
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
    fn resolution_sweep_flag_parses_rejects_and_defaults() {
        {
            let res = parse_sweep_list("720x480,1366x768").expect("parse ok");
            assert_eq!(res, vec![(720, 480), (1366, 768)]);
            let res = parse_sweep_list(" 800X600 , 1280x720 ").expect("parse ok");
            assert_eq!(res, vec![(800, 600), (1280, 720)]);
        }
        {
            assert!(parse_sweep_list("garbage").is_err());
            assert!(parse_sweep_list("").is_err());
            assert!(parse_sweep_list("0x600").is_err());
            assert!(parse_sweep_list("800x0").is_err());
            assert!(parse_sweep_list("800,600").is_err());
        }
        {
            // Pinned so a future edit that drops 800x600 (the worst-case
            // narrow viewport regression catcher) is caught by CI.
            assert!(DEFAULT_SWEEP.contains(&(800, 600)));
            assert!(DEFAULT_SWEEP.contains(&(1920, 1080)));
        }
    }
}
