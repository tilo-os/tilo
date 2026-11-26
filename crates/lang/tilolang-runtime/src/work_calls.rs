//! The host calls on the device's own work and what it holds, answered through
//! [`crate::host_work::EngineHost`], [`crate::host_work::VaultHost`] and
//! [`crate::host_work::HeldHost`].

use std::sync::Arc;

use tilolang_engine::Push;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i32, trap};
use crate::engine::{HostFail, Wasm};
use crate::host_work::{JobHandlers, NO_APPROVED_CHECKS, answered, held_slot, job_key, job_value};
use crate::run::JournalEntry;
use crate::value::Value;

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
