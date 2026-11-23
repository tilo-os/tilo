//! Isolated acoustic evidence on public PCM only. Never opens a microphone.
use local_model_service::speech::{MAX_SAMPLES, activity, confine_worker, worker};
use std::io::Read as _;
use std::time::Instant;
use zeroize::Zeroizing;

fn main() -> Result<(), String> {
    if std::env::var("TILO_TEST_PRIVATE_NAMESPACE").as_deref() != Ok("1") {
        return Err("requires a private hardware-free review capsule".into());
    }
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let model = match arguments.as_slice() {
        [flag, path] if flag == "--silero-model" => {
            use std::os::unix::fs::OpenOptionsExt as _;
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
                .map_err(|error| error.to_string())?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.len() != 885_098 {
                return Err("changed review model".into());
            }
            let mut bytes = Zeroizing::new(Vec::new());
            file.take(885_099)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            bytes
        }
        _ => {
            return Err("isolated review requires --silero-model PINNED_PUBLIC_GRAPH".into());
        }
    };
    // SAFETY: fresh single-threaded child; only standard pipes are owned.
    unsafe { worker::prepare() }.map_err(|error| format!("{error:?}"))?;
    confine_worker().map_err(|error| format!("{error:?}"))?;
    let mut bytes = Zeroizing::new(Vec::new());
    std::io::stdin()
        .take(u64::try_from(MAX_SAMPLES * 2 + 1).map_err(|error| error.to_string())?)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() || bytes.len() % 2 != 0 || bytes.len() > MAX_SAMPLES * 2 {
        return Err("invalid bounded mono16kHz signed little-endian PCM".into());
    }
    let (frames, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err("partial PCM frame".into());
    }
    let samples = Zeroizing::new(
        frames
            .iter()
            .map(|sample| f32::from(i16::from_le_bytes(*sample)) / 32768.0)
            .collect::<Vec<_>>(),
    );
    for normalize in [false, true] {
        {
            let start = Instant::now();
            let evidence = activity::observe_silero(&model, &samples, normalize)
                .map_err(|error| format!("{error:?}"))?;
            println!(
                "SILERO_EVIDENCE {}",
                serde_json::json!({
                    "model":"silero-v6.2.0-ggml", "normalize_analysis_copy":normalize,
                    "samples":samples.len(), "positive_frames":evidence.frames.positive_frames,
                    "first_positive_frame":evidence.frames.first_positive_frame,
                    "last_positive_frame":evidence.frames.last_positive_frame,
                    "longest_positive_run":evidence.frames.longest_positive_run,
                    "min_probability":evidence.min_probability,"max_probability":evidence.max_probability,
                    "mean_probability":evidence.mean_probability,"total_frames":evidence.total_frames,
                    "milliseconds":start.elapsed().as_secs_f64()*1000.0,"original_asr_pcm_changed":false,
                })
            );
        }
    }
    Ok(())
}
