//! The `YouTube` app's Grayjay link, as its own library in the runtime bundle
//! (`libtilo_grayjay_engine.so`), opened the first time `so.tilo.tube` asks it
//! to pair with or sync a Grayjay device.
//!
//! The app runs sandboxed with no network and no secret store, so it asks this
//! engine through `engine.call` and `engine.start`; the process that hosts it
//! opens this library once and files it under the app's name, so no other app
//! is answered by it. Pairing and syncing touch the network, so they are the
//! jobs the host names; the device's Grayjay key stays where it is and never
//! reaches the app.
//!
//! Outside the thin build this crate is an empty library.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
mod engine {
    use std::sync::Arc;

    use tilo_engine_loader::{Device, EngineApi, Frames, JobOp, JobSink, Needs};

    /// The app whose Grayjay link this is: the one that asks these ops, and no
    /// other.
    pub const APP: &str = tilolang_grayjay::APP;

    /// The engine over the key `device` holds for the app's Grayjay link, asked for this call
    /// and kept nowhere.
    fn keyed(device: &Device) -> Result<tilolang_grayjay::Engine, String> {
        let key = device.device_key(tilolang_grayjay::PURPOSE)?;
        Ok(tilolang_grayjay::Engine::with_key_source(Arc::new(
            tilolang_grayjay::FixedKey::taking(key),
        )))
    }

    /// The jobs this engine runs: pairing and syncing, which read this device's
    /// Grayjay key and reach the network.
    fn job_ops() -> &'static [JobOp] {
        const JOBS: &[JobOp] = &[
            JobOp::new("pair", Needs::SECRET_AND_NETWORK),
            JobOp::new("sync", Needs::SECRET_AND_NETWORK),
        ];
        JOBS
    }

    /// One `engine.call` for this app.
    fn call(device: &Device, op: &str, input: &str) -> Result<String, String> {
        keyed(device)?.call(op, input)
    }

    /// One `engine.start`: pairing or syncing, run as the job the host names.
    fn start(
        device: &Device,
        job: &str,
        op: &str,
        input: &str,
        moved: &JobSink,
    ) -> Result<(), String> {
        let moved = *moved;
        keyed(device)?.start(
            job,
            op,
            input,
            std::sync::Arc::new(move |state, detail| moved.moved(state, &detail.to_string())),
        )
    }

    /// One job's progress, as JSON.
    fn status(_device: &Device, job: &str) -> Result<String, String> {
        tilolang_grayjay::Engine::status(job)
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

    /// One stopped job: false when it is not this engine's or had finished.
    fn cancel(_device: &Device, job: &str) -> bool {
        tilolang_grayjay::Engine::cancel(job)
    }

    /// The one engine this library answers with.
    static API: EngineApi = EngineApi {
        app: APP,
        jobs: job_ops,
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
    /// the ops it does not answer are refused, without a device's key.
    pub fn self_check() -> Result<String, String> {
        if call(&no_device(), "no-such-op", "").is_ok() {
            return Err("the check's unknown op answered".to_owned());
        }
        if cancel(&no_device(), "no-such-job") {
            return Err("the check's unknown job stopped".to_owned());
        }
        Ok("refused an unknown op and job".to_owned())
    }

    /// A device a check that reads none of it stands on.
    fn no_device() -> Device {
        Device::none()
    }
}

/// The engine's check binary's report: whether the self-check passed.
#[cfg(feature = "thin")]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn tilo_grayjay_engine_check() -> bool {
    match engine::self_check() {
        Ok(report) => {
            println!("tilo-grayjay-engine: {report}");
            true
        }
        Err(error) => {
            eprintln!("tilo-grayjay-engine: {error}");
            false
        }
    }
}
