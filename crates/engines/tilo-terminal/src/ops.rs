//! The ops a terminal app asks its device for, over JSON: opening and driving a shell, reading
//! back what it shows, and the environments it can run in.
//!
//! The engine answers `engine.call` and `engine.start` for the Terminal app only, and the app
//! records what it learns in its own resources. A process's output never reaches this API as a
//! state: it is the grid's, and what a run printed comes back once, as the run its shell marked.

use std::sync::Arc;

use serde_json::{Value, json};
use tilolang_engine::{JobOp, JobSink, Needs, Provider, State};

use crate::live::{self, Event, Live};
use crate::provider::{
    OpenError, OpenOptions, Requester, environments_available, launch_command_from_context,
    ush_binary,
};
use crate::target::{SessionTarget, parse_target_argument};
use crate::undoability;

/// The app these ops answer for.
pub const APP: &str = "so.tilo.terminal";

/// How long an agent's run may take before the op stops waiting for its shell to finish it.
const RUN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);

/// What a device does for the Terminal app that only it can: open a link.
pub trait Opener: Send + Sync {
    /// # Errors
    ///
    /// Why nothing opened the address.
    fn open_uri(&self, url: &str) -> Result<(), String>;
}

/// A device that opens no links: a host with no window to hand one to.
pub struct NoOpener;

impl Opener for NoOpener {
    fn open_uri(&self, _url: &str) -> Result<(), String> {
        Err("this device opens no links".to_owned())
    }
}

/// One request answered now.
///
/// # Errors
///
/// What the op refused, in words the app shows.
pub fn call(op: &str, input: &str, opener: &dyn Opener) -> Result<String, String> {
    let ask = parse(input);
    match op {
        "open" => open(&ask),
        "send" | "type" => with_live(&ask, |live| {
            let text = text(&ask)?;
            live.write(text.as_bytes())
                .map(|()| "{}".to_owned())
                .map_err(|error| error.to_string())
        }),
        "run" => with_live(&ask, |live| {
            live.run(text(&ask)?)
                .map(|()| "{}".to_owned())
                .map_err(|error| error.to_string())
        }),
        "interrupt" => with_live(&ask, |live| {
            live.interrupt()
                .map(|()| "{}".to_owned())
                .map_err(|error| error.to_string())
        }),
        "clear" => with_live(&ask, |live| {
            live.clear();
            Ok("{}".to_owned())
        }),
        "resize" => with_live(&ask, |live| {
            let columns = number(&ask, "columns")?;
            let rows = number(&ask, "rows")?;
            live.resize(columns, rows).map(|()| "{}".to_owned())
        }),
        "focus" => with_live(&ask, |live| {
            live.focus(ask.get("focused").and_then(Value::as_bool).unwrap_or(false));
            Ok("{}".to_owned())
        }),
        "close" => with_live(&ask, |live| {
            live.close();
            let _ = live::release(live.id());
            Ok("{}".to_owned())
        }),
        "allow_clipboard" => with_live(&ask, |live| {
            live.set_allow_clipboard(ask.get("allow").and_then(Value::as_bool).unwrap_or(false));
            Ok("{}".to_owned())
        }),
        "inspect" => with_live(&ask, |live| {
            Ok(json!({
                "shell": live.id(),
                "shell_name": live.session().shell_name(),
                "relay_session": live.session().relay_session(),
                "ended": live.ended(),
                "end": live.end().map(|end| format!("{end:?}")),
            })
            .to_string())
        }),
        "read_screen" => with_live(&ask, |live| {
            let (text, truncated) = live.readable();
            let modes = live.modes();
            Ok(json!({
                "text": text,
                "truncated": truncated,
                "cursor_visible": modes.cursor_visible,
                "links": live
                    .links()
                    .into_iter()
                    .filter(|(_, url)| crate::is_public_web_link(url))
                    .map(|(id, url)| json!({"id": id, "url": url}))
                    .collect::<Vec<_>>(),
            })
            .to_string())
        }),
        "events" => with_live(&ask, |live| {
            let since = ask.get("since").and_then(Value::as_u64).unwrap_or(0);
            let (events, next) = live.events(since);
            let events: Vec<Value> = events.into_iter().map(event_value).collect();
            Ok(json!({"events": events, "next": next}).to_string())
        }),
        "rerun_test" => {
            let command = ask
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let test = ask.get("test").and_then(Value::as_str).unwrap_or_default();
            let again = crate::parse::rerun_test(command, test)
                .ok_or_else(|| "this command's tool cannot run one test".to_owned())?;
            Ok(json!({"command": again}).to_string())
        }
        "environments" => Ok(json!({
            "available": environments_available(false),
            "self_hosted": ush_binary().is_some(),
        })
        .to_string()),
        "open_link" => opener.open_uri(text(&ask)?).map(|()| "{}".to_owned()),
        other => Err(format!("`{other}` is no op this device runs")),
    }
}

/// One request started as a job: a run that the app reads back when its shell finished it.
///
/// # Errors
///
/// What could not start, in words the app shows.
pub fn start(op: &str, input: &str, _opener: &dyn Opener) -> Result<String, String> {
    let ask = parse(input);
    match op {
        "run" => {
            let live = live_of(&ask)?;
            let command = text(&ask)?.to_owned();
            let job = crate::commands::new_key();
            begin_run(&job, live, command);
            Ok(json!({ "job": job }).to_string())
        }
        other => Err(format!("`{other}` is no job this device starts")),
    }
}

/// How far a run got, as JSON.
///
/// # Errors
///
/// No job this device runs.
pub fn status(job: &str) -> Result<String, String> {
    let held = jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = held
        .get(job)
        .ok_or_else(|| format!("no job `{job}` runs on this device"))?
        .to_string();
    drop(held);
    Ok(state)
}

/// Stop a run: false when it is not this device's or it had finished.
pub fn cancel(job: &str) -> bool {
    let mut held = jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cancelled = match held.get_mut(job) {
        Some(state) => {
            state["state"] = json!("cancelled");
            true
        }
        None => false,
    };
    drop(held);
    cancelled
}

fn jobs() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, Value>> {
    static JOBS: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, Value>>> =
        std::sync::OnceLock::new();
    JOBS.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

fn set_job(job: &str, value: Value) {
    let mut held = jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    held.insert(job.to_owned(), value);
}

/// Wait for the run the shell marks next and answer with it, as the run's own record.
fn begin_run(job: &str, live: std::sync::Arc<Live>, command: String) {
    set_job(job, json!({"state": "running"}));
    let job = job.to_owned();
    let handle_job = job.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("terminal-run-{job}"))
        .spawn(move || {
            let job = handle_job;
            let (_, cursor) = live.events(0);
            if let Err(error) = live.run(&command) {
                set_job(
                    &job,
                    json!({"state": "failed", "error": error.to_string()}),
                );
                return;
            }
            let started = std::time::Instant::now();
            let mut cursor = cursor;
            loop {
                let (events, next) = live.events(cursor);
                cursor = next;
                for event in events {
                    if let Event::Finished(run) = event {
                        set_job(
                            &job,
                            json!({"state": "done", "run": finished_value(&run)}),
                        );
                        return;
                    }
                    if let Event::Exit(_) | Event::Lost(_) = event {
                        set_job(
                            &job,
                            json!({"state": "failed", "error": "the shell ended before the run did"}),
                        );
                        return;
                    }
                }
                if started.elapsed() > RUN_PATIENCE {
                    set_job(
                        &job,
                        json!({"state": "failed", "error": "the run did not finish in time"}),
                    );
                    return;
                }
                if live.ended() {
                    set_job(
                        &job,
                        json!({"state": "failed", "error": "the shell ended before the run did"}),
                    );
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    if spawned.is_err() {
        set_job(
            job.as_str(),
            json!({"state": "failed", "error": "the run's reader did not start"}),
        );
    }
}

/// A finished run as the app records it.
fn finished_value(run: &crate::commands::FinishedCommand) -> Value {
    let output = String::from_utf8_lossy(&run.output);
    json!({
        "command": run.command,
        "cwd": run.cwd,
        "started_ms": run.started_ms,
        "ended_ms": run.ended_ms,
        "exit": run.exit,
        "output": crate::parse::plain(&output),
        "truncated": run.truncated,
        "tool": crate::parse::tool_of(&run.command),
        "undoable": undoability::of(&run.command).unwrap_or(false),
        "number": run.number,
        "job": run.job,
        "refusal": run.refusal,
        "items": crate::parse::items(&run.command, &run.output),
    })
}

fn event_value(event: Event) -> Value {
    match event {
        Event::Title(title) => json!({"kind": "title", "title": title}),
        Event::Finished(run) => json!({"kind": "finished", "run": finished_value(&run)}),
        Event::Exit(code) => json!({"kind": "exit", "code": code}),
        Event::Lost(why) => json!({"kind": "lost", "reason": why}),
        Event::Bell => json!({"kind": "bell"}),
        Event::Clipboard(text, primary) => {
            json!({"kind": "clipboard", "text": text, "primary": primary})
        }
    }
}

fn parse(input: &str) -> Value {
    serde_json::from_str(input).unwrap_or_else(|_| json!({}))
}

fn text(ask: &Value) -> Result<&str, String> {
    ask.get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| "the ask names no text".to_owned())
}

fn number(ask: &Value, name: &str) -> Result<u16, String> {
    ask.get(name)
        .and_then(Value::as_u64)
        .and_then(|number| u16::try_from(number).ok())
        .ok_or_else(|| format!("the ask names no {name}"))
}

fn live_of(ask: &Value) -> Result<std::sync::Arc<Live>, String> {
    let id = ask
        .get("shell")
        .and_then(Value::as_str)
        .ok_or_else(|| "the ask names no session".to_owned())?;
    live::get(id).ok_or_else(|| format!("no session `{id}` is live on this device"))
}

fn with_live(
    ask: &Value,
    work: impl FnOnce(&Live) -> Result<String, String>,
) -> Result<String, String> {
    let live = live_of(ask)?;
    work(&live)
}

/// Open a session and answer its id, or why it could not open.
fn open(ask: &Value) -> Result<String, String> {
    let target = match ask.get("target").and_then(Value::as_str) {
        Some(target) if !target.is_empty() => SessionTarget::decode(target)
            .or_else(|| parse_target_argument(target))
            .ok_or_else(|| format!("`{target}` is no target a session runs on"))?,
        _ => SessionTarget::default(),
    };
    let requested_by = if ask.get("agent").and_then(Value::as_bool).unwrap_or(false) {
        Requester::Agent
    } else {
        Requester::Person
    };
    let columns = number(ask, "columns").unwrap_or(80).max(1);
    let rows = number(ask, "rows").unwrap_or(24).max(1);
    let context = ask.get("context").and_then(Value::as_str).unwrap_or("");
    let program = launch_command_from_context(context);
    let relay = ask.get("attach").and_then(Value::as_str);
    let workspace = ask.get("workspace").and_then(Value::as_str);
    let record = ask.get("record").and_then(Value::as_str);
    let options = OpenOptions {
        program: (!program.is_empty()).then_some(program.as_slice()),
        relay_session: relay,
        workspace,
        record,
        ..OpenOptions::default()
    };
    let (store, wake) = device();
    let live = Live::open(&target, requested_by, &options, store, columns, rows, wake)
        .map_err(|error| open_error(&error))?;
    Ok(json!({
        "shell": live.id(),
        "shell_name": live.session().shell_name(),
        "target": target.encode(),
        "relay_session": live.session().relay_session(),
        "waiting": false,
    })
    .to_string())
}

/// Why a session did not open, in words the app shows: the supervisor's own reason where it
/// gave one, this device's otherwise.
fn open_error(error: &OpenError) -> String {
    error.to_string()
}

/// What wakes a window when a session's grid changed.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The content store sessions keep their pictures in, and how their window is woken.
type Device = (tilo_content::ContentStore, Option<Wake>);

/// What the device gives every session: the content store its pictures and transcripts are kept
/// in, and how a window is woken when its grid changed.
fn device() -> Device {
    let held = CONFIGURED.get_or_init(|| (tilo_content::ContentStore::memory(), None::<Wake>));
    (held.0.clone(), held.1.clone())
}

static CONFIGURED: std::sync::OnceLock<Device> = std::sync::OnceLock::new();

/// Tell the ops the device they run on: the content store sessions keep their pictures in, and
/// what wakes a window when a session's grid changed. A session process sets this once, at its
/// start; a process that never does keeps an empty store and wakes nobody.
pub fn configure(content: tilo_content::ContentStore, wake: Option<Wake>) {
    let _ = CONFIGURED.set((content, wake));
}

/// The app's engine, as the process that draws its windows answers `engine.call` and
/// `engine.start`: the ops above, with the device's way to open a link.
struct Answers {
    opener: Arc<dyn Opener>,
}

impl Provider for Answers {
    fn jobs(&self) -> &'static [JobOp] {
        // A run drives a session this process keeps, so it runs here rather than in the device's
        // shared engine host.
        const JOBS: &[JobOp] = &[JobOp::new("run", Needs::APP_STATE)];
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        call(op, input, &*self.opener)
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        if op != "run" {
            return Err(format!("`{op}` is no job this device starts"));
        }
        let ask = parse(input);
        let live = live_of(&ask)?;
        let command = text(&ask)?.to_owned();
        begin_run_told(job, live, command, &moved);
        Ok(())
    }

    fn cancel(&self, job: &str) -> bool {
        let Some(live) = running()
            .lock()
            .map(|mut jobs| jobs.remove(job))
            .ok()
            .flatten()
        else {
            return false;
        };
        let _ = live.interrupt();
        true
    }
}

/// Register this crate as the Terminal app's engine: what the process that answers its windows
/// calls once at its start, before the app asks. `opener` is the device's way to open a link a
/// program printed.
pub fn register(opener: Arc<dyn Opener>) {
    tilolang_engine::register(APP, Arc::new(Answers { opener }));
}

/// The sessions the runs started here drive, by the job the device named.
fn running() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, Arc<Live>>> {
    static RUNNING: std::sync::OnceLock<
        std::sync::Mutex<std::collections::BTreeMap<String, Arc<Live>>>,
    > = std::sync::OnceLock::new();
    RUNNING.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

/// Run `command` in `live` as the job `job`, telling `moved` each move it makes: running once
/// the line is sent, done with the run the shell marked, or failed why nothing ran.
fn begin_run_told(job: &str, live: Arc<Live>, command: String, moved: &JobSink) {
    let job = job.to_owned();
    if let Ok(mut jobs) = running().lock() {
        jobs.insert(job.clone(), Arc::clone(&live));
    }
    let handle_job = job.clone();
    // The reader thread tells the run's end; a reader that never starts is told here.
    let told = moved.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("terminal-run-{job}"))
        .spawn(move || {
            let job = handle_job;
            let moved = told;
            moved.moved(State::Running, json!({}));
            let (_, cursor) = live.events(0);
            if let Err(error) = live.run(&command) {
                moved.failed(&error.to_string());
                if let Ok(mut jobs) = running().lock() {
                    jobs.remove(&job);
                }
                return;
            }
            let started = std::time::Instant::now();
            let mut cursor = cursor;
            loop {
                let (events, next) = live.events(cursor);
                cursor = next;
                for event in events {
                    if let Event::Finished(run) = event {
                        moved.moved(State::Done, json!({"run": finished_value(&run)}));
                        if let Ok(mut jobs) = running().lock() {
                            jobs.remove(&job);
                        }
                        return;
                    }
                    if let Event::Exit(_) | Event::Lost(_) = event {
                        moved.failed("the shell ended before the run did");
                        if let Ok(mut jobs) = running().lock() {
                            jobs.remove(&job);
                        }
                        return;
                    }
                }
                if started.elapsed() > RUN_PATIENCE {
                    moved.failed("the run did not finish in time");
                    if let Ok(mut jobs) = running().lock() {
                        jobs.remove(&job);
                    }
                    return;
                }
                if live.ended() {
                    moved.failed("the shell ended before the run did");
                    if let Ok(mut jobs) = running().lock() {
                        jobs.remove(&job);
                    }
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    if spawned.is_err() {
        moved.failed("the run's reader did not start");
        if let Ok(mut jobs) = running().lock() {
            jobs.remove(&job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device's shell is the person's: an agent's ask is refused before a pty is opened.
    #[test]
    fn an_agent_never_opens_the_devices_own_shell() {
        let refused = call(
            "open",
            r#"{"target":"host","agent":true,"columns":80,"rows":24}"#,
            &NoOpener,
        )
        .expect_err("an agent's host session is refused");
        assert!(
            refused.contains("agents may not open the device's own shell"),
            "{refused}"
        );
    }

    /// An ask that names no session, or no op this device runs, is refused in words a person
    /// reads, and never reaches a session.
    #[test]
    fn asks_that_name_nothing_are_refused_in_words() {
        assert_eq!(
            call("send", r#"{"shell":"absent","text":"ls"}"#, &NoOpener),
            Err("no session `absent` is live on this device".to_owned())
        );
        assert_eq!(
            call("frobnicate", "{}", &NoOpener),
            Err("`frobnicate` is no op this device runs".to_owned())
        );
        assert_eq!(
            call("open", r#"{"target":"../../etc"}"#, &NoOpener),
            Err("`../../etc` is no target a session runs on".to_owned())
        );
    }

    /// What a public link is: an address a terminal may offer whoever reads it, never a local
    /// path or one that carries a credential.
    #[test]
    fn only_plain_web_addresses_are_public_links() {
        assert!(crate::is_public_web_link("https://example.test/docs"));
        assert!(crate::is_public_web_link("http://example.test"));
        assert!(!crate::is_public_web_link(
            "file:///home/me/.ssh/id_ed25519"
        ));
        assert!(!crate::is_public_web_link("https://token@example.test/x"));
        assert!(!crate::is_public_web_link("https://example.test/?q=secret"));
        assert!(!crate::is_public_web_link("https://example.test/#fragment"));
        assert!(!crate::is_public_web_link("javascript:alert(1)"));
    }
}
