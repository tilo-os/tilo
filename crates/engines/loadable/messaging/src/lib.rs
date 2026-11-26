//! Messages' device work, as its own library in the runtime bundle
//! (`libtilo_messaging_engine.so`), opened the first time `so.tilo.messages`
//! asks for the person's messaging accounts.
//!
//! The app runs sandboxed and reaches the messaging service only through the
//! connection the launcher handed its process (`[sandbox.services] messaging`):
//! this engine answers `engine.call` and `engine.start` over that connection
//! (`tilo_messaging::engine`). The process that hosts the app opens this
//! library once and files it under the app's name, so no other app is
//! answered by it, and the service admits Messages alone.
//!
//! Outside the thin build this crate is an empty library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
mod engine {
    use std::sync::OnceLock;

    use tilo_engine_loader::{Device, EngineApi, Frames, JobOp, JobSink, Needs};
    use tilo_messaging::engine::Engine;

    /// The app whose device work this is: the one that asks these ops, and no
    /// other.
    pub const APP: &str = tilo_messaging::service::MESSAGES_APP;

    /// The connection this process was handed, taken once.
    fn engine() -> &'static Engine {
        static ENGINE: OnceLock<Engine> = OnceLock::new();
        ENGINE.get_or_init(Engine::inherited)
    }

    /// The jobs this engine runs: each waits on the service, over the
    /// connection only the app's own process holds.
    fn jobs() -> &'static [JobOp] {
        static JOBS: OnceLock<Vec<JobOp>> = OnceLock::new();
        JOBS.get_or_init(|| {
            tilo_messaging::engine::JOBS
                .iter()
                .map(|op| JobOp::new(op, Needs::APP_STATE))
                .collect()
        })
    }

    /// One `engine.call` for this app.
    fn call(_device: &Device, op: &str, input: &str) -> Result<String, String> {
        engine().call(op, input)
    }

    /// One `engine.start`: the job runs on a thread of its own and tells
    /// each move to the sink, which outlives it.
    fn start(
        _device: &Device,
        job: &str,
        op: &str,
        input: &str,
        moved: &JobSink,
    ) -> Result<(), String> {
        let moved = *moved;
        engine().start(job, op, input, move |state, detail| {
            moved.moved(state, detail);
        })
    }

    /// One job's progress: the device keeps each job's record.
    fn status(_device: &Device, _job: &str) -> Result<String, String> {
        Ok("{}".to_owned())
    }

    /// One stream: this engine runs none.
    fn stream(
        _device: &Device,
        _op: &str,
        _input: &str,
        _frames: &Frames,
    ) -> Result<String, String> {
        Err("this engine runs no streams".to_owned())
    }

    /// One stopped job: its answer is no longer anyone's.
    fn cancel(_device: &Device, job: &str) -> bool {
        engine().cancel(job)
    }

    /// The one engine this library answers with.
    static API: EngineApi = EngineApi {
        app: APP,
        jobs,
        call,
        start,
        status,
        stream,
        cancel,
    };

    /// The entry the process opens this library for.
    #[unsafe(no_mangle)]
    pub fn tilo_engine_entry() -> &'static EngineApi {
        &API
    }

    /// Proves this engine loads and runs against the runtime it is built
    /// with: with no service connected, the accounts it is asked about stand
    /// unavailable, as the app reads them.
    pub fn self_check() -> Result<String, String> {
        let answer = Engine::default().call(
            "state",
            r#"{"accounts":[{"service":"signal","account":"a1"}]}"#,
        )?;
        if !answer.contains(r#""state":"unavailable""#) {
            return Err(format!("the check's answer names no state: {answer}"));
        }
        Ok(format!("read account states, {} bytes", answer.len()))
    }
}

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_messaging_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-messaging-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-messaging-engine: {error}");
            false
        }
    }
}
