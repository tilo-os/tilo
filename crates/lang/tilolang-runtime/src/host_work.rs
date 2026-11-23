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
use crate::engine::Wasm;
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
        match self.engine.borrow().as_ref() {
            Some(host) => host.call(app, op, input),
            None => Err(no_engine()),
        }
    }

    fn engine_begin(&self, app: &str, op: &str, input: &str) -> Result<String, String> {
        match self.engine.borrow().as_ref() {
            Some(host) => host.start(app, op, input),
            None => Err(no_engine()),
        }
    }

    fn engine_status(&self, job: &str) -> Result<String, String> {
        match self.engine.borrow().as_ref() {
            Some(host) => host.status(job),
            None => Err(no_engine()),
        }
    }

    fn engine_stop(&self, job: &str) -> bool {
        self.engine
            .borrow()
            .as_ref()
            .is_some_and(|host| host.cancel(job))
    }
}

/// What an app reads when the device answers no engine work at all: never a false success.
fn no_engine() -> String {
    "this device answers no engine work for its apps".to_owned()
}
