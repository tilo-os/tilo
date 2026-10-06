//! Public, receipt-checked read-speech probes; never connects to a microphone.
#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    qualification::run()
}
#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
#[cfg(target_os = "linux")]
mod qualification {
    use clap::Parser;
    use local_model_service::speech::{MAX_SAMPLES, SAMPLE_RATE, SpeechEngine};
    use serde::Deserialize;
    use sha2::{Digest as _, Sha256};
    use std::fs;
    use std::io::Read as _;
    use std::path::{Path, PathBuf};
    use std::time::Instant;
    use zeroize::Zeroizing;

    type Failure = Box<dyn std::error::Error>;

    #[derive(Parser)]
    struct Options {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long, value_parser = ["tiny", "base", "small-q5_1"])]
        model: String,
        /// Isolate one public clip when investigating the model's load budget.
        #[arg(long)]
        single_case: Option<String>,
    }

    #[derive(Deserialize)]
    struct Manifest {
        dataset: String,
        revision: String,
        models: std::collections::BTreeMap<String, Model>,
        cases: Vec<Case>,
    }
    #[derive(Deserialize)]
    struct Model {
        file: String,
        size: u64,
        sha256: String,
    }
    #[derive(Clone, Deserialize)]
    struct Case {
        id: String,
        language: String,
        file: String,
        size: u64,
        sha256: String,
        reference: String,
        primary: bool,
    }
    struct Probe {
        case: Case,
        samples: Zeroizing<Vec<f32>>,
    }

    fn receipt(
        root: &Path,
        file: &str,
        size: u64,
        sha256: &str,
    ) -> Result<Zeroizing<Vec<u8>>, Failure> {
        if Path::new(file).file_name().and_then(|name| name.to_str()) != Some(file)
            || size > 256 * 1024 * 1024
        {
            return Err("invalid public artifact name or size".into());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        bytes.try_reserve_exact(usize::try_from(size)?.saturating_add(1))?;
        fs::File::open(root.join(file))?
            .take(size.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len())? != size || hex::encode(Sha256::digest(&*bytes)) != sha256 {
            return Err("public fixture receipt mismatch".into());
        }
        Ok(bytes)
    }

    fn wave(bytes: &[u8]) -> Result<Zeroizing<Vec<f32>>, Failure> {
        if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
            return Err("fixture is not WAV".into());
        }
        let mut offset = 12_usize;
        let mut valid_format = false;
        while let Some(header) = bytes.get(offset..offset.saturating_add(8)) {
            let length = usize::try_from(u32::from_le_bytes(
                header.get(4..).ok_or("header")?.try_into()?,
            ))?;
            offset = offset.checked_add(8).ok_or("WAV overflow")?;
            let end = offset.checked_add(length).ok_or("WAV overflow")?;
            let payload = bytes.get(offset..end).ok_or("truncated WAV")?;
            if header.get(..4) == Some(b"fmt ") {
                valid_format = payload.get(..8) == Some(&[1, 0, 1, 0, 128, 62, 0, 0])
                    && payload.get(14..16) == Some(&[16, 0]);
            } else if header.get(..4) == Some(b"data") {
                if !valid_format
                    || !payload.len().is_multiple_of(2)
                    || payload.len() / 2 > MAX_SAMPLES
                {
                    return Err("invalid bounded PCM16 fixture".into());
                }
                return Ok(Zeroizing::new(
                    payload
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|sample| f32::from(i16::from_le_bytes(*sample)) / 32768.0)
                        .collect(),
                ));
            }
            offset = end.checked_add(length % 2).ok_or("WAV overflow")?;
        }
        Err("fixture contains no PCM".into())
    }

    fn prepare(options: &Options) -> Result<(Zeroizing<Vec<u8>>, Vec<Probe>), Failure> {
        let metadata = fs::metadata(&options.manifest)?;
        if metadata.len() > 1024 * 1024 {
            return Err("manifest exceeds bound".into());
        }
        let manifest: Manifest = serde_json::from_slice(&fs::read(&options.manifest)?)?;
        if manifest.dataset != "google/fleurs"
            || manifest.revision != "70bb2e84b976b7e960aa89f1c648e09c59f894dd"
            || manifest.cases.len() != 33
        {
            return Err("unexpected public qualification manifest".into());
        }
        let root = options.manifest.parent().ok_or("manifest directory")?;
        let model = manifest.models.get(&options.model).ok_or("model receipt")?;
        let bytes = receipt(root, &model.file, model.size, &model.sha256)?;
        let mut probes = Vec::new();
        for case in manifest.cases {
            if options
                .single_case
                .as_ref()
                .is_some_and(|id| id != &case.id)
            {
                continue;
            }
            if case.size > u64::try_from(MAX_SAMPLES.saturating_mul(2).saturating_add(4096))?
                || case.reference.len() > 16 * 1024
            {
                return Err("fixture exceeds speech bounds".into());
            }
            let samples = wave(&receipt(root, &case.file, case.size, &case.sha256)?)?;
            probes.push(Probe { case, samples });
        }
        if options.single_case.is_some() {
            if probes.len() != 1 || !probes.first().is_some_and(|probe| probe.case.primary) {
                return Err("single public primary clip is unavailable".into());
            }
            return Ok((bytes, probes));
        }
        // Unique public utterances are spliced; this is not natural conversation.
        for language in ["en", "fr", "zh"] {
            let source: Vec<_> = probes
                .iter()
                .filter(|probe| probe.case.language == language)
                .collect();
            if source.len() != 8 {
                return Err("missing language probes".into());
            }
            let mut samples = Zeroizing::new(Vec::new());
            let mut references = Vec::new();
            for probe in &source {
                if samples.len().saturating_add(probe.samples.len()) > 100 * SAMPLE_RATE {
                    break;
                }
                samples.extend_from_slice(&probe.samples);
                references.push(probe.case.reference.clone());
            }
            if samples.len() < 60 * SAMPLE_RATE {
                return Err("long speech fixture is too short".into());
            }
            let mut case = source.first().ok_or("long fixture source")?.case.clone();
            case.id = format!("long-{language}");
            case.reference = references.join(" ");
            case.primary = true;
            probes.push(Probe { case, samples });
        }
        Ok((bytes, probes))
    }

    fn probe(engine: &mut SpeechEngine, model: &str, probe: &Probe) -> Result<(), Failure> {
        let language = engine
            .language(&probe.case.language)
            .map_err(|_| "language unavailable")?;
        let auto = engine
            .language("auto")
            .map_err(|_| "auto language unavailable")?;
        let prefix = probe
            .samples
            .get(..(5 * SAMPLE_RATE).min(probe.samples.len()))
            .ok_or("prefix")?;
        let start = Instant::now();
        let draft = engine
            .transcribe(prefix, &language)
            .map_err(|_| "draft recognition failed")?;
        let draft_ms = start.elapsed().as_millis();
        let start = Instant::now();
        let text = engine
            .transcribe(&probe.samples, &language)
            .map_err(|_| "recognition failed")?;
        let final_ms = start.elapsed().as_millis();
        let start = Instant::now();
        let detected = engine
            .transcribe(&probe.samples, &auto)
            .map_err(|_| "auto recognition failed")?;
        let auto_ms = start.elapsed().as_millis();
        println!(
            "SPEECH_BENCHMARK {}",
            serde_json::json!({
                "model": model, "id": probe.case.id, "language": probe.case.language,
                "samples": probe.samples.len(), "reference": probe.case.reference,
                "draft": draft, "text": text, "auto_text": detected,
                "draft_ms": draft_ms, "final_ms": final_ms, "auto_ms": auto_ms,
            })
        );
        Ok(())
    }

    pub fn run() -> Result<(), Failure> {
        let options = Options::parse();
        // SAFETY: a fresh single-threaded qualification child owns no ambient
        // descriptor. Production memory/dump/death limits apply before loading.
        unsafe { local_model_service::speech::worker::prepare() }
            .map_err(|_| "worker limits failed")?;
        let (mut model, probes) = prepare(&options)?;
        println!(
            "SPEECH_BENCHMARK_INPUT {}",
            serde_json::json!({
                "model": options.model, "model_bytes": model.len(), "model_buffer_capacity": model.capacity(),
                "audio_samples": probes.iter().map(|probe| probe.samples.len()).sum::<usize>(),
            })
        );
        local_model_service::speech::confine_worker().map_err(|_| "confinement failed")?;
        let start = Instant::now();
        let mut engine = SpeechEngine::load(&mut model).map_err(|_| "model load failed")?;
        drop(model);
        println!(
            "SPEECH_BENCHMARK_LOAD {}",
            serde_json::json!({
                "model": options.model, "load_ms": start.elapsed().as_millis(), "threads": 2,
                "memory_limit_bytes": 1024 * 1024 * 1024_u64,
                "loaded_probes": probes.len(),
                "probes": probes.iter().filter(|probe| probe.case.primary).count(),
            })
        );
        for input in probes.into_iter().filter(|probe| probe.case.primary) {
            probe(&mut engine, &options.model, &input)?;
        }
        Ok(())
    }
}
