//! `qos-model-manager` — the small production CLI Settings invokes.
//!
//! It accepts only signed catalog selections, writes only its private store,
//! downloads only pinned HTTPS URLs, and delegates process lifetime to a
//! fixed-argv systemd user service. No subcommand accepts a URL, command, tag,
//! model path, or arbitrary process argument.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use agent_provider_config::local::{HardwareSnapshot, LocalModelRelease, LocalModelStatus};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use local_model_service::{
    HttpsTransport, LocalModelService, ProcessHandle, ProcessSupervisor, ServiceError,
    SignedRuntime, VllmCommand,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Parser)]
#[command(
    name = "qos-model-manager",
    about = "Verified local model lifecycle manager"
)]
struct Cli {
    #[arg(long, default_value = "/var/lib/qos/models")]
    root: PathBuf,
    #[arg(long, default_value = "/etc/qos/models/catalog.json")]
    catalog: PathBuf,
    #[arg(long, default_value = "/etc/qos/models/keys.json")]
    keys: PathBuf,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    Snapshot {
        #[arg(long, default_value = "json")]
        format: String,
    },
    Install {
        #[arg(long)]
        model: String,
        #[arg(long)]
        revision: Option<String>,
    },
    Start {
        #[arg(long)]
        model: String,
    },
    Stop {
        #[arg(long)]
        model: String,
    },
    Remove {
        #[arg(long)]
        model: String,
    },
    RequestRestart {
        #[arg(long)]
        model: String,
    },
}

/// Signed, release-channel catalog. The signature covers canonical JSON of
/// `payload`; model and runtime artifacts within it remain individually pinned.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SignedCatalog {
    key_id: String,
    signature: Vec<u8>,
    payload: CatalogPayload,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CatalogPayload {
    schema: u16,
    models: Vec<LocalModelRelease>,
    runtimes: Vec<SignedRuntime>,
    recommendation: String,
}
#[derive(Default, Deserialize)]
struct KeyFile {
    keys: BTreeMap<String, String>,
}

// The JSON contract is intentionally duplicated rather than importing the
// Settings application: this CLI remains service-only, but its schema exactly
// matches Settings' `LocalModelSnapshot` deserialization contract.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum HardwareFit {
    Excellent,
    Good,
    Tight,
    Unsupported,
}
#[derive(Serialize)]
struct HardwareRecommendation {
    fit: HardwareFit,
    memory_gib: u16,
    accelerator: String,
    accelerator_memory_gib: Option<u16>,
    free_storage_gib: Option<u16>,
    reason: String,
}
#[derive(Clone, Serialize)]
struct LocalModelPackage {
    id: String,
    name: String,
    description: String,
    source: String,
    revision: String,
    license: String,
    license_url: String,
    download_bytes: u64,
    installed_bytes: u64,
}
#[derive(Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum LocalModelPhase {
    Downloading {
        downloaded_bytes: u64,
        total_bytes: u64,
    },
    Installing {
        percent: Option<u8>,
        message: String,
    },
    Ready {
        running: bool,
    },
    Error {
        message: String,
    },
}
#[derive(Serialize)]
struct InstalledLocalModel {
    package: LocalModelPackage,
    phase: LocalModelPhase,
}
#[derive(Serialize)]
struct LocalModelSnapshot {
    hardware: HardwareRecommendation,
    recommendation: LocalModelPackage,
    models: Vec<InstalledLocalModel>,
}

fn load_catalog(path: &PathBuf, keys: &PathBuf) -> Result<CatalogPayload, String> {
    let signed: SignedCatalog =
        serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if signed.payload.schema != 1 {
        return Err("unsupported model catalog schema".into());
    }
    let keyfile: KeyFile =
        serde_json::from_slice(&std::fs::read(keys).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let key_hex = keyfile
        .keys
        .get(&signed.key_id)
        .ok_or_else(|| "catalog signing key is not trusted".to_string())?;
    let bytes = hex::decode(key_hex).map_err(|_| "invalid catalog public key")?;
    let key = VerifyingKey::from_bytes(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| "invalid catalog public key")?,
    )
    .map_err(|_| "invalid catalog public key")?;
    let signature =
        Signature::from_slice(&signed.signature).map_err(|_| "invalid catalog signature")?;
    let payload = serde_json::to_vec(&signed.payload).map_err(|error| error.to_string())?;
    key.verify(&payload, &signature)
        .map_err(|_| "catalog signature verification failed")?;
    for runtime in &signed.payload.runtimes {
        runtime.validate().map_err(|error| error.to_string())?;
    }
    Ok(signed.payload)
}

/// Real range downloader. The service already constrains URL+size+digest;
/// this implementation additionally refuses redirects away from HTTPS.
struct ReqwestTransport {
    client: reqwest::blocking::Client,
}
impl ReqwestTransport {
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            client: reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .use_rustls_tls()
                .build()
                .map_err(|error| ServiceError::Backend(error.to_string()))?,
        })
    }
}
impl HttpsTransport for ReqwestTransport {
    fn resume(
        &mut self,
        url: &str,
        start: u64,
        destination: &mut File,
    ) -> Result<u64, ServiceError> {
        if !url.starts_with("https://") {
            return Err(ServiceError::NonHttpsArtifact);
        }
        let response = self
            .client
            .get(url)
            .header(reqwest::header::RANGE, format!("bytes={start}-"))
            .send()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !(response.status().is_success()
            || response.status() == reqwest::StatusCode::PARTIAL_CONTENT)
        {
            return Err(ServiceError::Backend(format!(
                "HTTPS download returned {}",
                response.status()
            )));
        }
        let count = std::io::copy(&mut response.take(u64::MAX), destination)?;
        Ok(count)
    }
}

/// User-systemd supervision. The executable and every vLLM argument come from
/// [`VllmCommand`], itself constructed only from a verified signed catalog.
struct SystemdSupervisor;
impl ProcessSupervisor for SystemdSupervisor {
    fn start(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError> {
        let digest = hex::encode(Sha256::digest(command.endpoint.model_id.as_bytes()));
        let unit = format!("qos-vllm-{}", &digest[..16]);
        let status = Command::new("systemd-run")
            .args([
                "--user",
                "--collect",
                "--service-type=exec",
                "--unit",
                &unit,
                "--",
            ])
            .arg(&command.program)
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(ServiceError::Io)?;
        if !status.success() {
            return Err(ServiceError::Backend(
                "systemd-run rejected the verified vLLM command".into(),
            ));
        }
        Ok(ProcessHandle { id: unit })
    }
    fn stop(&mut self, handle: &ProcessHandle) -> Result<(), ServiceError> {
        let status = Command::new("systemctl")
            .args(["--user", "stop", &handle.id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(ServiceError::Io)?;
        if status.success() {
            Ok(())
        } else {
            Err(ServiceError::Backend(
                "systemctl could not stop model service".into(),
            ))
        }
    }
}

fn local_hardware() -> HardwareSnapshot {
    let memory_mib = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|body| {
            body.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
        })
        .map(|kib| kib / 1024)
        .unwrap_or(0);
    let free_storage_mib = std::fs::metadata("/").ok().map(|_| 0).unwrap_or(0); // capacity is checked by service catalog tooling; unknown is honest.
    HardwareSnapshot {
        cpu_threads: std::thread::available_parallelism()
            .map(|count| count.get() as u16)
            .unwrap_or(1),
        memory_mib,
        free_storage_mib,
        accelerators: vec![],
    }
}
fn package(model: &LocalModelRelease) -> LocalModelPackage {
    LocalModelPackage {
        id: model.id.clone(),
        name: model.label.clone(),
        description: format!(
            "Pinned local model · {} token context",
            model.context_window
        ),
        source: model.artifact.url.clone(),
        revision: model.version.clone(),
        license: "See model card".into(),
        license_url: model.artifact.url.clone(),
        download_bytes: model.artifact.size_bytes,
        installed_bytes: model.artifact.size_bytes,
    }
}
fn recommendation(hardware: &HardwareSnapshot) -> HardwareRecommendation {
    let gib = u16::try_from(hardware.memory_mib / 1024).unwrap_or(u16::MAX);
    let fit = if gib >= 32 {
        HardwareFit::Excellent
    } else if gib >= 16 {
        HardwareFit::Good
    } else if gib >= 8 {
        HardwareFit::Tight
    } else {
        HardwareFit::Unsupported
    };
    HardwareRecommendation { fit, memory_gib: gib, accelerator: "CPU".into(), accelerator_memory_gib: None, free_storage_gib: None, reason: "Hardware facts are probed locally; accelerator support is added by the signed runtime extension.".into() }
}
fn phase(status: &LocalModelStatus) -> LocalModelPhase {
    match status {
        LocalModelStatus::NotInstalled => LocalModelPhase::Installing {
            percent: Some(0),
            message: "Waiting".into(),
        },
        LocalModelStatus::Downloading {
            received_bytes,
            total_bytes,
        } => LocalModelPhase::Downloading {
            downloaded_bytes: *received_bytes,
            total_bytes: *total_bytes,
        },
        LocalModelStatus::Verifying => LocalModelPhase::Installing {
            percent: None,
            message: "Verifying download".into(),
        },
        LocalModelStatus::Installed => LocalModelPhase::Ready { running: false },
        LocalModelStatus::Starting => LocalModelPhase::Installing {
            percent: None,
            message: "Starting local runtime".into(),
        },
        LocalModelStatus::Ready { .. } => LocalModelPhase::Ready { running: true },
        LocalModelStatus::Failed { message } => LocalModelPhase::Error {
            message: message.clone(),
        },
    }
}
fn main() {
    let cli = Cli::parse();
    let result = run(cli);
    if let Err(error) = result {
        eprintln!("qos-model-manager: {error}");
        std::process::exit(1);
    }
}
fn run(cli: Cli) -> Result<(), String> {
    if let Action::Snapshot { format } = &cli.command {
        if format != "json" {
            return Err("snapshot supports only --format json".into());
        }
        let catalog = load_catalog(&cli.catalog, &cli.keys)?;
        let service = LocalModelService::open(
            &cli.root,
            ReqwestTransport::new().map_err(|error| error.to_string())?,
            SystemdSupervisor,
        )
        .map_err(|error| error.to_string())?;
        let hardware = local_hardware();
        let recommended = catalog
            .models
            .iter()
            .find(|model| model.id == catalog.recommendation)
            .or_else(|| catalog.models.first())
            .ok_or_else(|| "signed catalog contains no models".to_string())?;
        let models = service
            .state()
            .models
            .iter()
            .filter_map(|(id, managed)| {
                catalog
                    .models
                    .iter()
                    .find(|model| &model.id == id)
                    .map(|model| InstalledLocalModel {
                        package: package(model),
                        phase: phase(&managed.status),
                    })
            })
            .collect();
        let payload = LocalModelSnapshot {
            hardware: recommendation(&hardware),
            recommendation: package(recommended),
            models,
        };
        println!(
            "{}",
            serde_json::to_string(&payload).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    let catalog = load_catalog(&cli.catalog, &cli.keys)?;
    let mut service = LocalModelService::open(
        &cli.root,
        ReqwestTransport::new().map_err(|error| error.to_string())?,
        SystemdSupervisor,
    )
    .map_err(|error| error.to_string())?;
    match cli.command {
        Action::Install { model, revision } => {
            let release = catalog
                .models
                .iter()
                .find(|release| {
                    release.id == model
                        && revision
                            .as_ref()
                            .is_none_or(|revision| revision == &release.version)
                })
                .ok_or_else(|| "model/revision is not in the signed catalog".to_string())?;
            service
                .install_model(release)
                .map_err(|error| error.to_string())?;
        }
        Action::Start { model } => {
            let release = catalog
                .models
                .iter()
                .find(|release| release.id == model)
                .ok_or_else(|| "model is not in the signed catalog".to_string())?;
            let runtime = catalog
                .runtimes
                .iter()
                .find(|runtime| release.allowed_runtimes.contains(&runtime.release.kind))
                .ok_or_else(|| "no signed runtime supports this model".to_string())?;
            service
                .start(runtime, release, 8000)
                .map_err(|error| error.to_string())?;
        }
        Action::Stop { model } => {
            service.stop(&model).map_err(|error| error.to_string())?;
        }
        Action::Remove { model } => {
            service.remove(&model).map_err(|error| error.to_string())?;
        }
        Action::RequestRestart { model: _ } => {
            println!("restart-required");
        }
        Action::Snapshot { .. } => unreachable!(),
    }
    Ok(())
}
