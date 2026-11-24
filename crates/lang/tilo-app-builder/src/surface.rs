//! An app's committed public surface: `surface.toml`.
//!
//! The app's own binary reports what its resource publishes (its verbs, their
//! parameters and kinds, its stored fields) and which of its view's controls
//! are bound to something the resource does not publish
//! (`ui::framework::headless::surface_report`, run as `<app> --surface`).
//! `tilolang surface` commits the surface next to the manifest, where review
//! sees every rename and the OS reads what an app that is not running
//! offers. `tilolang check` and CI compare the committed file with the code:
//! a difference, an incompatible storage change without a version bump, or a
//! control bound to nothing fails; a field or verb without a history message
//! in `locales/en-US.ftl` is reported.
//!
//! The file is the bytes a package ships and a change names by hash, so it
//! keeps only what deciding a change, checking storage and compiling against
//! it need ([`filed`]). The app's read effects are verbs of its own resource
//! here, as they are actions of it for anyone else.
//!
//! Field and parameter types are the language-neutral
//! `tilo_actions::surface::FieldType`, the records and enums they name
//! declared once under `[[resources.types]]`. A file written before that
//! holds each field's Rust type as text: it still parses, receivers read it
//! as before, and `check` counts it as current, so no app's surface is
//! rewritten for the vocabulary alone. Each app's file takes the new form the
//! next time its surface is written (`tilolang surface`), when the app is
//! next built or changed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;

use crate::contract::{ContractFinding, ContractReport, Level};

/// How long an app may take to print its surface report. It is what its code
/// publishes, printed before the app serves a window: an app that has not
/// printed in this long has hung.
const REPORT_WAIT: Duration = Duration::from_secs(120);

/// How long what a command started may keep its output open after the command
/// itself ended, before it is stopped with the rest of its tree: ending is
/// not finishing if a worker it started still holds the pipe.
const OUTPUT_GRACE: Duration = Duration::from_secs(5);

/// How long a build or a whole application set may take, cold: building one
/// app first for its report, or every app in one graph, takes minutes, not
/// tens of minutes.
pub const BUILD_WAIT: Duration = Duration::from_secs(20 * 60);

/// The most a build or a report may print.
const OUTPUT_LIMIT: usize = 64 * 1024 * 1024;

pub const FILE: &str = "surface.toml";

pub const FLAG: &str = "--surface";

const HEADER: &str = "# Written by `tilolang surface`; review changes like an API change.\n\n";

use tilo_resource::surface::file::normalized;
/// The file's format, written at its top. See [`Committed`].
pub use tilo_resource::surface::file::{FORMAT, filed};

/// A committed surface as its app has it: the format the file was written in,
/// and its resources.
#[derive(Clone, Debug)]
pub struct Committed {
    pub format: u64,
    pub resources: Vec<Value>,
}

#[derive(Debug, Default)]
pub struct SurfaceReport {
    pub resources: Vec<Value>,
    /// Controls bound to something the resource does not publish.
    pub unbound: Vec<String>,
    /// Why the view could not be built headless, if it could not.
    pub view: Option<String>,
    pub effects: Vec<String>,
    pub views: Vec<String>,
    /// The ids the framework publishes its own actions under, as the app's
    /// framework reports them.
    pub reserved: Vec<String>,
    /// What the app offers a person while it is closed: its manifest's
    /// `[[offers]]`, as the code publishes them ([`write_offers`]).
    pub offers: Vec<Value>,
}

/// Whether the app at `app_dir` runs through the framework, whose binary
/// answers [`FLAG`]: its entry point is `ui::main!(App)` or
/// `framework::run::<App>()`, not a component definition.
#[must_use]
pub fn is_framework_app(app_dir: &Path) -> bool {
    let Ok(main) = std::fs::read_to_string(app_dir.join("src/main.rs")) else {
        return false;
    };
    let main: String = main.split_whitespace().collect();
    (main.contains("ui::main!(") && !main.contains("ui::main!(definition"))
        || main.contains("framework::run::<")
}

/// Parse what an app's binary printed for [`FLAG`].
pub fn parse_report(output: &str) -> Result<SurfaceReport> {
    let start = output
        .find('{')
        .context("the app printed no surface report")?;
    let report: Value = serde_json::from_str(output.get(start..).unwrap_or_default().trim())
        .context("the app's surface report is not JSON")?;
    let resources = report
        .get("resources")
        .and_then(Value::as_array)
        .context("the surface report names no resources")?
        .iter()
        .map(normalized)
        .collect();
    let strings = |key: &str| -> Vec<String> {
        report
            .get(key)
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let unbound = strings("unbound");
    let view = report
        .get("view")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(SurfaceReport {
        resources,
        unbound,
        view,
        effects: strings("effects"),
        views: strings("views"),
        reserved: strings("reserved"),
        offers: report
            .get("offers")
            .and_then(Value::as_array)
            .map(|offers| offers.iter().map(normalized).collect())
            .unwrap_or_default(),
    })
}

/// A command that ended within its bound, and what it printed on standard
/// output.
pub struct Finished {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
}

/// Run `command` under the bound `wait`, letting its standard error through as
/// it comes and keeping its standard output. `doing` names what is being
/// waited for, so a command that hangs fails saying what it was and how long
/// it was given.
///
/// The bound covers the command and its output: its output ends with it,
/// unless something it started still holds the pipe, which is waited a little
/// longer ([`OUTPUT_GRACE`]) and then stopped. At either bound the command and
/// everything it started are stopped, so nothing this step runs can wait
/// without one.
pub fn run_bounded(mut command: ProcessCommand, wait: Duration, doing: &str) -> Result<Finished> {
    use std::io::{Read as _, Write as _};
    use std::process::Stdio;
    use std::sync::mpsc::{self, RecvTimeoutError};

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    bounded_command::own_process_tree(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("starting {doing}"))?;
    let stdout = child
        .stdout
        .take()
        .with_context(|| format!("{doing} has no standard output"))?;
    let mut stderr = child
        .stderr
        .take()
        .with_context(|| format!("{doing} has no standard error"))?;
    // Each stream is read on its own thread: neither can fill its pipe and
    // stop the command while this waits, nor be missed once it ends.
    let (written, kept) = mpsc::channel();
    let (spoken, said) = mpsc::channel();
    let reading = std::thread::spawn(move || {
        let _ = written.send(read_capped(stdout));
    });
    let echoing = std::thread::spawn(move || {
        let mut chunk = [0; 4096];
        while let Ok(count) = stderr.read(&mut chunk) {
            let Some(bytes) = chunk.get(..count) else {
                break;
            };
            if count == 0 || spoken.send(bytes.to_vec()).is_err() {
                break;
            }
        }
    });

    let deadline = std::time::Instant::now().checked_add(wait);
    let status = loop {
        // What it says belongs in the log as it says it, as an inherited
        // standard error would put it there.
        while let Ok(chunk) = said.try_recv() {
            let _ = std::io::stderr().write_all(&chunk);
        }
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("waiting for {doing}"))?
        {
            break status;
        }
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            bounded_command::terminate_tree(&mut child, Duration::from_secs(1));
            bail!(
                "{doing} did not come within {}s; it and what it started were stopped",
                wait.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let (stdout, over) = match kept.recv_timeout(OUTPUT_GRACE) {
        Ok(output) => output,
        Err(RecvTimeoutError::Timeout) => {
            bounded_command::terminate_tree(&mut child, Duration::from_secs(1));
            let _ = reading.join();
            let _ = echoing.join();
            bail!(
                "{doing} ended but something it started still held its output; it and what it started were stopped"
            );
        }
        Err(RecvTimeoutError::Disconnected) => bail!("{doing}: its output could not be read"),
    };
    let _ = reading.join();
    let _ = echoing.join();
    // Its whole standard error has been said by now.
    while let Ok(chunk) = said.try_recv() {
        let _ = std::io::stderr().write_all(&chunk);
    }
    if over {
        bail!(
            "{doing} printed more than {} MiB; stopped reading it",
            OUTPUT_LIMIT / (1024 * 1024)
        );
    }
    Ok(Finished { status, stdout })
}

/// Up to [`OUTPUT_LIMIT`] bytes of `pipe`, and whether it held more.
fn read_capped(pipe: impl std::io::Read) -> (Vec<u8>, bool) {
    use std::io::Read as _;

    let cap = u64::try_from(OUTPUT_LIMIT).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    let _ = pipe.take(cap.saturating_add(1)).read_to_end(&mut bytes);
    let over = u64::try_from(bytes.len()).unwrap_or(u64::MAX) > cap;
    bytes.truncate(OUTPUT_LIMIT);
    (bytes, over)
}

/// The app's binary, run for its [`FLAG`] report with no network: in a network
/// namespace of its own, so a lookup or a connection fails at once instead of
/// waiting on one that will never be answered.
fn report_command(app_dir: &Path, executable: &Path) -> Result<ProcessCommand> {
    without_network()?;
    let mut command = ProcessCommand::new(executable);
    command.arg(FLAG).current_dir(app_dir);
    Ok(with_network_namespace(command))
}

/// Wrap `command` so it starts in a network namespace of its own: `unshare`
/// with a user namespace mapping this process's ids, as the launcher starts an
/// app it granted no network. Nothing else about the process changes.
#[cfg(target_os = "linux")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "the platform with no namespace to give returns the command itself"
)]
fn with_network_namespace(command: ProcessCommand) -> ProcessCommand {
    let (uid, gid) = (
        rustix::process::getuid().as_raw(),
        rustix::process::getegid().as_raw(),
    );
    let mut wrapped = ProcessCommand::new("unshare");
    wrapped
        .arg("--user")
        .arg(format!("--map-user={uid}"))
        .arg(format!("--map-group={gid}"))
        .arg("--net")
        .arg("--")
        .arg(command.get_program())
        .args(command.get_args());
    if let Some(dir) = command.get_current_dir() {
        wrapped.current_dir(dir);
    }
    wrapped
}

/// A platform with no namespace to give: the report is what the code publishes
/// either way, and only Linux asks for it with none.
#[cfg(not(target_os = "linux"))]
fn with_network_namespace(command: ProcessCommand) -> ProcessCommand {
    command
}

/// Whether this machine gives a command a network namespace of its own, asked
/// once. A machine that cannot is a machine that would read a report without
/// the no-network it is promised, and that is a refusal, not a quiet run.
#[cfg(target_os = "linux")]
fn without_network() -> Result<()> {
    static ASKED: std::sync::OnceLock<std::result::Result<(), String>> = std::sync::OnceLock::new();
    let answer = ASKED.get_or_init(|| {
        let command = with_network_namespace(ProcessCommand::new("true"));
        match run_bounded(
            command,
            Duration::from_secs(60),
            "the empty network namespace a surface report is read in",
        ) {
            Ok(answer) if answer.status.success() => Ok(()),
            Ok(answer) => Err(format!("`unshare` failed with {}", answer.status)),
            Err(error) => Err(format!("{error:#}")),
        }
    });
    answer.clone().map_err(anyhow::Error::msg)
}

#[cfg(not(target_os = "linux"))]
fn without_network() -> Result<()> {
    Ok(())
}

/// Build the app at `app_dir` and answer its executable, so its report can be
/// read in the namespace [`report_command`] gives it rather than through a
/// `cargo run` that would put the build itself there with it.
fn build_app(app_dir: &Path) -> Result<PathBuf> {
    let (package, binary) = app_binary(app_dir)?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = ProcessCommand::new(cargo);
    build
        .current_dir(app_dir)
        .arg("build")
        .arg("--package")
        .arg(&package)
        .arg("--message-format=json-render-diagnostics");
    let doing = format!("the build of {}", app_dir.display());
    let answer = run_bounded(build, BUILD_WAIT, &doing)?;
    ensure!(
        answer.status.success(),
        "{doing} failed with {}",
        answer.status
    );
    for line in String::from_utf8_lossy(&answer.stdout).lines().rev() {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if message.get("reason").and_then(Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let named = message
            .get("target")
            .and_then(|target| target.get("name"))
            .and_then(Value::as_str)
            .is_some_and(|name| name == binary);
        if let Some(executable) = message.get("executable").and_then(Value::as_str)
            && named
        {
            return Ok(PathBuf::from(executable));
        }
    }
    bail!("{doing} made no executable named {binary:?}")
}

/// What a build of `app_dir` makes: its package and its binary.
fn app_binary(app_dir: &Path) -> Result<(String, String)> {
    let manifest_path = app_dir.join("Cargo.toml");
    let manifest = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?
        .parse::<toml::Table>()
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    let package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .with_context(|| format!("{} has no [package] table", manifest_path.display()))?;
    let name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .with_context(|| format!("{} has no package name", manifest_path.display()))?;
    let bins = manifest
        .get("bin")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|binary| binary.get("name").and_then(toml::Value::as_str))
        .collect::<Vec<_>>();
    let binary = package
        .get("default-run")
        .and_then(toml::Value::as_str)
        .or(match bins.as_slice() {
            [binary] => Some(*binary),
            _ => None,
        })
        .unwrap_or(name);
    Ok((name.to_owned(), binary.to_owned()))
}

/// Run the app's binary for its surface report.
pub fn report_of(app_dir: &Path, executable: &Path) -> Result<SurfaceReport> {
    let doing = format!("{}'s surface report", app_dir.display());
    let answer = run_bounded(report_command(app_dir, executable)?, REPORT_WAIT, &doing)?;
    if !answer.status.success() {
        bail!("{doing} failed with {}", answer.status);
    }
    parse_report(&String::from_utf8_lossy(&answer.stdout))
}

/// Keep the surface report `app`'s `executable` prints in `dir` as `<app>.json`, so a
/// surface that is behind its code can be written without building the app again:
/// `tilolang surface apps/<app> --report <dir>/<app>.json` (certify's outputs on t).
pub fn keep_report(app: &Path, executable: &Path, dir: &Path) -> Result<PathBuf> {
    let name = app.file_name().unwrap_or_default().to_string_lossy();
    let doing = format!("{}'s surface report", app.display());
    let answer = run_bounded(report_command(app, executable)?, REPORT_WAIT, &doing)?;
    if !answer.status.success() {
        bail!("{doing} failed with {}", answer.status);
    }
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = dir.join(format!("{name}.json"));
    std::fs::write(&file, answer.stdout).with_context(|| format!("writing {}", file.display()))?;
    Ok(file)
}

/// The app's surface file for `resources` (see [`ui::tilo_resource::surface::file`]).
///
/// # Errors
/// A resource that is no table, or holds a null.
pub fn render(resources: &[Value]) -> Result<String> {
    render_with(resources, HEADER)
}

/// [`render`] under `header`, the line that says what wrote the file: a
/// surface a test writes beside the code that publishes it names its test.
pub fn render_with(resources: &[Value], header: &str) -> Result<String> {
    tilo_resource::surface::file::render(header, resources).map_err(anyhow::Error::msg)
}

/// Write `resources` to the app's [`FILE`]. Returns its path.
pub fn write(app_dir: &Path, resources: &[Value]) -> Result<PathBuf> {
    let path = app_dir.join(FILE);
    std::fs::write(&path, render(resources)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn committed(app_dir: &Path) -> Result<Option<Committed>> {
    let path = app_dir.join(FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let toml: toml::Value =
        toml::from_str(&text).with_context(|| format!("{} is not TOML", path.display()))?;
    let json = serde_json::to_value(toml).context("reading the committed surface")?;
    Ok(Some(Committed {
        format: json.get("format").and_then(Value::as_u64).unwrap_or(1),
        resources: json
            .get("resources")
            .and_then(Value::as_array)
            .map(|resources| resources.iter().map(normalized).collect())
            .unwrap_or_default(),
    }))
}

fn keyed<'a>(items: impl IntoIterator<Item = &'a Value>, key: &str) -> BTreeMap<String, &'a Value> {
    items
        .into_iter()
        .filter_map(|item| {
            item.get(key)
                .and_then(Value::as_str)
                .map(|name| (name.to_owned(), item))
        })
        .collect()
}

fn by<'a>(items: Option<&'a Value>, key: &str) -> BTreeMap<String, &'a Value> {
    keyed(items.and_then(Value::as_array).into_iter().flatten(), key)
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Whether `before`, a committed field, is `now` as a file written before
/// field types had their own vocabulary holds it: its Rust type's text in
/// place of the type, and no named types. Such a file is read as it was
/// written ([`tilo_actions::surface::FieldType::from_rust_text`]) and stays
/// current until the app writes its surface again.
fn same_but_older(before: &Value, now: &Value) -> bool {
    if !rust_text(before) || rust_text(now) {
        return false;
    }
    let mut upgraded = before.clone();
    if let (Some(slot), Some(ty)) = (upgraded.get_mut("type"), now.get("type")) {
        slot.clone_from(ty);
    }
    upgraded == *now
}

/// Whether the committed resource `before` was written before field and
/// parameter types had their own vocabulary: it declares no named types, and
/// its fields hold Rust type text or its parameters no types.
fn written_before_types(before: &Value) -> bool {
    if before.get("types").is_some() {
        return false;
    }
    let fields = before.get("fields").and_then(Value::as_array);
    let verbs = before.get("verbs").and_then(Value::as_array);
    let params = verbs
        .into_iter()
        .flatten()
        .filter_map(|verb| verb.get("params").and_then(Value::as_array))
        .flatten();
    fields.is_some_and(|fields| fields.iter().any(rust_text))
        || params.clone().next().is_some()
            && params.clone().all(|param| param.get("type").is_none())
}

/// Whether the committed `before` is `now` but for what a file written
/// earlier leaves out: a verb's parameters' types (before the vocabulary),
/// and the value it answers with (before [`FORMAT`]). Such a file stays
/// current until its app's surface is written again.
fn same_but_earlier(before: &Value, now: &Value, untyped: bool, unreturned: bool) -> bool {
    let mut earlier = now.clone();
    if let Some(object) = earlier.as_object_mut() {
        if untyped && let Some(params) = object.get_mut("params").and_then(Value::as_array_mut) {
            for param in params.iter_mut().filter_map(Value::as_object_mut) {
                param.remove("type");
            }
        }
        if untyped || unreturned {
            object.remove("returns");
        }
    }
    earlier == *before
}

/// Whether `verb` is a read action: it changes nothing, so a file written
/// before surfaces published read actions ([`written_before_reads`]) is
/// current without it until its app is next written.
fn is_read(verb: &Value) -> bool {
    text(verb, "kind") == "read"
}

/// Whether the committed file `format` was written before surfaces published
/// their apps' read actions. Such a file stays current until the app's
/// surface is next written, which adds them with their parameters and the
/// values they answer with; once it has them ([`FORMAT`]), a read action it
/// lacks is drift as any other.
fn written_before_reads(format: u64) -> bool {
    format < FORMAT
}

/// The code's resource `now` as the committed file `before` holds it: what
/// the writer of that file left out comes out again (a verb's answer and
/// parameter types, a field's type in the code's vocabulary, read verbs it
/// has none of), so what remains are the differences to review.
fn expected(before: &Value, now: &Value, format: u64) -> Value {
    let older = written_before_types(before);
    let unread = written_before_reads(format);
    let mut expected = now.clone();
    if !older && !unread {
        return expected;
    }
    let old_verbs = by(before.get("verbs"), "id");
    if unread && let Some(verbs) = expected.get_mut("verbs").and_then(Value::as_array_mut) {
        verbs.retain(|verb| !is_read(verb) || old_verbs.contains_key(text(verb, "id")));
    }
    for verb in expected
        .get_mut("verbs")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if let Some(previous) = old_verbs.get(text(verb, "id"))
            && same_but_earlier(previous, verb, older, unread)
        {
            *verb = (**previous).clone();
        }
    }
    if older && let Some(object) = expected.as_object_mut() {
        object.remove("types");
    }
    if older {
        let old_fields = by(before.get("fields"), "path");
        for field in expected
            .get_mut("fields")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            if let Some(previous) = old_fields.get(text(field, "path"))
                && same_but_older(previous, field)
            {
                *field = (**previous).clone();
            }
        }
    }
    expected
}

/// How the committed surface `before` differs from `now`, one line each.
/// Every field of the resource is compared, not a list of them, so one the
/// writer adds is checked too.
fn differences(kind: &str, before: &Value, now: &Value, format: u64) -> Vec<String> {
    let expected = expected(before, now, format);
    let mut changes = Vec::new();
    for (what, list, key) in [("verb", "verbs", "id"), ("field", "fields", "path")] {
        let old = by(before.get(list), key);
        let new = by(expected.get(list), key);
        for (name, value) in &new {
            match old.get(name) {
                None => changes.push(format!("{kind}: new {what} `{name}`")),
                Some(previous) if previous != value => {
                    changes.push(format!("{kind}: {what} `{name}` changed"));
                }
                Some(_) => {}
            }
        }
        for name in old.keys().filter(|name| !new.contains_key(*name)) {
            changes.push(format!("{kind}: {what} `{name}` is gone"));
        }
    }
    let rest = |value: &Value| -> BTreeSet<String> {
        value
            .as_object()
            .into_iter()
            .flat_map(|object| object.keys())
            .filter(|key| !matches!(key.as_str(), "verbs" | "fields"))
            .cloned()
            .collect()
    };
    for key in rest(before).union(&rest(&expected)) {
        if before.get(key) != expected.get(key) {
            changes.push(format!("{kind}: {key} changed"));
        }
    }
    changes
}

/// The stored fields of `before` that `now` reads differently (gone, or
/// another type or layout), which older documents need a migration for.
fn incompatible(before: &Value, now: &Value) -> Vec<String> {
    let old = by(before.get("fields"), "path");
    let new = by(now.get("fields"), "path");
    old.iter()
        .filter_map(|(path, field)| match new.get(path) {
            None => Some(format!("`{path}` is gone")),
            Some(current)
                if other_type(field, current)
                    || text(current, "stored") != text(field, "stored") =>
            {
                Some(format!(
                    "`{path}` is now {} ({}), was {} ({})",
                    type_text(current),
                    text(current, "stored"),
                    type_text(field),
                    text(field, "stored")
                ))
            }
            Some(_) => None,
        })
        .collect()
}

/// Whether the field `now` holds another type than `before` did. A file
/// written before field types had their own vocabulary holds Rust type text,
/// compared as text; text and a typed field are not compared, only their
/// layouts are.
fn other_type(before: &Value, now: &Value) -> bool {
    match (rust_text(before), rust_text(now)) {
        (true, true) => stored_type(text(before, "type")) != stored_type(text(now, "type")),
        (true, false) | (false, true) => false,
        (false, false) => before.get("type") != now.get("type"),
    }
}

/// Whether `field`'s type is Rust type text, as a surface file written
/// before field types had their own vocabulary holds it, rather than a
/// [`tilo_actions::surface::FieldType`] (whose plain types are words too:
/// `text`, `int`).
fn rust_text(field: &Value) -> bool {
    field.get("type").is_some_and(|ty| {
        ty.is_string()
            && serde_json::from_value::<tilo_actions::surface::FieldType>(ty.clone()).is_err()
    })
}

fn type_text(field: &Value) -> String {
    match field.get("type") {
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// A field's type as the document stores it: a shared `Map<T>` is stored
/// as a `BTreeMap<String, T>` is, so moving a field between the two needs
/// no migration.
fn stored_type(ty: &str) -> String {
    let ty = ty
        .replace("ui::tilo_resource::", "")
        .replace("tilo_resource::", "")
        .replace("std::collections::", "");
    let mut out = String::with_capacity(ty.len());
    let mut rest = ty.as_str();
    while let Some(at) = rest.find("Map<") {
        let (before, after) = rest.split_at(at);
        out.push_str(before);
        let named = before
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        out.push_str(if named { "Map<" } else { "BTreeMap<String, " });
        rest = after.get("Map<".len()..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// The history messages `resource` asks of `locales/en-US.ftl`, as history
/// renders entries of the app `app` (its id): one per verb that changes
/// something (`<app>-<verb>`) and one per editable field
/// (`<app>-<type>-<field>`).
fn history_messages(app: &str, resource: &Value) -> Vec<String> {
    let app = tilo_history::app_prefix(app);
    let kebab = tilo_actions::entry::kebab;
    let mut keys = Vec::new();
    for verb in resource
        .get("verbs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if text(verb, "kind") != "read" {
            keys.push(format!("{app}-{}", kebab(text(verb, "id"))));
        }
    }
    let fields: Vec<&Value> = resource
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .collect();
    let readonly = |field: &Value| {
        field
            .get("readonly")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    // A field inside a read-only one is changed only by verbs too.
    let locked: Vec<String> = fields
        .iter()
        .filter(|field| readonly(field))
        .map(|field| format!("{}.", text(field, "path")))
        .collect();
    for field in fields {
        let path = text(field, "path");
        let editable = text(field, "stored") == "value"
            && !readonly(field)
            && !locked
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()));
        if editable {
            let key = text(field, "path").rsplit('.').next().unwrap_or_default();
            keys.push(format!(
                "{app}-{}-{}",
                kebab(text(field, "owner")),
                kebab(key)
            ));
        }
    }
    keys
}

/// What `tilolang check` reports about the app's surface: unbound controls,
/// drift from the committed file, incompatible storage changes without a
/// version bump, and history messages the catalogue lacks. A missing
/// committed file is `missing`'s level; a missing history message is an
/// error whenever the catalogue was read, because the app's history then
/// reads in the framework's generic words instead of its own.
#[must_use]
pub fn findings(
    app: &str,
    report: &SurfaceReport,
    committed: Option<&Committed>,
    catalog: Option<&BTreeSet<String>>,
    missing: Level,
) -> ContractReport {
    let file = PathBuf::from(FILE);
    let finding = |level, rule, message: String| ContractFinding {
        path: file.clone(),
        line: 0,
        level,
        rule,
        message,
    };
    let mut out = ContractReport::default();
    if let Some(view) = &report.view {
        out.findings.push(finding(
            Level::Error,
            "view",
            format!("the view could not be built headless: {view}"),
        ));
    }
    for unbound in &report.unbound {
        out.findings
            .push(finding(Level::Error, "binding", unbound.clone()));
    }
    for (what, id) in own_action_ids(report) {
        if report.reserved.contains(&id) {
            out.findings.push(finding(
                Level::Error,
                "reserved-id",
                format!(
                    "{what} `{id}` has the id of an action the framework publishes on every app; \
                     rename it (the framework's ids: {})",
                    report.reserved.join(", ")
                ),
            ));
        }
    }
    match committed {
        None => out.findings.push(finding(
            missing,
            "surface",
            format!("there is no committed {FILE}; write it with `tilolang surface`"),
        )),
        Some(committed) => {
            let before_format = committed.format;
            let committed: Vec<Value> = committed.resources.iter().map(filed).collect();
            let reported: Vec<Value> = report.resources.iter().map(filed).collect();
            let old = keyed(&committed, "kind");
            let new = keyed(&reported, "kind");
            let mut changes = Vec::new();
            for (kind, now) in &new {
                let Some(before) = old.get(kind) else {
                    changes.push(format!("new resource `{kind}`"));
                    continue;
                };
                changes.extend(differences(kind, before, now, before_format));
                let storage = incompatible(before, now);
                let bumped = now.get("version").and_then(Value::as_u64)
                    > before.get("version").and_then(Value::as_u64);
                if !storage.is_empty() && !bumped {
                    out.findings.push(finding(
                        Level::Error,
                        "storage",
                        format!(
                            "{kind} reads stored fields differently ({}) without a new storage version; bump `#[resource(version = N)]` and migrate older documents",
                            storage.join(", ")
                        ),
                    ));
                }
            }
            for kind in old.keys().filter(|kind| !new.contains_key(*kind)) {
                changes.push(format!("resource `{kind}` is gone"));
            }
            if !changes.is_empty() {
                out.findings.push(finding(
                    Level::Error,
                    "surface",
                    format!(
                        "the code's surface differs from {FILE} ({}); review it and write it with `tilolang surface`",
                        changes.join("; ")
                    ),
                ));
            }
        }
    }
    if let Some(catalog) = catalog {
        let missing: Vec<String> = report
            .resources
            .iter()
            .flat_map(|resource| history_messages(app, resource))
            .filter(|key| !catalog.contains(key))
            .collect();
        if !missing.is_empty() {
            let mut message = String::from(
                "history falls back to generic sentences without these messages in locales/en-US.ftl: ",
            );
            let _ = write!(message, "{}", missing.join(", "));
            out.findings.push(ContractFinding {
                path: PathBuf::from("locales/en-US.ftl"),
                ..finding(Level::Error, "history-message", message)
            });
        }
    }
    out
}

/// Every id the app publishes an action or view under itself: its
/// resources' verbs, its effects and its named views, each with what it is.
fn own_action_ids(report: &SurfaceReport) -> Vec<(&'static str, String)> {
    let verbs = report.resources.iter().flat_map(|resource| {
        resource
            .get("verbs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|verb| ("verb", text(verb, "id").to_owned()))
    });
    let effects = report.effects.iter().map(|id| ("effect", id.clone()));
    let views = report.views.iter().map(|id| ("view", id.clone()));
    verbs.chain(effects).chain(views).collect()
}

/// Write the surface of the app containing `start` to its [`FILE`]: from
/// the report its binary printed for [`FLAG`], saved at `saved`, else from
/// running it. Returns the file and the report, whose unbound controls the
/// caller shows.
pub fn write_app(start: &Path, saved: Option<&Path>) -> Result<(PathBuf, SurfaceReport)> {
    let app_dir = crate::resolve_app_dir(start)?;
    let report = if let Some(saved) = saved {
        parse_report(
            &std::fs::read_to_string(saved)
                .with_context(|| format!("reading {}", saved.display()))?,
        )?
    } else {
        ensure_framework_app(&app_dir)?;
        let executable = build_app(&app_dir)?;
        report_of(&app_dir, &executable)?
    };
    ensure_view_built(&report, &app_dir)?;
    let path = write(&app_dir, &report.resources)?;
    write_offers(&app_dir, &report.offers)?;
    sync_resource_kinds(&app_dir)?;
    Ok((path, report))
}

/// Refuse a report whose view did not build: what the view publishes (its read
/// actions) is missing from it, so writing it would drop them from the surface.
fn ensure_view_built(report: &SurfaceReport, app_dir: &Path) -> Result<()> {
    if let Some(view) = &report.view {
        bail!(
            "{}'s view could not be built headless ({view}), so the read actions it publishes are \
             unknown; write the surface where the app's window builds",
            app_dir.display()
        );
    }
    Ok(())
}

/// Add to the manifest's `[resources] kinds`, which the package signs, the
/// app's own kinds it does not list yet: a resource added or renamed in
/// code would otherwise be one a governed workspace never lets the app
/// create.
/// Kinds the manifest names already stay, whether the sources declare them
/// or not.
pub fn sync_resource_kinds(app_dir: &Path) -> Result<()> {
    let owned = crate::resource_kinds::owned_by_app(app_dir)?;
    let manifest = app_dir.join("tilo-app.toml");
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let mut kinds: Vec<String> = toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|table| {
            table
                .get("resources")?
                .get("kinds")?
                .clone()
                .try_into()
                .ok()
        })
        .unwrap_or_default();
    for kind in owned {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    let text = with_resource_kinds(&text, &kinds).unwrap_or(text);
    if std::fs::read_to_string(&manifest).ok().as_deref() != Some(text.as_str()) {
        std::fs::write(&manifest, text)
            .with_context(|| format!("writing {}", manifest.display()))?;
    }
    Ok(())
}

/// `manifest` with its `[resources] kinds` set to `kinds`, or `None` when
/// it names them already (or the app declares none and the manifest has no
/// such table).
fn with_resource_kinds(manifest: &str, kinds: &[String]) -> Option<String> {
    with_resources_list(manifest, "kinds", kinds)
}

/// `manifest` with its `[resources]` list `key` set to `values`, or `None`
/// when it names them already (or there are none and the manifest has no
/// such list).
fn with_resources_list(manifest: &str, key: &str, values: &[String]) -> Option<String> {
    let kinds = values;
    let declared: Option<Vec<String>> = toml::from_str::<toml::Table>(manifest)
        .ok()
        .and_then(|table| table.get("resources")?.get(key)?.clone().try_into().ok());
    if declared.as_deref() == Some(kinds) || (declared.is_none() && kinds.is_empty()) {
        return None;
    }
    let line = format!(
        "{key} = [{}]",
        kinds
            .iter()
            .map(|kind| format!("{kind:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut lines: Vec<String> = manifest.lines().map(str::to_owned).collect();
    if let Some(at) = lines.iter().position(|l| l.trim() == "[resources]") {
        let end = lines
            .iter()
            .skip(at.saturating_add(1))
            .position(|l| l.trim_start().starts_with('['))
            .map_or(lines.len(), |offset| {
                at.saturating_add(1).saturating_add(offset)
            });
        let existing = (at.saturating_add(1)..end).find(|&i| {
            lines.get(i).is_some_and(|l| {
                let l = l.trim_start();
                l.strip_prefix(key)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            })
        });
        match existing {
            Some(i) => {
                if let Some(slot) = lines.get_mut(i) {
                    *slot = line;
                }
            }
            None => lines.insert(at.saturating_add(1), line),
        }
    } else {
        let before = lines
            .iter()
            .position(|l| l.trim() == "[publisher]")
            .unwrap_or(lines.len());
        lines.splice(
            before..before,
            ["[resources]".to_owned(), line, String::new()],
        );
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Some(out)
}

fn ensure_framework_app(app_dir: &Path) -> Result<()> {
    if !is_framework_app(app_dir) {
        bail!(
            "{} does not run through `ui::main!(App)`, so its binary reports no surface",
            app_dir.display()
        );
    }
    Ok(())
}

/// The app id `tilo-app.toml` declares.
pub fn app_id(app_dir: &Path) -> Result<String> {
    let path = app_dir.join("tilo-app.toml");
    let source =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let manifest = tilo_app_package::Manifest::from_toml_str(&source)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(manifest.package.id)
}

/// Check the surface of the app at `start` against its committed [`FILE`]
/// (run from `executable` when it is built), with findings at `level`;
/// `None` for an app whose binary reports none. History messages are
/// asked of the app's source catalog when `level` is an error.
pub fn check_app(
    start: &Path,
    executable: Option<&Path>,
    level: Level,
) -> Result<Option<ContractReport>> {
    let app_dir = crate::resolve_app_dir(start)?;
    if !is_framework_app(&app_dir) {
        return Ok(None);
    }
    let built;
    let executable = if let Some(executable) = executable {
        executable
    } else {
        built = build_app(&app_dir)?;
        &built
    };
    let report = report_of(&app_dir, executable)?;
    check_report(&app_dir, &report, level).map(Some)
}

/// What [`check_app`] finds in the framework app at `app_dir` once its
/// surface `report` is in hand, however it was obtained.
pub fn check_report(
    app_dir: &Path,
    report: &SurfaceReport,
    level: Level,
) -> Result<ContractReport> {
    let catalog = match level {
        Level::Error => crate::contract::source_catalog(app_dir)?,
        Level::Warning => None,
    };
    let mut out = findings(
        &app_id(app_dir)?,
        report,
        committed(app_dir)?.as_ref(),
        catalog.as_ref(),
        level,
    );
    if report.view.is_none() {
        let manifest = std::fs::read_to_string(app_dir.join(MANIFEST))
            .with_context(|| format!("reading {}'s manifest", app_dir.display()))?;
        let expected = with_offers(&manifest, &report.offers);
        if expected != manifest {
            // The words the manifest needs, so a person reads what differs rather than being
            // sent to run another command to find out.
            // Only the offers: the rest of the manifest is what it already says.
            let wanted: Vec<&str> = expected
                .split("[[offers]]")
                .skip(1)
                .flat_map(|offer| {
                    std::iter::once("[[offers]]").chain(
                        offer
                            .lines()
                            .take_while(|line| !line.trim_start().starts_with('['))
                            .filter(|line| !line.trim().is_empty()),
                    )
                })
                .collect();
            out.findings.push(ContractFinding {
                path: PathBuf::from(MANIFEST),
                line: 0,
                level: Level::Error,
                rule: "surface",
                message: format!(
                    "the actions the code offers while the app is closed differ from the \
                     manifest's [[offers]]; write them with `tilolang surface`. They are: {}",
                    wanted.join(" ")
                ),
            });
        }
    }
    Ok(out)
}

/// The manifest beside an app's [`FILE`].
const MANIFEST: &str = "tilo-app.toml";

/// The line above the written `[[offers]]`.
const OFFERS_HEADER: &str = "# What the app offers a person while it is closed: written by `tilolang surface` from\n\
                             # what its code publishes. Invoking one opens the app and invokes it there.\n";

/// Write `offers` as the `[[offers]]` of the manifest in `app_dir`. Answers
/// whether the manifest changed.
pub fn write_offers(app_dir: &Path, offers: &[Value]) -> Result<bool> {
    let path = app_dir.join(MANIFEST);
    let manifest =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let written = with_offers(&manifest, offers);
    if written == manifest {
        return Ok(false);
    }
    std::fs::write(&path, written).with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// `manifest` with its `[[offers]]` tables, and the comment above them, replaced by
/// `offers`, written last. The `open-link` every app publishes is offered only by an app that
/// takes a scheme (`[providers] schemes`): it is how a link of one starts the app.
pub(crate) fn with_offers(manifest: &str, offers: &[Value]) -> String {
    let takes_links = toml::from_str::<toml::Table>(manifest)
        .ok()
        .and_then(|table| {
            let schemes = table.get("providers")?.get("schemes")?.as_array()?;
            Some(!schemes.is_empty())
        })
        .unwrap_or(false);
    let offers: Vec<&Value> = offers
        .iter()
        .filter(|offer| {
            takes_links
                || offer.get("action").and_then(Value::as_str)
                    != Some(tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION)
        })
        .collect();
    let lines: Vec<&str> = manifest.lines().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(lines.len());
    let mut in_offer = false;
    for line in &lines {
        let header = line.trim_start().starts_with('[');
        if line.trim() == "[[offers]]" {
            in_offer = true;
            // The comment and blank lines just above an offer are about it.
            while kept
                .last()
                .is_some_and(|above| above.trim().is_empty() || above.trim_start().starts_with('#'))
            {
                kept.pop();
            }
            continue;
        }
        if header {
            in_offer = false;
        }
        if !in_offer {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    let mut out = kept.join("\n");
    out.push('\n');
    if !offers.is_empty() {
        let _ = write!(out, "\n{OFFERS_HEADER}\n");
    }
    // What each says a person reads in the app's own words; a kind and an operation the
    // manifest takes by default go unsaid.
    for offer in offers {
        out.push_str("[[offers]]\n");
        for key in [
            "resource",
            "resource-kind",
            "action",
            "label",
            "kind",
            "operation",
            "confirm",
            "asks",
            "keywords",
            "icon",
        ] {
            let Some(value) = offer.get(key).filter(|value| {
                !matches!(
                    (key, value.as_str()),
                    ("kind", Some("undoable")) | ("operation", Some("effect"))
                ) && !(key == "asks" && value.as_array().is_some_and(Vec::is_empty))
            }) else {
                continue;
            };
            let Ok(value) = toml::Value::try_from(value) else {
                continue;
            };
            let _ = writeln!(out, "{key} = {value}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offer_asking_two_values_writes_them_as_fields_a_manifest_reads_back() {
        let manifest = "[package]\nid = \"so.example\"\n\n[publisher]\nidentity = \"x\"\n";
        let offer = serde_json::json!({
            "resource": "so.example.vault",
            "action": "add-item",
            "label": "Add item",
            "asks": [
                { "name": "issuer", "type": "text", "description": "Issuer" },
                { "name": "secret", "type": "secret", "description": "Shared secret" },
            ],
        });
        let written = with_offers(manifest, &[offer]);
        let parsed: toml::Table = toml::from_str(&written).expect("the manifest reads back");
        let offers: Vec<tilo_app_package::manifest::OfferedAction> =
            parsed["offers"].clone().try_into().expect("the offer reads");
        assert_eq!(
            offers[0]
                .asked()
                .iter()
                .map(|asked| (asked.name.as_str(), asked.ty.as_str(), asked.description.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("issuer", "text", Some("Issuer")),
                ("secret", "secret", Some("Shared secret")),
            ],
            "both fields are written and read back with their types and words"
        );
    }

    #[test]
    fn the_manifest_names_the_kinds_the_sources_declare() {
        let kinds = vec!["so.example.table".to_owned()];
        let fresh = "[package]\nid = \"so.example\"\n\n[publisher]\nidentity = \"x\"\n";
        let added = with_resource_kinds(fresh, &kinds).unwrap();
        assert!(added.contains("[resources]\nkinds = [\"so.example.table\"]\n\n[publisher]"));
        assert_eq!(with_resource_kinds(&added, &kinds), None, "already named");
        let renamed = added.replace("so.example.table", "so.example.greeting");
        assert_eq!(
            with_resource_kinds(&renamed, &kinds).as_deref(),
            Some(added.as_str())
        );
        assert_eq!(with_resource_kinds(fresh, &[]), None, "nothing to declare");
    }

    #[test]
    fn an_app_keeping_chains_preserves_legacy_queue_compatibility() {
        let app = tempfile::tempdir().unwrap();
        std::fs::write(
            app.path().join("Cargo.toml"),
            "[package]\nname = \"example\"\n",
        )
        .unwrap();
        std::fs::write(
            app.path().join("tilo-app.toml"),
            "[package]\nid = \"so.example\"\n\n[resources]\nkinds = []\nown = [\"so.example.draft\"]\n\n[publisher]\nidentity = \"x\"\n",
        )
        .unwrap();
        std::fs::create_dir(app.path().join("src")).unwrap();
        std::fs::write(app.path().join("src/lib.rs"), "impl Chained for Part {}\n").unwrap();
        sync_resource_kinds(app.path()).unwrap();
        let manifest = std::fs::read_to_string(app.path().join("tilo-app.toml")).unwrap();
        let unsent = tilo_actions::surface::CHAIN_UNSENT_KIND;
        assert!(
            manifest.contains(&format!("kinds = [\"{unsent}\"]")),
            "{manifest}"
        );
        assert!(
            manifest.contains("own = [\"so.example.draft\"]"),
            "{manifest}"
        );
        sync_resource_kinds(app.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(app.path().join("tilo-app.toml")).unwrap(),
            manifest,
            "declared once"
        );
    }

    fn resource(version: u64, title_type: &str, verbs: &[&str]) -> Value {
        serde_json::json!({
            "kind": "notes.book",
            "label": "Notes",
            "version": version,
            "verbs": verbs.iter().map(|id| serde_json::json!({
                "id": id, "label": id, "kind": "undoable", "params": [
                    {"name": "title", "required": true, "schema": {"type": "string", "default": null}}
                ]
            })).collect::<Vec<_>>(),
            "fields": [
                {"path": "title", "owner": "book", "title": "Title", "stored": "value", "type": title_type},
                {"path": "pages", "owner": "book", "title": "Pages", "stored": "collection", "type": "Collection<Page>"},
                {"path": "pages.*.text", "owner": "page", "title": "Text", "stored": "value", "type": "String", "readonly": true},
                {"path": "meta", "owner": "book", "title": "Meta", "stored": "record", "type": "Meta", "readonly": true},
                {"path": "meta.author", "owner": "meta", "title": "Author", "stored": "value", "type": "String"},
            ],
        })
    }

    fn report(resources: &[Value]) -> SurfaceReport {
        SurfaceReport {
            resources: resources.iter().map(normalized).collect(),
            unbound: Vec::new(),
            view: None,
            effects: Vec::new(),
            views: Vec::new(),
            reserved: Vec::new(),
            offers: Vec::new(),
        }
    }

    /// A committed surface of `resources`, as the writer writes it.
    fn written(resources: Vec<Value>) -> Committed {
        Committed {
            format: FORMAT,
            resources,
        }
    }

    fn rules(found: &ContractReport) -> Vec<(&'static str, Level)> {
        found
            .findings
            .iter()
            .map(|finding| (finding.rule, finding.level))
            .collect()
    }

    #[test]
    fn an_actions_answer_reads_back_with_its_type() {
        let mut book = resource(1, "String", &["rename"]);
        book["verbs"][0]["returns"] = serde_json::json!({
            "type": {"named": "Renamed"},
            "schema": {
                "type": "object",
                "properties": {"title": {"type": "string", "description": "The title"}},
            },
        });
        book["types"] = serde_json::json!([{
            "name": "Renamed",
            "fields": [{"name": "title", "type": "text"}],
        }]);
        let text = render(&[book]).unwrap();
        assert!(!text.contains("The title"), "{text}");
        let value: Value =
            serde_json::to_value(toml::from_str::<toml::Value>(&text).unwrap()).unwrap();
        let surface: tilo_actions::surface::Surface =
            serde_json::from_value(value["resources"][0].clone()).unwrap();
        let returns = surface.verbs[0].returns.clone().unwrap();
        assert_eq!(
            returns.ty,
            tilo_actions::surface::FieldType::Named("Renamed".to_owned())
        );
        assert_eq!(returns.schema["properties"]["title"]["type"], "string");
        assert_eq!(surface.types[0].name, "Renamed");
    }

    tilo_test_env::scenario! {
        fn a_written_surface_file_reads_back_as_what_was_written;

        phase a_written_surface_reads_back_equal_and_passes;
        {
            let temp = tempfile::tempdir().unwrap();
            let now = report(&[resource(1, "String", &["rename"])]);
            write(temp.path(), &now.resources).unwrap();
            let committed = committed(temp.path()).unwrap().unwrap();
            assert_eq!(committed.format, FORMAT);
            let filed_now: Vec<Value> = now.resources.iter().map(filed).collect();
            assert_eq!(committed.resources, filed_now);
            // What the file leaves out is not drift.
            assert!(committed.resources[0]["fields"][0].get("title").is_none());
            let catalog: BTreeSet<String> = ["notes-rename", "notes-book-title"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            let found = findings(
                "so.tilo.notes",
                &now,
                Some(&committed),
                Some(&catalog),
                Level::Error,
            );
            assert!(found.findings.is_empty(), "{}", found.render());
        }

        phase a_written_surface_reads_as_what_receivers_decide_with;
        {
            let mut book = resource(1, "String", &["rename"]);
            book["fields"][1]["ops"] = serde_json::json!({"type": "object", "title": "Text ops"});
            book["verbs"][0]["params"][0]["schema"]["description"] = "The new title".into();
            let text = render(&[book.clone()]).unwrap();
            assert!(!text.contains("title = \"Title\""), "{text}");
            assert!(!text.contains("label = \"rename\""), "{text}");
            assert!(!text.contains("The new title"), "{text}");
            let value: Value =
                serde_json::to_value(toml::from_str::<toml::Value>(&text).unwrap()).unwrap();
            let surface: tilo_actions::surface::Surface =
                serde_json::from_value(value["resources"][0].clone()).unwrap();
            assert_eq!(surface.verbs[0].id, "rename");
            assert!(surface.verbs[0].label.is_empty());
            assert_eq!(surface.fields[0].stored, "value");
            assert_eq!(surface.fields[1].stored, "collection");
            assert!(surface.fields[2].readonly);
            let ops = surface.fields[1].ops.clone().unwrap();
            assert!(ops.as_str().unwrap().starts_with("sha256:"));
            book["fields"][1]["ops"]["title"] = "Other ops".into();
            assert_ne!(filed(&book)["fields"][1]["ops"], ops);
            assert_eq!(render(&[value["resources"][0].clone()]).unwrap(), text);
        }

        phase a_file_written_before_read_actions_takes_them_when_written_again;
        {
            let before = vec![normalized(&resource(1, "String", &["rename"]))];
            let mut now = report(&[resource(1, "String", &["rename"])]);
            let read = serde_json::json!({
                "id": "read-form-fields",
                "kind": "read",
                "returns": {"type": {"named": "FormFields"}},
            });
            now.resources[0]["verbs"]
                .as_array_mut()
                .unwrap()
                .push(read.clone());
            let older = Committed {
                format: 1,
                resources: before,
            };
            let found = findings("so.tilo.notes", &now, Some(&older), None, Level::Warning);
            assert!(found.findings.is_empty(), "{}", found.render());

            let temp = tempfile::tempdir().unwrap();
            write(temp.path(), &now.resources).unwrap();
            let committed = committed(temp.path()).unwrap().unwrap();
            let found = findings(
                "so.tilo.notes",
                &now,
                Some(&committed),
                None,
                Level::Warning,
            );
            assert!(found.findings.is_empty(), "{}", found.render());

            // A file that has them takes no other new verb.
            let mut more = report(&[resource(1, "String", &["rename"])]);
            more.resources[0]["verbs"].as_array_mut().unwrap().extend([
                read,
                serde_json::json!({"id": "retitle", "kind": "undoable"}),
            ]);
            let found = findings(
                "so.tilo.notes",
                &more,
                Some(&committed),
                None,
                Level::Warning,
            );
            assert_eq!(rules(&found), [("surface", Level::Error)]);
            assert!(found.findings[0].message.contains("retitle"));
        }
    }

    #[test]
    fn drift_storage_changes_bindings_and_messages_are_reported() {
        let before = vec![normalized(&resource(1, "String", &["rename"]))];
        // A renamed verb is drift; a field read as another type is a
        // storage change that needs a version.
        let now = report(&[resource(1, "Atom", &["retitle"])]);
        let found = findings(
            "so.tilo.notes",
            &now,
            Some(&written(before.clone())),
            Some(&BTreeSet::new()),
            Level::Error,
        );
        assert_eq!(
            rules(&found),
            [
                ("storage", Level::Error),
                ("surface", Level::Error),
                ("history-message", Level::Error)
            ]
        );
        assert!(
            !found.passed(),
            "an app whose history messages the catalogue lacks is not checked"
        );
        assert!(found.findings[1].message.contains("verb `retitle`"));
        assert!(found.findings[2].message.contains("notes-retitle"));
        assert!(found.findings[2].message.contains("notes-book-title"));
        assert!(!found.findings[2].message.contains("notes-page-text"));
        assert!(!found.findings[2].message.contains("notes-meta-author"));

        // With a new storage version the change is drift to commit only.
        let bumped = report(&[resource(2, "Atom", &["rename"])]);
        let found = findings(
            "so.tilo.notes",
            &bumped,
            Some(&written(before)),
            None,
            Level::Error,
        );
        assert_eq!(rules(&found), [("surface", Level::Error)]);

        // A plain map moved to a shared one is stored alike: drift only.
        let map = vec![normalized(&resource(
            1,
            "BTreeMap<String, Progress>",
            &["rename"],
        ))];
        let shared = report(&[resource(1, "tilo_resource::Map<Progress>", &["rename"])]);
        let found = findings(
            "so.tilo.notes",
            &shared,
            Some(&written(map.clone())),
            None,
            Level::Error,
        );
        assert_eq!(rules(&found), [("surface", Level::Error)]);
        let other = report(&[resource(1, "Map<Atom>", &["rename"])]);
        let found = findings(
            "so.tilo.notes",
            &other,
            Some(&written(map)),
            None,
            Level::Error,
        );
        assert_eq!(
            rules(&found),
            [("storage", Level::Error), ("surface", Level::Error)]
        );

        let mut unbound = report(&[resource(1, "String", &["rename"])]);
        unbound.unbound.push("a control is bound to `x`".into());
        let found = findings("so.tilo.notes", &unbound, None, None, Level::Warning);
        assert_eq!(
            rules(&found),
            [("binding", Level::Error), ("surface", Level::Warning)]
        );
        assert!(!found.passed());
    }

    #[test]
    fn every_field_the_writer_writes_is_compared() {
        let before = filed(&resource(1, "String", &["rename"]));
        for (field, value) in [
            ("label", serde_json::json!("Notes again")),
            ("version", serde_json::json!(2)),
            ("person", serde_json::json!(true)),
            ("own", serde_json::json!(true)),
            ("shows", serde_json::json!(["open"])),
        ] {
            let mut now = before.clone();
            now[field] = value;
            assert_eq!(
                differences("notes.book", &before, &now, FORMAT),
                [format!("notes.book: {field} changed")],
                "{field}"
            );
        }
        let mut other = before;
        other["shows"] = serde_json::json!(["open"]);
        let mut now = other.clone();
        now["shows"] = serde_json::json!(["open", "show"]);
        assert_eq!(
            differences("notes.book", &other, &now, FORMAT),
            ["notes.book: shows changed"]
        );
    }

    tilo_test_env::scenario! {
        fn a_changed_person_record_and_a_framework_id_are_findings;

        phase a_record_that_stopped_being_the_persons_is_drift;
        {
            let before = filed(&resource(1, "String", &["rename"]));
            let mut now = before.clone();
            now["person"] = Value::Bool(true);
            assert_eq!(
                differences("notes.book", &before, &now, FORMAT),
                ["notes.book: person changed"]
            );
            assert_eq!(
                differences("notes.book", &now, &before, FORMAT),
                ["notes.book: person changed"]
            );
        }

        phase an_action_with_a_framework_id_is_refused;
        {
            let mut now = report(&[resource(1, "String", &["rename", "undo"])]);
            now.effects = vec!["zoom-in".into(), "open".into()];
            now.views = vec!["history".into(), "reader".into()];
            now.reserved = ["open", "undo", "redo", "history", "field-edit"]
                .into_iter()
                .map(str::to_owned)
                .collect();
            let found = findings("so.tilo.notes", &now, None, None, Level::Warning);
            let refused: Vec<&str> = found
                .findings
                .iter()
                .filter(|finding| finding.rule == "reserved-id")
                .map(|finding| finding.message.as_str())
                .collect();
            assert_eq!(refused.len(), 3, "{refused:?}");
            assert!(refused[0].starts_with("verb `undo`"));
            assert!(refused[1].starts_with("effect `open`"));
            assert!(refused[2].starts_with("view `history`"));
            assert!(!found.passed());
        }
    }

    tilo_test_env::scenario! {
        fn a_surface_report_is_read_from_a_framework_apps_binary;

        phase the_report_is_read_from_what_the_binary_printed;
        {
            let printed = "starting\n{\"resources\": [{\"kind\": \"notes.book\", \"verbs\": [], \"fields\": []}], \"unbound\": [\"x\"], \"effects\": [\"zoom-in\"], \"views\": [\"reader\"], \"reserved\": [\"open\", \"undo\"], \"view\": null}\n";
            let parsed = parse_report(printed).unwrap();
            assert_eq!(parsed.resources.len(), 1);
            assert_eq!(parsed.unbound, ["x"]);
            assert_eq!(parsed.effects, ["zoom-in"]);
            assert_eq!(parsed.views, ["reader"]);
            assert_eq!(parsed.reserved, ["open", "undo"]);
            assert!(parsed.view.is_none());
        }

        phase only_framework_apps_report_a_surface;
        {
            let temp = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(temp.path().join("src")).unwrap();
            std::fs::write(temp.path().join("src/main.rs"), "ui::main!(demo::App);\n").unwrap();
            assert!(is_framework_app(temp.path()));
            std::fs::write(
                temp.path().join("src/main.rs"),
                "ui::main!(definition = demo::definition);\n",
            )
            .unwrap();
            assert!(!is_framework_app(temp.path()));
        }
    }

    #[test]
    fn a_surface_is_not_written_from_a_report_whose_view_did_not_build() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path();
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(app.join("src/main.rs"), "ui::main!(demo::App);\n").unwrap();
        let saved = app.join("demo.json");
        std::fs::write(
            &saved,
            "{\"resources\": [], \"unbound\": [], \"effects\": [], \"views\": [], \
             \"reserved\": [], \"view\": \"no display\"}",
        )
        .unwrap();
        let refused = write_app(app, Some(&saved)).unwrap_err();
        assert!(refused.to_string().contains("no display"), "{refused:#}");
        assert!(!app.join(FILE).exists(), "no surface is written");
    }

    #[test]
    fn messages_history_reads_in_its_own_words() {
        let app_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/messages");
        assert!(is_framework_app(&app_dir), "Messages is a framework app");
        let printed =
            ui::framework::headless::surface_report::<messages::MessagesShell>().to_string();
        let surface = parse_report(&printed).expect("Messages reports its surface");
        let report = check_report(&app_dir, &surface, Level::Error).expect("the check runs");
        let missing: Vec<&str> = report
            .findings
            .iter()
            .filter(|finding| finding.rule == "history-message")
            .map(|finding| finding.message.as_str())
            .collect();
        assert!(missing.is_empty(), "{missing:#?}");
        assert!(report.passed(), "{:#?}", report.findings);
    }
}
