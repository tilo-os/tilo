//! The device's own work, asked of it by name.
//!
//! An app runs sandboxed, with no decoder, no renderer and no graphics stack of its own. Work
//! that needs one is the device's: a video cut rendered, a page painted, a drawing rasterised.
//! The app asks for it by name and the host answers — now (`engine.call`), for work that ends
//! within a frame or two, or as a job that says how far it has got and can be stopped
//! (`engine.start`, `engine.status`, `engine.cancel`).
//!
//! What a name means is the device's, not the language's: the host registers the ops it answers
//! for each app and refuses a name the app does not own. The answer is JSON, which the app reads
//! into its own records with `json.decode<T>`, so one vocabulary serves every engine.

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i32, trap};
use crate::engine::{HostFail, Wasm};
use crate::run::JournalEntry;
use crate::value::Value;

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
