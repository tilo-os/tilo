//! One confined child serving a private speech pipe. No microphone is opened.
use super::{
    FRAME, RequestKind, SpeechConfig, SpeechEngine, SpeechError, SpeechInput, SpeechRecognition,
    SpeechReply, speech_model_file,
};
use crate::catalog::VerifiedCatalog;
use std::io::{Read, Write};
use std::path::Path;

/// Write only the bounded typed worker reply.
/// # Errors
/// Fails on serialization or a closed pipe.
pub fn reply(output: &mut impl Write, message: &SpeechReply) -> Result<(), SpeechError> {
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
pub fn serve(
    root: &Path,
    verified: &VerifiedCatalog,
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), SpeechError> {
    let sealed = speech_model_file(root, verified)?;
    let model = sealed.id().to_owned();
    let revision = sealed.revision().to_owned();
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
    let config: SpeechConfig =
        serde_json::from_slice(payload).map_err(|_| SpeechError::Protocol)?;
    drop(frame);
    // Only an immutable, receipt-checked descriptor survives confinement.
    // Native parsing has no path access; failures precede Ready and capture.
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get().min(2));
    super::confine_worker()?;
    let mut engine = SpeechEngine::load_file(&sealed)?;
    drop(sealed);
    engine.set_threads(threads)?;
    engine.set_vocabulary(&config.vocabulary)?;
    engine.prepare()?;
    let language = engine.language(&config.language)?;
    drop(config);
    let mut recognition = SpeechRecognition::default();
    reply(
        output,
        &SpeechReply::Ready {
            sequence: session.sequence(),
            model,
            revision,
        },
    )?;
    loop {
        let frame = FRAME
            .read(input)
            .map_err(|_| SpeechError::Protocol)?
            .ok_or(SpeechError::WorkerStopped)?;
        let (kind, _) = session.accept(&frame)?;
        match kind {
            RequestKind::Audio => {}
            RequestKind::Draft => {
                let text =
                    recognition.transcribe(&mut engine, session.samples(), &language, false)?;
                reply(
                    output,
                    &SpeechReply::Draft {
                        sequence: session.sequence(),
                        text,
                    },
                )?;
            }
            RequestKind::Finish => {
                let text =
                    recognition.transcribe(&mut engine, session.samples(), &language, true)?;
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
