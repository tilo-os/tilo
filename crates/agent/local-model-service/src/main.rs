//! `qos-model-manager` — the small production CLI Settings invokes.
//!
//! It accepts only signed catalog selections, writes only its private store,
//! downloads only pinned HTTPS URLs, and delegates process lifetime to a
//! fixed-argv systemd user service. No subcommand accepts a URL, command, tag,
//! model path, or arbitrary process argument.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agent_provider_config::local::{
    Accelerator, HardwareSnapshot, LocalModelRelease, LocalModelStatus,
};
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
    /// Developer-only override. It is rejected whenever the manager has
    /// elevated privileges, so a GUI caller cannot redirect privileged writes.
    #[arg(long)]
    root: Option<PathBuf>,
    #[arg(long)]
    catalog: Option<PathBuf>,
    #[arg(long)]
    keys: Option<PathBuf>,
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
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        model: String,
        #[arg(long)]
        revision: Option<String>,
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
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(30 * 60))
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
        maximum_bytes: u64,
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
        if start > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(ServiceError::Backend(
                "resumed HTTPS request did not return 206 Partial Content".into(),
            ));
        }
        if start == 0 && !response.status().is_success() {
            return Err(ServiceError::Backend(format!(
                "HTTPS download returned {}",
                response.status()
            )));
        }
        if start > 0 {
            let range = response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("");
            if !range.starts_with(&format!("bytes {start}-")) {
                return Err(ServiceError::Backend(
                    "resumed HTTPS response had an invalid Content-Range".into(),
                ));
            }
        }
        if response
            .content_length()
            .is_some_and(|length| length > maximum_bytes)
        {
            return Err(ServiceError::PartialTooLarge);
        }
        let mut response = response;
        let mut count = 0_u64;
        let mut buffer = [0_u8; 32 * 1024];
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let read = u64::try_from(read).map_err(|_| ServiceError::PartialTooLarge)?;
            count = count
                .checked_add(read)
                .ok_or(ServiceError::PartialTooLarge)?;
            if count > maximum_bytes {
                return Err(ServiceError::PartialTooLarge);
            }
            destination.write_all(
                &buffer[..usize::try_from(read).map_err(|_| ServiceError::PartialTooLarge)?],
            )?;
        }
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
        let devices: &[&str] = match command.runtime_kind {
            agent_provider_config::local::RuntimeKind::Cpu => &[],
            agent_provider_config::local::RuntimeKind::NvidiaCuda => {
                &["--device", "nvidia.com/gpu=all"]
            }
            agent_provider_config::local::RuntimeKind::AmdRocm => {
                &["--device", "/dev/kfd", "--device", "/dev/dri"]
            }
        };
        let status = Command::new("systemd-run")
            .args([
                "--user",
                "--collect",
                "--service-type=exec",
                "--unit",
                &unit,
                "--",
                "podman",
                "run",
                "--rm",
                "--pull",
                "never",
                "--network",
                "host",
            ])
            .args(devices)
            .arg(&command.runtime_image)
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
        let address = command
            .endpoint
            .url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or_default();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect_timeout(
                &address.parse().map_err(|_| ServiceError::InvalidEndpoint)?,
                Duration::from_millis(250),
            )
            .is_ok()
            {
                return Ok(ProcessHandle { id: unit });
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &unit])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return Err(ServiceError::Backend(
            "verified vLLM service did not become ready".into(),
        ));
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
    let free_storage_mib = free_storage_mib("/var/lib/qos/models").unwrap_or(0);
    let mut accelerators = Vec::new();
    // NVIDIA's vendor tool is called with a fixed query/format, never a
    // caller-provided argument. Sysfs discovery keeps absence honest.
    if let Ok(output) = Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.total,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        && output.status.success()
    {
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let mut values = line.split(',').map(str::trim);
            if let (Some(memory), Some(driver)) = (values.next(), values.next())
                && let Ok(vram_mib) = memory.parse()
            {
                accelerators.push(Accelerator::Nvidia {
                    vram_mib,
                    driver: driver.into(),
                });
            }
        }
    }
    let rocm = std::fs::read_to_string("/opt/rocm/.info/version")
        .ok()
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty());
    if let Ok(cards) = std::fs::read_dir("/sys/class/drm") {
        for card in cards.flatten() {
            let device = card.path().join("device");
            if std::fs::read_to_string(device.join("vendor"))
                .ok()
                .is_some_and(|vendor| vendor.trim() == "0x1002")
            {
                let vram_mib = std::fs::read_to_string(device.join("mem_info_vram_total"))
                    .ok()
                    .and_then(|bytes| bytes.trim().parse::<u64>().ok())
                    .map(|bytes| bytes / 1024 / 1024)
                    .unwrap_or(0);
                accelerators.push(Accelerator::Amd {
                    vram_mib,
                    rocm: rocm.clone(),
                });
            }
        }
    }
    HardwareSnapshot {
        cpu_threads: std::thread::available_parallelism()
            .map(|count| count.get() as u16)
            .unwrap_or(1),
        memory_mib,
        free_storage_mib,
        accelerators,
    }
}
fn free_storage_mib(path: &str) -> Option<u64> {
    let path = std::ffi::CString::new(path).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return None;
    }
    let info = unsafe { info.assume_init() };
    u64::try_from(info.f_bavail)
        .ok()?
        .checked_mul(u64::try_from(info.f_frsize).ok()?)?
        .checked_div(1024 * 1024)
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
fn paths(cli: &Cli) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    // This binary is normally invoked through a privileged desktop service.
    // Do not permit a GUI client to redirect root/catalog/key paths in that
    // mode, even if it can influence argv.
    let elevated = unsafe { libc::geteuid() == 0 };
    if elevated && (cli.root.is_some() || cli.catalog.is_some() || cli.keys.is_some()) {
        return Err("path overrides are forbidden for the privileged model manager".into());
    }
    Ok((
        cli.root
            .clone()
            .unwrap_or_else(|| PathBuf::from("/var/lib/qos/models")),
        cli.catalog
            .clone()
            .unwrap_or_else(|| PathBuf::from("/etc/qos/models/catalog.json")),
        cli.keys
            .clone()
            .unwrap_or_else(|| PathBuf::from("/etc/qos/models/keys.json")),
    ))
}

fn allocate_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

fn run(cli: Cli) -> Result<(), String> {
    let (root, catalog_path, key_path) = paths(&cli)?;
    if let Action::Snapshot { format } = &cli.command {
        if format != "json" {
            return Err("snapshot supports only --format json".into());
        }
        let catalog = load_catalog(&catalog_path, &key_path)?;
        let service = LocalModelService::open(
            &root,
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
    let catalog = load_catalog(&catalog_path, &key_path)?;
    let mut service = LocalModelService::open(
        &root,
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
                .queue_install(release)
                .map_err(|error| error.to_string())?;
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let mut child = Command::new(executable);
            if unsafe { libc::geteuid() != 0 } {
                child
                    .arg("--root")
                    .arg(&root)
                    .arg("--catalog")
                    .arg(&catalog_path)
                    .arg("--keys")
                    .arg(&key_path);
            }
            child
                .args(["worker", "--model", &model])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(revision) = revision.as_deref() {
                child.args(["--revision", revision]);
            }
            child
                .spawn()
                .map_err(|error| format!("could not enqueue local model worker: {error}"))?;
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
                .start(runtime, release, allocate_loopback_port()?)
                .map_err(|error| error.to_string())?;
        }
        Action::Stop { model } => {
            service.stop(&model).map_err(|error| error.to_string())?;
        }
        Action::Remove { model } => {
            service.remove(&model).map_err(|error| error.to_string())?;
        }
        Action::RequestRestart { model: _ } => {
            return Err(
                "restart requests must be performed by the authorized system update service".into(),
            );
        }
        Action::Worker { model, revision } => {
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
            let runtime = catalog
                .runtimes
                .iter()
                .find(|runtime| release.allowed_runtimes.contains(&runtime.release.kind))
                .ok_or_else(|| "no signed runtime supports this model".to_string())?;
            if let Err(error) = service.install_with_runtime(runtime, release) {
                let _ = service.record_failure(&model, error.to_string());
                return Err(error.to_string());
            }
        }
        Action::Snapshot { .. } => unreachable!(),
    }
    Ok(())
}
