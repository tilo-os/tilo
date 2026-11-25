//! Chip Design's device work, as its own library in the runtime bundle
//! (`libtilo_chip_engine.so`), opened the first time `so.tilo.chip-design`
//! asks it to check a document or resolve a design.
//!
//! An app runs sandboxed, so it asks this engine through `engine.call`; the
//! process that hosts it opens this library once and files it under the app's
//! name, so no other app is answered by it. Every op answers now: a document is
//! checked in one breath, so the engine runs no jobs.
//!
//! Outside the thin build this crate is an empty library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
mod engine {
    use tilo_engine_loader::{Device, EngineApi, Frames, JobOp, JobSink};

    /// The app whose device work this is: the one that asks these ops, and no
    /// other.
    pub const APP: &str = tilo_chip_design_engine::APP;

    /// The jobs this engine runs: none.
    fn jobs() -> &'static [JobOp] {
        &[]
    }

    /// One `engine.call` for this app.
    fn call(_device: &Device, op: &str, input: &str) -> Result<String, String> {
        tilo_chip_design_engine::call(op, input)
    }

    /// One `engine.start`: this engine runs no jobs.
    fn start(
        _device: &Device,
        _job: &str,
        op: &str,
        _input: &str,
        _moved: &JobSink,
    ) -> Result<(), String> {
        Err(format!("`{op}` is no work this device starts"))
    }

    /// One job's progress: this engine runs no jobs.
    fn status(_device: &Device, _job: &str) -> Result<String, String> {
        Err("this engine runs no jobs".to_owned())
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

    /// One stopped job: this engine runs none, so it holds none.
    fn cancel(_device: &Device, _job: &str) -> bool {
        false
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

    /// Proves this engine loads and runs against the runtime it is built with:
    /// an empty design resolved to the answer the app reads.
    pub fn self_check() -> Result<String, String> {
        let answer = tilo_chip_design_engine::call("graph", "{}")?;
        if !answer.starts_with('{') {
            return Err(format!("the check's answer is no object: {answer}"));
        }
        Ok(format!("resolved an empty design: {answer}"))
    }
}

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_chip_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-chip-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-chip-engine: {error}");
            false
        }
    }
}
