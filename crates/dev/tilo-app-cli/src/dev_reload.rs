//! The in-place reload loop's fast path, and where each reload spends its time.
//!
//! Cargo builds the reload library once, with `tilo-app` as its rustc wrapper
//! so the exact rustc command is recorded. An edit that touches only files that
//! rustc read (its dep-info) is rebuilt by running that command again: Cargo's
//! startup, resolution and fingerprint walk over every unit the toolkit brings
//! in are skipped. Anything else (a manifest, a build script, the toolkit, a
//! new file, a failure that is not in the app's own code) goes through Cargo.
//!
//! On Linux the fast path also links without the Rust dylibs the running app
//! already has loaded. Reading the toolkit dylib's million exported symbols was
//! most of each link; the reloaded library resolves them at `dlopen` instead.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use notify::Event;

use super::{DEV_STOP_REQUESTED, DevCandidate, newer_source_during_build, terminate_build_child};

/// Set on the reload library's Cargo build: where the wrapper writes the
/// library's rustc command.
const CAPTURE_ENV: &str = "TILO_DEV_RUSTC_CAPTURE";
/// The crate whose command is recorded. Every other rustc passes through.
const CAPTURE_CRATE_ENV: &str = "TILO_DEV_RUSTC_CRATE";
const CAPTURE_FILE: &str = "rustc-command.json";
/// Appended to a staged library's path for the file the app writes once that
/// library's first frame is presented.
pub(crate) const APP_TIMINGS_SUFFIX: &str = ".timings.json";
/// Set on a fast rebuild's rustc, which runs `tilo-app` as its linker: the
/// linker to hand the link to.
const LINKER_ENV: &str = "TILO_DEV_LINKER";
/// Directories whose Rust dylibs the link leaves out, as a search path.
const LINKER_SKIP_ENV: &str = "TILO_DEV_LINKER_SKIP";
/// Variables that describe the Cargo process rather than the compilation.
/// The jobserver descriptors in particular do not exist outside Cargo.
const CARGO_ONLY_ENV: [&str; 8] = [
    CAPTURE_ENV,
    CAPTURE_CRATE_ENV,
    LINKER_ENV,
    LINKER_SKIP_ENV,
    "CARGO_MAKEFLAGS",
    "MAKEFLAGS",
    "MFLAGS",
    "RUSTC_WRAPPER",
];

/// When `tilo-app` runs as the reload library's rustc wrapper or linker, do
/// that job and exit. Returns only when this process is neither.
pub fn run_as_build_tool_if_requested() {
    if let Some(linker) = std::env::var_os(LINKER_ENV) {
        run_as_linker(&linker);
    }
    let Some(capture) = std::env::var_os(CAPTURE_ENV) else {
        return;
    };
    let mut args = std::env::args_os().skip(1);
    let Some(rustc) = args.next() else {
        return;
    };
    let args: Vec<OsString> = args.collect();
    let wanted = std::env::var(CAPTURE_CRATE_ENV).unwrap_or_default();
    if is_reload_library(&args, &wanted)
        && let Some(command) = RustcCommand::current(&rustc, &args)
    {
        // A failed write only loses the fast path for the next edit.
        let _ = command.write(Path::new(&capture));
    }
    let mut rustc_command = ProcessCommand::new(&rustc);
    rustc_command
        .args(&args)
        .env_remove(CAPTURE_ENV)
        .env_remove(CAPTURE_CRATE_ENV);
    become_command(rustc_command, &rustc);
}

fn run_as_linker(linker: &OsString) -> ! {
    let skipped: Vec<PathBuf> = std::env::var_os(LINKER_SKIP_ENV)
        .map(|dirs| std::env::split_paths(&dirs).collect())
        .unwrap_or_default();
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut command = ProcessCommand::new(linker);
    command
        .args(link_args_without_dylibs(&args, &skipped))
        .env_remove(LINKER_ENV)
        .env_remove(LINKER_SKIP_ENV);
    become_command(command, linker)
}

/// `args` without the Rust dylibs in `dirs`. The library being written
/// lives there too, so the operand of `-o` stays.
fn link_args_without_dylibs<'a>(
    args: &'a [OsString],
    dirs: &'a [PathBuf],
) -> impl Iterator<Item = &'a OsString> {
    args.iter()
        .enumerate()
        .filter(move |(index, arg)| {
            let path = Path::new(arg);
            let output = index
                .checked_sub(1)
                .and_then(|before| args.get(before))
                .is_some_and(|before| before == "-o");
            output
                || !(path.extension().is_some_and(|extension| extension == "so")
                    && path
                        .parent()
                        .is_some_and(|parent| dirs.iter().any(|dir| dir == parent)))
        })
        .map(|(_, arg)| arg)
}

#[expect(
    clippy::exit,
    reason = "a wrapper that cannot start the real tool must fail the way that tool would"
)]
fn become_command(mut command: ProcessCommand, program: &OsString) -> ! {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let error = command.exec();
        eprintln!(
            "tilo-app: cannot run {}: {error}",
            program.to_string_lossy()
        );
        std::process::exit(127);
    }
    #[cfg(not(unix))]
    {
        let status = command.status().map_or_else(
            |error| {
                eprintln!(
                    "tilo-app: cannot run {}: {error}",
                    program.to_string_lossy()
                );
                127
            },
            |status| status.code().unwrap_or(1),
        );
        std::process::exit(status);
    }
}

fn is_reload_library(args: &[OsString], crate_name: &str) -> bool {
    let pairs = || args.windows(2);
    !crate_name.is_empty()
        && pairs().any(
            |pair| matches!(pair, [flag, name] if flag == "--crate-name" && name == crate_name),
        )
        && pairs()
            .any(|pair| matches!(pair, [flag, kind] if flag == "--crate-type" && kind == "cdylib"))
}

/// The environment the reload library's Cargo build gives `tilo-app` as its
/// rustc wrapper, for a library crate named `crate_name`.
pub(crate) fn capture_env(session: &Path, crate_name: &str) -> Vec<(OsString, OsString)> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    // A command recorded before this build may describe other flags.
    let _ = std::fs::remove_file(session.join(CAPTURE_FILE));
    vec![
        (OsString::from("RUSTC_WRAPPER"), exe.into_os_string()),
        (
            OsString::from(CAPTURE_ENV),
            session.join(CAPTURE_FILE).into_os_string(),
        ),
        (
            OsString::from(CAPTURE_CRATE_ENV),
            OsString::from(crate_name),
        ),
    ]
}

/// One recorded rustc invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustcCommand {
    program: PathBuf,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
}

impl RustcCommand {
    fn current(rustc: &OsString, args: &[OsString]) -> Option<Self> {
        let args = args
            .iter()
            .map(|arg| arg.to_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        let env = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .filter(|(key, _)| !CARGO_ONLY_ENV.contains(&key.as_str()))
            .collect();
        Some(Self {
            program: PathBuf::from(rustc),
            args,
            env,
            cwd: std::env::current_dir().ok()?,
        })
    }

    fn write(&self, path: &Path) -> Result<()> {
        let document = serde_json::json!({
            "program": self.program,
            "args": self.args,
            "env": self.env,
            "cwd": self.cwd,
        });
        let partial = path.with_extension("partial");
        std::fs::write(&partial, serde_json::to_vec(&document)?)?;
        std::fs::rename(&partial, path)?;
        Ok(())
    }

    fn read(path: &Path) -> Option<Self> {
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        let text = |value: &serde_json::Value| value.as_str().map(str::to_owned);
        Some(Self {
            program: PathBuf::from(text(document.get("program")?)?),
            args: document
                .get("args")?
                .as_array()?
                .iter()
                .map(text)
                .collect::<Option<_>>()?,
            env: document
                .get("env")?
                .as_object()?
                .iter()
                .map(|(key, value)| Some((key.clone(), text(value)?)))
                .collect::<Option<_>>()?,
            cwd: PathBuf::from(text(document.get("cwd")?)?),
        })
    }

    fn value_of(&self, flag: &str) -> Option<&str> {
        self.args.windows(2).find_map(|pair| match pair {
            [name, value] if name == flag => Some(value.as_str()),
            _ => None,
        })
    }

    fn codegen_option(&self, name: &str) -> Option<&str> {
        let prefix = format!("{name}=");
        self.args.iter().enumerate().find_map(|(index, arg)| {
            let option = arg
                .strip_prefix("-C")
                .filter(|rest| !rest.is_empty())
                .or_else(|| {
                    (arg == "-C")
                        .then(|| self.args.get(index.checked_add(1)?))
                        .flatten()
                        .map(String::as_str)
                })?;
            option.strip_prefix(&prefix)
        })
    }

    /// The `-L dependency=` directories Cargo put its builds in.
    fn dependency_dirs(&self) -> Vec<PathBuf> {
        self.args
            .windows(2)
            .filter_map(|pair| match pair {
                [flag, value] if flag == "-L" => value.strip_prefix("dependency="),
                _ => None,
            })
            .map(|dir| self.cwd.join(dir))
            .collect()
    }

    /// `<out-dir>/<crate><extra-filename>.d`, which rustc rewrites on every run.
    fn dep_info(&self) -> Option<PathBuf> {
        let out_dir = self.cwd.join(self.value_of("--out-dir")?);
        let name = self.value_of("--crate-name")?;
        let extra = self.codegen_option("extra-filename").unwrap_or_default();
        Some(out_dir.join(format!("{name}{extra}.d")))
    }

    fn library(&self) -> Option<PathBuf> {
        let out_dir = self.cwd.join(self.value_of("--out-dir")?);
        let name = self.value_of("--crate-name")?;
        let extra = self.codegen_option("extra-filename").unwrap_or_default();
        Some(out_dir.join(format!(
            "{}{name}{extra}{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        )))
    }
}

/// A recorded library command and the files its last run read.
#[derive(Debug)]
pub(crate) struct FastRebuild {
    command: RustcCommand,
    sources: HashSet<PathBuf>,
}

impl FastRebuild {
    /// The command recorded in `session`, if its dep-info can be read.
    pub(crate) fn load(session: &Path) -> Option<Self> {
        let command = RustcCommand::read(&session.join(CAPTURE_FILE))?;
        let text = std::fs::read_to_string(command.dep_info()?).ok()?;
        let sources = dep_info_sources(&text, &command.cwd);
        (!sources.is_empty()).then_some(Self { command, sources })
    }

    /// Whether rustc alone sees every change in `changed`: each one is a
    /// file it read last time. A new file, a manifest, a build script or a
    /// toolkit source needs Cargo.
    pub(crate) fn covers(&self, changed: &[&Path]) -> bool {
        !changed.is_empty()
            && changed.iter().all(|path| {
                let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                self.sources.contains(&path)
            })
    }

    fn command(&self) -> ProcessCommand {
        let mut command = ProcessCommand::new(&self.command.program);
        command
            .args(&self.command.args)
            .env_clear()
            .envs(&self.command.env)
            .current_dir(&self.command.cwd);
        #[cfg(target_os = "linux")]
        self.link_against_loaded_dylibs(&mut command);
        command
    }

    /// Run `tilo-app` as the linker so the link leaves out Cargo's Rust
    /// dylibs. The host process loaded them with its own binary, so an ELF
    /// library resolves their symbols from there when it is opened.
    #[cfg(target_os = "linux")]
    fn link_against_loaded_dylibs(&self, command: &mut ProcessCommand) {
        let (Ok(exe), Ok(dirs)) = (
            std::env::current_exe(),
            std::env::join_paths(self.command.dependency_dirs()),
        ) else {
            return;
        };
        let linker = self.command.codegen_option("linker").unwrap_or("cc");
        let mut option = OsString::from("linker=");
        option.push(exe);
        command
            .arg("-C")
            .arg(option)
            .env(LINKER_ENV, linker)
            .env(LINKER_SKIP_ENV, dirs);
    }
}

/// Every prerequisite in a Makefile-style dep-info file, resolved against
/// `cwd` the way rustc wrote them.
fn dep_info_sources(text: &str, cwd: &Path) -> HashSet<PathBuf> {
    let mut sources = HashSet::new();
    for line in text.lines() {
        if line.starts_with('#') {
            continue;
        }
        // rustc lists each input again as an empty rule, `path:`.
        let Some(path) = line.strip_suffix(':') else {
            continue;
        };
        let path = cwd.join(path.replace("\\ ", " "));
        sources.insert(path.canonicalize().unwrap_or(path));
    }
    sources
}

/// Result of a fast rebuild.
pub(crate) enum FastOutcome {
    /// The candidate, or a newer edit that cancelled it.
    Done(DevCandidate),
    /// rustc reported errors in the app's own code; Cargo would report the same.
    Failed(anyhow::Error),
    /// Something outside the app's code went wrong. Cargo decides.
    Fallback(String),
}

/// Rerun the recorded rustc command, stopping it for a newer edit.
pub(crate) fn rebuild_with_rustc(
    fast: &FastRebuild,
    events: &mpsc::Receiver<notify::Result<Event>>,
    roots: &[PathBuf],
    mut observe: impl FnMut(&Event),
    timings: &mut Option<ReloadTimings>,
) -> FastOutcome {
    let mut command = fast.command();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let spawned = SystemTime::now();
    let mut rustc = match command.spawn() {
        Ok(child) => child,
        Err(error) => return FastOutcome::Fallback(format!("cannot start rustc: {error}")),
    };
    let Some(stderr) = rustc.stderr.take() else {
        terminate_build_child(&mut rustc);
        return FastOutcome::Fallback("rustc has no diagnostic stream".to_owned());
    };
    let (line_tx, line_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut report = RustcReport::default();
    let mut linking = None;
    loop {
        if DEV_STOP_REQUESTED.load(Ordering::Relaxed) {
            terminate_build_child(&mut rustc);
            return FastOutcome::Failed(anyhow::anyhow!("development stopped"));
        }
        if newer_source_during_build(events, roots, &mut observe) {
            terminate_build_child(&mut rustc);
            return FastOutcome::Done(DevCandidate::Superseded);
        }
        while let Ok(Ok(line)) = line_rx.try_recv() {
            report.line(&line, &fast.sources, &fast.command.cwd);
        }
        if linking.is_none() && timings.is_some() && has_children(rustc.id()) {
            linking = Some(SystemTime::now());
        }
        match rustc.try_wait() {
            Ok(Some(status)) => {
                while let Ok(Ok(line)) = line_rx.recv() {
                    report.line(&line, &fast.sources, &fast.command.cwd);
                }
                let exited = SystemTime::now();
                if let Some(timings) = timings.as_mut() {
                    timings.path = "rustc";
                    timings.set("cargo", 0.0);
                    let linked = linking.unwrap_or(exited);
                    timings.set("compile", ms_between(spawned, linked));
                    timings.set("link", ms_between(linked, exited));
                }
                if status.success() {
                    let library = report
                        .library
                        .clone()
                        .or_else(|| fast.command.library())
                        .filter(|path| written_since(path, spawned));
                    return match library {
                        Some(library) => FastOutcome::Done(DevCandidate::Built(library)),
                        None => FastOutcome::Fallback("rustc wrote no reload library".to_owned()),
                    };
                }
                if report.errors_in_app_code {
                    return FastOutcome::Failed(anyhow::anyhow!("rustc failed with {status}"));
                }
                return FastOutcome::Fallback(format!("rustc failed with {status}"));
            }
            Ok(None) => {}
            Err(error) => {
                terminate_build_child(&mut rustc);
                return FastOutcome::Fallback(format!("waiting for rustc: {error}"));
            }
        }
        std::thread::sleep(Duration::from_millis(if timings.is_some() {
            2
        } else {
            10
        }));
    }
}

/// What a direct rustc run printed.
#[derive(Default)]
struct RustcReport {
    library: Option<PathBuf>,
    /// An error points into a file of the app. An error anywhere else (a
    /// missing dependency, a lock on the incremental directory) is Cargo's.
    errors_in_app_code: bool,
}

impl RustcReport {
    fn line(&mut self, line: &str, sources: &HashSet<PathBuf>, cwd: &Path) {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            eprintln!("{line}");
            return;
        };
        let field = |key: &str| message.get(key).and_then(serde_json::Value::as_str);
        match field("$message_type") {
            Some("artifact") => {
                if field("emit") == Some("link") {
                    self.library = field("artifact").map(PathBuf::from);
                }
            }
            Some("diagnostic") => {
                if let Some(rendered) = field("rendered") {
                    eprint!("{rendered}");
                }
                if field("level") == Some("error") {
                    let spans = message.get("spans").and_then(serde_json::Value::as_array);
                    self.errors_in_app_code |= spans.into_iter().flatten().any(|span| {
                        span.get("file_name")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|file| {
                                let path = cwd.join(file);
                                sources.contains(&path.canonicalize().unwrap_or(path))
                            })
                    });
                }
            }
            Some(_) => {}
            None => eprintln!("{line}"),
        }
    }
}

/// Whether `pid` has started a child: rustc's only child is the linker.
fn has_children(pid: u32) -> bool {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return false;
    };
    tasks.flatten().any(|task| {
        std::fs::read_to_string(task.path().join("children"))
            .is_ok_and(|children| !children.trim().is_empty())
    })
}

/// Whether `path` was written at or after `since`: a library an earlier build
/// left behind is not this build's.
fn written_since(path: &Path, since: SystemTime) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.modified().is_ok_and(|modified| modified >= since)
    })
}

/// Milliseconds from `from` to `to`, zero when the clock stepped back.
fn ms_between(from: SystemTime, to: SystemTime) -> f64 {
    to.duration_since(from)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn unix_ms(at: SystemTime) -> f64 {
    ms_between(SystemTime::UNIX_EPOCH, at)
}

/// The phases a reload reports, in order.
pub(crate) const PHASES: [&str; 11] = [
    "debounce",
    "cargo",
    "compile",
    "link",
    "stage",
    "signal",
    "checkpoint",
    "dlopen",
    "state_handoff",
    "first_frame",
    "present",
];

/// One reload, from the change the watcher saw to the first presented frame.
#[derive(Debug, Clone)]
pub(crate) struct ReloadTimings {
    /// `rustc` for the fast path, `cargo` otherwise.
    pub path: &'static str,
    pub changed: SystemTime,
    pub phases: HashMap<&'static str, f64>,
    library: Option<PathBuf>,
    signalled: Option<SystemTime>,
    presented: Option<SystemTime>,
}

impl ReloadTimings {
    pub(crate) fn new(changed: SystemTime) -> Self {
        Self {
            path: "cargo",
            changed,
            phases: HashMap::new(),
            library: None,
            signalled: None,
            presented: None,
        }
    }

    pub(crate) fn set(&mut self, phase: &'static str, ms: f64) {
        self.phases.insert(phase, ms);
    }

    pub(crate) fn add(&mut self, phase: &'static str, ms: f64) {
        *self.phases.entry(phase).or_default() += ms;
    }

    pub(crate) fn staged(&mut self, library: PathBuf) {
        self.library = Some(library);
    }

    pub(crate) fn signalled(&mut self) {
        self.signalled = Some(SystemTime::now());
    }

    /// Fold in the app's own line for this library. Returns false until the
    /// app has written it.
    fn join_app(&mut self) -> bool {
        let (Some(library), Some(signalled)) = (&self.library, self.signalled) else {
            return false;
        };
        let mut path = library.as_os_str().to_os_string();
        path.push(APP_TIMINGS_SUFFIX);
        let Some(line) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        else {
            return false;
        };
        let _ = std::fs::remove_file(&path);
        let number = |key: &str| line.get(key).and_then(serde_json::Value::as_f64);
        let (Some(received), Some(submitted), Some(presented)) = (
            number("received_unix_ms"),
            number("submitted_unix_ms"),
            number("presented_unix_ms"),
        ) else {
            return false;
        };
        let checkpoint = number("checkpoint_ms").unwrap_or_default();
        let load = number("load_ms").unwrap_or_default();
        let handoff =
            number("handoff_ms").unwrap_or_default() + number("install_ms").unwrap_or_default();
        self.set("signal", (received - unix_ms(signalled)).max(0.0));
        self.set("checkpoint", checkpoint);
        self.set("dlopen", load);
        self.set("state_handoff", handoff);
        let installed = received + checkpoint + load + handoff;
        self.set("first_frame", (submitted - installed).max(0.0));
        self.set("present", (presented - submitted).max(0.0));
        self.presented = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs_f64(presented.max(0.0) / 1000.0));
        true
    }

    fn total_ms(&self) -> Option<f64> {
        self.presented
            .map(|presented| ms_between(self.changed, presented))
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        let phases = PHASES
            .iter()
            .filter_map(|phase| {
                self.phases
                    .get(phase)
                    .map(|ms| ((*phase).to_owned(), serde_json::json!(round(*ms))))
            })
            .collect::<serde_json::Map<_, _>>();
        serde_json::json!({
            "event": "reload",
            "path": self.path,
            "changed_unix_ms": round(unix_ms(self.changed)),
            "first_frame_unix_ms": self.presented.map(|at| round(unix_ms(at))),
            "total_ms": self.total_ms().map(round),
            "phases_ms": phases,
        })
    }

    fn summary(&self) -> String {
        let phases = PHASES
            .iter()
            .filter_map(|phase| {
                self.phases
                    .get(phase)
                    .map(|ms| format!("{} {ms:.0}", phase.replace('_', " ")))
            })
            .collect::<Vec<_>>()
            .join(" · ");
        match self.total_ms() {
            Some(total) => format!("reload {total:.0} ms ({}): {phases}", self.path),
            None => format!("reload without a presented frame ({}): {phases}", self.path),
        }
    }
}

fn round(ms: f64) -> f64 {
    (ms * 10.0).round() / 10.0
}

/// Per-session state of the in-place loop: which edits Cargo has not seen,
/// and the reloads `--timings` is still waiting on.
pub(crate) struct DevLoop {
    session: PathBuf,
    timings_enabled: bool,
    /// Sources as they were when Cargo last compiled the reload library.
    pub baseline: HashMap<PathBuf, (SystemTime, u64)>,
    /// Set when the watcher saw a change; taken by the build it starts.
    pub changed_at: Option<SystemTime>,
    /// Bumped for every source change, so a build can tell it went stale.
    pub source_revision: Arc<AtomicU64>,
    waiting: Vec<(ReloadTimings, SystemTime)>,
    pub reports: Vec<serde_json::Value>,
}

/// How long a reload waits for the app to report its first frame.
const APP_REPORT_DEADLINE: Duration = Duration::from_secs(30);

impl DevLoop {
    pub(crate) fn new(
        session: PathBuf,
        timings_enabled: bool,
        source_revision: Arc<AtomicU64>,
    ) -> Self {
        Self {
            session,
            timings_enabled,
            baseline: HashMap::new(),
            changed_at: None,
            source_revision,
            waiting: Vec::new(),
            reports: Vec::new(),
        }
    }

    pub(crate) fn session(&self) -> &Path {
        &self.session
    }

    /// A timer for the build that is about to start, when `--timings` is on.
    pub(crate) fn start(&mut self) -> Option<ReloadTimings> {
        let changed = self.changed_at.take()?;
        self.timings_enabled.then(|| ReloadTimings::new(changed))
    }

    /// Wait for the app's side of a reload that was just signalled.
    pub(crate) fn signalled(&mut self, mut timings: ReloadTimings) {
        timings.signalled();
        self.waiting.push((timings, SystemTime::now()));
    }

    /// Report every signalled reload the app has finished, or given up on.
    pub(crate) fn poll(&mut self) {
        let now = SystemTime::now();
        let waiting = std::mem::take(&mut self.waiting);
        for (mut timings, since) in waiting {
            let joined = timings.join_app();
            let expired = now
                .duration_since(since)
                .is_ok_and(|waited| waited > APP_REPORT_DEADLINE);
            if joined || expired {
                self.report(&timings);
            } else {
                self.waiting.push((timings, since));
            }
        }
    }

    /// Report a reload that never reached the app, such as a failed build.
    pub(crate) fn report(&mut self, timings: &ReloadTimings) {
        eprintln!("[tilo-app] {}", timings.summary());
        self.reports.push(timings.to_json());
    }

    /// Wait out reloads the app has not reported yet, at most `limit`.
    pub(crate) fn finish(&mut self, limit: Duration) {
        let started = std::time::Instant::now();
        while !self.waiting.is_empty() && started.elapsed() < limit {
            self.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        for (timings, _) in std::mem::take(&mut self.waiting) {
            self.report(&timings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_info_lists_every_input_rustc_read() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(temp.path().join("src/a b.rs"), "").unwrap();
        let text = "/t/deps/libapp-1.so: src/lib.rs src/a\\ b.rs\n\n/t/deps/app-1.d: src/lib.rs src/a\\ b.rs\n\nsrc/lib.rs:\nsrc/a\\ b.rs:\n\n# env-dep:CARGO_PKG_NAME=app\n";
        let sources = dep_info_sources(text, temp.path());
        let root = temp.path().canonicalize().unwrap();
        assert_eq!(
            sources,
            HashSet::from([root.join("src/lib.rs"), root.join("src/a b.rs")])
        );
    }

    #[test]
    fn the_link_leaves_out_dependency_dylibs_but_not_its_output() {
        let deps = PathBuf::from("/t/debug/deps");
        let args: Vec<OsString> = [
            "-m64",
            "/t/debug/deps/app.app.cgu.0.rcgu.o",
            "/t/debug/deps/libruntime_dylib.so",
            "/sysroot/lib/libstd-1.so",
            "-o",
            "/t/debug/deps/libapp.so",
            "-Wl,--gc-sections",
        ]
        .map(OsString::from)
        .to_vec();
        let kept: Vec<&str> = link_args_without_dylibs(&args, std::slice::from_ref(&deps))
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(
            kept,
            [
                "-m64",
                "/t/debug/deps/app.app.cgu.0.rcgu.o",
                "/sysroot/lib/libstd-1.so",
                "-o",
                "/t/debug/deps/libapp.so",
                "-Wl,--gc-sections",
            ]
        );
    }

    #[test]
    fn only_edits_rustc_read_skip_cargo() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for file in ["src/lib.rs", "src/new.rs", "Cargo.toml"] {
            std::fs::write(root.join(file), "").unwrap();
        }
        let fast = FastRebuild {
            command: RustcCommand {
                program: PathBuf::from("rustc"),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: root.clone(),
            },
            sources: HashSet::from([root.join("src/lib.rs")]),
        };
        assert!(fast.covers(&[&root.join("src/lib.rs")]));
        assert!(
            !fast.covers(&[]),
            "nothing changed is not a reason to build"
        );
        assert!(!fast.covers(&[&root.join("src/lib.rs"), &root.join("Cargo.toml")]));
        assert!(
            !fast.covers(&[&root.join("src/new.rs")]),
            "a new module needs Cargo"
        );
    }

    #[test]
    fn the_recorded_command_names_its_dep_info_and_library() {
        let command = RustcCommand {
            program: PathBuf::from("rustc"),
            args: [
                "--crate-name",
                "notes",
                "--crate-type",
                "cdylib",
                "--out-dir",
                "/t/debug/deps",
                "-C",
                "extra-filename=-0123",
                "-Cincremental=/t/inc",
            ]
            .map(str::to_owned)
            .to_vec(),
            env: BTreeMap::new(),
            cwd: PathBuf::from("/apps/notes"),
        };
        assert_eq!(
            command.dep_info().unwrap(),
            Path::new("/t/debug/deps/notes-0123.d")
        );
        assert_eq!(
            command.library().unwrap(),
            Path::new(&format!(
                "/t/debug/deps/{}notes-0123{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ))
        );
        assert_eq!(command.codegen_option("incremental"), Some("/t/inc"));
        assert!(is_reload_library(
            &command.args.iter().map(OsString::from).collect::<Vec<_>>(),
            "notes"
        ));
        assert!(!is_reload_library(
            &command.args.iter().map(OsString::from).collect::<Vec<_>>(),
            "other"
        ));
    }
}
