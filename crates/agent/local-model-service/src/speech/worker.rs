//! One confined child serving a private speech pipe. No microphone is opened.
use super::{DraftPolicy, FRAME, RequestKind, SpeechConfig, SpeechError, SpeechInput, SpeechReply};
#[cfg(feature = "whisper")]
use super::{SpeechEngine, SpeechRecognition, speech_model_file};
#[cfg(feature = "whisper")]
use crate::catalog::VerifiedCatalog;
use std::io::{Read, Write};
#[cfg(feature = "whisper")]
use std::path::Path;

/// Write only the bounded typed worker reply.
/// # Errors
/// Fails on serialization or a closed pipe.
pub fn reply(output: &mut impl Write, message: &SpeechReply) -> Result<(), SpeechError> {
    if let SpeechReply::Draft { text, .. } | SpeechReply::Done { text, .. } = message
        && text.len() > super::MAX_TEXT_BYTES
    {
        return Err(SpeechError::TextTooLong);
    }
    let bytes = serde_json::to_vec(message).map_err(|_| SpeechError::Protocol)?;
    FRAME
        .write(output, &bytes)
        .map_err(|_| SpeechError::WorkerStopped)?;
    output.flush().map_err(|_| SpeechError::WorkerStopped)
}

/// Bound memory, disable dumps/tracing, arrange parent-death termination and
/// close every ambient descriptor except the standard pipes.
/// # Safety
/// Call only in a fresh, single-threaded owned child, with no Rust descriptor
/// owner for descriptors numbered three or above. Never call in the compositor.
/// # Errors
/// Fails if any mandatory process limit or descriptor cleanup cannot be applied.
pub unsafe fn prepare() -> Result<(), SpeechError> {
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let memory = libc::rlimit {
        rlim_cur: 1024 * 1024 * 1024,
        rlim_max: 1024 * 1024 * 1024,
    };
    // SAFETY: getppid is an argument-free query. This binary is a child,
    // never a standalone service; a dead parent cannot keep speech running.
    let parent = unsafe { libc::getppid() };
    if parent <= 1 {
        return Err(SpeechError::WorkerStopped);
    }
    // SAFETY: the limit points to the exact libc type. The standalone child
    // needs only standard pipes, and no owner descriptor survives close_range.
    let limits = unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &raw const zero) == 0
            && libc::setrlimit(libc::RLIMIT_AS, &raw const memory) == 0
            && libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0
            && libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) == 0
            && libc::getppid() == parent
            && libc::close_range(3, u32::MAX, 0) == 0
    };
    if limits {
        Ok(())
    } else {
        Err(SpeechError::WorkerStopped)
    }
}

/// Run the production handler with a previously verified catalogue witness.
/// The CLI chooses image trust before invoking this function. Qualification
/// uses an isolated ephemeral witness in a separate, uninstalled child.
/// # Errors
/// Fails closed on object/licence, protocol, confinement or engine errors.
#[cfg(feature = "whisper")]
pub fn serve(
    root: &Path,
    verified: &VerifiedCatalog,
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), SpeechError> {
    serve_with_diagnostics(root, verified, input, output, false)
}

/// Run the same handler with explicitly requested aggregate preparation
/// diagnostics. No model path, vocabulary, audio or transcript is logged.
/// # Errors
/// The same receipt, protocol and confinement checks as [`serve`].
#[cfg(feature = "whisper")]
pub fn serve_with_diagnostics(
    root: &Path,
    verified: &VerifiedCatalog,
    input: &mut impl Read,
    output: &mut impl Write,
    diagnostics: bool,
) -> Result<(), SpeechError> {
    let verification_started = std::time::Instant::now();
    let sealed = speech_model_file(root, verified)?;
    if sealed.format() == agent_provider_config::local::ModelArtifactFormat::WhistleCact {
        #[cfg(feature = "whistle-aot")]
        return super::aot::serve(sealed, input, output);
        #[cfg(not(feature = "whistle-aot"))]
        return Err(SpeechError::ModelUnavailable);
    }
    let verify_ms = verification_started.elapsed().as_secs_f64() * 1000.0;
    let model = sealed.id().to_owned();
    let revision = sealed.revision().to_owned();
    let (session, config) = configure(input)?;
    // Only an immutable, receipt-checked descriptor survives confinement.
    // Native parsing has no path access; failures precede Ready and capture.
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get().min(2));
    super::confine_worker()?;
    let native_started = std::time::Instant::now();
    let mut engine = SpeechEngine::load_file(&sealed)?;
    drop(sealed);
    engine.set_threads(threads)?;
    engine.set_vocabulary(&config.vocabulary)?;
    engine.prepare()?;
    let language = engine.language(&config.language)?;
    if diagnostics {
        eprintln!(
            "SPEECH_PREPARATION {}",
            serde_json::json!({
                "verify_ms": verify_ms,
                "native_ms": native_started.elapsed().as_secs_f64() * 1000.0,
                "flash_attention": engine.flash_attention_enabled(),
            })
        );
    }
    drop(config);
    let mut decoder = WhisperDecoder {
        engine,
        recognition: SpeechRecognition::default(),
        language,
    };
    serve_decoder((&model, &revision), session, &mut decoder, input, output)
}

/// One bounded decoder session. Streaming engines receive each new PCM sample
/// exactly once; full-context engines may use the shared cumulative input.
/// All methods execute in the owned confined child, never the compositor.
pub trait SpeechDecoder {
    /// Declare scheduling supported by this implementation, never by model
    /// bytes or a remote collaborator. Older workers default to full context.
    fn draft_policy(&self) -> DraftPolicy {
        DraftPolicy::FullContext
    }
    /// Accept only newly appended PCM, once per audio frame.
    /// # Errors
    /// Rejects invalid audio, exhausted resources or native failure.
    fn push(&mut self, samples: &[f32]) -> Result<(), SpeechError>;
    /// Produce the current cumulative hypothesis without retiring the session.
    /// # Errors
    /// Rejects invalid state, oversized text or native failure.
    fn draft(&mut self, samples: &[f32]) -> Result<String, SpeechError>;
    /// Flush and retire this decoder, returning cumulative final text.
    /// # Errors
    /// Rejects invalid state, oversized text or native failure.
    fn finish(&mut self, samples: &[f32]) -> Result<String, SpeechError>;
}

#[cfg(feature = "whisper")]
struct WhisperDecoder {
    engine: SpeechEngine,
    recognition: SpeechRecognition,
    language: std::ffi::CString,
}
#[cfg(feature = "whisper")]
impl SpeechDecoder for WhisperDecoder {
    fn push(&mut self, _samples: &[f32]) -> Result<(), SpeechError> {
        Ok(())
    }
    fn draft(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
        self.recognition
            .transcribe(&mut self.engine, samples, &self.language, false)
    }
    fn finish(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
        self.recognition
            .transcribe(&mut self.engine, samples, &self.language, true)
    }
}

/// Read the existing strict configuration frame before engine preparation.
/// # Errors
/// Rejects an absent, oversized, malformed or out-of-sequence configuration.
pub fn configure(input: &mut impl Read) -> Result<(SpeechInput, SpeechConfig), SpeechError> {
    let frame = zeroize::Zeroizing::new(
        FRAME
            .with_max_len(super::MAX_CONFIG_BYTES + 9)
            .read(input)
            .map_err(|_| SpeechError::Protocol)?
            .ok_or(SpeechError::WorkerStopped)?,
    );
    let mut session = SpeechInput::default();
    let (kind, payload) = session.accept(&frame)?;
    if kind != RequestKind::Configure {
        return Err(SpeechError::Protocol);
    }
    let config = serde_json::from_slice(payload).map_err(|_| SpeechError::Protocol)?;
    Ok((session, config))
}

/// Serve the real private pipe using an already prepared confined decoder.
/// Readiness follows configuration, model verification, confinement and native
/// preparation. Completion retires the session; failures stop before reuse.
/// # Errors
/// Rejects invalid input, native failure, oversized replies or a closed pipe.
pub fn serve_decoder(
    identity: (&str, &str),
    mut session: SpeechInput,
    decoder: &mut impl SpeechDecoder,
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), SpeechError> {
    if session.sequence() != 1 || !session.samples().is_empty() {
        return Err(SpeechError::Protocol);
    }
    reply(
        output,
        &SpeechReply::Ready {
            sequence: session.sequence(),
            model: identity.0.to_owned(),
            revision: identity.1.to_owned(),
            draft_policy: decoder.draft_policy(),
        },
    )?;
    loop {
        let frame = zeroize::Zeroizing::new(
            FRAME
                .read(input)
                .map_err(|_| SpeechError::Protocol)?
                .ok_or(SpeechError::WorkerStopped)?,
        );
        let previous = session.samples().len();
        let (kind, _) = session.accept(&frame)?;
        match kind {
            RequestKind::Audio => {
                decoder.push(
                    session
                        .samples()
                        .get(previous..)
                        .ok_or(SpeechError::Protocol)?,
                )?;
            }
            RequestKind::Draft => {
                let text = decoder.draft(session.samples())?;
                reply(
                    output,
                    &SpeechReply::Draft {
                        sequence: session.sequence(),
                        text,
                    },
                )?;
            }
            RequestKind::Finish => {
                let text = decoder.finish(session.samples())?;
                reply(
                    output,
                    &SpeechReply::Done {
                        sequence: session.sequence(),
                        text,
                    },
                )?;
                return Ok(());
            }
            RequestKind::Configure => return Err(SpeechError::Protocol),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speech::request;
    use std::io::Cursor;

    #[derive(Default)]
    struct RecordingDecoder {
        pushes: Vec<Vec<f32>>,
        drafts: Vec<Vec<f32>>,
        finals: Vec<Vec<f32>>,
        reject_audio: bool,
    }
    impl SpeechDecoder for RecordingDecoder {
        fn push(&mut self, samples: &[f32]) -> Result<(), SpeechError> {
            self.pushes.push(samples.to_vec());
            if self.reject_audio {
                Err(SpeechError::Recognition)
            } else {
                Ok(())
            }
        }
        fn draft(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
            self.drafts.push(samples.to_vec());
            Ok("draft".to_owned())
        }
        fn finish(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
            self.finals.push(samples.to_vec());
            Ok("final".to_owned())
        }
    }
    fn append(input: &mut Vec<u8>, kind: RequestKind, sequence: u64, payload: &[u8]) {
        FRAME
            .write(input, &request(kind, sequence, payload))
            .expect("fixture frame");
    }
    fn configured(input: &mut Cursor<Vec<u8>>) -> SpeechInput {
        configure(input).expect("real configuration parser").0
    }
    fn replies(bytes: Vec<u8>) -> Vec<SpeechReply> {
        let mut input = Cursor::new(bytes);
        let mut messages = Vec::new();
        while let Some(frame) = FRAME.read(&mut input).expect("bounded reply frame") {
            messages.push(serde_json::from_slice(&frame).expect("typed reply"));
        }
        messages
    }

    #[test]
    fn real_pipe_sends_only_new_pcm_and_retires_after_finish() {
        let mut bytes = Vec::new();
        append(
            &mut bytes,
            RequestKind::Configure,
            1,
            br#"{"language":"en"}"#,
        );
        append(&mut bytes, RequestKind::Audio, 2, &[0, 128, 0, 0]);
        append(&mut bytes, RequestKind::Draft, 3, &[]);
        append(&mut bytes, RequestKind::Audio, 4, &[255, 127]);
        append(&mut bytes, RequestKind::Draft, 5, &[]);
        append(&mut bytes, RequestKind::Finish, 6, &[]);
        append(&mut bytes, RequestKind::Audio, 7, &[0, 0]);
        let mut input = Cursor::new(bytes);
        let session = configured(&mut input);
        let mut decoder = RecordingDecoder::default();
        let mut output = Vec::new();
        serve_decoder(
            ("fixture", "revision"),
            session,
            &mut decoder,
            &mut input,
            &mut output,
        )
        .expect("actual worker dispatch");
        let last = 32767.0 / 32768.0;
        assert_eq!(decoder.pushes, [vec![-1.0, 0.0], vec![last]]);
        assert_eq!(decoder.drafts, [vec![-1.0, 0.0], vec![-1.0, 0.0, last]]);
        assert_eq!(decoder.finals, [vec![-1.0, 0.0, last]]);
        let messages = replies(output);
        assert!(matches!(
            messages.as_slice(),
            [
                SpeechReply::Ready { sequence: 1, .. },
                SpeechReply::Draft { sequence: 3, .. },
                SpeechReply::Draft { sequence: 5, .. },
                SpeechReply::Done { sequence: 6, .. }
            ]
        ));
        assert!(
            FRAME
                .read(&mut input)
                .expect("unconsumed next session audio")
                .is_some()
        );
    }

    #[test]
    fn decoder_failure_stops_pipe_before_draft_or_final() {
        let mut bytes = Vec::new();
        append(
            &mut bytes,
            RequestKind::Configure,
            1,
            br#"{"language":"en"}"#,
        );
        append(&mut bytes, RequestKind::Audio, 2, &[0, 0]);
        append(&mut bytes, RequestKind::Draft, 3, &[]);
        append(&mut bytes, RequestKind::Finish, 4, &[]);
        let mut input = Cursor::new(bytes);
        let session = configured(&mut input);
        let mut decoder = RecordingDecoder {
            reject_audio: true,
            ..Default::default()
        };
        let mut output = Vec::new();
        assert!(matches!(
            serve_decoder(
                ("fixture", "revision"),
                session,
                &mut decoder,
                &mut input,
                &mut output
            ),
            Err(SpeechError::Recognition)
        ));
        assert_eq!(decoder.pushes, [vec![0.0]]);
        assert!(decoder.drafts.is_empty() && decoder.finals.is_empty());
        assert!(matches!(
            replies(output).as_slice(),
            [SpeechReply::Ready { sequence: 1, .. }]
        ));
    }

    #[test]
    fn unconfigured_decoder_never_emits_readiness() {
        let mut decoder = RecordingDecoder::default();
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();
        assert!(matches!(
            serve_decoder(
                ("fixture", "revision"),
                SpeechInput::default(),
                &mut decoder,
                &mut input,
                &mut output
            ),
            Err(SpeechError::Protocol)
        ));
        assert!(output.is_empty());
        assert!(
            decoder.pushes.is_empty() && decoder.drafts.is_empty() && decoder.finals.is_empty()
        );
    }

    #[derive(Default)]
    struct CorrectingEndpointDecoder {
        finished: bool,
        final_samples: Vec<f32>,
    }
    impl SpeechDecoder for CorrectingEndpointDecoder {
        fn draft_policy(&self) -> DraftPolicy {
            DraftPolicy::Incremental
        }
        fn push(&mut self, _samples: &[f32]) -> Result<(), SpeechError> {
            Ok(())
        }
        fn draft(&mut self, _samples: &[f32]) -> Result<String, SpeechError> {
            Ok("Vielen Dank.".to_owned())
        }
        fn finish(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
            self.final_samples = samples.to_vec();
            self.finished = true;
            Ok("Für Hongkong — nein, für Kowloon. 東京 café".to_owned())
        }
    }

    #[test]
    fn endpoint_correction_replaces_provisional_text_and_preserves_spoken_corrections() {
        let mut bytes = Vec::new();
        append(
            &mut bytes,
            RequestKind::Configure,
            1,
            br#"{"language":"de","vocabulary":["Hongkong","Kowloon"]}"#,
        );
        append(&mut bytes, RequestKind::Audio, 2, &[0, 0]);
        append(&mut bytes, RequestKind::Draft, 3, &[]);
        append(&mut bytes, RequestKind::Audio, 4, &[0, 64]);
        append(&mut bytes, RequestKind::Finish, 5, &[]);
        let mut input = Cursor::new(bytes);
        let session = configured(&mut input);
        let mut decoder = CorrectingEndpointDecoder::default();
        let mut output = Vec::new();
        serve_decoder(
            ("fixture", "revision"),
            session,
            &mut decoder,
            &mut input,
            &mut output,
        )
        .expect("real production pipe endpoint dispatch");
        assert!(decoder.finished);
        assert_eq!(decoder.final_samples, [0.0, 0.5]);
        assert!(matches!(replies(output).as_slice(), [
            SpeechReply::Ready { draft_policy: DraftPolicy::Incremental, .. },
            SpeechReply::Draft { sequence: 3, text: provisional },
            SpeechReply::Done { sequence: 5, text: final_text }
        ] if provisional == "Vielen Dank." && final_text == "Für Hongkong — nein, für Kowloon. 東京 café"));
    }

    #[test]
    fn stable_provisional_text_is_not_committed_when_the_pipe_ends_before_finish() {
        let mut bytes = Vec::new();
        append(
            &mut bytes,
            RequestKind::Configure,
            1,
            br#"{"language":"de"}"#,
        );
        append(&mut bytes, RequestKind::Audio, 2, &[0, 0]);
        append(&mut bytes, RequestKind::Draft, 3, &[]);
        let mut input = Cursor::new(bytes);
        let session = configured(&mut input);
        let mut decoder = CorrectingEndpointDecoder::default();
        let mut output = Vec::new();
        assert!(matches!(
            serve_decoder(
                ("fixture", "revision"),
                session,
                &mut decoder,
                &mut input,
                &mut output
            ),
            Err(SpeechError::WorkerStopped)
        ));
        assert!(!decoder.finished);
        assert!(matches!(replies(output).as_slice(), [
            SpeechReply::Ready { .. }, SpeechReply::Draft { sequence: 3, text }
        ] if text == "Vielen Dank."));
    }
}
