//! Public-fixture AOT qualification before connecting any worker protocol.
//! The controller authenticates compiler/runtime/source/configuration receipts.
//! None of these review receipts grants model catalogue or executable authority.
#[path = "whistle_aot_review/artifact.rs"]
mod artifact;
#[path = "whistle_v8_worker_review/bridge.rs"]
mod bridge;
#[path = "whistle_v8_worker_review/custody.rs"]
mod custody;
use local_model_service::speech::{self, SpeechError, worker};
use std::process::ExitCode;
use std::time::Instant;
use zeroize::Zeroizing;

fn run() -> Result<(), SpeechError> {
    // SAFETY: fresh single-threaded qualification child, no ambient FD owners.
    unsafe { worker::prepare()? };
    // SAFETY: argument-free identity queries; review inputs cannot elevate trust.
    if unsafe { libc::geteuid() == 0 || libc::geteuid() != libc::getuid() } {
        return Err(SpeechError::ModelUnavailable);
    }
    let args: Vec<String> = std::env::args().collect();
    let [
        _,
        artifact_path,
        artifact_sha,
        model_path,
        library_path,
        library_sha,
        pcm_path,
        pcm_sha,
        language,
    ] = args.as_slice()
    else {
        return Err(SpeechError::Protocol);
    };
    if !["en", "de", "fr", "es", "it", "nl", "pl"].contains(&language.as_str()) {
        return Err(SpeechError::UnsupportedLanguage);
    }
    // Review-only custody: AOT must come from this controller's pinned factory.
    let artifact = artifact::verified(artifact_path, artifact_sha)?;
    let model = custody::verified(
        model_path,
        16_919_407,
        "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb",
    )?;
    let pcm = custody::verified(pcm_path, 30 * 16_000 * 4, pcm_sha)?;
    if pcm.len() % 4 != 0 {
        return Err(SpeechError::Protocol);
    }
    let samples = Zeroizing::new(
        pcm.chunks_exact(4)
            .map(|bytes| {
                bytes
                    .try_into()
                    .map(f32::from_le_bytes)
                    .map_err(|_| SpeechError::Protocol)
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    if samples
        .iter()
        .any(|sample| !sample.is_finite() || sample.abs() > 1.0)
    {
        return Err(SpeechError::Protocol);
    }
    let library = bridge::Library::load(library_path, library_sha)?;
    speech::confine_worker()?;
    let start = Instant::now();
    let mut decoder = library.decoder(&artifact, &model, language.as_bytes())?;
    let startup_seconds = start.elapsed().as_secs_f64();
    drop(artifact);
    drop(model);
    drop(pcm);
    let start = Instant::now();
    let raw = Zeroizing::new(decoder.call(2, &samples)?);
    let result: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| SpeechError::Recognition)?;
    if result
        .get("text")
        .and_then(serde_json::Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err(SpeechError::Recognition);
    }
    // Explicit public-fixture output only; this is never a live speech worker.
    println!(
        "WHISTLE_AOT_RECOGNITION {}",
        serde_json::json!({
            "startup_seconds": startup_seconds,
            "batch_seconds": start.elapsed().as_secs_f64(),
            "production_confinement": true,
            "raw_result": result,
        })
    );
    Ok(())
}
fn main() -> ExitCode {
    if run().is_ok() {
        ExitCode::SUCCESS
    } else {
        eprintln!("authenticated AOT qualification stopped");
        ExitCode::FAILURE
    }
}
