//! Default-off public-fixture worker qualification, not model admission.
//! Reuses the production private-pipe dispatcher and parser confinement.
#[path = "whistle_v8_worker_review/bridge.rs"]
mod bridge;
#[path = "whistle_v8_worker_review/custody.rs"]
mod custody;
#[path = "whistle_v8_worker_review/policy.rs"]
mod policy;
use local_model_service::speech::{self, DraftPolicy, SpeechConfig, SpeechError, worker};
use std::process::ExitCode;
use zeroize::Zeroizing;

const LIMIT: usize = 30 * 16_000;
struct Decoder<'a> {
    native: bridge::Decoder<'a>,
    accepted: usize,
    processed: usize,
    provisional: Zeroizing<String>,
}
impl worker::SpeechDecoder for Decoder<'_> {
    fn draft_policy(&self) -> DraftPolicy {
        DraftPolicy::Incremental
    }
    fn push(&mut self, samples: &[f32]) -> Result<(), SpeechError> {
        self.accepted = self
            .accepted
            .checked_add(samples.len())
            .ok_or(SpeechError::AudioTooLong)?;
        if self.accepted > LIMIT {
            return Err(SpeechError::AudioTooLong);
        }
        Ok(())
    }
    fn draft(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
        if samples.len() != self.accepted {
            return Err(SpeechError::Protocol);
        }
        if samples.len() > self.processed {
            let new = samples.get(self.processed..).ok_or(SpeechError::Protocol)?;
            self.provisional = Zeroizing::new(self.native.call(0, new)?);
            self.processed = samples.len();
        }
        Ok(self.provisional.to_string())
    }
    fn finish(&mut self, samples: &[f32]) -> Result<String, SpeechError> {
        self.draft(samples)?;
        self.native.call(1, samples)
    }
}
fn validate(config: &SpeechConfig) -> Result<(), SpeechError> {
    if !["auto", "en", "de", "fr", "es", "it", "nl", "pl"].contains(&config.language.as_str()) {
        return Err(SpeechError::UnsupportedLanguage);
    }
    if config.vocabulary.len() > 256 {
        return Err(SpeechError::Protocol);
    }
    let mut total = 0_usize;
    for word in &config.vocabulary {
        total = total
            .checked_add(word.len().saturating_add(1))
            .ok_or(SpeechError::Protocol)?;
        if word.is_empty()
            || word.chars().count() > 128
            || total > 16 * 1024
            || word.chars().any(|c| c.is_control() || c.is_whitespace())
            || word.contains("<|")
            || word.contains("|>")
        {
            return Err(SpeechError::Protocol);
        }
    }
    Ok(())
}
fn run() -> Result<(), SpeechError> {
    // SAFETY: this opt-in binary starts as a fresh single-threaded child;
    // no owner for descriptors >=3 exists before prepare closes them.
    unsafe { worker::prepare()? };
    // Review input overrides are never permitted with elevated identity.
    // SAFETY: uid queries are argument-free.
    if unsafe { libc::geteuid() == 0 || libc::geteuid() != libc::getuid() } {
        return Err(SpeechError::ModelUnavailable);
    }
    let args: Vec<String> = std::env::args().collect();
    if matches!(args.as_slice(), [_, flag] if flag == "--policy-probe") {
        return policy::probe();
    }
    let [_, module_path, model_path, bridge_path, bridge_sha] = args.as_slice() else {
        return Err(SpeechError::Protocol);
    };
    let module = custody::verified(
        module_path,
        923_348,
        "c43f48e11f302087250d1e406024956781cd2f02595a5e343b3d7ddd5ef707fa",
    )?;
    let model = custody::verified(
        model_path,
        16_919_407,
        "b6e02f048568ac5d01a2042556c658061e699acbc0aa2a1439f52f3d461dffeb",
    )?;
    let library = bridge::Library::load(bridge_path, bridge_sha)?;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let (session, config) = worker::configure(&mut input)?;
    validate(&config)?;
    let configuration =
        Zeroizing::new(serde_json::to_vec(&config).map_err(|_| SpeechError::Protocol)?);
    // The same production filter must accept actual compiled initialization.
    // No relaxed JIT profile, filesystem rule or exception is introduced.
    speech::confine_worker()?;
    let native = library.decoder(&module, &model, &configuration)?;
    drop(config);
    drop(configuration);
    drop(module);
    drop(model);
    let mut decoder = Decoder {
        native,
        accepted: 0,
        processed: 0,
        provisional: Zeroizing::new(String::new()),
    };
    worker::serve_decoder(
        (
            "whistle-public-review",
            "b358ddadd89b7a713b5aa131f23032d3cca1b251",
        ),
        session,
        &mut decoder,
        &mut input,
        &mut output,
    )
}
fn main() -> ExitCode {
    if run().is_ok() {
        ExitCode::SUCCESS
    } else {
        // Paths, configuration, native diagnostics and transcripts stay private.
        eprintln!("compiled speech review stopped");
        ExitCode::FAILURE
    }
}
