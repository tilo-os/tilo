//! The work this session does for its apps: what `engine.call`, `engine.start`, `engine.status`
//! and `engine.cancel` are answered with.
//!
//! An app runs sandboxed and can decode nothing, paint nothing and raster nothing itself, so the
//! device does it: a video cut rendered, a page painted, a drawing rasterised. A crate that owns
//! such an engine registers it for its app at its start ([`register`]), and the app asks by name.
//! Every name is the registering app's: an app asks its own ops and no other's, so one app can
//! never borrow another's engine.
//!
//! An op answers now ([`Provider::call`]) or runs as a job ([`Provider::start`]), which the app
//! asks about by the id it was given ([`Provider::status`]) and can stop ([`Provider::cancel`]).
//! A job whose work is a moving picture is a stream ([`Provider::stream`]): its frames reach the
//! app's window as they arrive, and it is shown by `tilo-live://<job>`. Answers are JSON; the app
//! reads them into its own records.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tilolang_runtime::host_work::Push;

/// One app's engine, as the crate that owns it answers the session's asks.
pub trait Provider {
    /// One request answered now. JSON on success, or the words of a refusal.
    ///
    /// # Errors
    ///
    /// An input the engine does not accept, or work it cannot do.
    fn call(&self, op: &str, input: &str) -> Result<String, String>;

    /// One request started as a job: answered with the job's id.
    ///
    /// # Errors
    ///
    /// As [`Self::call`], for a request that cannot start.
    fn start(&self, op: &str, input: &str) -> Result<String, String>;

    /// [`Self::start`], with `push` for the job's record: what it says of itself as it moves
    /// ([`Push::state`]) and when it ends ([`Push::ended`]), which the app's views read and its
    /// `done:` action runs on. An engine that pushes nothing starts as [`Self::start`] does.
    ///
    /// # Errors
    ///
    /// As [`Self::start`].
    fn start_held(&self, op: &str, input: &str, push: Arc<dyn Push>) -> Result<String, String> {
        let _ = push;
        self.start(op, input)
    }

    /// One stream started: a job that hands `push` its newest frame as each arrives, the app
    /// showing it by `tilo-live://<job>`. The engine wakes the app's window after each frame.
    ///
    /// # Errors
    ///
    /// As [`Self::call`], for a stream that cannot start, or an engine that runs none.
    fn stream(&self, op: &str, input: &str, push: Arc<dyn Push>) -> Result<String, String> {
        let _ = (op, input, push);
        Err("this engine runs no streams".to_owned())
    }

    /// How far a job has got, as JSON.
    ///
    /// # Errors
    ///
    /// A refusal to answer at all, which the app reads as the job's failure.
    fn status(&self, job: &str) -> Result<String, String>;

    /// Stop a job: false when it is not this engine's or it had finished.
    fn cancel(&self, job: &str) -> bool;
}

/// A provider that answers nothing: what an app's engine stands as until its crate registers one.
struct Unregistered;

impl Provider for Unregistered {
    fn call(&self, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("`{op}` is no op this device runs"))
    }

    fn start(&self, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("`{op}` is no op this device runs"))
    }

    fn stream(&self, op: &str, _input: &str, _push: Arc<dyn Push>) -> Result<String, String> {
        Err(format!("`{op}` is no stream this device runs"))
    }

    fn status(&self, job: &str) -> Result<String, String> {
        Err(format!("no job `{job}` runs on this device"))
    }

    fn cancel(&self, _job: &str) -> bool {
        false
    }
}

/// The engines registered so far, by the app that owns each: one registry for the device,
/// which the session's app host and the crates that register engines both reach.
fn engines() -> &'static Mutex<BTreeMap<String, Arc<dyn Provider + Send + Sync>>> {
    static ENGINES: OnceLock<Mutex<BTreeMap<String, Arc<dyn Provider + Send + Sync>>>> =
        OnceLock::new();
    ENGINES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The registry, held: a lock poisoned by a panic in one engine still registers and answers
/// the rest, since what it guards is a map of providers and no half-written value.
fn held() -> MutexGuard<'static, BTreeMap<String, Arc<dyn Provider + Send + Sync>>> {
    engines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Register `provider` as the engine of `app`: the ops the app names are answered by it from
/// then on. A crate calls this once, at its start, for the app whose engine it owns.
pub fn register(app: &str, provider: Arc<dyn Provider + Send + Sync>) {
    held().insert(app.to_owned(), provider);
}

/// The engine of `app`, or one that refuses every ask: an app whose engine has not been
/// registered yet is refused, never answered by another app's.
fn engine_of(app: &str) -> Arc<dyn Provider + Send + Sync> {
    held()
        .get(app)
        .cloned()
        .unwrap_or_else(|| Arc::new(Unregistered))
}

/// One request answered now for `app`.
///
/// # Errors
///
/// The engine's refusal, or that this device runs none for `app`.
pub fn answer(app: &str, op: &str, input: &str) -> Result<String, String> {
    engine_of(app).call(op, input)
}

/// One request started as a job for `app`.
///
/// # Errors
///
/// The engine's refusal, or that this device runs none for `app`.
pub fn begin(app: &str, op: &str, input: &str) -> Result<String, String> {
    engine_of(app).start(op, input)
}

/// One job started for `app`, its record pushed to `push` as it moves and when it ends.
///
/// # Errors
///
/// The engine's refusal, or that this device runs none for `app`.
pub fn begin_held(app: &str, op: &str, input: &str, push: Arc<dyn Push>) -> Result<String, String> {
    engine_of(app).start_held(op, input, push)
}

/// One stream started for `app`: its frames reach `push` as the device receives them.
///
/// # Errors
///
/// The engine's refusal, or that this device runs none for `app`.
pub fn stream(app: &str, op: &str, input: &str, push: Arc<dyn Push>) -> Result<String, String> {
    engine_of(app).stream(op, input, push)
}

/// How far the job `job` has got.
///
/// # Errors
///
/// The engine's refusal: no engine knows that job.
pub fn progress(job: &str) -> Result<String, String> {
    // A job belongs to whichever engine started it; each is asked in turn, and the first that
    // knows it answers. Engines refuse a job that is not theirs.
    let providers: Vec<Arc<dyn Provider + Send + Sync>> = held().values().cloned().collect();
    for provider in providers {
        if let Ok(answer) = provider.status(job) {
            return Ok(answer);
        }
    }
    Err(format!("no job `{job}` runs on this device"))
}

/// Stop the job `job`: false when no engine runs it or it had finished.
pub fn stop(job: &str) -> bool {
    let providers: Vec<Arc<dyn Provider + Send + Sync>> = held().values().cloned().collect();
    providers.iter().any(|provider| provider.cancel(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Cut;

    impl Provider for Cut {
        fn call(&self, op: &str, input: &str) -> Result<String, String> {
            match op {
                "cut.probe" => Ok(format!(r#"{{"input":"{input}"}}"#)),
                _ => Err(format!("`{op}` is not a cut op")),
            }
        }

        fn start(&self, op: &str, _input: &str) -> Result<String, String> {
            match op {
                "cut.render" => Ok("render-1".to_owned()),
                _ => Err(format!("`{op}` is not a cut op")),
            }
        }

        fn status(&self, job: &str) -> Result<String, String> {
            match job {
                "render-1" => Ok(r#"{"state":"done"}"#.to_owned()),
                _ => Err(format!("no job `{job}`")),
            }
        }

        fn cancel(&self, job: &str) -> bool {
            job == "render-1"
        }
    }

    #[test]
    fn an_app_s_ops_reach_its_own_engine() {
        register("so.tilo.cut", Arc::new(Cut));
        assert_eq!(
            answer("so.tilo.cut", "cut.probe", "x"),
            Ok(r#"{"input":"x"}"#.to_owned())
        );
        assert_eq!(
            begin("so.tilo.cut", "cut.render", "{}"),
            Ok("render-1".to_owned())
        );
        assert_eq!(progress("render-1"), Ok(r#"{"state":"done"}"#.to_owned()));
        assert!(stop("render-1"));
    }

    #[test]
    fn an_app_never_borrows_another_s_engine() {
        register("so.tilo.cut", Arc::new(Cut));
        assert_eq!(
            answer("so.tilo.other", "cut.probe", "x"),
            Err("`cut.probe` is no op this device runs".to_owned())
        );
        assert_eq!(
            begin("so.tilo.other", "cut.render", "{}"),
            Err("`cut.render` is no op this device runs".to_owned())
        );
        // A job belongs to the engine that started it, so a job nobody started is nobody's.
        assert!(!stop("render-2"));
        assert_eq!(
            progress("render-2"),
            Err("no job `render-2` runs on this device".to_owned())
        );
    }

    #[test]
    fn an_op_the_engine_does_not_know_is_refused_in_its_words() {
        register("so.tilo.cut", Arc::new(Cut));
        assert_eq!(
            answer("so.tilo.cut", "cut.paint", ""),
            Err("`cut.paint` is not a cut op".to_owned())
        );
    }
}
