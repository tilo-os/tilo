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

use tilolang_engine::{Frame, Push};

use crate::value::Value;

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
        .and_then(|d| d.get("why").or_else(|| d.get("error")))
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

/// A device's answer as a journal keeps it: what it said, or why it refused.
pub(crate) fn answered(result: Result<String, String>) -> Value {
    match result {
        Ok(text) => Value::Text(text),
        Err(why) => Value::Error(why),
    }
}
