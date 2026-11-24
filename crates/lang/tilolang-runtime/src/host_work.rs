//! The device's own work, asked of it by name.
//!
//! An app runs sandboxed, with no decoder, no renderer and no graphics stack of its own. Work
//! that needs one is the device's: a video cut rendered, a page painted, a drawing rasterised.
//! The app asks for it by name and the host answers — now (`engine.call`), for work that ends
//! within a frame or two, as a job that says how far it has got and can be stopped
//! (`engine.start`, `engine.status`, `engine.cancel`), or as a stream (`engine.stream`) whose
//! newest frame the device keeps and the app shows by its address while the stream runs. A
//! stream also says of itself what a view reads (`engine.state`): whether it is connecting, what
//! it is, or why it ended — pushed as it changes, so the window knows a connection dropped
//! without asking.
//!
//! A job says how far it has got the same way, pushed as it moves, and says when it ends. A job
//! started with `done:` (`engine.start(op, input, done: imported(lib))`) is held work: when it
//! ends, the action runs as the one who started it, with the job's last word as its last
//! argument ([`crate::Runtime::finish_jobs`]), whether or not a window shows the app then.
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

    /// The job `slot` ended, saying `state` of itself last: what it made, or why it stopped. A
    /// view reads it as it reads any state the job pushed, and the action started with the job
    /// (`done:`) runs with it.
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
    /// The jobs that ended since the runtime last looked, oldest first: whose `done:` runs next.
    ended: std::sync::Mutex<Vec<String>>,
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

    /// The job `slot` ended, saying `text` last: kept as what it says of itself, and queued for
    /// the action it was started with.
    pub fn put_ended(&self, slot: &str, text: &str) {
        held(&self.ended).push(slot.to_owned());
        self.put_state(slot, text);
    }

    /// The jobs that ended since this was last asked, oldest first.
    pub fn take_ended(&self) -> Vec<String> {
        std::mem::take(&mut *held(&self.ended))
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
        self.put_ended(slot, state);
    }
}

/// Hold the caller between asks of a job it waits on. A device with no thread to wait on — a page
/// in a browser — refuses instead of spinning, so the work is asked for with `engine.start` and
/// answered by the job's `done:` there.
fn wait_for_job() -> Result<(), String> {
    /// How long a wait sleeps between asks of the job, so a quick job is not held back.
    const POLL: std::time::Duration = std::time::Duration::from_millis(5);
    if cfg!(target_arch = "wasm32") {
        return Err(
            "this page has no thread to wait on the device's work: start it with `engine.start` \
             and its `done:`"
                .to_owned(),
        );
    }
    std::thread::sleep(POLL);
    Ok(())
}

/// Whether the job state `state` says the job ended, as JSON `{"state": "done", ...}`: `Some(true)`
/// once it did, `Some(false)` while it still runs, `None` when it names no state this contract
/// knows.
fn ended(state: &str) -> Option<bool> {
    let parsed: serde_json::Value = serde_json::from_str(state).ok()?;
    match parsed.get("state")?.as_str()? {
        "queued" | "running" => Some(false),
        "done" | "failed" | "cancelled" => Some(true),
        _ => None,
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

    /// One request started as a job: the same arguments as [`Self::call`], answered with the
    /// job's id, which [`Self::status`] and [`Self::cancel`] are asked about.
    ///
    /// # Errors
    ///
    /// As [`Self::call`], for a request that cannot start.
    fn start(&self, app: &str, op: &str, input: &str) -> Result<String, String>;

    /// [`Self::start`], with `push` for what the job says of itself as it moves
    /// ([`Push::state`]) and when it ends ([`Push::ended`]): the job's record, which a view reads
    /// with `engine.state(job)` and never by asking the engine. An engine that pushes nothing
    /// starts the job as [`Self::start`] does.
    ///
    /// # Errors
    ///
    /// As [`Self::start`].
    fn start_held(
        &self,
        app: &str,
        op: &str,
        input: &str,
        push: Arc<dyn Push>,
    ) -> Result<String, String> {
        let _ = push;
        self.start(app, op, input)
    }

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

    /// How far a job has got, as JSON: its state (`queued`, `running`, `done`, `failed` or
    /// `cancelled`), how much it has done of how much, what it made, or why it failed. A job the
    /// host does not know is `failed` with the reason, never a panic.
    ///
    /// # Errors
    ///
    /// A refusal to answer at all, which the app reads as the job's failure.
    fn status(&self, job: &str) -> Result<String, String>;

    /// Stop a job. False when the host does not know it or it had already finished.
    fn cancel(&self, job: &str) -> bool;

    /// One check the project's host approved, started by its id as a job for the invocation the
    /// app's action runs for: answered with the job's id, which [`Self::status`] and
    /// [`Self::cancel`] are asked about. Its status says how far the host reports it has got,
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

    /// `engine.call`, `engine.start`, `engine.check`, `engine.status` and `engine.cancel`.
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
                self.result(self.engine_begin(&app, &op, &input, None).map(Value::text))
            }
            Imp::EngineStartDone => {
                let app = self.text(a.h(0)?)?;
                let op = self.text(a.h(1)?)?;
                let input = self.text(a.h(2)?)?;
                let Value::Action(done) = self.val_whole(a.h(3)?)? else {
                    return trap("a job's `done:` is an action");
                };
                let started = self.engine_begin(&app, &op, &input, Some(done));
                self.result(started.map(Value::text))
            }
            Imp::EngineRun => {
                let app = self.text(a.h(0)?)?;
                let op = self.text(a.h(1)?)?;
                let input = self.text(a.h(2)?)?;
                self.result(self.engine_run(&app, &op, &input).map(Value::text))
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
            Imp::EngineStatus => {
                let job = self.text(a.h(0)?)?;
                self.result(self.engine_status(&job).map(Value::text))
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

    /// One job the device's engine runs, pushing what it says of itself to the window's record of
    /// it. With `done`, the job is held work: the action runs when the job ends, as the one who
    /// started it now ([`crate::Runtime::finish_jobs`]). A run replayed from its journal starts
    /// nothing again and holds nothing again.
    fn engine_begin(
        &self,
        app: &str,
        op: &str,
        input: &str,
        done: Option<crate::value::ActionRef>,
    ) -> Result<String, String> {
        let starter = match done {
            Some(_) => Some(self.looking().map_err(|fail| match fail {
                HostFail::Trap(why) => why,
                HostFail::Suspend => "a job is started by someone".to_owned(),
            })?),
            None => None,
        };
        let answer = self.device_work("start", op, input, || {
            let push: Arc<dyn Push> = Arc::clone(&self.live_frames) as Arc<dyn Push>;
            let begun = match self.engine_work.borrow().as_ref() {
                Some(host) => host.start_held(app, op, input, push),
                None => Err(no_engine()),
            };
            if let (Ok(said), Some(done), Some(starter)) = (&begun, done, starter) {
                self.job_done
                    .borrow_mut()
                    .insert(job_of(said), (starter, done));
            }
            answered(begun)
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

    /// `engine.run`: the device's work started and waited for, answering the job's own state as
    /// `engine.status` writes it once the job ends: `done` with what it made, `failed` with why,
    /// or `cancelled`. The whole wait is one ask of the device, so a run replayed answers from its
    /// journal instead of starting the work a second time.
    fn engine_run(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        let answer = self.device_work("run", op, input, || {
            let borrowing = self.engine_work.borrow();
            let Some(host) = borrowing.as_ref() else {
                return Value::Error(no_engine());
            };
            let job = match host.start(app, op, input) {
                Ok(job) => job,
                Err(why) => return Value::Error(why),
            };
            loop {
                let state = match host.status(&job) {
                    Ok(state) => state,
                    Err(why) => return Value::Error(why),
                };
                match ended(&state) {
                    Some(true) => return Value::text(state),
                    Some(false) => {
                        if let Err(why) = wait_for_job() {
                            return Value::Error(why);
                        }
                    }
                    None => {
                        return Value::Error(format!(
                            "the device answered how job `{job}` is doing without saying: {state}"
                        ));
                    }
                }
            }
        })?;
        text_answer(answer)
    }

    fn engine_status(&self, job: &str) -> Result<String, String> {
        let answer = self.device_work("status", job, "", || {
            match self.engine_work.borrow().as_ref() {
                Some(host) => answered(host.status(job)),
                None => Value::Error(no_engine()),
            }
        })?;
        text_answer(answer)
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

/// The job a start answered: the `job` its JSON names (`{"job": "scan-1"}`), or the whole
/// answer for an engine that answers the bare id.
pub(crate) fn job_of(said: &str) -> String {
    serde_json::from_str::<serde_json::Value>(said)
        .ok()
        .and_then(|json| {
            json.get("job")
                .and_then(|job| job.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| said.to_owned())
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
