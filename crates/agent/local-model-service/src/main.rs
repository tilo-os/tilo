//! `tilo-model-manager` — the small production CLI Settings invokes.
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
use std::path::{Path, PathBuf};
use std::process::Command;
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
    name = "tilo-model-manager",
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
    /// Ask the root-owned hardware policy for this machine's exact signed
    /// `BootPack` and enqueue it for immutable deployment staging.
    StageAccelerator,
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        model: String,
        #[arg(long)]
        revision: Option<String>,
    },
    /// Durable background activation. Kept separate from download so an
    /// already-installed model can be restarted without blocking Settings.
    #[command(hide = true)]
    WorkerStart {
        #[arg(long)]
        model: String,
    },
    /// The launcher-owned per-user service endpoint. Sandboxed apps reach it
    /// through `TILO_SVC_MODELS_FD`; it alone has network and storage access.
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        socket: PathBuf,
    },
}

const MODELS_SERVICE_FD: &str = "TILO_SVC_MODELS_FD";
const RPC_VERSION: u16 = 1;
/// Every RPC request and response: a big-endian `u32` length, then JSON.
const RPC_FRAMING: wire_frame::Framing = wire_frame::Framing::big_endian(1024 * 1024);

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
    StageAccelerator,
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
    vision: bool,
    face_recognition: bool,
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
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum AcceleratorSetupState {
    Available,
    Queued,
    Staging,
    RestartRequired,
    Active,
    Error { message: String },
}
#[derive(Serialize)]
struct AcceleratorSetup {
    boot_pack_id: String,
    status: AcceleratorSetupState,
}
#[derive(Serialize)]
struct LocalModelSnapshot {
    hardware: HardwareRecommendation,
    recommendation: Option<LocalModelPackage>,
    models: Vec<InstalledLocalModel>,
    accelerator_setup: Option<AcceleratorSetup>,
}

const SYSTEM_EXTENSION_SOCKET: &str = "/run/tilo/system-extension-api/request.sock";
const SYSTEM_EXTENSION_STATUS: &str = "/run/tilo/system-extension-api/status.json";

#[derive(Deserialize)]
struct BootPackRecommendations {
    schema_version: u16,
    recommendations: Vec<String>,
}

#[derive(Deserialize)]
struct BootPackStatus {
    schema_version: u16,
    activation_class: String,
    state: String,
    extension_id: String,
    reboot_required: bool,
    error: Option<String>,
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
    if signed.signature_domain != "tilo.local-model.catalog.v1" {
        return Err("unexpected catalog signature domain".into());
    }
    let mut payload = b"tilo.local-model.catalog.v1\0".to_vec();
    payload.extend(serde_json::to_vec(&signed.payload).map_err(|error| error.to_string())?);
    key.verify(&payload, &signature)
        .map_err(|_| "catalog signature verification failed")?;
    for runtime in &signed.payload.runtimes {
        runtime.validate().map_err(|error| error.to_string())?;
    }
    Ok(signed.payload)
}

/// Range downloader. The service already constrains URL+size+digest;
/// this implementation additionally refuses redirects away from HTTPS.
struct ReqwestTransport {
    client: reqwest::blocking::Client,
}
impl ReqwestTransport {
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            client: tilo_net::http::blocking_client()
                .redirect(reqwest::redirect::Policy::none())
                .tls_backend_rustls()
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
        let mut buffer = vec![0_u8; 32 * 1024];
        loop {
            let read = response.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            // `Read` never reports more bytes than the buffer holds.
            let chunk = buffer.get(..read).ok_or(ServiceError::PartialTooLarge)?;
            let read = u64::try_from(read).map_err(|_| ServiceError::PartialTooLarge)?;
            count = count
                .checked_add(read)
                .ok_or(ServiceError::PartialTooLarge)?;
            if count > maximum_bytes {
                return Err(ServiceError::PartialTooLarge);
            }
            destination.write_all(chunk)?;
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
        let mut load = Command::new("podman");
        load.args(["load", "--input"]).arg(archive);
        let output =
            bounded_command::run_prepared(load, None, 1024 * 1024, Duration::from_secs(1800))
                .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !output.status.success() {
            return Err(ServiceError::Backend(
                "podman could not import verified runtime archive".into(),
            ));
        }
        let mut inspect = Command::new("podman");
        inspect.args([
            "image",
            "inspect",
            "--format",
            "{{json .RepoDigests}}",
            signed_image,
        ]);
        let output =
            bounded_command::run_prepared(inspect, None, 1024 * 1024, Duration::from_secs(30))
                .map_err(|error| ServiceError::Backend(error.to_string()))?;
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
        let unit = format!("tilo-vllm-{}", digest.get(..16).unwrap_or(&digest));
        let mut launch = Command::new("systemd-run");
        launch
            .args([
                "--user",
                "--collect",
                "--service-type=exec",
                "--unit",
                &unit,
                "--",
                "podman",
            ])
            .args(podman_vllm_argv(command));
        let output =
            bounded_command::run_prepared(launch, None, 1024 * 1024, Duration::from_secs(30))
                .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !output.status.success() {
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
        // Loading model weights routinely takes minutes. This worker is
        // already detached from Settings, so favour a truthful bounded wait
        // over reporting a healthy large model as failed after 20 seconds.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10 * 60) {
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
        let mut stop = Command::new("systemctl");
        stop.args(["--user", "stop", &unit]);
        let _ = bounded_command::run_prepared(stop, None, 64 * 1024, Duration::from_secs(30));
        Err(ServiceError::Backend(
            "verified vLLM service did not become ready".into(),
        ))
    }
    fn stop(&mut self, handle: &ProcessHandle) -> Result<(), ServiceError> {
        let mut stop = Command::new("systemctl");
        stop.args(["--user", "stop", &handle.id]);
        let output = bounded_command::run_prepared(stop, None, 64 * 1024, Duration::from_secs(30))
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if output.status.success() {
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
        .map_or(0, |kib| kib / 1024);
    let free_storage_mib = free_storage_mib("/var/lib/tilo/models").unwrap_or(0);
    let mut accelerators = Vec::new();
    // NVIDIA's vendor tool is called with a fixed query/format, never a
    // caller-provided argument. Sysfs discovery distinguishes an absent
    // accelerator from an unavailable vendor tool.
    let mut nvidia = Command::new("nvidia-smi");
    nvidia.args([
        "--query-gpu=memory.total,driver_version",
        "--format=csv,noheader,nounits",
    ]);
    if let Ok(output) =
        bounded_command::run_prepared(nvidia, None, 1024 * 1024, Duration::from_secs(5))
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
                    .map_or(0, |bytes| bytes / 1024 / 1024);
                accelerators.push(Accelerator::Amd {
                    vram_mib,
                    rocm: rocm.clone(),
                });
            } else if vendor.as_deref() == Some("0x10de")
                && !accelerators
                    .iter()
                    .any(|item| matches!(item, Accelerator::Nvidia { .. }))
            {
                // Retain the PCI device when `nvidia-smi` is absent so policy
                // asks for the signed driver boot pack instead of quietly
                // launching an unsuitable CPU model.
                accelerators.push(Accelerator::Nvidia {
                    vram_mib: 0,
                    driver: String::new(),
                });
            }
        }
    }
    HardwareSnapshot {
        cpu_threads: std::thread::available_parallelism()
            .map_or(1, |count| u16::try_from(count.get()).unwrap_or(u16::MAX)),
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
    #[allow(
        clippy::useless_conversion,
        reason = "statvfs field widths differ between libc targets"
    )]
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
        vision: model.vision,
        face_recognition: model.face_recognition,
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
                    Accelerator::Amd { .. } => None,
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
    let is_serve = matches!(&cli.command, Action::Serve { .. });
    let result = match &cli.command {
        Action::Serve { socket } => serve(socket),
        Action::Worker { .. } | Action::WorkerStart { .. } => run_local(cli),
        _ if service_fd().is_some() => forward_to_service(&cli),
        _ => run_local(cli),
    };
    match result {
        Ok(Some(output)) => println!("{output}"),
        Ok(None) => {}
        Err(error) => {
            if is_serve {
                // Socket-activated serve plus Restart=on-failure is a loop
                // when the activation fd has no pathname or the store is
                // missing. Stay up so P03 does not see 33 restarts.
                eprintln!("tilo-model-manager idle: {error}");
                loop {
                    std::thread::park();
                }
            }
            eprintln!("tilo-model-manager: {error}");
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
        Action::StageAccelerator => Ok(RpcAction::StageAccelerator),
        Action::Worker { .. } | Action::WorkerStart { .. } | Action::Serve { .. } => {
            Err("internal model manager action cannot be proxied".into())
        }
    }
}

fn write_rpc<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), String> {
    let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    RPC_FRAMING
        .write(stream, &body)
        .map_err(|error| match error {
            wire_frame::FrameError::Io(error) => error.to_string(),
            _ => "model service RPC frame is too large".into(),
        })
}

enum RpcReadError {
    Closed,
    Invalid(String),
}

fn read_rpc<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T, RpcReadError> {
    let body = match RPC_FRAMING.read(stream) {
        Ok(Some(body)) => body,
        Ok(None) => return Err(RpcReadError::Closed),
        Err(wire_frame::FrameError::TooLarge(_)) => {
            return Err(RpcReadError::Invalid(
                "model service RPC frame is too large".into(),
            ));
        }
        Err(error) => return Err(RpcReadError::Invalid(error.to_string())),
    };
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
    // SAFETY: the Tilo launcher gives this process sole ownership of the
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
    let Ok(mut length) = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()) else {
        return false;
    };
    // SAFETY: `credentials` points to a correctly-sized output buffer and
    // `stream` is a Unix-domain socket supplied by UnixListener::accept.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &raw mut length,
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
        RpcAction::StageAccelerator => Action::StageAccelerator,
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
    let listener = if let Some(listener) = systemd_listener(socket)? {
        listener
    } else {
        bind_listener(socket)?
    };
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve_client(stream),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(None)
}

fn bind_listener(socket: &PathBuf) -> Result<UnixListener, String> {
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
    Ok(listener)
}

#[allow(
    clippy::disallowed_methods,
    reason = "systemd socket activation: LISTEN_* must not leak to spawned children; cleared once, right after the descriptor is taken at the start of `serve`, before any thread is spawned"
)]
fn systemd_listener(expected: &Path) -> Result<Option<UnixListener>, String> {
    const SYSTEMD_LISTEN_FD: RawFd = 3;

    let pid_matches = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(std::process::id());
    if !pid_matches {
        return Ok(None);
    }
    if std::env::var("LISTEN_FDS").as_deref() != Ok("1") {
        return Err("systemd must pass exactly one model-manager listener".into());
    }
    // SAFETY: fcntl only validates whether fd 3 is open; it consumes no pointer.
    if unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_GETFD) } == -1 {
        return Err("systemd model-manager listener fd is closed".into());
    }
    // SAFETY: systemd's LISTEN_PID/FDS contract transfers ownership of fd 3
    // to this process exactly once.
    let listener = unsafe { UnixListener::from_raw_fd(SYSTEMD_LISTEN_FD) };
    let actual = listener.local_addr().map_err(|error| error.to_string())?;
    if actual.as_pathname() != Some(expected) {
        return Err("systemd model-manager listener has an unexpected path".into());
    }
    // The manager starts Podman/model processes. Keep the authenticated
    // control listener and systemd's one-shot activation metadata out of every
    // child process spawned after activation.
    if unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err("cannot mark systemd model-manager listener close-on-exec".into());
    }
    // SAFETY: activation is consumed once, before worker threads exist.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    Ok(Some(listener))
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
            PathBuf::from("/usr/share/tilo/system-extensions/local-model-catalog.json")
        }),
        cli.keys.clone().unwrap_or_else(|| {
            PathBuf::from("/usr/share/tilo/system-extensions/local-model-keys.json")
        }),
    ))
}

fn user_model_root() -> PathBuf {
    xdg_paths::xdg::data_home()
        .unwrap_or_else(|| PathBuf::from("/tmp/.local/share"))
        .join("tilo/models")
}

fn allocate_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| error.to_string())
}

fn system_extension_request(request: &str) -> Result<Vec<u8>, String> {
    if request.contains(['\n', '\r']) {
        return Err("system extension request must be one line".into());
    }
    let mut stream = UnixStream::connect(SYSTEM_EXTENSION_SOCKET)
        .map_err(|error| format!("accelerator setup service is unavailable: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| error.to_string())?;
    let mut response = Vec::new();
    stream
        .take(64 * 1024)
        .read_to_end(&mut response)
        .map_err(|error| error.to_string())?;
    if response.is_empty() {
        return Err("accelerator setup service returned no response".into());
    }
    Ok(response)
}

fn recommended_boot_packs() -> Result<Vec<String>, String> {
    let response: BootPackRecommendations = serde_json::from_slice(&system_extension_request(
        "boot-pack-recommend",
    )?)
    .map_err(|error| format!("accelerator setup service returned invalid status: {error}"))?;
    if response.schema_version != 1 {
        return Err("accelerator setup service returned an unsupported schema".into());
    }
    Ok(response.recommendations)
}

fn accelerator_setup_snapshot() -> Option<AcceleratorSetup> {
    if let Ok(bytes) = std::fs::read(SYSTEM_EXTENSION_STATUS)
        && let Ok(status) = serde_json::from_slice::<BootPackStatus>(&bytes)
        && status.schema_version == 1
        && status.activation_class == "boot_pack"
        && !status.extension_id.is_empty()
    {
        let state = match status.state.as_str() {
            "queued" => AcceleratorSetupState::Queued,
            "staging" => AcceleratorSetupState::Staging,
            "staged" if status.reboot_required => AcceleratorSetupState::RestartRequired,
            "active" if !status.reboot_required => AcceleratorSetupState::Active,
            "failed" => AcceleratorSetupState::Error {
                message: status.error.unwrap_or_else(|| {
                    "The signed accelerator deployment could not be staged.".into()
                }),
            },
            _ => AcceleratorSetupState::Error {
                message: "The accelerator setup service returned an inconsistent state.".into(),
            },
        };
        return Some(AcceleratorSetup {
            boot_pack_id: status.extension_id,
            status: state,
        });
    }

    match recommended_boot_packs() {
        Ok(mut recommendations) if recommendations.len() == 1 => Some(AcceleratorSetup {
            boot_pack_id: recommendations.remove(0),
            status: AcceleratorSetupState::Available,
        }),
        Ok(recommendations) if recommendations.len() > 1 => Some(AcceleratorSetup {
            boot_pack_id: "multiple-devices".into(),
            status: AcceleratorSetupState::Error {
                message: "This device needs more than one accelerator BootPack; this release cannot compose them safely yet.".into(),
            },
        }),
        _ => None,
    }
}

fn stage_recommended_accelerator() -> Result<(), String> {
    let mut recommendations = recommended_boot_packs()?;
    if recommendations.len() != 1 {
        return Err(if recommendations.is_empty() {
            "No signed accelerator BootPack matches this device.".into()
        } else {
            "More than one accelerator BootPack matches this device; automatic staging is intentionally disabled.".into()
        });
    }
    let id = recommendations.remove(0);
    if id.is_empty()
        || id.starts_with(['.', '_', '-'])
        || id.ends_with(['.', '_', '-'])
        || !id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || ".-_".contains(character)
        })
    {
        return Err("hardware policy returned an invalid BootPack id".into());
    }
    let response = system_extension_request(&format!("boot-pack-stage {id}"))?;
    let accepted = serde_json::from_slice::<serde_json::Value>(&response)
        .ok()
        .and_then(|value| value.get("accepted").and_then(serde_json::Value::as_bool))
        .unwrap_or(false);
    if !accepted {
        return Err("accelerator setup service did not accept the staging request".into());
    }
    Ok(())
}

fn run_local(cli: Cli) -> Result<Option<String>, String> {
    let (root, catalog_path, key_path) = paths(&cli)?;
    if let Action::Snapshot { format } = &cli.command {
        return local_snapshot(&root, &catalog_path, &key_path, format).map(Some);
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
            enqueue_worker(&root, &catalog_path, &key_path, &model, revision.as_deref())
                .inspect_err(|error| {
                    let _ = service.record_failure(&model, error);
                })?;
        }
        Action::Start { model } => {
            if !catalog.models.iter().any(|release| release.id == model) {
                return Err("model is not in the signed catalog".to_string());
            }
            service
                .mark_starting(&model)
                .map_err(|error| error.to_string())?;
            enqueue_start_worker(&root, &catalog_path, &key_path, &model).inspect_err(|error| {
                let _ = service.record_failure(&model, error);
            })?;
        }
        Action::Stop { model } => {
            service.stop(&model).map_err(|error| error.to_string())?;
        }
        Action::Remove { model } => {
            service.remove(&model).map_err(|error| error.to_string())?;
        }
        Action::StageAccelerator => stage_recommended_accelerator()?,
        Action::Worker { model, revision } => {
            run_install_worker(&mut service, &catalog, &model, revision.as_ref())?;
        }
        Action::WorkerStart { model } => {
            let release = catalog
                .models
                .iter()
                .find(|release| release.id == model)
                .ok_or_else(|| "model is not in the signed catalog".to_string())?;
            let runtime = signed_runtime_for(&catalog, &local_hardware(), release)?;
            if let Err(error) = service.start(runtime, release, allocate_loopback_port()?) {
                let _ = service.record_failure(&model, error.to_string());
                return Err(error.to_string());
            }
        }
        // The dispatcher answers these itself and never hands them over.
        Action::Snapshot { .. } | Action::Serve { .. } => {
            return Err("not a local model action".into());
        }
    }
    Ok(None)
}

/// The signed catalog, this machine's recommendation and installed models,
/// as the JSON snapshot Settings renders.
fn local_snapshot(
    root: &PathBuf,
    catalog_path: &PathBuf,
    key_path: &PathBuf,
    format: &str,
) -> Result<String, String> {
    if format != "json" {
        return Err("snapshot supports only --format json".into());
    }
    let catalog = load_catalog(catalog_path, key_path)?;
    let service = LocalModelService::open(
        root,
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
        accelerator_setup: accelerator_setup_snapshot(),
    };
    serde_json::to_string(&payload).map_err(|error| error.to_string())
}

/// Install then start one signed release in the detached worker, recording
/// any failure in the service state Settings observes.
fn run_install_worker(
    service: &mut LocalModelService<ReqwestTransport, SystemdSupervisor>,
    catalog: &CatalogPayload,
    model: &str,
    revision: Option<&String>,
) -> Result<(), String> {
    let release = catalog
        .models
        .iter()
        .find(|release| {
            release.id == model && revision.is_none_or(|revision| revision == &release.version)
        })
        .ok_or_else(|| "model/revision is not in the signed catalog".to_string())?;
    let runtime = signed_runtime_for(catalog, &local_hardware(), release)?;
    if let Err(error) = service
        .install_with_runtime_progress(runtime, release)
        .and_then(|_| {
            service
                .start(
                    runtime,
                    release,
                    allocate_loopback_port().map_err(ServiceError::Backend)?,
                )
                .map(|_| ())
        })
    {
        let _ = service.record_failure(model, error.to_string());
        return Err(error.to_string());
    }
    Ok(())
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
    let unit = format!(
        "tilo-model-download-{}",
        digest.get(..16).unwrap_or(&digest)
    );
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
    command.args(["worker", "--model", model]);
    if let Some(revision) = revision {
        command.args(["--revision", revision]);
    }
    let output = bounded_command::run_prepared(command, None, 64 * 1024, Duration::from_secs(30))
        .map_err(|error| format!("could not enqueue model worker: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err("systemd-run rejected the durable model worker".into())
    }
}

fn enqueue_start_worker(
    root: &PathBuf,
    catalog: &PathBuf,
    keys: &PathBuf,
    model: &str,
) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let digest = hex::encode(Sha256::digest(model.as_bytes()));
    let unit = format!("tilo-model-start-{}", digest.get(..16).unwrap_or(&digest));
    let mut command = Command::new("systemd-run");
    command
        .args(["--user", "--collect", "--service-type=exec", "--unit"])
        .arg(&unit)
        .arg("--")
        .arg(executable);
    if unsafe { libc::geteuid() != 0 } {
        command
            .arg("--root")
            .arg(root)
            .arg("--catalog")
            .arg(catalog)
            .arg("--keys")
            .arg(keys);
    }
    command.args(["worker-start", "--model", model]);
    let output = bounded_command::run_prepared(command, None, 64 * 1024, Duration::from_secs(30))
        .map_err(|error| format!("could not enqueue model start: {error}"))?;
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| "systemd-run rejected the durable model start".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_provider_config::local::RuntimeKind;

    /// The RPC wire bytes are pinned and one service connection serves two sequential requests.
    #[test]
    fn the_model_service_rpc_is_pinned_and_serves_sequential_requests() {
        // Phase: rpc_wire_bytes_are_pinned.
        // The model service RPC bytes: a big-endian `u32` length, then JSON. A
        // close before the length is a clean end of the connection.
        {
            let (mut client, mut server) = UnixStream::pair().unwrap();
            write_rpc(&mut client, &serde_json::json!({"k": 1})).unwrap();
            let mut sent = [0_u8; 11];
            server.read_exact(&mut sent).unwrap();
            assert_eq!(sent, *b"\0\0\0\x07{\"k\":1}");
            server.write_all(b"\0\0\0\x07{\"k\":2}").unwrap();
            let value: serde_json::Value = read_rpc(&mut client).unwrap_or_else(|_| panic!("read"));
            assert_eq!(value["k"], 2);
            drop(server);
            assert!(matches!(
                read_rpc::<serde_json::Value>(&mut client),
                Err(RpcReadError::Closed)
            ));
        }

        // Phase: service_connection_serves_two_sequential_requests.
        {
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
            let first: RpcResponse =
                read_rpc(&mut client).unwrap_or_else(|_| panic!("response one"));
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
            let second: RpcResponse =
                read_rpc(&mut client).unwrap_or_else(|_| panic!("response two"));
            assert!(second.ok);
            assert_eq!(second.output.as_deref(), Some("started:tiny"));
            drop(client);
            worker.join().unwrap();
        }
    }

    /// Podman mounts only the verified model path at its container path; runtime selection prefers a
    /// confirmed NVIDIA over CPU, uses `ROCm` only when the driver is present, and uses CPU when no
    /// supported accelerator exists.
    #[test]
    fn the_model_runtime_matches_the_verified_hardware() {
        // Phase: podman_mounts_only_verified_model_path_and_uses_container_path.
        {
            let command = VllmCommand {
                runtime_image: "registry.example/tilo/vllm@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                runtime_kind: RuntimeKind::Cpu,
                args: vec!["serve".into(), "/models/model".into()],
                model_path: PathBuf::from("/home/ada/.local/share/tilo/models/sha256/abc"),
                model_container_path: "/models/model".into(),
                endpoint: agent_provider_config::local::LocalEndpoint { url: "http://127.0.0.1:9000/v1".into(), model_id: "tiny".into() },
                runtime_extension_id: "runtime".into(),
            };
            let argv = podman_vllm_argv(&command);
            assert!(argv.windows(2).any(|pair| pair
                == [
                    "--mount",
                    "type=bind,src=/home/ada/.local/share/tilo/models/sha256/abc,dst=/models/model,ro"
                ]));
            assert!(
                argv.windows(2)
                    .any(|pair| pair == ["--security-opt", "label=disable"])
            );
            assert!(argv.ends_with(&["serve".into(), "/models/model".into()]));
            assert!(
                !argv
                    .iter()
                    .any(|arg| arg == "/home/ada/.local/share/tilo/models/sha256/abc")
            );
        }

        // Phase: runtime_selection_prefers_confirmed_nvidia_over_cpu.
        {
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

        // Phase: runtime_selection_uses_rocm_only_when_driver_is_present.
        {
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

        // Phase: runtime_selection_uses_cpu_when_no_supported_accelerator_exists.
        {
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
}
