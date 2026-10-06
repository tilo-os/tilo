use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use local_model_service::catalog::{IMAGE_CATALOG, IMAGE_KEYS, VerifiedCatalog};
use local_model_service::speech::{
    FRAME, RequestKind, SpeechConfig, SpeechEngine, SpeechError, SpeechInput, SpeechReply,
    speech_model_bytes,
};

#[derive(Parser)]
struct Cli {
    /// Developer validation store. Elevated workers refuse all overrides.
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    catalog: Option<PathBuf>,
    #[arg(long)]
    keys: Option<PathBuf>,
}

pub fn main() -> std::process::ExitCode {
    let args = Cli::parse();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let result = run(&args, &mut stdin.lock(), &mut output);
    if let Err(error) = result {
        let _ = reply(&mut output, &SpeechReply::Error { sequence: 0, error });
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn reply(output: &mut impl Write, message: &SpeechReply) -> Result<(), SpeechError> {
    let bytes = serde_json::to_vec(message).map_err(|_| SpeechError::Protocol)?;
    FRAME
        .write(output, &bytes)
        .map_err(|_| SpeechError::WorkerStopped)?;
    output.flush().map_err(|_| SpeechError::WorkerStopped)
}

fn close_ambient_handles() -> Result<(), SpeechError> {
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

fn run(args: &Cli, input: &mut impl Read, output: &mut impl Write) -> Result<(), SpeechError> {
    close_ambient_handles()?;
    // SAFETY: these argument-free identity queries cannot fail.
    let elevated = unsafe { libc::geteuid() == 0 || libc::geteuid() != libc::getuid() };
    if elevated && (args.root.is_some() || args.catalog.is_some() || args.keys.is_some()) {
        return Err(SpeechError::ModelUnavailable);
    }
    let root = args
        .root
        .clone()
        .unwrap_or_else(local_model_service::user_model_root);
    let catalogue = args
        .catalog
        .as_deref()
        .unwrap_or_else(|| Path::new(IMAGE_CATALOG));
    let keys = args
        .keys
        .as_deref()
        .unwrap_or_else(|| Path::new(IMAGE_KEYS));
    let verified =
        VerifiedCatalog::load(catalogue, keys).map_err(|_| SpeechError::ModelUnavailable)?;
    let (model, revision, mut bytes) = speech_model_bytes(&root, &verified)?;
    let frame = FRAME
        .with_max_len(1033)
        .read(input)
        .map_err(|_| SpeechError::Protocol)?
        .ok_or(SpeechError::WorkerStopped)?;
    let mut session = SpeechInput::default();
    let (kind, payload) = session.accept(&frame)?;
    if kind != RequestKind::Configure {
        return Err(SpeechError::Protocol);
    }
    let config: SpeechConfig =
        serde_json::from_slice(payload).map_err(|_| SpeechError::Protocol)?;
    // The exact signed object is now in memory; all files are closed before
    // native parsing. Unsupported confinement fails before Ready and capture.
    local_model_service::speech::confine_worker()?;
    let mut engine = SpeechEngine::load(&mut bytes)?;
    drop(bytes);
    let language = engine.language(&config.language)?;
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
                let text = engine.transcribe(session.samples(), &language)?;
                reply(
                    output,
                    &SpeechReply::Draft {
                        sequence: session.sequence(),
                        text,
                    },
                )?;
            }
            RequestKind::Finish => {
                let text = engine.transcribe(session.samples(), &language)?;
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
