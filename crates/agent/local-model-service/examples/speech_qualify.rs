//! Remote-only qualification with pinned public upstream speech, never a mic.
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
    use local_model_service::speech::{SAMPLE_RATE, SpeechEngine};
    use sha2::{Digest as _, Sha256};
    use std::io::Read as _;
    use std::time::{Duration, Instant};
    use zeroize::Zeroizing;

    fn fetch(
        url: &str,
        size: u64,
        digest: &str,
    ) -> Result<Zeroizing<Vec<u8>>, Box<dyn std::error::Error>> {
        if let Some(cache) = std::env::var_os("TILO_SPEECH_QUALIFY_FIXTURE_CACHE") {
            use std::os::unix::fs::OpenOptionsExt as _;
            let path = std::path::Path::new(&cache);
            if !path.is_absolute() {
                return Err("qualification cache must be absolute".into());
            }
            let mut bytes = Zeroizing::new(Vec::new());
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path.join(digest))?
                .take(size.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if u64::try_from(bytes.len())? != size || hex::encode(Sha256::digest(&*bytes)) != digest
            {
                return Err("public qualification cache receipt mismatch".into());
            }
            return Ok(bytes);
        }
        let client = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(180))
            .build()?;
        let mut bytes = Zeroizing::new(Vec::new());
        client
            .get(url)
            .send()?
            .error_for_status()?
            .take(size.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len())? != size || hex::encode(Sha256::digest(&*bytes)) != digest {
            return Err("public qualification fixture receipt mismatch".into());
        }
        Ok(bytes)
    }

    fn wav(bytes: &[u8]) -> Result<Zeroizing<Vec<i16>>, Box<dyn std::error::Error>> {
        if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
            return Err("fixture is not a WAVE file".into());
        }
        let mut offset = 12_usize;
        let mut format = false;
        while let Some(header) = bytes.get(offset..offset.saturating_add(8)) {
            let size = usize::try_from(u32::from_le_bytes(
                header.get(4..).ok_or("WAVE header")?.try_into()?,
            ))?;
            offset = offset.checked_add(8).ok_or("WAVE offset")?;
            let end = offset.checked_add(size).ok_or("WAVE length")?;
            let payload = bytes.get(offset..end).ok_or("truncated WAVE")?;
            if header.get(..4) == Some(b"fmt ") {
                format = payload.get(..8) == Some(&[1, 0, 1, 0, 128, 62, 0, 0])
                    && payload.get(14..16) == Some(&[16, 0]);
            } else if header.get(..4) == Some(b"data") {
                if !format || payload.len() % 2 != 0 {
                    return Err("fixture must be mono 16 kHz PCM16".into());
                }
                return Ok(Zeroizing::new(
                    payload
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| i16::from_le_bytes(*pair))
                        .collect(),
                ));
            }
            offset = end.checked_add(size % 2).ok_or("WAVE alignment")?;
        }
        Err("fixture has no PCM data".into())
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        root: std::path::PathBuf,
        catalog: std::path::PathBuf,
        keys: std::path::PathBuf,
    }
    use agent_provider_config::local::{InProcessCapability, LocalModelStatus};
    use ed25519_dalek::{Signer as _, SigningKey};
    use local_model_service::catalog::{
        CATALOG_SIGNATURE_DOMAIN, CatalogPayload, SignedCatalog, catalog_signing_bytes,
    };
    use local_model_service::speech::{FRAME, RequestKind, SpeechReply, request};
    use local_model_service::{InProcessRecord, LicenseAcceptance, ManagedModel, ServiceState};
    use std::fs;
    use std::io::Write as _;
    use std::process::{Child, Command, Stdio};

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn activity_fixture(
        root: &std::path::Path,
        state: &mut ServiceState,
        bytes: &[u8],
    ) -> Result<agent_provider_config::local::LocalModelRelease, Box<dyn std::error::Error>> {
        let sha = hex::encode(Sha256::digest(bytes));
        fs::write(root.join("sha256").join(&sha), bytes)?;
        let release: agent_provider_config::local::LocalModelRelease = serde_json::from_value(
            serde_json::json!({
                "id":"activity-qualification", "label":"Public acoustic fixture", "description":"Isolated test only", "version":"9ffd54a1e1ee413ddf265af9913beaf518d1639b",
                "artifact":{"url":"https://example.invalid/public-activity-fixture", "sha256":sha, "size_bytes":bytes.len()},
                "artifact_format":"silero_ggml", "license_id":"MIT", "license_url":"https://example.invalid/LICENSE", "requires_acceptance":true,
                "installed_size_bytes":bytes.len(), "allowed_runtimes":[], "required_memory_mib":64, "required_accelerator_mib":0, "context_window":0,
                "vision":false, "face_recognition":false, "segmentation":false
            }),
        )?;
        release.validate_shape()?;
        state.models.insert(
            release.id.clone(),
            ManagedModel {
                model_sha256: sha.clone(),
                status: LocalModelStatus::Installed,
                process: None,
                in_process: Some(InProcessRecord {
                    revision: release.version.clone(),
                    license_id: release.license_id.clone(),
                    size_bytes: u64::try_from(bytes.len())?,
                    capabilities: vec![InProcessCapability::SpeechActivity],
                    recorded_at_unix_ms: 1,
                }),
            },
        );
        state.accepted_licenses.insert(
            release.id.clone(),
            LicenseAcceptance {
                model_version: release.version.clone(),
                model_sha256: sha,
                license_id: release.license_id.clone(),
            },
        );
        Ok(release)
    }

    fn fixture(
        model: &[u8],
        activity: Option<&[u8]>,
    ) -> Result<Fixture, Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("store");
        fs::create_dir_all(root.join("sha256"))?;
        let sha = hex::encode(Sha256::digest(model));
        let whistle = model.len() == 16_919_407
            && sha == "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb";
        let (format, license, revision) = if whistle {
            (
                "whistle_cact",
                "Apache-2.0",
                "b358ddadd89b7a713b5aa131f23032d3cca1b251",
            )
        } else {
            (
                "whisper_ggml",
                "MIT",
                "5359861c739e955e79d9a303bcbc70fb988958b1",
            )
        };
        let object = root.join("sha256").join(&sha);
        fs::write(&object, model)?;
        let release: agent_provider_config::local::LocalModelRelease = serde_json::from_value(
            serde_json::json!({
                "id": "speech-qualification", "label": "Public fixture", "description": "Isolated test only", "version": revision,
                "artifact": {"url": "https://example.invalid/public-fixture", "sha256": sha, "size_bytes": model.len()},
                "artifact_format": format, "license_id": license, "license_url": "https://example.invalid/LICENSE", "requires_acceptance": true,
                "installed_size_bytes": model.len(), "allowed_runtimes": [], "required_memory_mib": 1024, "required_accelerator_mib": 0, "context_window": 0,
                "vision": false, "face_recognition": false, "segmentation": false
            }),
        )?;
        let mut state = ServiceState::default();
        state.models.insert(
            release.id.clone(),
            ManagedModel {
                model_sha256: sha.clone(),
                status: LocalModelStatus::Installed,
                process: None,
                in_process: Some(InProcessRecord {
                    revision: release.version.clone(),
                    license_id: license.into(),
                    size_bytes: u64::try_from(model.len())?,
                    capabilities: vec![InProcessCapability::SpeechRecognition],
                    recorded_at_unix_ms: 1,
                }),
            },
        );
        state.accepted_licenses.insert(
            release.id.clone(),
            LicenseAcceptance {
                model_version: release.version.clone(),
                model_sha256: sha,
                license_id: license.into(),
            },
        );
        let mut releases = vec![release];
        if let Some(bytes) = activity {
            releases.push(activity_fixture(&root, &mut state, bytes)?);
        }
        fs::write(root.join("state.json"), serde_json::to_vec(&state)?)?;
        // Ephemeral fixture key, never installed in any OS or user trust store.
        let key = SigningKey::from_bytes(&[5; 32]);
        let payload = CatalogPayload {
            schema: 1,
            models: releases,
            runtimes: vec![],
            recommendation: String::new(),
        };
        let signed = SignedCatalog {
            schema: 1,
            signature_domain: CATALOG_SIGNATURE_DOMAIN.into(),
            key_id: "isolated-test-only".into(),
            signature: key
                .sign(&catalog_signing_bytes(&payload)?)
                .to_bytes()
                .to_vec(),
            payload,
        };
        let catalog = fixture.path().join("catalog.json");
        let keys = fixture.path().join("keys.json");
        fs::write(&catalog, serde_json::to_vec(&signed)?)?;
        fs::write(
            &keys,
            serde_json::to_vec(
                &serde_json::json!({"keys": {"isolated-test-only": hex::encode(key.verifying_key().to_bytes())}}),
            )?,
        )?;
        let verified = local_model_service::catalog::VerifiedCatalog::load(&catalog, &keys)?;
        let (_, _, verified_bytes) =
            local_model_service::speech::speech_model_bytes(&root, &verified)
                .map_err(|_| "qualification store receipt failed")?;
        if verified_bytes.len() != model.len()
            || verified_bytes.capacity() > model.len().saturating_add(1)
        {
            return Err("verified model buffer exceeds its exact receipt allocation".into());
        }
        Ok(Fixture {
            _directory: fixture,
            root,
            catalog,
            keys,
        })
    }

    fn worker_journey(
        model: &[u8],
        samples: &[i16],
        core: bool,
        vocabulary: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Fixture {
            _directory,
            root,
            catalog,
            keys,
        } = fixture(model, None)?;
        let current = std::env::current_exe()?;
        let executable = std::env::current_exe()?
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("worker directory")?
            .join("tilo-speech-worker");
        for cancel in [false, true, false] {
            let mut command = Command::new(if core { &current } else { &executable });
            command.env_clear();
            if core {
                command.arg("--fixture").arg(&root).arg(&catalog).arg(&keys);
            } else {
                command
                    .arg("--root")
                    .arg(&root)
                    .arg("--catalog")
                    .arg(&catalog)
                    .arg("--keys")
                    .arg(&keys);
            }
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let mut child = OwnedChild(command.spawn()?);
            worker_stream(&mut child.0, samples, cancel, vocabulary)?;
        }
        Ok(())
    }

    fn worker_stream(
        child: &mut Child,
        samples: &[i16],
        cancel: bool,
        vocabulary: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut input = child.stdin.take().ok_or("worker input")?;
        let mut output = child.stdout.take().ok_or("worker output")?;
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            while let Ok(Some(frame)) = FRAME.read(&mut output) {
                if sender
                    .send(serde_json::from_slice::<SpeechReply>(&frame))
                    .is_err()
                {
                    break;
                }
            }
        });
        let next = || {
            receiver
                .recv_timeout(Duration::from_secs(20))
                .map_err(|_| "worker reply deadline")
                .and_then(|message| message.map_err(|_| "worker reply schema"))
        };
        let configuration = if vocabulary {
            br#"{"language":"en","vocabulary":["TiLo","Americans"]}"#.as_slice()
        } else {
            br#"{"language":"en"}"#.as_slice()
        };
        FRAME.write(
            &mut input,
            &request(RequestKind::Configure, 1, configuration),
        )?;
        input.flush()?;
        match next()? {
            SpeechReply::Ready { sequence: 1, .. } => {}
            SpeechReply::Error { error, .. } => {
                return Err(format!("worker readiness: {error:?}").into());
            }
            _ => return Err("worker did not become ready".into()),
        }
        let pcm: Vec<_> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut sequence = 1_u64;
        for chunk in pcm.chunks(64 * 1024) {
            sequence = sequence.checked_add(1).ok_or("sequence")?;
            FRAME.write(&mut input, &request(RequestKind::Audio, sequence, chunk))?;
        }
        sequence = sequence.checked_add(1).ok_or("sequence")?;
        FRAME.write(&mut input, &request(RequestKind::Draft, sequence, &[]))?;
        input.flush()?;
        if cancel {
            child.kill()?;
            let status = child.wait()?;
            if status.success() {
                return Err("canceled worker succeeded".into());
            }
        } else {
            match next()? {
                SpeechReply::Draft {
                    sequence: reply,
                    text,
                } if reply == sequence
                    && text.to_lowercase().contains("ask not")
                    && (!vocabulary || !text.to_lowercase().contains("tilo")) => {}
                _ => return Err("worker draft journey failed".into()),
            }
            sequence = sequence.checked_add(1).ok_or("sequence")?;
            FRAME.write(&mut input, &request(RequestKind::Finish, sequence, &[]))?;
            input.flush()?;
            match next()? {
                SpeechReply::Done {
                    sequence: reply,
                    text,
                } if reply == sequence
                    && text.to_lowercase().contains("country")
                    && (!vocabulary || !text.to_lowercase().contains("tilo")) => {}
                _ => return Err("worker final journey failed".into()),
            }
            if !child.wait()?.success() {
                return Err("worker final exit failed".into());
            }
        }
        drop(input);
        reader.join().map_err(|_| "worker reader failed")?;
        Ok(())
    }

    fn child_fixture(paths: &[String]) -> Result<(), Box<dyn std::error::Error>> {
        // SAFETY: this fresh qualification child has only standard pipes and
        // owns no ambient descriptor. It is never part of the installed image.
        unsafe { local_model_service::speech::worker::prepare() }
            .map_err(|_| "child preparation failed")?;
        let root = std::path::Path::new(paths.first().ok_or("fixture store")?);
        let catalog = std::path::Path::new(paths.get(1).ok_or("fixture catalogue")?);
        let keys = std::path::Path::new(paths.get(2).ok_or("fixture keys")?);
        let verified = local_model_service::catalog::VerifiedCatalog::load(catalog, keys)?;
        local_model_service::speech::worker::serve(
            root,
            &verified,
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        )
        .map_err(|_| "production worker handler failed")?;
        Ok(())
    }

    fn special_mode(arguments: &[String]) -> Result<bool, Box<dyn std::error::Error>> {
        if arguments == ["--help"] {
            println!(
                "speech_qualify [--worker[-small] | --worker-core[-small] | --worker-vocabulary-core | --hold-fixture REVIEWED_MODEL_FILE]"
            );
            return Ok(true);
        }
        if arguments.first().is_some_and(|arg| arg == "--fixture") && arguments.len() == 4 {
            child_fixture(arguments.get(1..).ok_or("fixture arguments")?)?;
            return Ok(true);
        }
        // The parent owns this pipe and a bounded disposable namespace. EOF
        // drops every fixture file; this never changes an OS/user trust store.
        if arguments.first().is_some_and(|arg| arg == "--hold-fixture")
            && (2..=3).contains(&arguments.len())
        {
            let path = arguments.get(1).ok_or("public model path")?;
            let mut model = Zeroizing::new(Vec::new());
            fs::File::open(path)?
                .take(256 * 1024 * 1024 + 1)
                .read_to_end(&mut model)?;
            let digest = hex::encode(Sha256::digest(&*model));
            let reviewed = [
                (
                    16_919_407,
                    "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb",
                ),
                (
                    77_691_713,
                    "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
                ),
                (
                    147_951_465,
                    "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
                ),
                (
                    81_768_585,
                    "c577b9a86e7e048a0b7eada054f4dd79a56bbfa911fbdacf900ac5b567cbb7d9",
                ),
                (
                    190_085_487,
                    "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
                ),
            ];
            if !reviewed
                .iter()
                .any(|(size, sha)| model.len() == *size && digest == *sha)
            {
                return Err("unreviewed public model fixture".into());
            }
            let activity = if let Some(path) = arguments.get(2) {
                let mut bytes = Zeroizing::new(Vec::new());
                fs::File::open(path)?
                    .take(4 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() != 885_098
                    || hex::encode(Sha256::digest(&*bytes))
                        != "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987"
                {
                    return Err("unreviewed acoustic graph fixture".into());
                }
                Some(bytes)
            } else {
                None
            };
            let held = fixture(&model, activity.as_deref().map(Vec::as_slice))?;
            drop(model);
            println!(
                "{}",
                serde_json::json!({
                    "root": held.root, "catalog": held.catalog, "keys": held.keys,
                    "ephemeral": true, "model_sha256": digest,
                })
            );
            std::io::stdout().flush()?;
            let mut end = [0];
            let _ = std::io::stdin().read(&mut end)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn qualify_vocabulary(
        engine: &mut SpeechEngine,
        samples: &[f32],
        language: &std::ffi::CString,
        baseline: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        engine
            .set_vocabulary(&["TiLo".into(), "Americans".into()])
            .map_err(|_| "public vocabulary configuration failed")?;
        let biased = engine
            .transcribe(samples, language)
            .map_err(|_| "public vocabulary decode failed")?;
        if !biased.to_lowercase().contains("ask not") || biased.to_lowercase().contains("tilo") {
            return Err("public vocabulary hallucinated an unspoken hint".into());
        }
        if engine.set_vocabulary(&["<|endoftext|>".into()]).is_ok() {
            return Err("decoder accepted a special-token hint".into());
        }
        let cleared = engine
            .transcribe(samples, language)
            .map_err(|_| "cleared vocabulary decode failed")?;
        if cleared != baseline {
            return Err("failed vocabulary replacement retained prior bias".into());
        }
        Ok(())
    }

    fn languages(
        engine: &SpeechEngine,
    ) -> Result<(std::ffi::CString, std::ffi::CString), Box<dyn std::error::Error>> {
        let en = engine.language("en").map_err(|_| "English unavailable")?;
        let auto = engine
            .language("auto")
            .map_err(|_| "auto language unavailable")?;
        if engine.language("yue").is_ok() || engine.language("en-US").is_ok() {
            return Err("unsupported language accepted".into());
        }
        engine
            .language("fr")
            .map_err(|_| "multilingual model unavailable")?;
        Ok((en, auto))
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let arguments = std::env::args().skip(1).collect::<Vec<_>>();
        if special_mode(&arguments)? {
            return Ok(());
        }
        let small = arguments == ["--worker-small"] || arguments == ["--worker-core-small"];
        let mut model = if small {
            fetch(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-small-q5_1.bin",
                190_085_487,
                "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
            )?
        } else {
            fetch(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.bin",
                77_691_713,
                "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
            )?
        };
        let wave = fetch(
            "https://raw.githubusercontent.com/ggml-org/whisper.cpp/2eeeba56e9edd762b4b38467bab96c2517163158/samples/jfk.wav",
            352_078,
            "59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e",
        )?;
        let pcm = wav(&wave)?;
        let samples = Zeroizing::new(
            pcm.iter()
                .map(|sample| f32::from(*sample) / 32768.0)
                .collect::<Vec<_>>(),
        );
        drop(wave);
        let vocabulary = arguments == ["--worker-vocabulary-core"];
        if arguments == ["--worker"] || arguments == ["--worker-core"] || small || vocabulary {
            worker_journey(
                &model,
                &pcm,
                arguments == ["--worker-core"]
                    || arguments == ["--worker-core-small"]
                    || vocabulary,
                vocabulary,
            )?;
        } else if !arguments.is_empty() {
            return Err("expected no arguments, --worker[-small] or --worker-core[-small]".into());
        }
        drop(pcm);
        local_model_service::speech::confine_worker().map_err(|_| "worker confinement failed")?;
        let start = Instant::now();
        let mut engine = SpeechEngine::load(&mut model).map_err(|_| "model load failed")?;
        drop(model);
        engine.prepare().map_err(|_| "decoder allocation failed")?;
        let load_ms = start.elapsed().as_millis();
        let (en, auto) = languages(&engine)?;
        let prefix = samples.get(..5 * SAMPLE_RATE).ok_or("short fixture")?;
        let start = Instant::now();
        let draft = engine
            .transcribe(prefix, &en)
            .map_err(|_| "partial decode failed")?;
        let draft_ms = start.elapsed().as_millis();
        let start = Instant::now();
        let text = engine
            .transcribe(&samples, &en)
            .map_err(|_| "final decode failed")?;
        let final_ms = start.elapsed().as_millis();
        let lower = text.to_lowercase();
        if draft.is_empty()
            || !lower.contains("ask not")
            || !lower.contains("country")
            || !lower.contains("do for")
        {
            return Err("public speech recognition journey failed".into());
        }
        if vocabulary {
            qualify_vocabulary(&mut engine, &samples, &en, &text)?;
        }
        let detected = engine
            .transcribe(&samples, &auto)
            .map_err(|_| "auto language decode failed")?;
        if !detected.to_lowercase().contains("ask not") {
            return Err("automatic English recognition failed".into());
        }
        if !engine
            .transcribe(&vec![0.0; SAMPLE_RATE], &en)
            .map_err(|_| "silence decode failed")?
            .is_empty()
        {
            return Err("silence produced text".into());
        }
        if engine
            .transcribe(&samples, &en)
            .map_err(|_| "repeat decode failed")?
            != text
        {
            return Err("repeated independent decode retained stale state".into());
        }
        println!(
            "pinned public speech: partial/final/auto/repeated/silence/language bounds passed; load_ms={load_ms} draft_ms={draft_ms} final_ms={final_ms} samples={}",
            samples.len()
        );
        Ok(())
    }
}
