//! Summarises private Tilo session flight records without controlling the
//! desktop.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::ExitCode;

use tilo_session_record::report::{self, RenderBudgets};

const USAGE: &str = "usage: tilo-session-report [-h] [--root ROOT] [--previous] [--output OUTPUT] \
[--render-gate] [--require-direct] [--key-latency] [--render-budget-ms RENDER_BUDGET_MS] \
[--frame-work-budget-ms FRAME_WORK_BUDGET_MS] [--key-budget-ms KEY_BUDGET_MS] \
[--min-damage-limited-percent PERCENT]";

const HELP: &str = "
Summarise private Tilo session flight records without controlling the desktop.

options:
  -h, --help            show this help message and exit
  --root ROOT
  --previous            report the prior login
  --output OUTPUT       write private JSON instead of stdout
  --render-gate         exit 1 unless presentation meets the render-path contract and budgets
  --require-direct      with --render-gate: direct GPU scanout must present even if it never started
  --key-latency         print key-to-present latency of typing, per app (from KEY-LATENCY records)
  --render-budget-ms RENDER_BUDGET_MS
  --frame-work-budget-ms FRAME_WORK_BUDGET_MS
  --key-budget-ms KEY_BUDGET_MS
                        with --render-gate: typed keys show within this at the median, per app
  --min-damage-limited-percent PERCENT
                        with --render-gate: this share of direct frames repairs only its damage
                        or scans a client out";

#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per command-line flag"
)]
struct Options {
    root: PathBuf,
    previous: bool,
    output: Option<PathBuf>,
    render_gate: bool,
    require_direct: bool,
    key_latency: bool,
    render_budget_ms: i64,
    frame_work_budget_ms: i64,
    key_budget_ms: Option<i64>,
    min_damage_limited_percent: Option<i64>,
}

fn default_root() -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME").map_or_else(
        || {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".local/state")
        },
        PathBuf::from,
    );
    state.join("tilo/session-logs")
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("{USAGE}\ntilo-session-report: error: {message}");
    ExitCode::from(2)
}

fn parse(args: &[String]) -> Result<Options, ExitCode> {
    let mut options = Options {
        root: default_root(),
        previous: false,
        output: None,
        render_gate: false,
        require_direct: false,
        key_latency: false,
        render_budget_ms: 33,
        frame_work_budget_ms: 16,
        key_budget_ms: None,
        min_damage_limited_percent: None,
    };
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |flag: &str| {
            inline
                .clone()
                .or_else(|| rest.next().cloned())
                .ok_or_else(|| usage_error(&format!("argument {flag}: expected one argument")))
        };
        let number = |flag: &str, text: String| {
            text.trim()
                .parse::<i64>()
                .map_err(|_| usage_error(&format!("argument {flag}: invalid int value: '{text}'")))
        };
        match name {
            "-h" | "--help" => {
                println!("{USAGE}\n{HELP}");
                return Err(ExitCode::SUCCESS);
            }
            "--root" => options.root = PathBuf::from(value("--root")?),
            "--output" => options.output = Some(PathBuf::from(value("--output")?)),
            "--render-budget-ms" => {
                options.render_budget_ms =
                    number("--render-budget-ms", value("--render-budget-ms")?)?;
            }
            "--frame-work-budget-ms" => {
                options.frame_work_budget_ms =
                    number("--frame-work-budget-ms", value("--frame-work-budget-ms")?)?;
            }
            "--key-budget-ms" => {
                options.key_budget_ms = Some(number("--key-budget-ms", value("--key-budget-ms")?)?);
            }
            "--min-damage-limited-percent" => {
                options.min_damage_limited_percent = Some(number(
                    "--min-damage-limited-percent",
                    value("--min-damage-limited-percent")?,
                )?);
            }
            "--previous" if inline.is_none() => options.previous = true,
            "--render-gate" if inline.is_none() => options.render_gate = true,
            "--require-direct" if inline.is_none() => options.require_direct = true,
            "--key-latency" if inline.is_none() => options.key_latency = true,
            other => return Err(usage_error(&format!("unrecognized arguments: {other}"))),
        }
    }
    Ok(options)
}

/// Writes the report readable by its owner only.
fn write_private(path: &PathBuf, payload: &str) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(payload.as_bytes())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = match parse(&args) {
        Ok(options) => options,
        Err(code) => return code,
    };
    let report = report::session_report(&options.root, options.previous);
    if options.key_latency {
        print!("{}", report::key_latency_table(&report));
        return ExitCode::SUCCESS;
    }
    if options.render_gate {
        let failures = report::render_gate(
            &report,
            RenderBudgets {
                require_direct: options.require_direct,
                render_us: options.render_budget_ms.saturating_mul(1000),
                frame_work_us: options.frame_work_budget_ms.saturating_mul(1000),
                key_to_present_us: options
                    .key_budget_ms
                    .map(|budget| budget.saturating_mul(1000)),
                min_damage_limited_percent: options.min_damage_limited_percent,
            },
        );
        let flight = report.get("flight");
        let frames = report.get("frames");
        let shown = |section: Option<&tilo_session_record::value::Value>, key: &str| {
            section.and_then(|section| section.get(key)).map_or_else(
                || "None".to_owned(),
                tilo_session_record::value::Value::repr,
            )
        };
        println!(
            "render gate: last phase summary {}",
            shown(flight, "render_phase_last")
        );
        println!(
            "render gate: last direct scanout status {}",
            shown(flight, "direct_scanout_status_last")
        );
        println!(
            "render gate: main-thread frame work {}",
            shown(frames, "frame_work_us")
        );
        for failure in &failures {
            eprintln!("render gate: FAIL {failure}");
        }
        if failures.is_empty() {
            println!("render gate: PASS");
            return ExitCode::SUCCESS;
        }
        return ExitCode::FAILURE;
    }
    let payload = format!("{}\n", report.to_json());
    match &options.output {
        Some(path) => {
            if let Err(error) = write_private(path, &payload) {
                eprintln!(
                    "tilo-session-report: cannot write {}: {error}",
                    path.display()
                );
                return ExitCode::FAILURE;
            }
        }
        None => print!("{payload}"),
    }
    ExitCode::SUCCESS
}
