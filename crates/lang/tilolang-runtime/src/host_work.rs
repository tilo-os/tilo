//! The device's own work, asked of it by name.
//!
//! An app runs sandboxed, with no decoder, no renderer and no graphics stack of its own. Work
//! that needs one is the device's: a video cut rendered, a page painted, a drawing rasterised.
//! The app asks for it by name and the host answers — now (`engine.call`), for work that ends
//! within a frame or two, as a job that says how far it has got and can be stopped
//! (`engine.start`, `engine.status`, `engine.cancel`), or as a stream (`engine.stream`) whose
//! newest frame the device keeps and the app shows by its address while the stream runs.
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

/// What an engine hands its app's window as a stream runs: each frame, as it arrives. The host
/// keeps the newest frame of each slot and wakes the window, which paints it on its next draw;
/// nothing here is polled.
pub trait Push: Send + Sync {
    /// The stream `frame.slot` shows `frame` now.
    fn frame(&self, frame: Frame);
}

/// What wakes an app's window when a stream it shows has a new frame.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The newest frame of every stream the device's engines ran: what its window paints, and what an
/// embedder reads by the stream's slot.
#[derive(Default)]
pub struct LiveFrames {
    newest: std::sync::Mutex<std::collections::HashMap<String, Frame>>,
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

    /// Forget the stream `slot`: one that ended shows nothing rather than its last frame.
    pub fn forget(&self, slot: &str) {
        held(&self.newest).remove(slot);
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
}

impl Core {
    /// `engine.call`, `engine.start`, `engine.status` and `engine.cancel`.
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
                self.result(self.engine_begin(&app, &op, &input).map(Value::text))
            }
            Imp::EngineStream => {
                let app = self.text(a.h(0)?)?;
                let op = self.text(a.h(1)?)?;
                let input = self.text(a.h(2)?)?;
                self.result(self.engine_stream(&app, &op, &input).map(Value::text))
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

    fn engine_begin(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        let answer = self.device_work("start", op, input, || {
            match self.engine_work.borrow().as_ref() {
                Some(host) => answered(host.start(app, op, input)),
                None => Value::Error(no_engine()),
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
