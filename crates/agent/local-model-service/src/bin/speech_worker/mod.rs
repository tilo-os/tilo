use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use local_model_service::catalog::{IMAGE_CATALOG, IMAGE_KEYS, VerifiedCatalog};
use local_model_service::speech::{SpeechError, SpeechReply, worker::reply};

#[derive(Parser)]
struct Cli {
    /// Developer validation store. Elevated workers refuse all overrides.
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    catalog: Option<PathBuf>,
    #[arg(long)]
    keys: Option<PathBuf>,
    /// Emit aggregate preparation timing and native attention mode to stderr.
    /// Never includes model paths, vocabulary, audio or transcript text.
    #[arg(long)]
    diagnostics: bool,
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

fn run(args: &Cli, input: &mut impl Read, output: &mut impl Write) -> Result<(), SpeechError> {
    // SAFETY: this standalone child owns only its standard pipes, is not
    // multithreaded and has no Rust descriptor owner for an ambient handle.
    unsafe {
        local_model_service::speech::worker::prepare()?;
    }
    let elevated = rustix::process::geteuid().as_raw() == 0
        || rustix::process::geteuid().as_raw() != rustix::process::getuid().as_raw();
    if elevated
        && (args.root.is_some()
            || args.catalog.is_some()
            || args.keys.is_some()
            || args.diagnostics)
    {
        return Err(SpeechError::ModelUnavailable);
    }
    let root = args
        .root
        .clone()
        .or_else(local_model_service::user_model_root)
        .ok_or(SpeechError::ModelUnavailable)?;
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
    local_model_service::speech::worker::serve_with_diagnostics(
        &root,
        &verified,
        input,
        output,
        args.diagnostics,
    )
}
