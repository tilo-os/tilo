//! The device's own work, asked of it by name.
//!
//! An app runs sandboxed, with no decoder, no renderer and no graphics stack of its own. Work
//! that needs one is the device's: a video cut rendered, a page painted, a drawing rasterised.
//! The app asks for it by name and the host answers — now (`engine.call`), for work that ends
//! within a frame or two, as a job the device records and runs on without the app, running the
//! actions the start named as it moves and ends (`engine.start`, `engine.job`, `engine.cancel`),
//! or as a stream (`engine.stream`) whose
//! newest frame the device keeps and the app shows by its address while the stream runs. A
//! stream also says of itself what a view reads (`engine.state`): whether it is connecting, what
//! it is, or why it ended — pushed as it changes, so the window knows a connection dropped
//! without asking.
//!
//! What a name means is the device's, not the language's: the host registers the ops it answers
//! for each app and refuses a name the app does not own. The answer is JSON, which the app reads
//! into its own records with `json.decode<T>`, so one vocabulary serves every engine.

use std::sync::Arc;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i32, trap};
use crate::engine::{HostFail, Wasm};
use crate::run::JournalEntry;
use crate::value::Value;

/// One picture a running stream holds for its app: the stream's slot, the frame's number in that
/// stream, and its pixels — `width`×`height` RGBA8, straight alpha.
#[derive(Clone, Debug)]
pub struct Frame {
    pub slot: String,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u8>>,
}

/// What an engine hands its app's window as a stream runs: each frame, as it arrives, and what
/// the stream says of itself. The host keeps the newest of each and wakes the window, which
/// paints the frame and reads the state on its next draw; nothing here is polled.
pub trait Push: Send + Sync {
    /// The stream `frame.slot` shows `frame` now.
    fn frame(&self, frame: Frame);

    /// The stream `slot` says `state` of itself now, as JSON a view reads with `engine.state`:
    /// that it is connecting, what it shows once it runs, or why it ended. The host keeps the
    /// newest and wakes the window; a host that reads no stream state keeps nothing.
    fn state(&self, slot: &str, state: &str) {
        let _ = (slot, state);
    }

    /// The job `slot` ended, saying `state` of itself last: what it made, or why it stopped.
    fn ended(&self, slot: &str, state: &str) {
        self.state(slot, state);
    }
}

/// What a stream last said of itself, at the generation it said it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamState {
    pub generation: u64,
    pub text: String,
}

/// What wakes an app's window when a stream it shows has a new frame.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The newest frame of every stream the device's engines ran: what its window paints, and what an
/// embedder reads by the stream's slot.
#[derive(Default)]
pub struct LiveFrames {
    newest: std::sync::Mutex<std::collections::HashMap<String, Frame>>,
    /// What each stream last said of itself.
    states: std::sync::Mutex<std::collections::HashMap<String, StreamState>>,
    /// Counts what streams said, so a state pushed later is newer than every one before it,
    /// whichever stream said it.
    said: std::sync::atomic::AtomicU64,
    wake: std::sync::Mutex<Option<Wake>>,
}

/// A lock held even after a thread panicked while holding it: a frame lost one way must not
/// stop every later one.
fn held<T>(lock: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl LiveFrames {
    /// A new frame of the stream `frame.slot`: the newest one it shows from now on.
    pub fn put(&self, frame: Frame) {
        let slot = frame.slot.clone();
        held(&self.newest).insert(slot, frame);
        let wake = held(&self.wake).clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// The newest frame of the stream `slot`.
    #[must_use]
    pub fn get(&self, slot: &str) -> Option<Frame> {
        held(&self.newest).get(slot).cloned()
    }

    /// What the stream `slot` says of itself now.
    pub fn put_state(&self, slot: &str, text: &str) {
        let generation = self
            .said
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        held(&self.states).insert(
            slot.to_owned(),
            StreamState {
                generation,
                text: text.to_owned(),
            },
        );
        let wake = held(&self.wake).clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// What the stream `slot` says of itself, and the generation it said it at.
    #[must_use]
    pub fn state(&self, slot: &str) -> Option<StreamState> {
        held(&self.states).get(slot).cloned()
    }

    /// What every stream says of itself now.
    #[must_use]
    pub fn states(&self) -> Vec<(String, StreamState)> {
        held(&self.states)
            .iter()
            .map(|(slot, state)| (slot.clone(), state.clone()))
            .collect()
    }

    /// Forget the stream `slot`: one that ended shows nothing rather than its last frame, and
    /// says nothing of itself rather than its last word.
    pub fn forget(&self, slot: &str) {
        held(&self.newest).remove(slot);
        held(&self.states).remove(slot);
    }

    /// Wake `wake` whenever a frame arrives: the window showing the streams.
    pub fn wake_with(&self, wake: Wake) {
        *held(&self.wake) = Some(wake);
    }
}

impl Push for LiveFrames {
    fn frame(&self, frame: Frame) {
        self.put(frame);
    }

    fn state(&self, slot: &str, state: &str) {
        self.put_state(slot, state);
    }

    fn ended(&self, slot: &str, state: &str) {
        self.put_state(slot, state);
    }
}

/// What the device answers an app's engine work with (`Runtime::set_engine`).
pub trait EngineHost {
    /// One request answered now: the app asking, the op it names, and its input as the app wrote
    /// it. The answer is JSON on success, or the words of a refusal.
    ///
    /// # Errors
    ///
    /// A refusal the app reads: an op it does not own, an input it does not accept, a device that
    /// cannot do the work.
    fn call(&self, app: &str, op: &str, input: &str) -> Result<String, String>;

    /// One request started as a job: the same arguments as [`Self::call`], and what the job
    /// runs as it moves (`handlers`), answered with the job's path
    /// (`devices/<device>/jobs/<job>`), which [`Self::job`] and [`Self::cancel`] take. The
    /// device that holds the job runs the handlers as calls of the caller, children of the
    /// call that started it, whether or not the app's window or process is still there.
    ///
    /// # Errors
    ///
    /// As [`Self::call`], for a request that cannot start.
    fn start(
        &self,
        app: &str,
        op: &str,
        input: &str,
        handlers: &JobHandlers,
    ) -> Result<String, String>;

    /// One stream started: a job that keeps running and hands `push` its newest frame as each one
    /// arrives, the app showing it by `tilo-live://<job>`. The device wakes the app's window after
    /// each frame, so the picture moves without the app asking how far the stream has got.
    ///
    /// # Errors
    ///
    /// As [`Self::call`], for a stream that cannot start; a device that runs no streams refuses
    /// here rather than answering a job that never sends a frame.
    fn stream(
        &self,
        app: &str,
        op: &str,
        input: &str,
        push: Arc<dyn Push>,
    ) -> Result<String, String> {
        let _ = (app, op, input, push);
        Err("this device streams nothing to its apps".to_owned())
    }

    /// The job `path` as the device that holds it records it now: its record's JSON
    /// (`{id, app, workspace, invocation, by, op, state, detail}`), read once through the one
    /// record read; `None` for a job no device records.
    ///
    /// # Errors
    ///
    /// The read was refused: the caller may not read the job's app.
    fn job(&self, path: &str) -> Result<Option<serde_json::Value>, String>;

    /// Stop a job. False when the host does not know it or it had already finished.
    fn cancel(&self, job: &str) -> bool;

    /// One check the project's host approved, started by its id as a job for the invocation the
    /// app's action runs for: answered with the job's id, which [`Self::job`] and
    /// [`Self::cancel`] are asked about. Its record says how far the host reports it has got,
    /// and once it ends, the receipt of what the host observed (`receipt`), why it did not run
    /// (`failed`), or that it was stopped (`cancelled`).
    ///
    /// # Errors
    ///
    /// Why no check starts: a device where no project's approved checks run, an action the
    /// workspace did not dispatch, an id the host does not take.
    fn check(&self, app: &str, check_id: &str) -> Result<String, String> {
        let _ = (app, check_id);
        Err(NO_APPROVED_CHECKS.to_owned())
    }
}

/// The code of this moment from a vault item's seed, as the host that holds the vault answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultCode {
    /// The digits, grouped as the app shows them being the app's own business: the vault returns
    /// them bare.
    pub code: String,
    /// The instant the code stops being good, as RFC 3339 text.
    pub until: String,
    /// How many seconds one code stands.
    pub period: i64,
    /// How many digits a code has.
    pub digits: i64,
    /// The hash the code is made with (`SHA1`, `SHA256` or `SHA512`).
    pub algorithm: String,
}

/// What answers `vault.code`: the vault of the workspace, where the seed is. A host that keeps no
/// vault answers nothing, and the view shows its waiting state.
pub trait VaultHost {
    /// The code the item `reference` names makes now, or nothing for an item this host cannot
    /// read.
    fn code(&self, reference: &str) -> Option<VaultCode>;
}

/// What a device that runs no project's approved checks answers `engine.check`.
pub const NO_APPROVED_CHECKS: &str = "No project's approved checks run on this device";

impl Core {
    /// `vault.code(reference)`: the code of this moment, as the vault answers it.
    pub(crate) fn vault_import(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let reference = self.text(a.h(0)?)?;
        let code = match self.vault.borrow().as_ref() {
            Some(vault) => vault.code(&reference),
            None => None,
        };
        let Some(code) = code else {
            return self.ret_h(Value::none());
        };
        let code = Value::Tuple(vec![
            Value::text(code.code),
            Value::text(code.until),
            Value::Int(code.period),
            Value::Int(code.digits),
            Value::text(code.algorithm),
        ]);
        self.ret_h(Value::some(code))
    }
}

/// What a job runs as it moves, as `engine.start` names it: the action `done:` names, run once
/// when the job ends however it ends, and `progress:`'s, run as it moves on. Each is the
/// handler's JSON ([`crate::value::job_handler_to_json`]), which the device that holds the job
/// keeps without reading and hands back to the app's runtime ([`crate::Runtime::job_call`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobHandlers {
    pub done: Option<serde_json::Value>,
    pub progress: Option<serde_json::Value>,
}

/// The window the readings of jobs are kept under.
const JOBS_WINDOW: &str = "$jobs";

/// The slot holding the job `path` as views read it.
pub(crate) fn job_key(path: &str) -> crate::view_state::StateKey {
    crate::view_state::StateKey::runtime(JOBS_WINDOW, None, path)
}

/// A job's record as the app reads it: a `host.Job`, its fields in the order the host declares
/// them. What the op says of itself stays the op's own JSON (`detail`), from which its progress
/// and its error are read where it says them.
#[must_use]
pub fn job_value(record: &serde_json::Value) -> Value {
    let text = |v: Option<&serde_json::Value>| {
        Value::text(v.and_then(serde_json::Value::as_str).unwrap_or_default())
    };
    let detail = record.get("detail");
    let int = |field: &str| {
        Value::Int(
            detail
                .and_then(|d| d.get(field))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
        )
    };
    let state = record
        .get("state")
        .and_then(serde_json::Value::as_str)
        .filter(|state| {
            matches!(
                *state,
                "queued" | "running" | "done" | "failed" | "cancelled"
            )
        })
        .unwrap_or("failed");
    let error = detail
        .and_then(|d| d.get("error"))
        .or_else(|| record.get("why"))
        .or_else(|| record.get("error"));
    Value::Tuple(vec![
        text(record.get("id")),
        text(record.get("op")),
        Value::Enum(crate::value::EnumValue {
            ty: tilolang::host::JOB_STATE_TYPE.to_owned(),
            variant: state.to_owned(),
            fields: std::collections::BTreeMap::new(),
        }),
        int("done"),
        int("total"),
        text(error),
        Value::text(detail.map(ToString::to_string).unwrap_or_default()),
    ])
}

/// The app's held connections as the device keeps them (`connection.*`): work with no end, such
/// as a language server or a gateway, that the app keeps open in a process of its own until it
/// or the person closes it. It is never an engine job, and closing a window ends none of it.
pub trait HeldHost {
    /// Open the app's connection `key` of `kind` with the JSON `ask`, or join it when one with
    /// that key is open already.
    ///
    /// # Errors
    ///
    /// The app keeps no connection of that kind, the ask is refused, or the connection cannot be
    /// made.
    fn open(&self, app: &str, kind: &str, key: &str, ask: &str) -> Result<String, String>;

    /// One message for connection `key`, answered now; whatever more it says arrives as its word.
    ///
    /// # Errors
    ///
    /// No such connection is open, or it refused the message.
    fn send(&self, key: &str, message: &str) -> Result<String, String>;

    /// End connection `key`: false when none was open.
    fn close(&self, key: &str) -> bool;

    /// Hand `push` every word a connection says of itself from now on, at the slot
    /// [`held_slot`] names, starting with the newest word of each.
    fn follow(&self, push: Arc<dyn Push>);
}

/// Where the word connection `key` said is kept beside the streams' words.
#[must_use]
pub fn held_slot(key: &str) -> String {
    format!("held:{key}")
}

impl Core {
    /// `connection.open`, `connection.send`, `connection.said` and `connection.close`.
    pub(crate) fn held_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::HeldOpen => {
                let app = self.text(a.h(0)?)?;
                let kind = self.text(a.h(1)?)?;
                let key = self.text(a.h(2)?)?;
                let ask = self.text(a.h(3)?)?;
                let name = format!("{kind}:{key}");
                let answer = self.device_work("connection.open", &name, &ask, || {
                    match self.held_work.borrow().as_ref() {
                        Some(host) => answered(host.open(&app, &kind, &key, &ask)),
                        None => Value::Error(no_held(&kind)),
                    }
                });
                self.result(answer.and_then(text_answer).map(Value::text))
            }
            Imp::HeldSend => {
                let key = self.text(a.h(0)?)?;
                let message = self.text(a.h(1)?)?;
                let answer = self.device_work("connection.send", &key, &message, || {
                    match self.held_work.borrow().as_ref() {
                        Some(host) => answered(host.send(&key, &message)),
                        None => Value::Error(format!("no connection `{key}` is open")),
                    }
                });
                self.result(answer.and_then(text_answer).map(Value::text))
            }
            Imp::HeldSaid => {
                let key = self.text(a.h(0)?)?;
                self.ret_h(self.engine_state(&held_slot(&key)))
            }
            Imp::HeldClose => {
                let key = self.text(a.h(0)?)?;
                let closed = self
                    .device_work("connection.close", &key, "", || {
                        Value::Bool(
                            self.held_work
                                .borrow()
                                .as_ref()
                                .is_some_and(|host| host.close(&key)),
                        )
                    })
                    .is_ok_and(|answer| answer == Value::Bool(true));
                // What it said goes with it: a view shows nothing of a connection that ended.
                self.live_frames.forget(&held_slot(&key));
                ret_i32(i32::from(closed))
            }
            _ => trap("not a held connection's import"),
        }
    }

    /// `engine.call`, `engine.start`, `engine.job`, `engine.stream`, `engine.state`,
    /// `engine.picture` and `engine.cancel`.
    pub(crate) fn engine_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::EngineCall => {
                let app = self.text(a.h(0)?)?;
                let op = self.text(a.h(1)?)?;
                let input = self.text(a.h(2)?)?;
                self.result(self.engine_ask(&app, &op, &input).map(Value::text))
            }
            Imp::EngineStart => {
                let app = self.text(a.h(0)?)?;
                let op = self.text(a.h(1)?)?;
                let input = self.text(a.h(2)?)?;
                let handlers = JobHandlers {
                    done: self.job_handler(a.h(3)?)?,
                    progress: self.job_handler(a.h(4)?)?,
                };
                self.result(
                    self.engine_begin(&app, &op, &input, &handlers)
                        .map(Value::text),
                )
            }
            Imp::EngineJob => {
                let job = self.text(a.h(0)?)?;
                self.ret_h(self.engine_job(&job))
            }
            Imp::EngineStream => {
                let app = self.text(a.h(0)?)?;
                let op = self.text(a.h(1)?)?;
                let input = self.text(a.h(2)?)?;
                self.result(self.engine_stream(&app, &op, &input).map(Value::text))
            }
            Imp::EngineState => {
                let job = self.text(a.h(0)?)?;
                self.ret_h(self.engine_state(&job))
            }
            Imp::EnginePicture => {
                let app = self.text(a.h(0)?)?;
                let op = self.text(a.h(1)?)?;
                let input = self.text(a.h(2)?)?;
                self.result(self.engine_picture(&app, &op, &input))
            }
            Imp::EngineCancel => {
                let job = self.text(a.h(0)?)?;
                ret_i32(i32::from(self.engine_stop(&job)))
            }
            Imp::EngineCheck => {
                let app = self.text(a.h(0)?)?;
                let check_id = self.text(a.h(1)?)?;
                self.result(self.engine_check(&app, &check_id).map(Value::text))
            }
            _ => trap("not an engine import"),
        }
    }

    fn engine_ask(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        let answer = self.device_work("call", op, input, || {
            match self.engine_work.borrow().as_ref() {
                Some(host) => answered(host.call(app, op, input)),
                None => Value::Error(no_engine()),
            }
        })?;
        text_answer(answer)
    }

    /// One picture the device's engine answers: the same request as `engine.call`, whose answer
    /// names the bytes the device kept (`url`, `name`, `media`, `size`), read into the `File` the
    /// app shows by its address.
    fn engine_picture(&self, app: &str, op: &str, input: &str) -> Result<Value, String> {
        let text = self.engine_ask(app, op, input)?;
        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|why| format!("the picture's answer is no JSON: {why}"))?;
        let file = crate::blob::file_from_json(&json)?;
        Ok(Value::file(file))
    }

    /// What `done:` or `progress:` names, as the job keeps it: none, or the handler's JSON.
    fn job_handler(&self, h: i32) -> HResult<Option<serde_json::Value>> {
        match self.val(h)? {
            Value::Opt(None) => Ok(None),
            Value::Opt(Some(handler)) => match *handler {
                Value::Action(handler) => crate::value::job_handler_to_json(&handler)
                    .map(Some)
                    .map_err(HostFail::Trap),
                other => trap(format!(
                    "a job's handler is an action, not a {}",
                    other.kind_name()
                )),
            },
            other => trap(format!(
                "a job's handler is an optional action, not a {}",
                other.kind_name()
            )),
        }
    }

    fn engine_begin(
        &self,
        app: &str,
        op: &str,
        input: &str,
        handlers: &JobHandlers,
    ) -> Result<String, String> {
        let answer = self.device_work("start", op, input, || {
            match self.engine_work.borrow().as_ref() {
                Some(host) => answered(host.start(app, op, input, handlers)),
                None => Value::Error(no_engine()),
            }
        })?;
        text_answer(answer)
    }

    /// One check the project's host approved, started as a job: asked of the host once, so a
    /// run that runs again after a call it made follows the job it started rather than starting
    /// the check twice.
    fn engine_check(&self, app: &str, check_id: &str) -> Result<String, String> {
        let answer = self.device_work("check", check_id, "", || {
            match self.engine_work.borrow().as_ref() {
                Some(host) => answered(host.check(app, check_id)),
                None => Value::Error(NO_APPROVED_CHECKS.to_owned()),
            }
        })?;
        text_answer(answer)
    }

    /// One stream the device's engine runs: the same request as `engine.start`, whose frames the
    /// device keeps and the app's window shows by `tilo-live://<job>`.
    fn engine_stream(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        let answer = self.device_work("stream", op, input, || {
            let push: Arc<dyn Push> = Arc::clone(&self.live_frames) as Arc<dyn Push>;
            match self.engine_work.borrow().as_ref() {
                Some(host) => answered(host.stream(app, op, input, push)),
                None => Value::Error(no_engine()),
            }
        })?;
        text_answer(answer)
    }

    /// What the stream `job` says of itself now, as a view reads it (`engine.state`): the newest
    /// word the device pushed, empty while it said nothing. In a view the read is a dependency:
    /// the boundary runs again when the stream says something new, whichever frame it arrives on.
    fn engine_state(&self, job: &str) -> Value {
        let window = self.frames.borrow().view().map(|vs| vs.window.clone());
        let Some(window) = window else {
            // An action reads one word throughout: the newest the stream said, never a
            // dependency it would have to be run again for.
            return Value::text(
                self.live_frames
                    .state(job)
                    .map(|state| state.text)
                    .unwrap_or_default(),
            );
        };
        let key = crate::view_state::stream_key(&window, job);
        let (generation, seen) = {
            let states = self.view_states.borrow();
            (
                states.generation(&key),
                states.get(&key).cloned().unwrap_or_else(|| Value::text("")),
            )
        };
        self.note_state_read(key, generation, &seen);
        seen
    }

    /// The job `path` as its device records it (`engine.job`). A view reads the record the host
    /// follows for it, and runs again as the job moves: the first read asks the host to follow
    /// it ([`crate::Runtime::take_wanted_jobs`]). An action reads it once, as it stands now.
    fn engine_job(&self, path: &str) -> Value {
        let key = job_key(path);
        if self.frames.borrow().view().is_some() {
            if !self.jobs_followed.borrow().contains(path) {
                self.jobs_wanted.borrow_mut().insert(path.to_owned());
            }
            let (generation, seen) = {
                let states = self.view_states.borrow();
                (
                    states.generation(&key),
                    states.get(&key).cloned().unwrap_or_else(Value::none),
                )
            };
            self.note_state_read(key, generation, &seen);
            return seen;
        }
        let answer = self.device_work("job", path, "", || {
            match self.engine_work.borrow().as_ref() {
                Some(host) => match host.job(path) {
                    Ok(Some(record)) => Value::some(job_value(&record)),
                    Ok(None) => Value::none(),
                    Err(why) => Value::Error(why),
                },
                None => Value::Error(no_engine()),
            }
        });
        match answer {
            Ok(Value::Error(_)) | Err(_) => Value::none(),
            Ok(record) => record,
        }
    }

    fn engine_stop(&self, job: &str) -> bool {
        // A stream's frames end with its job: the window shows nothing of it, not its last frame.
        self.live_frames.forget(job);
        self.device_work("cancel", job, "", || {
            Value::Bool(
                self.engine_work
                    .borrow()
                    .as_ref()
                    .is_some_and(|host| host.cancel(job)),
            )
        })
        .is_ok_and(|answer| answer == Value::Bool(true))
    }

    /// Answer the device work `kind` names, asked of it once: a run that is run again after a
    /// call it made answers from its journal, so it does not ask the device twice and does not
    /// take another branch on an answer that moved on.
    fn device_work(
        &self,
        kind: &str,
        name: &str,
        input: &str,
        live: impl FnOnce() -> Value,
    ) -> Result<Value, String> {
        {
            let mut frames = self.frames.borrow_mut();
            let Some(run) = frames.run_mut() else {
                drop(frames);
                return Ok(live());
            };
            if let Some(entry) = run.journal.entries.get(run.cursor) {
                let JournalEntry::Engine {
                    kind: before_kind,
                    name: before_name,
                    input: before_input,
                    answer,
                } = entry
                else {
                    return Err(
                        "the run diverged from its journal: expected the device's answer"
                            .to_owned(),
                    );
                };
                if before_kind != kind || before_name != name || before_input != input {
                    return Err(format!(
                        "the run diverged from its journal: it now asks the device to {kind} \
                         `{name}` with {input:?}, where it asked it to {before_kind} \
                         `{before_name}` with {before_input:?}"
                    ));
                }
                let answer = answer.clone();
                run.cursor = run.cursor.saturating_add(1);
                return Ok(answer);
            }
        }
        let answer = live();
        self.journal_push(JournalEntry::Engine {
            kind: kind.to_owned(),
            name: name.to_owned(),
            input: input.to_owned(),
            answer: answer.clone(),
        })
        .map_err(|fail| match fail {
            HostFail::Trap(why) => why,
            HostFail::Suspend => "answering the device's work cannot wait".to_owned(),
        })?;
        Ok(answer)
    }
}

/// What an open answers on a device that holds no connection for its apps.
fn no_held(kind: &str) -> String {
    format!("this device holds no `{kind}` connection for the app")
}

/// A device's answer as a journal keeps it: what it said, or why it refused.
fn answered(result: Result<String, String>) -> Value {
    match result {
        Ok(text) => Value::Text(text),
        Err(why) => Value::Error(why),
    }
}

/// The text a device answered, or the refusal it gave.
fn text_answer(answer: Value) -> Result<String, String> {
    match answer {
        Value::Text(text) => Ok(text),
        Value::Error(why) => Err(why),
        other => Err(format!(
            "the device answered a {}, not text",
            other.kind_name()
        )),
    }
}

/// What an app reads when the device answers no engine work at all: never a false success.
fn no_engine() -> String {
    "this device answers no engine work for its apps".to_owned()
}
