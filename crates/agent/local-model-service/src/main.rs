//! `qos-model-manager` — the small production CLI Settings invokes.
//!
//! It accepts only signed catalog selections, writes only its private store,
//! downloads only pinned HTTPS URLs, and delegates process lifetime to a
//! fixed-argv systemd user service. No subcommand accepts a URL, command, tag,
//! model path, or arbitrary process argument.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agent_provider_config::local::{
    Accelerator, HardwareSnapshot, LocalEndpoint, LocalModelCatalog, LocalModelRelease,
    LocalModelStatus, RuntimeDecision, RuntimeKind, select_runtime,
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
        /// Explicit acknowledgement for a catalog entry that requires it.
        #[arg(long)]
        accept_license: bool,
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
    /// The launcher-owned per-user service endpoint. Sandboxed apps reach it
    /// through `QOS_SVC_MODELS_FD`; it alone has network and storage access.
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        socket: PathBuf,
    },
}

const MODELS_SERVICE_FD: &str = "QOS_SVC_MODELS_FD";
const RPC_VERSION: u16 = 1;
const MAX_RPC_FRAME: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
enum RpcAction {
    Snapshot,
    Install {
        model: String,
        revision: Option<String>,
        accept_license: bool,
    },
    Start {
        model: String,
    },
    Stop {
        model: String,
    },
    Remove {
        model: String,
    },
    RequestRestart {
        model: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct RpcRequest {
    version: u16,
    action: RpcAction,
}

#[derive(Debug, Serialize, Deserialize)]
struct RpcResponse {
    ok: bool,
    output: Option<String>,
    error: Option<String>,
}

/// Signed, release-channel catalog. The signature covers canonical JSON of
/// `payload`; model and runtime artifacts within it remain individually pinned.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SignedCatalog {
    schema: u16,
    signature_domain: String,
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
        endpoint: Option<LocalEndpoint>,
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
    recommendation: Option<LocalModelPackage>,
    models: Vec<InstalledLocalModel>,
}

fn load_catalog(path: &PathBuf, keys: &PathBuf) -> Result<CatalogPayload, String> {
    let signed: SignedCatalog =
        serde_json::from_slice(&std::fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    if signed.schema != 1 {
        return Err("unsupported signed catalog envelope schema".into());
    }
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
    if signed.signature_domain != "qos.local-model.catalog.v1" {
        return Err("unexpected catalog signature domain".into());
    }
    let mut payload = b"qos.local-model.catalog.v1\0".to_vec();
    payload.extend(serde_json::to_vec(&signed.payload).map_err(|error| error.to_string())?);
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
        progress: &mut dyn FnMut(u64),
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
            // State is published by the service after every confirmed write,
            // not optimistically from Content-Length.
            progress(count);
        }
        Ok(count)
    }
}

/// User-systemd supervision. The executable and every vLLM argument come from
/// [`VllmCommand`], itself constructed only from a verified signed catalog.
struct SystemdSupervisor;
fn podman_vllm_argv(command: &VllmCommand) -> Vec<String> {
    let mut argv = vec![
        "run".into(),
        "--rm".into(),
        "--pull".into(),
        "never".into(),
        "--network".into(),
        "host".into(),
    ];
    match command.runtime_kind {
        agent_provider_config::local::RuntimeKind::Cpu => {}
        agent_provider_config::local::RuntimeKind::NvidiaCuda => {
            argv.extend(["--device".into(), "nvidia.com/gpu=all".into()]);
        }
        agent_provider_config::local::RuntimeKind::AmdRocm => {
            argv.extend([
                "--device".into(),
                "/dev/kfd".into(),
                "--device".into(),
                "/dev/dri".into(),
            ]);
        }
    }
    // The host object is derived exclusively from its verified SHA-256 path.
    // `label=disable` lets rootless podman read the existing per-user store
    // on SELinux systems without recursively relabeling shared model bytes.
    argv.extend([
        "--mount".into(),
        format!(
            "type=bind,src={},dst={},ro",
            command.model_path.display(),
            command.model_container_path
        ),
        "--security-opt".into(),
        "label=disable".into(),
        command.runtime_image.clone(),
    ]);
    argv.extend(command.args.iter().cloned());
    argv
}
impl ProcessSupervisor for SystemdSupervisor {
    fn install_runtime_image(
        &mut self,
        archive: &std::path::Path,
        signed_image: &str,
    ) -> Result<(), ServiceError> {
        // `podman load` receives a verified, content-addressed archive path;
        // it is never given a registry tag or caller-controlled image string.
        let status = Command::new("podman")
            .args(["load", "--input"])
            .arg(archive)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(ServiceError::Io)?;
        if !status.success() {
            return Err(ServiceError::Backend(
                "podman could not import verified runtime archive".into(),
            ));
        }
        let output = Command::new("podman")
            .args([
                "image",
                "inspect",
                "--format",
                "{{json .RepoDigests}}",
                signed_image,
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(ServiceError::Io)?;
        if !output.status.success() {
            return Err(ServiceError::Backend(
                "podman cannot inspect imported signed runtime image".into(),
            ));
        }
        let digests: Vec<String> = serde_json::from_slice(&output.stdout).map_err(|_| {
            ServiceError::Backend("podman returned invalid image digest metadata".into())
        })?;
        if !digests.iter().any(|digest| digest == signed_image) {
            return Err(ServiceError::Backend(
                "imported OCI archive did not provide the exact signed image digest".into(),
            ));
        }
        Ok(())
    }
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
                "podman",
            ])
            .args(podman_vllm_argv(command))
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
            let vendor = std::fs::read_to_string(device.join("vendor"))
                .ok()
                .map(|value| value.trim().to_string());
            if vendor.as_deref() == Some("0x1002") {
                let vram_mib = std::fs::read_to_string(device.join("mem_info_vram_total"))
                    .ok()
                    .and_then(|bytes| bytes.trim().parse::<u64>().ok())
                    .map(|bytes| bytes / 1024 / 1024)
                    .unwrap_or(0);
                accelerators.push(Accelerator::Amd {
                    vram_mib,
                    rocm: rocm.clone(),
                });
            } else if vendor.as_deref() == Some("0x10de")
                && !accelerators
                    .iter()
                    .any(|item| matches!(item, Accelerator::Nvidia { .. }))
            {
                // The PCI device is real even when `nvidia-smi` is absent.
                // Retain that fact so policy asks for the signed driver boot
                // pack instead of quietly launching an unsuitable CPU model.
                accelerators.push(Accelerator::Nvidia {
                    vram_mib: 0,
                    driver: String::new(),
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
        description: model.description.clone(),
        source: model.artifact.url.clone(),
        revision: model.version.clone(),
        license: model.license_id.clone(),
        license_url: model.license_url.clone(),
        download_bytes: model.artifact.size_bytes,
        installed_bytes: model.installed_size_bytes,
    }
}
fn runtime_kind_for(
    hardware: &HardwareSnapshot,
    available: impl IntoIterator<Item = RuntimeKind>,
) -> Result<RuntimeKind, String> {
    let kind = match select_runtime(hardware) {
        RuntimeDecision::Ready(kind) => kind,
        RuntimeDecision::Unavailable(reason) => return Err(reason.into()),
    };
    if kind == RuntimeKind::Cpu
        && hardware
            .accelerators
            .iter()
            .any(|accelerator| match accelerator {
                Accelerator::Nvidia { driver, .. } => driver.trim().is_empty(),
                Accelerator::Amd { rocm, .. } => rocm.is_none(),
            })
    {
        return Err(
            "An accelerator was detected but its signed driver runtime is inactive. Install the matching signed driver boot pack and restart before local inference."
                .into(),
        );
    }
    if available.into_iter().any(|candidate| candidate == kind) {
        Ok(kind)
    } else if kind == RuntimeKind::Cpu {
        Err("The signed CPU runtime is unavailable in this image.".into())
    } else {
        Err(format!(
            "{} was detected, but its signed runtime is not active. Install the matching signed driver boot pack and restart before local inference.",
            kind.label()
        ))
    }
}

fn model_catalog(catalog: &CatalogPayload) -> LocalModelCatalog {
    LocalModelCatalog {
        runtimes: catalog
            .runtimes
            .iter()
            .map(|runtime| runtime.release.clone())
            .collect(),
        models: catalog.models.clone(),
    }
}

fn recommended_model<'a>(
    catalog: &'a CatalogPayload,
    hardware: &HardwareSnapshot,
) -> Result<(&'a LocalModelRelease, RuntimeKind), String> {
    let kind = runtime_kind_for(
        hardware,
        catalog.runtimes.iter().map(|runtime| runtime.release.kind),
    )?;
    let model_id = model_catalog(catalog)
        .recommend(hardware, kind)
        .map(|model| model.id.clone())
        .ok_or_else(|| {
            format!(
                "No signed {} model fits the detected RAM, accelerator memory, and free storage.",
                kind.label()
            )
        })?;
    let model = catalog
        .models
        .iter()
        .find(|candidate| candidate.id == model_id)
        .ok_or_else(|| "catalog recommendation target disappeared".to_string())?;
    Ok((model, kind))
}

fn signed_runtime_for<'a>(
    catalog: &'a CatalogPayload,
    hardware: &HardwareSnapshot,
    model: &LocalModelRelease,
) -> Result<&'a SignedRuntime, String> {
    let kind = runtime_kind_for(
        hardware,
        catalog.runtimes.iter().map(|runtime| runtime.release.kind),
    )?;
    catalog
        .runtimes
        .iter()
        .filter(|runtime| runtime.release.kind == kind && model.allowed_runtimes.contains(&kind))
        .max_by_key(|runtime| &runtime.release.version)
        .ok_or_else(|| format!("The selected model has no signed {} runtime.", kind.label()))
}

fn recommendation(
    hardware: &HardwareSnapshot,
    selected: Result<RuntimeKind, String>,
    has_model: bool,
) -> HardwareRecommendation {
    let gib = u16::try_from(hardware.memory_mib / 1024).unwrap_or(u16::MAX);
    let (accelerator, accelerator_memory_gib, runtime_reason) = match selected {
        Ok(RuntimeKind::NvidiaCuda) => {
            let memory = hardware
                .accelerators
                .iter()
                .filter_map(|item| match item {
                    Accelerator::Nvidia { vram_mib, .. } => Some(*vram_mib),
                    _ => None,
                })
                .max();
            (
                "NVIDIA CUDA".into(),
                memory.map(|value| u16::try_from(value / 1024).unwrap_or(u16::MAX)),
                "A signed CUDA runtime was selected from locally probed driver and VRAM facts."
                    .into(),
            )
        }
        Ok(RuntimeKind::AmdRocm) => {
            let memory = hardware
                .accelerators
                .iter()
                .filter_map(|item| match item {
                    Accelerator::Amd {
                        vram_mib,
                        rocm: Some(_),
                        ..
                    } => Some(*vram_mib),
                    _ => None,
                })
                .max();
            (
                "AMD ROCm".into(),
                memory.map(|value| u16::try_from(value / 1024).unwrap_or(u16::MAX)),
                "A signed ROCm runtime was selected from locally probed driver and VRAM facts."
                    .into(),
            )
        }
        Ok(RuntimeKind::Cpu) => (
            "CPU".into(),
            None,
            "No compatible accelerator runtime is active; the signed CPU runtime was selected."
                .into(),
        ),
        Err(reason) => ("Unavailable".into(), None, reason),
    };
    let fit = if !has_model || accelerator == "Unavailable" {
        HardwareFit::Unsupported
    } else if gib >= 32 {
        HardwareFit::Excellent
    } else if gib >= 16 {
        HardwareFit::Good
    } else if gib >= 8 {
        HardwareFit::Tight
    } else {
        HardwareFit::Unsupported
    };
    HardwareRecommendation {
        fit,
        memory_gib: gib,
        accelerator,
        accelerator_memory_gib,
        free_storage_gib: Some(u16::try_from(hardware.free_storage_mib / 1024).unwrap_or(u16::MAX)),
        reason: runtime_reason,
    }
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
        LocalModelStatus::Installed => LocalModelPhase::Ready {
            running: false,
            endpoint: None,
        },
        LocalModelStatus::Starting => LocalModelPhase::Installing {
            percent: None,
            message: "Starting local runtime".into(),
        },
        LocalModelStatus::Ready { endpoint } => LocalModelPhase::Ready {
            running: true,
            endpoint: Some(endpoint.clone()),
        },
        LocalModelStatus::Failed { message } => LocalModelPhase::Error {
            message: message.clone(),
        },
    }
}
fn main() {
    let cli = Cli::parse();
    let result = match &cli.command {
        Action::Serve { socket } => serve(socket),
        Action::Worker { .. } => run_local(cli),
        _ if service_fd().is_some() => forward_to_service(&cli),
        _ => run_local(cli),
    };
    match result {
        Ok(Some(output)) => println!("{output}"),
        Ok(None) => {}
        Err(error) => {
            eprintln!("qos-model-manager: {error}");
            std::process::exit(1);
        }
    }
}

fn service_fd() -> Option<RawFd> {
    let raw = std::env::var(MODELS_SERVICE_FD)
        .ok()?
        .parse::<RawFd>()
        .ok()?;
    if raw < 3 {
        return None;
    }
    // The launcher transfers one connected socket descriptor. Do not treat a
    // string value alone as authority: it must name a live descriptor.
    if unsafe { libc::fcntl(raw, libc::F_GETFD) } == -1 {
        return None;
    }
    Some(raw)
}

fn rpc_action(command: &Action) -> Result<RpcAction, String> {
    match command {
        Action::Snapshot { format } if format == "json" => Ok(RpcAction::Snapshot),
        Action::Snapshot { .. } => Err("snapshot supports only --format json".into()),
        Action::Install {
            model,
            revision,
            accept_license,
        } => Ok(RpcAction::Install {
            model: model.clone(),
            revision: revision.clone(),
            accept_license: *accept_license,
        }),
        Action::Start { model } => Ok(RpcAction::Start {
            model: model.clone(),
        }),
        Action::Stop { model } => Ok(RpcAction::Stop {
            model: model.clone(),
        }),
        Action::Remove { model } => Ok(RpcAction::Remove {
            model: model.clone(),
        }),
        Action::RequestRestart { model } => Ok(RpcAction::RequestRestart {
            model: model.clone(),
        }),
        Action::Worker { .. } | Action::Serve { .. } => {
            Err("internal model manager action cannot be proxied".into())
        }
    }
}

fn write_rpc<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if body.len() > MAX_RPC_FRAME {
        return Err("model service RPC frame is too large".into());
    }
    let size = u32::try_from(body.len()).map_err(|_| "model service RPC frame is too large")?;
    stream
        .write_all(&size.to_be_bytes())
        .map_err(|error| error.to_string())?;
    stream.write_all(&body).map_err(|error| error.to_string())
}

enum RpcReadError {
    Closed,
    Invalid(String),
}

fn read_rpc<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T, RpcReadError> {
    let mut size = [0_u8; 4];
    match stream.read_exact(&mut size) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(RpcReadError::Closed);
        }
        Err(error) => return Err(RpcReadError::Invalid(error.to_string())),
    }
    let size = usize::try_from(u32::from_be_bytes(size))
        .map_err(|_| RpcReadError::Invalid("invalid model service RPC frame".into()))?;
    if size > MAX_RPC_FRAME {
        return Err(RpcReadError::Invalid(
            "model service RPC frame is too large".into(),
        ));
    }
    let mut body = vec![0_u8; size];
    stream
        .read_exact(&mut body)
        .map_err(|error| RpcReadError::Invalid(error.to_string()))?;
    serde_json::from_slice(&body)
        .map_err(|_| RpcReadError::Invalid("invalid model service RPC payload".into()))
}

fn forward_to_service(cli: &Cli) -> Result<Option<String>, String> {
    // Path options are developer-only direct-mode facilities. A sandbox may
    // request only a typed catalog verb against the launcher-selected daemon.
    if cli.root.is_some() || cli.catalog.is_some() || cli.keys.is_some() {
        return Err("path overrides are unavailable through the model service".into());
    }
    let fd = service_fd().ok_or_else(|| "model service descriptor is unavailable".to_string())?;
    // SAFETY: the qOS launcher gives this process sole ownership of the
    // connected descriptor. It is consumed exactly once for this RPC.
    let mut stream = unsafe { UnixStream::from_raw_fd(fd) };
    write_rpc(
        &mut stream,
        &RpcRequest {
            version: RPC_VERSION,
            action: rpc_action(&cli.command)?,
        },
    )?;
    let response: RpcResponse = read_rpc(&mut stream).map_err(|error| match error {
        RpcReadError::Closed => "model service closed the RPC connection".into(),
        RpcReadError::Invalid(error) => error,
    })?;
    if response.ok {
        Ok(response.output)
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "model service rejected request".into()))
    }
}

fn same_user(stream: &UnixStream) -> bool {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` points to a correctly-sized output buffer and
    // `stream` is a Unix-domain socket supplied by UnixListener::accept.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    result == 0
        && usize::try_from(length).ok() == Some(std::mem::size_of::<libc::ucred>())
        // SAFETY: getsockopt succeeded and wrote the complete ucred buffer.
        && unsafe { credentials.assume_init().uid } == unsafe { libc::geteuid() }
}

fn cli_for_rpc(action: RpcAction) -> Cli {
    let command = match action {
        RpcAction::Snapshot => Action::Snapshot {
            format: "json".into(),
        },
        RpcAction::Install {
            model,
            revision,
            accept_license,
        } => Action::Install {
            model,
            revision,
            accept_license,
        },
        RpcAction::Start { model } => Action::Start { model },
        RpcAction::Stop { model } => Action::Stop { model },
        RpcAction::Remove { model } => Action::Remove { model },
        RpcAction::RequestRestart { model } => Action::RequestRestart { model },
    };
    Cli {
        root: None,
        catalog: None,
        keys: None,
        command,
    }
}

fn serve_requests(
    stream: &mut UnixStream,
    handler: impl Fn(RpcAction) -> Result<Option<String>, String>,
) {
    // The launcher holds the other endpoint for the Settings lifetime; every
    // small CLI child inherits a duplicate. Keep this connection alive across
    // sequential request frames instead of treating one child exit as a
    // service disconnect.
    loop {
        let response = match read_rpc::<RpcRequest>(stream) {
            Ok(request) if request.version == RPC_VERSION => match handler(request.action) {
                Ok(output) => RpcResponse {
                    ok: true,
                    output,
                    error: None,
                },
                Err(error) => RpcResponse {
                    ok: false,
                    output: None,
                    error: Some(error),
                },
            },
            Ok(_) => RpcResponse {
                ok: false,
                output: None,
                error: Some("unsupported model service RPC version".into()),
            },
            Err(RpcReadError::Closed) => return,
            Err(RpcReadError::Invalid(error)) => RpcResponse {
                ok: false,
                output: None,
                error: Some(error),
            },
        };
        if write_rpc(stream, &response).is_err() {
            return;
        }
    }
}

fn serve_client(mut stream: UnixStream) {
    if !same_user(&stream) {
        let _ = write_rpc(
            &mut stream,
            &RpcResponse {
                ok: false,
                output: None,
                error: Some("model service peer is not the session user".into()),
            },
        );
        return;
    }
    serve_requests(&mut stream, |action| run_local(cli_for_rpc(action)));
}

fn serve(socket: &PathBuf) -> Result<Option<String>, String> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if socket.exists() {
        let metadata = std::fs::symlink_metadata(socket).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_socket() {
            return Err("refusing to replace a non-socket model service path".into());
        }
        std::fs::remove_file(socket).map_err(|error| error.to_string())?;
    }
    let listener = UnixListener::bind(socket).map_err(|error| error.to_string())?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve_client(stream),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(None)
}
fn paths(cli: &Cli) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    // The model manager is deliberately a per-user service: Settings never
    // needs root or a writable image path to provision a model. Keep override
    // rejection when someone mistakenly elevates the normal user binary.
    let elevated = unsafe { libc::geteuid() == 0 };
    if elevated && (cli.root.is_some() || cli.catalog.is_some() || cli.keys.is_some()) {
        return Err("path overrides are forbidden for the privileged model manager".into());
    }
    Ok((
        cli.root.clone().unwrap_or_else(user_model_root),
        cli.catalog.clone().unwrap_or_else(|| {
            PathBuf::from("/usr/share/qos/system-extensions/local-model-catalog.json")
        }),
        cli.keys.clone().unwrap_or_else(|| {
            PathBuf::from("/usr/share/qos/system-extensions/local-model-keys.json")
        }),
    ))
}

fn user_model_root() -> PathBuf {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data).join("qos/models");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".local/share/qos/models")
}

fn allocate_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

fn run_local(cli: Cli) -> Result<Option<String>, String> {
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
        let (recommended, runtime) = match recommended_model(&catalog, &hardware) {
            Ok((model, kind)) => (Some(package(model)), Ok(kind)),
            Err(reason) => (None, Err(reason)),
        };
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
            hardware: recommendation(&hardware, runtime, recommended.is_some()),
            recommendation: recommended,
            models,
        };
        return Ok(Some(
            serde_json::to_string(&payload).map_err(|error| error.to_string())?,
        ));
    }
    let catalog = load_catalog(&catalog_path, &key_path)?;
    let mut service = LocalModelService::open(
        &root,
        ReqwestTransport::new().map_err(|error| error.to_string())?,
        SystemdSupervisor,
    )
    .map_err(|error| error.to_string())?;
    match cli.command {
        Action::Install {
            model,
            revision,
            accept_license,
        } => {
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
            if release.requires_acceptance && !accept_license {
                return Err(
                    "this signed model release requires --accept-license before download".into(),
                );
            }
            if release.requires_acceptance {
                service
                    .record_license_acceptance(release)
                    .map_err(|error| error.to_string())?;
            }
            service
                .queue_install(release)
                .map_err(|error| error.to_string())?;
            enqueue_worker(&root, &catalog_path, &key_path, &model, revision.as_deref()).map_err(
                |error| {
                    let _ = service.record_failure(&model, &error);
                    error
                },
            )?;
        }
        Action::Start { model } => {
            let release = catalog
                .models
                .iter()
                .find(|release| release.id == model)
                .ok_or_else(|| "model is not in the signed catalog".to_string())?;
            let runtime = signed_runtime_for(&catalog, &local_hardware(), release)?;
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
            let runtime = signed_runtime_for(&catalog, &local_hardware(), release)?;
            if let Err(error) = service.install_with_runtime_progress(runtime, release) {
                let _ = service.record_failure(&model, error.to_string());
                return Err(error.to_string());
            }
        }
        Action::Snapshot { .. } | Action::Serve { .. } => unreachable!(),
    }
    Ok(None)
}

fn enqueue_worker(
    root: &PathBuf,
    catalog: &PathBuf,
    keys: &PathBuf,
    model: &str,
    revision: Option<&str>,
) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let digest = hex::encode(Sha256::digest(model.as_bytes()));
    let unit = format!("qos-model-download-{}", &digest[..16]);
    let mut command = Command::new("systemd-run");
    command
        .args(["--user", "--collect", "--service-type=exec", "--unit"])
        .arg(&unit)
        .arg("--")
        .arg(executable);
    // The transient worker retains the selected developer store only outside
    // elevated mode. Packaged Settings never supplies these overrides.
    if unsafe { libc::geteuid() != 0 } {
        command
            .arg("--root")
            .arg(root)
            .arg("--catalog")
            .arg(catalog)
            .arg("--keys")
            .arg(keys);
    }
    command
        .args(["worker", "--model", model])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(revision) = revision {
        command.args(["--revision", revision]);
    }
    let status = command
        .status()
        .map_err(|error| format!("could not enqueue model worker: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("systemd-run rejected the durable model worker".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_provider_config::local::RuntimeKind;

    #[test]
    fn service_connection_serves_two_sequential_requests() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            serve_requests(&mut server, |action| match action {
                RpcAction::Snapshot => Ok(Some("snapshot".into())),
                RpcAction::Start { model } => Ok(Some(format!("started:{model}"))),
                _ => Err("unexpected test action".into()),
            });
        });
        write_rpc(
            &mut client,
            &RpcRequest {
                version: RPC_VERSION,
                action: RpcAction::Snapshot,
            },
        )
        .unwrap();
        let first: RpcResponse = read_rpc(&mut client).unwrap_or_else(|_| panic!("response one"));
        assert!(first.ok);
        assert_eq!(first.output.as_deref(), Some("snapshot"));
        write_rpc(
            &mut client,
            &RpcRequest {
                version: RPC_VERSION,
                action: RpcAction::Start {
                    model: "tiny".into(),
                },
            },
        )
        .unwrap();
        let second: RpcResponse = read_rpc(&mut client).unwrap_or_else(|_| panic!("response two"));
        assert!(second.ok);
        assert_eq!(second.output.as_deref(), Some("started:tiny"));
        drop(client);
        worker.join().unwrap();
    }

    #[test]
    fn podman_mounts_only_verified_model_path_and_uses_container_path() {
        let command = VllmCommand {
            runtime_image: "registry.example/qos/vllm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            runtime_kind: RuntimeKind::Cpu,
            args: vec!["serve".into(), "/models/model".into()],
            model_path: PathBuf::from("/home/ada/.local/share/qos/models/sha256/abc"),
            model_container_path: "/models/model".into(),
            endpoint: agent_provider_config::local::LocalEndpoint { url: "http://127.0.0.1:9000/v1".into(), model_id: "tiny".into() },
            runtime_extension_id: "runtime".into(),
        };
        let argv = podman_vllm_argv(&command);
        assert!(argv.windows(2).any(|pair| pair
            == [
                "--mount",
                "type=bind,src=/home/ada/.local/share/qos/models/sha256/abc,dst=/models/model,ro"
            ]));
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--security-opt", "label=disable"])
        );
        assert!(argv.ends_with(&["serve".into(), "/models/model".into()]));
        assert!(
            !argv
                .iter()
                .any(|arg| arg == "/home/ada/.local/share/qos/models/sha256/abc")
        );
    }

    #[test]
    fn runtime_selection_prefers_confirmed_nvidia_over_cpu() {
        let hardware = HardwareSnapshot {
            cpu_threads: 8,
            memory_mib: 32 * 1024,
            free_storage_mib: 100 * 1024,
            accelerators: vec![Accelerator::Nvidia {
                vram_mib: 12 * 1024,
                driver: "560.35".into(),
            }],
        };
        assert_eq!(
            runtime_kind_for(&hardware, [RuntimeKind::Cpu, RuntimeKind::NvidiaCuda]).unwrap(),
            RuntimeKind::NvidiaCuda
        );
        let error = runtime_kind_for(&hardware, [RuntimeKind::Cpu]).unwrap_err();
        assert!(error.contains("boot pack"));
    }

    #[test]
    fn runtime_selection_uses_rocm_only_when_driver_is_present() {
        let hardware = HardwareSnapshot {
            cpu_threads: 8,
            memory_mib: 32 * 1024,
            free_storage_mib: 100 * 1024,
            accelerators: vec![Accelerator::Amd {
                vram_mib: 16 * 1024,
                rocm: Some("6.2".into()),
            }],
        };
        assert_eq!(
            runtime_kind_for(&hardware, [RuntimeKind::AmdRocm]).unwrap(),
            RuntimeKind::AmdRocm
        );
        let mut missing_driver = hardware;
        missing_driver.accelerators = vec![Accelerator::Amd {
            vram_mib: 16 * 1024,
            rocm: None,
        }];
        let error = runtime_kind_for(&missing_driver, [RuntimeKind::Cpu]).unwrap_err();
        assert!(error.contains("boot pack"));
    }

    #[test]
    fn runtime_selection_uses_cpu_when_no_supported_accelerator_exists() {
        let hardware = HardwareSnapshot {
            cpu_threads: 4,
            memory_mib: 8 * 1024,
            free_storage_mib: 100 * 1024,
            accelerators: vec![],
        };
        assert_eq!(
            runtime_kind_for(&hardware, [RuntimeKind::Cpu]).unwrap(),
            RuntimeKind::Cpu
        );
    }
}
