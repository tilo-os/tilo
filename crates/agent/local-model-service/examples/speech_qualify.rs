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
    fn fixture(model: &[u8]) -> Result<Fixture, Box<dyn std::error::Error>> {
        let fixture = tempfile::tempdir()?;
        let root = fixture.path().join("store");
        fs::create_dir_all(root.join("sha256"))?;
        let sha = hex::encode(Sha256::digest(model));
        let object = root.join("sha256").join(&sha);
        fs::write(&object, model)?;
        let release: agent_provider_config::local::LocalModelRelease = serde_json::from_value(
            serde_json::json!({
                "id": "speech-qualification", "label": "Public fixture", "description": "Isolated test only", "version": "5359861c739e955e79d9a303bcbc70fb988958b1",
                "artifact": {"url": "https://example.invalid/public-fixture", "sha256": sha, "size_bytes": model.len()},
                "artifact_format": "whisper_ggml", "license_id": "MIT", "license_url": "https://example.invalid/LICENSE", "requires_acceptance": true,
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
                    license_id: "MIT".into(),
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
                license_id: "MIT".into(),
            },
        );
        fs::write(root.join("state.json"), serde_json::to_vec(&state)?)?;
        // Ephemeral fixture key, never installed in any OS or user trust store.
        let key = SigningKey::from_bytes(&[5; 32]);
        let payload = CatalogPayload {
            schema: 1,
            models: vec![release],
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
        local_model_service::speech::speech_model_bytes(&root, &verified)
            .map_err(|_| "qualification store receipt failed")?;
        Ok(Fixture {
            _directory: fixture,
            root,
            catalog,
            keys,
        })
    }

    fn worker_journey(model: &[u8], samples: &[i16]) -> Result<(), Box<dyn std::error::Error>> {
        let Fixture {
            _directory,
            root,
            catalog,
            keys,
        } = fixture(model)?;
        let executable = std::env::current_exe()?
            .parent()
            .and_then(std::path::Path::parent)
            .ok_or("worker directory")?
            .join("tilo-speech-worker");
        for cancel in [false, true] {
            let mut command = Command::new(&executable);
            command
                .env_clear()
                .arg("--root")
                .arg(&root)
                .arg("--catalog")
                .arg(&catalog)
                .arg("--keys")
                .arg(&keys)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let mut child = OwnedChild(command.spawn()?);
            worker_stream(&mut child.0, samples, cancel)?;
        }
        Ok(())
    }

    fn worker_stream(
        child: &mut Child,
        samples: &[i16],
        cancel: bool,
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
        FRAME.write(
            &mut input,
            &request(RequestKind::Configure, 1, br#"{"language":"en"}"#),
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
                } if reply == sequence && text.to_lowercase().contains("ask not") => {}
                _ => return Err("worker draft journey failed".into()),
            }
            sequence = sequence.checked_add(1).ok_or("sequence")?;
            FRAME.write(&mut input, &request(RequestKind::Finish, sequence, &[]))?;
            input.flush()?;
            match next()? {
                SpeechReply::Done {
                    sequence: reply,
                    text,
                } if reply == sequence && text.to_lowercase().contains("country") => {}
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

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let mut model = fetch(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.bin",
            77_691_713,
            "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
        )?;
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
        let worker = std::env::args().skip(1).collect::<Vec<_>>();
        if worker == ["--worker"] {
            worker_journey(&model, &pcm)?;
        } else if !worker.is_empty() {
            return Err("expected no arguments or --worker".into());
        }
        drop(pcm);
        local_model_service::speech::confine_worker().map_err(|_| "worker confinement failed")?;
        let start = Instant::now();
        let mut engine = SpeechEngine::load(&mut model).map_err(|_| "model load failed")?;
        drop(model);
        let load_ms = start.elapsed().as_millis();
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
