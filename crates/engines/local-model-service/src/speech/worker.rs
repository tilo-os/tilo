//! One confined child serving a private speech pipe. No microphone is opened.
use super::{DraftPolicy, FRAME, RequestKind, SpeechConfig, SpeechError, SpeechInput, SpeechReply};
#[cfg(feature = "whisper")]
use super::{
    SpeechEngine, SpeechRecognition, recognition::Admission, speech_activity_file,
    speech_model_file,
};
#[cfg(feature = "whisper")]
use crate::catalog::VerifiedCatalog;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd as _, OwnedFd, RawFd};
#[cfg(feature = "whisper")]
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The serving loop's wake counter. A provisional decode already holds its own
/// decoder state, so the loop reads requests while it runs and comes back for
/// the result through this counter instead of waiting on the decode.
#[derive(Clone)]
pub struct DraftWake(Arc<OwnedFd>);
impl DraftWake {
    fn new() -> Result<Self, SpeechError> {
        // SAFETY: eventfd only reads its integer arguments and returns a
        // descriptor this owner closes, or -1.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(SpeechError::WorkerStopped);
        }
        // SAFETY: the descriptor above is fresh and owned by nobody else.
        Ok(Self(Arc::new(unsafe { OwnedFd::from_raw_fd(fd) })))
    }
    fn fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
    fn signal(&self) {
        let one = 1_u64;
        // SAFETY: the counter is live for this owner's lifetime and the
        // argument is exactly the eight bytes eventfd reads.
        unsafe {
            libc::write(self.fd(), (&raw const one).cast(), 8);
        }
    }
    fn clear(&self) {
        let mut count = 0_u64;
        // SAFETY: as above; a non-blocking read drains one posted count.
        unsafe {
            libc::read(self.fd(), (&raw mut count).cast(), 8);
        }
    }
}

/// The inherited request pipe, read straight from the descriptor: `stdin()`
/// buffers, which would hide a request that arrived behind the frame being
/// read from the serving loop that waits on the descriptor itself.
pub fn request_pipe() -> std::fs::File {
    // SAFETY: descriptor zero is this worker's inherited pipe, owned here for
    // the life of the process.
    unsafe { std::fs::File::from_raw_fd(0) }
}

/// Write only the bounded typed worker reply.
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
pub unsafe fn prepare() -> Result<(), SpeechError> {
    let zero = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // Two decoder states over one model are live during a provisional decode;
    // one gibibyte ended in ggml's address-space assertion there.
    let memory = libc::rlimit {
        rlim_cur: 2 * 1024 * 1024 * 1024,
        rlim_max: 2 * 1024 * 1024 * 1024,
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

/// Run the production handler with a previously verified catalogue approval.
/// The CLI chooses image trust before invoking this function. Qualification
/// uses an isolated ephemeral approval in a separate, uninstalled child.
/// # Errors
/// Fails closed on object/licence, protocol, confinement or engine errors.
#[cfg(feature = "whisper")]
pub fn serve(
    root: &Path,
    verified: &VerifiedCatalog,
    input: &mut (impl Read + AsRawFd),
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
    input: &mut (impl Read + AsRawFd),
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
    let sealed_activity = speech_activity_file(root, verified)?;
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
    let activity = sealed_activity
        .as_ref()
        .map(super::activity::SpeechActivity::load_file)
        .transpose()?;
    drop(sealed_activity);
    engine.set_threads(threads)?;
    engine.set_vocabulary(&config.vocabulary)?;
    engine.prepare()?;
    let language = engine.language(&config.language)?;
    SpeechEngine::enable_draft_interrupt()?;
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
        activity,
        recognition: SpeechRecognition::default(),
        language,
        running: None,
        result: Arc::new(Mutex::new(None)),
        abandoned: false,
    };
    serve_decoder((&model, &revision), session, &mut decoder, input, output)
}

/// One bounded decoder session. Streaming engines receive each new PCM sample
/// exactly once; full-context engines may use the shared cumulative input.
/// All methods execute in the owned confined child, never the compositor.
pub trait SpeechDecoder {
    /// Advertise only a handler installed before Ready in this owned child.
    fn finish_interrupts_draft(&self) -> bool {
        false
    }
    /// Declare scheduling supported by this implementation, never by model
    /// bytes or a remote member. Older workers default to full context.
    fn draft_policy(&self) -> DraftPolicy {
        DraftPolicy::FullContext
    }
    /// Accept only newly appended PCM, once per audio frame.
    fn push(&mut self, samples: &[f32]) -> Result<(), SpeechError>;
    /// Begin the current cumulative hypothesis without retiring the session.
    /// The decode must not hold the serving loop: signal `wake` with the result
    /// through [`SpeechDecoder::poll_draft`] instead of returning it.
    fn start_draft(&mut self, samples: &[f32], wake: &DraftWake) -> Result<(), SpeechError>;
    /// Take a completed provisional text, if one arrived.
    fn poll_draft(&mut self) -> Option<Result<String, SpeechError>>;
    /// Flush and retire this decoder, returning cumulative final text.
    fn finish(&mut self, samples: &[f32]) -> Result<String, SpeechError>;
}

#[cfg(feature = "whisper")]
struct DraftResult {
    text: Result<String, SpeechError>,
    engine: SpeechEngine,
    recognition: SpeechRecognition,
}

/// One provisional decode at a time, over a second decoder state of the same
/// weights. The finished result carries its state back, so the next draft and
/// the final continue from the confirmed prefixes it decoded.
#[cfg(feature = "whisper")]
struct WhisperDecoder {
    engine: SpeechEngine,
    activity: Option<super::activity::SpeechActivity>,
    recognition: SpeechRecognition,
    language: std::ffi::CString,
    running: Option<std::thread::JoinHandle<()>>,
    result: Arc<Mutex<Option<DraftResult>>>,
    abandoned: bool,
}
#[cfg(feature = "whisper")]
impl SpeechDecoder for WhisperDecoder {
    fn finish_interrupts_draft(&self) -> bool {
        true
    }
    fn push(&mut self, _samples: &[f32]) -> Result<(), SpeechError> {
        Ok(())
    }
    fn start_draft(&mut self, samples: &[f32], wake: &DraftWake) -> Result<(), SpeechError> {
        // The wire protocol has one outstanding request, so a second draft
        // before the first reply is a protocol error, not a queue.
        if self.running.is_some() {
            return Err(SpeechError::Protocol);
        }
        // The activity graph has one owner: measure admission here, where the
        // session's samples are appended, and hand the decode a fixed answer.
        let admission = Admission::measure(self.activity.as_mut(), samples)?;
        let mut engine = self.engine.fork();
        let mut recognition = self.recognition.clone();
        let language = self.language.clone();
        let audio = zeroize::Zeroizing::new(samples.to_vec());
        let result = Arc::clone(&self.result);
        let wake = wake.clone();
        self.running = Some(std::thread::spawn(move || {
            // The final outranks every provisional decode when the worker's
            // CPUs are saturated, and ggml's own threads inherit this value.
            // SAFETY: this thread sets its own low priority; arguments are
            // integers and the value is the documented lowest one.
            unsafe {
                libc::setpriority(libc::PRIO_PROCESS, 0, 19);
            }
            let text = recognition
                .transcribe_admitted(&mut engine, admission, &audio, &language, false)
                .or_else(|error| match error {
                    // A stop is a replaced hypothesis, not a failure: the
                    // client still receives the Draft reply it waits for.
                    SpeechError::DraftInterrupted => Ok(String::new()),
                    error => Err(error),
                });
            if let Ok(mut slot) = result.lock() {
                *slot = Some(DraftResult {
                    text,
                    engine,
                    recognition,
                });
            }
            wake.signal();
        }));
        Ok(())
    }
    fn poll_draft(&mut self) -> Option<Result<String, SpeechError>> {
        let draft = self.result.lock().ok()?.take()?;
        // The thread has posted, so reclaiming it cannot block the loop.
        self.running.take();
        if self.abandoned {
            return None;
        }
        self.engine = draft.engine;
        self.recognition = draft.recognition;
        Some(draft.text)
    }
    fn finish(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
        // A provisional decode still running keeps its own state and is left
        // to the stop poll; this final starts now, on the loop's state.
        self.abandoned = true;
        super::engine::stop_drafts();
        let admission = Admission::measure(self.activity.as_mut(), samples)?;
        self.recognition.transcribe_admitted(
            &mut self.engine,
            admission,
            samples,
            &self.language,
            true,
        )
    }
}

/// Read the existing strict configuration frame before engine preparation.
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
pub fn serve_decoder(
    identity: (&str, &str),
    mut session: SpeechInput,
    decoder: &mut impl SpeechDecoder,
    input: &mut (impl Read + AsRawFd),
    output: &mut impl Write,
) -> Result<(), SpeechError> {
    if session.sequence() != 1 || !session.samples().is_empty() {
        return Err(SpeechError::Protocol);
    }
    let wake = DraftWake::new()?;
    reply(
        output,
        &SpeechReply::Ready {
            sequence: session.sequence(),
            model: identity.0.to_owned(),
            revision: identity.1.to_owned(),
            draft_policy: decoder.draft_policy(),
            finish_interrupts_draft: decoder.finish_interrupts_draft(),
        },
    )?;
    // The provisional decode the client is still waiting for.
    let mut pending: Option<u64> = None;
    loop {
        // A final that superseded a still-running decode drops its text here.
        if let Some(text) = decoder.poll_draft()
            && let Some(sequence) = pending.take()
        {
            reply(
                output,
                &SpeechReply::Draft {
                    sequence,
                    text: text?,
                },
            )?;
        }
        let Some(frame) = read_request(input, &wake)? else {
            continue;
        };
        let frame = zeroize::Zeroizing::new(frame);
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
                decoder.start_draft(session.samples(), &wake)?;
                pending = Some(session.sequence());
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

/// Wait for the next request without letting a provisional decode hold the
/// pipe. `None` means only a decode completion arrived.
fn read_request(
    input: &mut (impl Read + AsRawFd),
    wake: &DraftWake,
) -> Result<Option<Vec<u8>>, SpeechError> {
    let mut fds = [
        libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: poll reads exactly the two initialized entries above.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(SpeechError::WorkerStopped);
        }
        if fds[1].revents & libc::POLLIN != 0 {
            wake.clear();
            if fds[0].revents & (libc::POLLIN | libc::POLLHUP) == 0 {
                return Ok(None);
            }
        }
        break;
    }
    FRAME
        .read(input)
        .map_err(|_| SpeechError::Protocol)?
        .ok_or(SpeechError::WorkerStopped)
        .map(Some)
}
