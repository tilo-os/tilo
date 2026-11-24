#![allow(
    clippy::doc_markdown,
    reason = "YouTube and Grayjay are names, not code: they read as written in prose"
)]

//! YouTube's Grayjay link, as a package reaches it: `engine.call` and `engine.start` answered
//! for `so.tilo.tube` by [`tilolang_grayjay`].
//!
//! The app runs sandboxed with no network and no secret store, so it asks by name. Pairing and
//! syncing touch the network, so they are jobs the app follows by id; the device's Grayjay key
//! is the platform's app-scoped secret, which this engine reads where it is and never hands to
//! the app. No other app is answered by this engine.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tilolang_tilo::engine::{JobOp, JobSink, Needs, Provider, State};

/// Register the engine once, at the host's first ask: every later one finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        tilolang_tilo::engine::register(
            tilolang_grayjay::APP,
            Arc::new(Grayjay {
                engine: tilolang_grayjay::Engine::production(),
                running: Mutex::new(BTreeMap::new()),
            }),
        );
    });
}

/// The registry's entry for the YouTube app.
struct Grayjay {
    engine: tilolang_grayjay::Engine,
    /// The bridge's own id of each job it runs, by the job's path: what a stop reaches.
    running: Mutex<BTreeMap<String, String>>,
}

impl Provider for Grayjay {
    fn jobs(&self) -> &'static [JobOp] {
        // Pairing and syncing reach other devices over the network with this device's Grayjay
        // key, the app's own secret.
        const JOBS: &[JobOp] = &[
            JobOp::new("pair", Needs::SECRET_AND_NETWORK),
            JobOp::new("sync", Needs::SECRET_AND_NETWORK),
        ];
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        self.engine.call(op, input)
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        let told: tilolang_grayjay::Told = Arc::new(move |word, detail| {
            moved.moved(State::from_word(word).unwrap_or(State::Failed), detail);
        });
        let id = self.engine.start_told(op, input, Some(told))?;
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job.to_owned(), id);
        Ok(())
    }

    fn cancel(&self, job: &str) -> bool {
        let id = self
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(job);
        id.is_some_and(|id| self.engine.cancel(&id))
    }
}
