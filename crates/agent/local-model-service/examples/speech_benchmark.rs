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
    use local_model_service::speech::{MAX_SAMPLES, SAMPLE_RATE, SpeechEngine, SpeechRecognition};
    use serde::Deserialize;
    use sha2::{Digest as _, Sha256};
    use std::fs;
    use std::io::Read as _;
    use std::io::Write as _;
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
        /// Diagnostic comparison: decode fixed-size segments.
        #[arg(long, value_parser = clap::value_parser!(u64).range(5..=30))]
        chunk_seconds: Option<u64>,
        /// Diagnostic only: choose the quietest 100 ms within 25% of each boundary.
        #[arg(long, requires = "chunk_seconds")]
        chunk_silence: bool,
        /// Exercise the bounded recognition path used by the production worker.
        #[arg(long, conflicts_with = "chunk_seconds")]
        worker_boundaries: bool,
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
        let long_language = options
            .single_case
            .as_deref()
            .and_then(|id| id.strip_prefix("long-"));
        for case in manifest.cases {
            if options.single_case.as_ref().is_some_and(|id| {
                long_language.map_or(id != &case.id, |language| language != case.language)
            }) {
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
        if options.single_case.is_some() && long_language.is_none() {
            if probes.len() != 1 || !probes.first().is_some_and(|probe| probe.case.primary) {
                return Err("single public primary clip is unavailable".into());
            }
            return Ok((bytes, probes));
        }
        // Unique public utterances are spliced; this is not natural conversation.
        for language in ["en", "fr", "zh"] {
            if long_language.is_some_and(|selected| selected != language) {
                continue;
            }
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
        if let Some(id) = options.single_case.as_ref() {
            let selected = probes
                .into_iter()
                .find(|probe| &probe.case.id == id)
                .ok_or("single long public clip is unavailable")?;
            return Ok((bytes, vec![selected]));
        }
        Ok((bytes, probes))
    }

    fn phase(case: &Case, phase: &str) -> Result<(), Failure> {
        println!(
            "SPEECH_BENCHMARK_PHASE {}",
            serde_json::json!({"id": case.id, "phase": phase})
        );
        std::io::stdout().flush()?;
        Ok(())
    }

    fn segment_length(samples: &[f32], desired: usize, silence: bool) -> usize {
        let padding = desired / 4;
        if !silence {
            return desired.min(samples.len());
        }
        if samples.len() <= desired.saturating_add(padding) {
            return samples.len();
        }
        let window = SAMPLE_RATE / 10;
        (desired.saturating_sub(padding)..desired.saturating_add(padding))
            .step_by(SAMPLE_RATE / 50)
            .filter_map(|offset| {
                let frame = samples.get(offset..offset.saturating_add(window))?;
                let energy = frame
                    .iter()
                    .map(|sample| f64::from(*sample).powi(2))
                    .sum::<f64>();
                Some((offset.saturating_add(window / 2), energy))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map_or(desired, |(boundary, _)| boundary)
    }

    fn recognize(
        engine: &mut SpeechEngine,
        samples: &[f32],
        language: &std::ffi::CString,
        options: &Options,
    ) -> Result<String, Failure> {
        if options.worker_boundaries {
            return SpeechRecognition::default()
                .transcribe(engine, samples, language, true)
                .map_err(|_| "production worker recognition failed".into());
        }
        let Some(seconds) = options.chunk_seconds else {
            return engine
                .transcribe(samples, language)
                .map_err(|_| "recognition failed".into());
        };
        let size = usize::try_from(seconds)?
            .checked_mul(SAMPLE_RATE)
            .ok_or("segment bound")?;
        let mut text = String::new();
        let mut remaining = samples;
        while !remaining.is_empty() {
            let boundary = segment_length(remaining, size, options.chunk_silence);
            let (chunk, next) = remaining.split_at(boundary);
            remaining = next;
            let part = engine
                .transcribe(chunk, language)
                .map_err(|_| "segment recognition failed")?;
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(part);
            if text.len() > 16384 {
                return Err("public segmented output exceeds bound".into());
            }
        }
        Ok(text)
    }

    fn probe(
        engine: &mut SpeechEngine,
        model: &str,
        probe: &Probe,
        options: &Options,
    ) -> Result<(), Failure> {
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
        phase(&probe.case, "draft-fixed-language")?;
        let start = Instant::now();
        let draft = recognize(engine, prefix, &language, options)
            .map_err(|_| "draft recognition failed")?;
        let draft_ms = start.elapsed().as_millis();
        phase(&probe.case, "final-fixed-language")?;
        let start = Instant::now();
        let text = recognize(engine, &probe.samples, &language, options)
            .map_err(|_| "recognition failed")?;
        let final_ms = start.elapsed().as_millis();
        phase(&probe.case, "final-auto-language")?;
        let start = Instant::now();
        let detected = recognize(engine, &probe.samples, &auto, options)
            .map_err(|_| "auto recognition failed")?;
        let auto_ms = start.elapsed().as_millis();
        if probe.case.id.starts_with("long-")
            && recognize(engine, &probe.samples, &language, options)
                .map_err(|_| "repeated long recognition failed")?
                != text
        {
            return Err("long recognition retained state from another call".into());
        }
        println!(
            "SPEECH_BENCHMARK {}",
            serde_json::json!({
                "model": model, "id": probe.case.id, "language": probe.case.language,
                "chunk_seconds": options.chunk_seconds, "chunk_silence": options.chunk_silence,
                "worker_boundaries": options.worker_boundaries,
                "samples": probe.samples.len(), "reference": probe.case.reference,
                "draft": draft, "text": text, "auto_text": detected,
                "draft_ms": draft_ms, "final_ms": final_ms, "auto_ms": auto_ms,
            })
        );
        Ok(())
    }

    pub fn run() -> Result<(), Failure> {
        let options = Options::parse();
        if options.single_case.is_none() {
            return batch(&options);
        }
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
        engine.prepare().map_err(|_| "decoder allocation failed")?;
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
            probe(&mut engine, &options.model, &input, &options)?;
        }
        Ok(())
    }

    fn batch(options: &Options) -> Result<(), Failure> {
        if fs::metadata(&options.manifest)?.len() > 1024 * 1024 {
            return Err("manifest exceeds bound".into());
        }
        let manifest: Manifest = serde_json::from_slice(&fs::read(&options.manifest)?)?;
        let mut ids: Vec<_> = manifest
            .cases
            .into_iter()
            .filter(|case| case.primary)
            .map(|case| case.id)
            .collect();
        if ids.len() != 18 {
            return Err("missing primary public probes".into());
        }
        ids.extend(["long-en", "long-fr", "long-zh"].map(str::to_owned));
        for id in ids {
            // One fresh child at a time models one real dictation's memory,
            // rather than preloading unrelated recordings into its budget.
            let mut command = std::process::Command::new(std::env::current_exe()?);
            command
                .arg("--manifest")
                .arg(&options.manifest)
                .arg("--model")
                .arg(&options.model)
                .arg("--single-case")
                .arg(&id);
            if let Some(seconds) = options.chunk_seconds {
                command.arg("--chunk-seconds").arg(seconds.to_string());
            }
            if options.chunk_silence {
                command.arg("--chunk-silence");
            }
            if options.worker_boundaries {
                command.arg("--worker-boundaries");
            }
            let output = bounded_command::run_prepared(
                command,
                None,
                128 * 1024,
                std::time::Duration::from_secs(300),
            )?;
            std::io::stdout().lock().write_all(&output.stdout)?;
            if !output.status.success() {
                std::io::stderr().lock().write_all(&output.stderr)?;
                return Err(format!("public probe {id} failed: {}", output.status).into());
            }
        }
        Ok(())
    }
}
