//! Authenticator's device work, as its own library in the runtime bundle
//! (`libtilo_totp_engine.so`), opened the first time `so.tilo.totp` reads a
//! setup link, decodes a picture's QR code or checks a key.
//!
//! An app runs sandboxed with no file system of its own, so it asks this engine
//! through `engine.call`; the process that hosts it opens this library once and
//! files it under the app's name, so no other app is answered by it. Every op
//! answers now: Authenticator asks its device no job.
//!
//! Outside the thin build this crate is an empty library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
mod engine {
    use tilo_engine_loader::{Device, EngineApi, Frames, JobOp, JobSink};

    /// The app whose device work this is: the one that asks these ops, and no
    /// other.
    pub const APP: &str = totp_engine::APP;

    /// The jobs this engine runs: none.
    fn jobs() -> &'static [JobOp] {
        &[]
    }

    /// One `engine.call` for this app.
    fn call(_device: &Device, op: &str, input: &str) -> Result<String, String> {
        totp_engine::TotpEngine::new().call(op, input)
    }

    /// One `engine.start`: this engine runs no jobs.
    fn start(
        _device: &Device,
        _job: &str,
        op: &str,
        _input: &str,
        _moved: &JobSink,
    ) -> Result<(), String> {
        totp_engine::TotpEngine::new().start(op, "").map(|_| ())
    }

    /// One job's progress: this engine runs no jobs.
    fn status(_device: &Device, job: &str) -> Result<String, String> {
        totp_engine::TotpEngine::new().status(job)
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
    fn cancel(_device: &Device, job: &str) -> bool {
        totp_engine::TotpEngine::new().cancel(job)
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
    /// the setup link the app's paste field reads first, read back by its
    /// issuer.
    pub fn self_check() -> Result<String, String> {
        let answer = totp_engine::TotpEngine::new().call(
            "parse",
            r#"{"uri":"otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&issuer=Acme"}"#,
        )?;
        if !answer.contains("\"Acme\"") {
            return Err(format!("the check's answer names no issuer: {answer}"));
        }
        Ok(format!("read a setup link, {} bytes", answer.len()))
    }
}

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_totp_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-totp-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-totp-engine: {error}");
            false
        }
    }
}
