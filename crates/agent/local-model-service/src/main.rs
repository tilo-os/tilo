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
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agent_provider_config::local::snapshot::{
    AcceleratorSetup, AcceleratorSetupState, HardwareFit, HardwareRecommendation,
    InProcessLocalModel, InstalledLocalModel, LocalModelPackage, LocalModelPhase,
    LocalModelSnapshot,
};
use agent_provider_config::local::{
    Accelerator, HardwareSnapshot, LocalModelRelease, LocalModelStatus, RuntimeDecision,
    RuntimeKind, select_runtime_for_model,
};
use clap::{Parser, Subcommand};
use local_model_service::catalog::CatalogPayload;
use local_model_service::run_observation::{Observed, RunCheckpoint, RuntimeSample};
use local_model_service::supervision::SystemdSupervisor;
use local_model_service::{
    HttpsTransport, LocalModelService, ServiceError, SignedRuntime, user_model_root,
};
#[cfg(test)]
use local_model_service::{
    VllmCommand,
    supervision::{model_endpoint_ready, podman_vllm_argv},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod memory_capacity;

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
    /// Offline developer artifact store, indexed only by signed SHA-256.
    #[arg(long)]
    artifact_dir: Option<PathBuf>,
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
    /// Direct unconfined operator recovery of one retained orphan run identity.
    RecoverOrphan {
        #[arg(long)]
        run_id: String,
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

/// What `snapshot` answers: the snapshot Settings reads, with what the
/// service observed of its runtimes.
#[derive(Serialize)]
struct ServiceSnapshot {
    #[serde(flatten)]
    snapshot: LocalModelSnapshot,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    runtime_observations: BTreeMap<String, Observed<RuntimeSample>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retained_run_observations: Option<Observed<Vec<RunCheckpoint>>>,
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

fn load_catalog(path: &Path, keys: &Path) -> Result<CatalogPayload, String> {
    local_model_service::catalog::VerifiedCatalog::load(path, keys)
        .map(local_model_service::catalog::VerifiedCatalog::into_payload)
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

enum ManagerTransport {
    Https(ReqwestTransport),
    Offline(local_model_service::developer::OfflineTransport),
}
impl ManagerTransport {
    fn new(dir: Option<&Path>, catalog: &CatalogPayload) -> Result<Self, ServiceError> {
        match dir {
            Some(dir) => local_model_service::developer::OfflineTransport::new(dir, catalog)
                .map(Self::Offline),
            None => ReqwestTransport::new().map(Self::Https),
        }
    }
}
impl HttpsTransport for ManagerTransport {
    fn validate_pin(
        &self,
        pin: &agent_provider_config::local::PinnedArtifact,
    ) -> Result<(), ServiceError> {
        match self {
            Self::Https(t) => t.validate_pin(pin),
            Self::Offline(t) => t.validate_pin(pin),
        }
    }
    fn resume(
        &mut self,
        url: &str,
        start: u64,
        maximum: u64,
        progress: &mut dyn FnMut(u64),
        destination: &mut File,
    ) -> Result<u64, ServiceError> {
        match self {
            Self::Https(t) => t.resume(url, start, maximum, progress, destination),
            Self::Offline(t) => t.resume(url, start, maximum, progress, destination),
        }
    }
}

fn local_hardware(root: &Path) -> Result<HardwareSnapshot, String> {
    let memory_mib = memory_capacity::runtime_memory_mib()?;
    let free_storage_mib = store_free_storage_mib(root).unwrap_or(0);
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
    Ok(HardwareSnapshot {
        cpu_threads: std::thread::available_parallelism()
            .map_or(1, |count| u16::try_from(count.get()).unwrap_or(u16::MAX)),
        memory_mib,
        free_storage_mib,
        accelerators,
    })
}
/// Observe the selected store's filesystem, including a normal not-yet-created
/// store. Only `NotFound` permits bounded ascent; invalid paths and probe errors
/// remain unavailable rather than borrowing capacity from an unrelated store.
fn store_free_storage_mib(root: &Path) -> Option<u64> {
    store_free_storage_mib_with(root, free_storage_mib)
}

fn store_free_storage_mib_with(
    root: &Path,
    mut probe: impl FnMut(&Path) -> Option<u64>,
) -> Option<u64> {
    if !root.is_absolute()
        || root.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return None;
    }
    let mut path = root;
    for _ in 0..32 {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() => return probe(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path = path.parent()?,
            Ok(_) | Err(_) => return None,
        }
    }
    None
}

fn free_storage_mib(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
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
        in_process: model.runs_in_process(),
        segmentation: model.segmentation,
    }
}
fn runtime_kind_for(
    hardware: &HardwareSnapshot,
    model: &LocalModelRelease,
    available: impl IntoIterator<Item = RuntimeKind>,
) -> Result<RuntimeKind, String> {
    match select_runtime_for_model(hardware, model, available) {
        RuntimeDecision::Ready(kind) => Ok(kind),
        RuntimeDecision::Unavailable(reason) => Err(reason.into()),
    }
}

fn recommended_model<'a>(
    catalog: &'a CatalogPayload,
    hardware: &HardwareSnapshot,
) -> Result<(&'a LocalModelRelease, RuntimeKind), String> {
    catalog.models.iter()
        .filter(|model| !model.runs_in_process())
        .filter_map(|model| {
            let runtime = signed_runtime_for(catalog, hardware, model).ok()?;
            let required_storage = model.installed_size_bytes.div_ceil(1024 * 1024);
            (required_storage <= hardware.free_storage_mib).then_some((model, runtime.release.kind))
        })
        .max_by_key(|(model, kind)| {
            let priority = match kind {
                RuntimeKind::NvidiaCuda => 2,
                RuntimeKind::AmdRocm => 1,
                RuntimeKind::Cpu => 0,
            };
            (priority, model.context_window, std::cmp::Reverse(model.artifact.size_bytes))
        })
        .ok_or_else(|| "No signed model and compatible runtime fit the detected RAM, accelerator memory, and selected store's free storage.".into())
}

fn signed_runtime_for<'a>(
    catalog: &'a CatalogPayload,
    hardware: &HardwareSnapshot,
    model: &LocalModelRelease,
) -> Result<&'a SignedRuntime, String> {
    let kind = runtime_kind_for(
        hardware,
        model,
        catalog
            .runtimes
            .iter()
            .filter(|runtime| {
                model.allowed_runtimes.contains(&runtime.release.kind)
                    && runtime.release.supports_format(model.artifact_format)
            })
            .map(|runtime| runtime.release.kind),
    )?;
    catalog
        .runtimes
        .iter()
        .filter(|runtime| {
            runtime.release.kind == kind
                && model.allowed_runtimes.contains(&kind)
                && runtime.release.supports_format(model.artifact_format)
        })
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
    if let Err(error) = validate_developer_mode(&cli) {
        eprintln!("tilo-model-manager: {error}");
        std::process::exit(1);
    }
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
        Action::RecoverOrphan { .. }
        | Action::Worker { .. }
        | Action::WorkerStart { .. }
        | Action::Serve { .. } => Err("internal model manager action cannot be proxied".into()),
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
    if cli.root.is_some()
        || cli.catalog.is_some()
        || cli.keys.is_some()
        || cli.artifact_dir.is_some()
    {
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
        && unsafe { credentials.assume_init().uid } == rustix::process::geteuid().as_raw()
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
        artifact_dir: None,
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

fn serve(socket: &Path) -> Result<Option<String>, String> {
    // SAFETY: activation is taken over before any thread is spawned.
    let activated = unsafe { tilo_files::native::private::activated_socket(socket) }
        .map_err(|error| error.to_string())?;
    let listener = if let Some(listener) = activated {
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

fn bind_listener(socket: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    tilo_files::native::private::bind_socket(socket).map_err(|error| error.to_string())
}

fn validate_developer_mode(cli: &Cli) -> Result<(), String> {
    // NoNewPrivs is kernel state and cannot be unset by an app. Unknown/error
    // state fails closed; launcher markers and service FDs are extra refusals.
    let confined = service_fd().is_some()
        || std::env::var_os("TILO_APP_DIR").is_some()
        || unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 0;
    validate_developer_inputs(cli, rustix::process::geteuid().as_raw(), confined)
}

fn validate_developer_inputs(cli: &Cli, uid: u32, confined: bool) -> Result<(), String> {
    if matches!(cli.command, Action::RecoverOrphan { .. })
        && (uid == 0 || confined || cli.root.is_none())
    {
        return Err(
            "orphan recovery requires unprivileged unconfined direct mode and explicit root".into(),
        );
    }
    if cli.artifact_dir.is_some()
        && (uid == 0
            || confined
            || cli.root.is_none()
            || cli.catalog.is_none()
            || cli.keys.is_none()
            || matches!(cli.command, Action::Serve { .. } | Action::StageAccelerator))
    {
        return Err("offline artifacts require unprivileged unconfined direct mode with explicit root/catalog/keys".into());
    }
    Ok(())
}

fn selected_catalog(cli: &Cli, catalog: &Path, keys: &Path) -> Result<CatalogPayload, String> {
    if cli.artifact_dir.is_some() {
        local_model_service::catalog::VerifiedCatalog::load_developer_offline(catalog, keys)
            .map(local_model_service::catalog::VerifiedCatalog::into_payload)
    } else {
        load_catalog(catalog, keys)
    }
}

fn paths(cli: &Cli) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    validate_developer_mode(cli)?;
    // The model manager is deliberately a per-user service: Settings never
    // needs root or a writable image path to provision a model. Keep override
    // rejection when someone mistakenly elevates the normal user binary.
    let elevated = rustix::process::geteuid().as_raw() == 0;
    if elevated && (cli.root.is_some() || cli.catalog.is_some() || cli.keys.is_some()) {
        return Err("path overrides are forbidden for the privileged model manager".into());
    }
    Ok((
        cli.root
            .clone()
            .or_else(user_model_root)
            .ok_or("no data home for the model store: set HOME or pass --root")?,
        cli.catalog.clone().unwrap_or_else(|| {
            PathBuf::from("/usr/share/tilo/system-extensions/local-model-catalog.json")
        }),
        cli.keys.clone().unwrap_or_else(|| {
            PathBuf::from("/usr/share/tilo/system-extensions/local-model-keys.json")
        }),
    ))
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

fn open_cli_service(
    cli: &Cli,
    root: &Path,
    catalog: &CatalogPayload,
) -> Result<LocalModelService<ManagerTransport, SystemdSupervisor>, String> {
    LocalModelService::open(
        root,
        ManagerTransport::new(cli.artifact_dir.as_deref(), catalog)
            .map_err(|error| error.to_string())?,
        SystemdSupervisor,
    )
    .map_err(|error| error.to_string())
}

fn run_local(cli: Cli) -> Result<Option<String>, String> {
    let (root, catalog_path, key_path) = paths(&cli)?;
    if let Action::RecoverOrphan { run_id } = &cli.command {
        local_model_service::ownership::recover_orphan(&root, &mut SystemdSupervisor, run_id)
            .map_err(|error| error.to_string())?;
        return Ok(None);
    }
    if let Action::Snapshot { format } = &cli.command {
        return local_snapshot(
            &root,
            &catalog_path,
            &key_path,
            format,
            cli.artifact_dir.as_deref(),
        )
        .map(Some);
    }
    let catalog = selected_catalog(&cli, &catalog_path, &key_path)?;
    let mut service = open_cli_service(&cli, &root, &catalog)?;
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
            enqueue_worker(
                &root,
                &catalog_path,
                &key_path,
                &model,
                revision.as_deref(),
                cli.artifact_dir.as_deref(),
            )
            .inspect_err(|error| {
                let _ = service.record_failure(&model, error);
            })?;
        }
        Action::Start { model } => {
            let release = catalog
                .models
                .iter()
                .find(|release| release.id == model)
                .ok_or_else(|| "model is not in the signed catalog".to_string())?;
            refuse_in_process(release)?;
            // Refuse before reporting an admitted background start; its worker
            // observes the same runtime slice again immediately before launch.
            signed_runtime_for(&catalog, &local_hardware(&root)?, release)?;
            service
                .mark_starting(&model)
                .map_err(|error| error.to_string())?;
            enqueue_start_worker(
                &root,
                &catalog_path,
                &key_path,
                &model,
                cli.artifact_dir.as_deref(),
            )
            .inspect_err(|error| {
                let _ = service.record_failure(&model, error);
            })?;
        }
        Action::Stop { model } => {
            if let Some(release) = catalog.models.iter().find(|release| release.id == model) {
                refuse_in_process(release)?;
            }
            service.stop(&model).map_err(|error| error.to_string())?;
        }
        Action::Remove { model } => {
            service.remove(&model).map_err(|error| error.to_string())?;
        }
        Action::StageAccelerator => stage_recommended_accelerator()?,
        Action::Worker { model, revision } => {
            run_install_worker(&mut service, &catalog, &model, revision.as_ref(), &root)?;
        }
        Action::WorkerStart { model } => run_start_worker(&mut service, &catalog, &model, &root)?,
        // The dispatcher answers these itself and never hands them over.
        Action::RecoverOrphan { .. } | Action::Snapshot { .. } | Action::Serve { .. } => {
            return Err("not a local model action".into());
        }
    }
    Ok(None)
}

fn run_start_worker(
    service: &mut LocalModelService<ManagerTransport, SystemdSupervisor>,
    catalog: &CatalogPayload,
    model: &str,
    root: &Path,
) -> Result<(), String> {
    let release = catalog
        .models
        .iter()
        .find(|release| release.id == model)
        .ok_or_else(|| "model is not in the signed catalog".to_string())?;
    refuse_in_process(release)?;
    let runtime = local_hardware(root)
        .and_then(|hardware| signed_runtime_for(catalog, &hardware, release))
        .inspect_err(|error| {
            let _ = service.record_failure(model, error);
        })?;
    if let Err(error) = service.start(runtime, release, allocate_loopback_port()?) {
        let _ = service.record_failure(model, error.to_string());
        return Err(error.to_string());
    }
    Ok(())
}

/// The signed catalog, this machine's recommendation and installed models,
/// as the JSON snapshot Settings renders.
fn local_snapshot(
    root: &PathBuf,
    catalog_path: &Path,
    key_path: &Path,
    format: &str,
    artifact_dir: Option<&Path>,
) -> Result<String, String> {
    if format != "json" {
        return Err("snapshot supports only --format json".into());
    }
    let catalog = if artifact_dir.is_some() {
        local_model_service::catalog::VerifiedCatalog::load_developer_offline(
            catalog_path,
            key_path,
        )
        .map(local_model_service::catalog::VerifiedCatalog::into_payload)?
    } else {
        load_catalog(catalog_path, key_path)?
    };
    let service = LocalModelService::open(
        root,
        ManagerTransport::new(artifact_dir, &catalog).map_err(|error| error.to_string())?,
        SystemdSupervisor,
    )
    .map_err(|error| error.to_string())?;
    let hardware = local_hardware(root)?;
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
                .find(|model| &model.id == id && !model.runs_in_process())
                .map(|model| InstalledLocalModel {
                    package: package(model),
                    phase: phase(&managed.status),
                })
        })
        .collect();
    let payload = ServiceSnapshot {
        snapshot: LocalModelSnapshot {
            hardware: recommendation(&hardware, runtime, recommended.is_some()),
            recommendation: recommended,
            models,
            in_process: in_process_models(&catalog.models, service.state()),
            accelerator_setup: accelerator_setup_snapshot(),
        },
        runtime_observations: service.runtime_observations(),
        retained_run_observations: match local_model_service::run_observation::retained(root) {
            local_model_service::run_observation::Observed::Actual { value }
                if value.is_empty() =>
            {
                None
            }
            retained => Some(retained),
        },
    };
    serde_json::to_string(&payload).map_err(|error| error.to_string())
}

/// Every signed in-process model with its install state. These do not
/// depend on the hardware probe: they are small and run on any CPU.
fn in_process_models(
    models: &[LocalModelRelease],
    state: &local_model_service::ServiceState,
) -> Vec<InProcessLocalModel> {
    models
        .iter()
        .filter(|model| model.runs_in_process())
        .map(|model| InProcessLocalModel {
            package: package(model),
            phase: state
                .models
                .get(&model.id)
                .map(|managed| phase(&managed.status)),
        })
        .collect()
}

fn refuse_in_process(release: &LocalModelRelease) -> Result<(), String> {
    if release.runs_in_process() {
        Err(format!(
            "{} runs inside the apps that use it; there is nothing to start or stop",
            release.label
        ))
    } else {
        Ok(())
    }
}

/// Install then start one signed release in the detached worker, recording
/// any failure in the service state Settings observes. An in-process model
/// is only installed: no runtime, hardware gate or service applies to it.
fn run_install_worker(
    service: &mut LocalModelService<ManagerTransport, SystemdSupervisor>,
    catalog: &CatalogPayload,
    model: &str,
    revision: Option<&String>,
    root: &Path,
) -> Result<(), String> {
    let release = catalog
        .models
        .iter()
        .find(|release| {
            release.id == model && revision.is_none_or(|revision| revision == &release.version)
        })
        .ok_or_else(|| "model/revision is not in the signed catalog".to_string())?;
    if release.runs_in_process() {
        if let Err(error) = service.install_in_process_with_progress(release) {
            let _ = service.record_failure(model, error.to_string());
            return Err(error.to_string());
        }
        return Ok(());
    }
    let runtime = local_hardware(root)
        .and_then(|hardware| signed_runtime_for(catalog, &hardware, release))
        .inspect_err(|error| {
            let _ = service.record_failure(model, error);
        })?;
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
    artifact_dir: Option<&Path>,
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
    if rustix::process::geteuid().as_raw() != 0 {
        command
            .arg("--root")
            .arg(root)
            .arg("--catalog")
            .arg(catalog)
            .arg("--keys")
            .arg(keys);
    }
    if let Some(dir) = artifact_dir {
        command.arg("--artifact-dir").arg(dir);
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
    artifact_dir: Option<&Path>,
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
    if rustix::process::geteuid().as_raw() != 0 {
        command
            .arg("--root")
            .arg(root)
            .arg("--catalog")
            .arg(catalog)
            .arg("--keys")
            .arg(keys);
    }
    if let Some(dir) = artifact_dir {
        command.arg("--artifact-dir").arg(dir);
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
    use agent_provider_config::local::{LocalEndpoint, RuntimeKind};

    #[test]
    fn offline_developer_cli_requires_complete_unconfined_direct_mode() {
        let args = [
            "manager",
            "--root",
            "/private/store",
            "--catalog",
            "/private/catalog",
            "--keys",
            "/private/keys",
            "--artifact-dir",
            "/private/artifacts",
            "worker",
            "--model",
            "pinned",
        ];
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(validate_developer_inputs(&cli, 1000, false).is_ok());
        assert!(validate_developer_inputs(&cli, 0, false).is_err());
        assert!(validate_developer_inputs(&cli, 1000, true).is_err());
        for missing in ["--root", "--catalog", "--keys"] {
            let mut filtered = Vec::new();
            let mut skip = false;
            for arg in args {
                if skip {
                    skip = false;
                    continue;
                }
                if arg == missing {
                    skip = true;
                    continue;
                }
                filtered.push(arg);
            }
            assert!(
                validate_developer_inputs(&Cli::try_parse_from(filtered).unwrap(), 1000, false)
                    .is_err()
            );
        }
        let serve = Cli::try_parse_from([
            "manager",
            "--root",
            "/private/store",
            "--catalog",
            "/private/catalog",
            "--keys",
            "/private/keys",
            "--artifact-dir",
            "/private/artifacts",
            "serve",
            "--socket",
            "/private/socket",
        ])
        .unwrap();
        assert!(validate_developer_inputs(&serve, 1000, false).is_err());
        let plain = cli_for_rpc(RpcAction::Snapshot);
        assert!(plain.artifact_dir.is_none());
    }

    #[test]
    fn orphan_recovery_has_no_app_rpc_or_confined_operator_route() {
        let cli = Cli::try_parse_from([
            "manager",
            "--root",
            "/private/store",
            "recover-orphan",
            "--run-id",
            &"b".repeat(64),
        ])
        .unwrap();
        assert!(validate_developer_inputs(&cli, 1000, false).is_ok());
        assert!(validate_developer_inputs(&cli, 1000, true).is_err());
        assert!(validate_developer_inputs(&cli, 0, false).is_err());
        assert!(rpc_action(&cli.command).is_err());
        let no_root =
            Cli::try_parse_from(["manager", "recover-orphan", "--run-id", &"b".repeat(64)])
                .unwrap();
        assert!(validate_developer_inputs(&no_root, 1000, false).is_err());
    }

    #[test]
    fn checked_in_catalog_signature_survives_speech_format_support() {
        let image_inputs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/usr/share/tilo/system-extensions");
        let catalog = load_catalog(
            &image_inputs.join("local-model-catalog.json"),
            &image_inputs.join("local-model-keys.json"),
        )
        .expect("the existing image catalogue must retain its trusted signature");
        assert!(catalog.models.iter().any(|model| model.segmentation));
    }

    #[test]
    fn readiness_requires_healthy_http_and_exact_model_identity() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let responses = [
                ("/health", 503, r#"{"error":"Loading model"}"#),
                ("/health", 200, r#"{"status":"ok"}"#),
                ("/v1/models", 200, r#"{"data":[{"id":"foreign-model"}]}"#),
                ("/health", 200, r#"{"status":"ok"}"#),
                ("/v1/models", 200, r#"{"data":[{"id":"selected-model"}]}"#),
            ];
            for (path, status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = [0_u8; 4096];
                let count = stream.read(&mut request).unwrap();
                assert!(
                    String::from_utf8_lossy(&request[..count])
                        .starts_with(&format!("GET {path} HTTP/1.1"))
                );
                write!(stream, "HTTP/1.1 {status} response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let client = tilo_net::http::blocking_client()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let endpoint = LocalEndpoint {
            url: format!("http://{address}/v1"),
            model_id: "selected-model".into(),
        };
        for url in [
            "http://example.invalid/v1",
            "http://127.0.0.1:0/v1",
            "http://127.0.0.1:80/other",
        ] {
            assert!(!model_endpoint_ready(
                &client,
                &LocalEndpoint {
                    url: url.into(),
                    model_id: endpoint.model_id.clone()
                }
            ));
        }
        assert!(
            !model_endpoint_ready(&client, &endpoint),
            "listening while loading must remain pending"
        );
        assert!(
            !model_endpoint_ready(&client, &endpoint),
            "healthy wrong alias must remain pending"
        );
        assert!(model_endpoint_ready(&client, &endpoint));
        server.join().unwrap();
    }

    #[test]
    fn snapshot_observation_fields_are_absent_for_legacy_empty_state() {
        let payload = ServiceSnapshot {
            snapshot: LocalModelSnapshot {
                hardware: recommendation(
                    &HardwareSnapshot::default(),
                    Err("unavailable".into()),
                    false,
                ),
                recommendation: None,
                models: Vec::new(),
                in_process: Vec::new(),
                accelerator_setup: None,
            },
            runtime_observations: BTreeMap::new(),
            retained_run_observations: None,
        };
        let wire = serde_json::to_value(payload).unwrap();
        assert!(wire.get("runtime_observations").is_none());
        assert!(wire.get("retained_run_observations").is_none());
        let old = serde_json::json!({"models":{}, "accepted_licenses":{}});
        let state: local_model_service::ServiceState = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(state).unwrap(), old);
    }

    fn catalog_fixture() -> CatalogPayload {
        let inputs = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../os/usr/share/tilo/system-extensions");
        load_catalog(
            &inputs.join("local-model-catalog.json"),
            &inputs.join("local-model-keys.json"),
        )
        .unwrap()
    }

    fn signed_selector_runtime() -> SignedRuntime {
        use agent_provider_config::local::{PinnedArtifact, RuntimeExecution, RuntimeRelease};
        let pin = PinnedArtifact {
            url: "https://artifacts.example.invalid/runtime".into(),
            sha256: "a".repeat(64),
            size_bytes: 10,
        };
        let manifest = system_extension::ExtensionManifest {
            schema_version: system_extension::SCHEMA_VERSION,
            id: "selector-runtime".into(),
            kind: system_extension::ExtensionKind::LocalModelRuntime,
            version: "test".into(),
            artifact: system_extension::PinnedArtifact {
                source: pin.url.clone(),
                sha256: pin.sha256.clone(),
                size_bytes: pin.size_bytes,
            },
            target: system_extension::ImageConstraint {
                base_image_digest: format!("sha256:{}", "b".repeat(64)),
                architecture: None,
                kernel_abi: None,
            },
            requires_reboot: false,
        };
        let signed = system_extension::SignedExtensionManifest::sign(
            manifest,
            "selector-test-key",
            &ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
        )
        .unwrap();
        let runtime = SignedRuntime {
            accepted: system_extension::AcceptedExtension {
                signed,
                artifact: system_extension::ArtifactReceipt {
                    sha256: pin.sha256.clone(),
                    size_bytes: pin.size_bytes,
                },
                committed_revision: 1,
            },
            release: RuntimeRelease {
                id: "selector-runtime".into(),
                kind: RuntimeKind::NvidiaCuda,
                version: "test".into(),
                artifact: pin,
                oci_image: format!("registry.example.invalid/llama@sha256:{}", "b".repeat(64)),
                execution: Some(RuntimeExecution::LlamaCpp {
                    max_context_tokens: 4096,
                    gpu_layers: 8,
                    parallel_slots: None,
                }),
            },
        };
        runtime.validate().unwrap();
        runtime
    }

    #[test]
    fn explicit_signed_engine_selection_and_recommendation_use_model_memory() {
        use agent_provider_config::local::{ModelArtifactFormat, RuntimeExecution};
        // Mutated release metadata is confined to this selector fixture. Catalog
        // trust verification remains exercised separately on the image inputs.
        let mut catalog = catalog_fixture();
        let mut model = runtime_model();
        model.required_accelerator_mib = 4096;
        model.artifact_format = ModelArtifactFormat::LlamaCppGguf;
        catalog.models = vec![model];
        catalog.runtimes = vec![signed_selector_runtime()];
        let mut hardware = HardwareSnapshot {
            cpu_threads: 8,
            memory_mib: 32768,
            free_storage_mib: 1024,
            accelerators: vec![
                Accelerator::Amd {
                    vram_mib: 512,
                    rocm: None,
                },
                Accelerator::Nvidia {
                    vram_mib: 8188,
                    driver: "615.71.09".into(),
                },
            ],
        };
        assert_eq!(
            signed_runtime_for(&catalog, &hardware, &catalog.models[0])
                .unwrap()
                .release
                .kind,
            RuntimeKind::NvidiaCuda
        );
        assert_eq!(
            recommended_model(&catalog, &hardware).unwrap().1,
            RuntimeKind::NvidiaCuda
        );
        catalog.models[0].required_accelerator_mib = 8189;
        assert!(signed_runtime_for(&catalog, &hardware, &catalog.models[0]).is_err());
        assert!(recommended_model(&catalog, &hardware).is_err());
        catalog.models[0].required_accelerator_mib = 4096;
        catalog.runtimes[0].release.execution = None;
        assert!(signed_runtime_for(&catalog, &hardware, &catalog.models[0]).is_err());
        catalog.runtimes[0].release.execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 4096,
            gpu_layers: 8,
            parallel_slots: None,
        });
        // Free storage must fit the complete installed artifact, rounded up.
        catalog.models[0].installed_size_bytes = 1024 * 1024 + 1;
        hardware.free_storage_mib = 1;
        assert!(recommended_model(&catalog, &hardware).is_err());
        hardware.free_storage_mib = 2;
        assert!(recommended_model(&catalog, &hardware).is_ok());
    }

    #[test]
    fn runtime_selection_and_snapshot_fit_use_effective_slice_capacity() {
        use agent_provider_config::local::ModelArtifactFormat;
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("user.slice/app.slice")).unwrap();
        std::fs::write(root.path().join("cgroup.controllers"), "memory\n").unwrap();
        std::fs::write(root.path().join("user.slice/memory.max"), "max\n").unwrap();
        let limit = root.path().join("user.slice/app.slice/memory.max");
        std::fs::write(&limit, "21474836480\n").unwrap();
        let properties = "LoadState=loaded\nControlGroup=/user.slice/app.slice\n";
        let meminfo = "MemTotal: 33554432 kB\nMemAvailable: 1024 kB\n";
        let mut model = runtime_model();
        model.artifact_format = ModelArtifactFormat::LlamaCppGguf;
        model.required_memory_mib = 24576;
        let mut catalog = catalog_fixture();
        catalog.models = vec![model];
        catalog.runtimes = vec![signed_selector_runtime()];
        let mut hardware = HardwareSnapshot {
            cpu_threads: 8,
            memory_mib: memory_capacity::capacity_mib(meminfo, properties, root.path()).unwrap(),
            free_storage_mib: 1024,
            accelerators: vec![Accelerator::Nvidia {
                vram_mib: 8188,
                driver: "test".into(),
            }],
        };
        assert!(signed_runtime_for(&catalog, &hardware, &catalog.models[0]).is_err());
        assert!(recommended_model(&catalog, &hardware).is_err());
        let report = recommendation(
            &hardware,
            Err("Insufficient runtime memory capacity".into()),
            false,
        );
        assert!(matches!(report.fit, HardwareFit::Unsupported));
        assert_eq!(report.memory_gib, 20);
        // Unlimited limits restore physical capacity, not the 1 MiB headroom.
        std::fs::write(limit, "max\n").unwrap();
        hardware.memory_mib =
            memory_capacity::capacity_mib(meminfo, properties, root.path()).unwrap();
        assert_eq!(
            signed_runtime_for(&catalog, &hardware, &catalog.models[0])
                .unwrap()
                .release
                .kind,
            RuntimeKind::NvidiaCuda
        );
        assert_eq!(
            recommended_model(&catalog, &hardware).unwrap().1,
            RuntimeKind::NvidiaCuda
        );
    }

    #[test]
    fn capacity_probes_selected_store_and_only_bounded_normal_missing_parents() {
        let temporary = tempfile::tempdir().unwrap();
        let store = temporary.path().join("selected-store");
        std::fs::create_dir(&store).unwrap();
        // Record the one actual probe used by each production-path invocation.
        // Free bytes can change between independent statvfs calls on a live store.
        let expected = std::cell::Cell::new(None);
        let calls = std::cell::RefCell::new(Vec::new());
        let mut probe = |path: &Path| {
            assert_eq!(path, store);
            calls.borrow_mut().push(path.to_owned());
            let available = free_storage_mib(path).expect("actual filesystem must be observable");
            assert!(available > 0);
            expected.set(Some(available));
            Some(available)
        };
        let observed = store_free_storage_mib_with(&store, &mut probe);
        assert_eq!(observed, expected.get());
        assert_eq!(calls.borrow().as_slice(), std::slice::from_ref(&store));
        let observed = store_free_storage_mib_with(&store.join("not-created/models"), &mut probe);
        assert_eq!(observed, expected.get());
        assert_eq!(calls.borrow().as_slice(), [store.clone(), store.clone()]);
        let file = temporary.path().join("file");
        std::fs::write(&file, "not a directory").unwrap();
        assert_eq!(store_free_storage_mib(&file), None);
        assert_eq!(store_free_storage_mib(&file.join("child")), None);
        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(&store, &link).unwrap();
        assert_eq!(store_free_storage_mib(&link), None);
        assert_eq!(store_free_storage_mib(Path::new("relative/models")), None);
        assert_eq!(store_free_storage_mib(&store.join("../other")), None);
        let deep = (0..33).fold(store, |path, _| path.join("absent"));
        assert_eq!(store_free_storage_mib(&deep), None);
    }

    fn runtime_model() -> LocalModelRelease {
        let mut model = release("chat", false);
        model.allowed_runtimes = [
            RuntimeKind::Cpu,
            RuntimeKind::NvidiaCuda,
            RuntimeKind::AmdRocm,
        ]
        .into();
        model
    }

    fn release(id: &str, in_process: bool) -> LocalModelRelease {
        LocalModelRelease {
            id: id.into(),
            label: id.into(),
            description: "test".into(),
            version: "d".repeat(64),
            artifact: agent_provider_config::local::PinnedArtifact {
                url: format!("https://artifacts.example.invalid/{}/model", "d".repeat(64)),
                sha256: "e".repeat(64),
                size_bytes: 10,
            },
            artifact_format: if in_process {
                agent_provider_config::local::ModelArtifactFormat::OnnxFile
            } else {
                agent_provider_config::local::ModelArtifactFormat::VllmGguf
            },
            license_id: "Apache-2.0".into(),
            license_url: "https://artifacts.example.invalid/LICENSE".into(),
            requires_acceptance: false,
            installed_size_bytes: 10,
            allowed_runtimes: if in_process {
                std::collections::BTreeSet::new()
            } else {
                [RuntimeKind::Cpu].into()
            },
            required_memory_mib: 1,
            required_accelerator_mib: 0,
            context_window: if in_process { 0 } else { 4096 },
            vision: false,
            face_recognition: false,
            segmentation: in_process,
        }
    }

    /// Every in-process catalog model is offered with its install state —
    /// absent until set up — and runtime models never join that list or
    /// accept a start for an in-process one.
    #[test]
    fn the_snapshot_offers_every_in_process_model_with_its_state() {
        let models = [
            release("chat", false),
            release("subject", true),
            release("other-subject", true),
        ];
        let mut state = local_model_service::ServiceState::default();
        state.models.insert(
            "subject".into(),
            local_model_service::ManagedModel {
                model_sha256: "e".repeat(64),
                status: LocalModelStatus::Installed,
                process: None,
                in_process: None,
            },
        );
        let listed = in_process_models(&models, &state);
        let json = serde_json::to_value(&listed).unwrap();
        assert_eq!(json.as_array().unwrap().len(), 2);
        assert_eq!(json[0]["package"]["id"], "subject");
        assert_eq!(json[0]["package"]["in_process"], true);
        assert_eq!(json[0]["package"]["segmentation"], true);
        assert_eq!(json[0]["phase"]["state"], "ready");
        assert_eq!(json[0]["phase"]["running"], false);
        assert_eq!(json[1]["package"]["id"], "other-subject");
        assert!(json[1]["phase"].is_null());
        assert!(refuse_in_process(&models[1]).is_err());
        assert!(refuse_in_process(&models[0]).is_ok());
    }

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
                endpoint: LocalEndpoint { url: "http://127.0.0.1:9000/v1".into(), model_id: "tiny".into() },
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
                runtime_kind_for(
                    &hardware,
                    &runtime_model(),
                    [RuntimeKind::Cpu, RuntimeKind::NvidiaCuda]
                )
                .unwrap(),
                RuntimeKind::NvidiaCuda
            );
            let error =
                runtime_kind_for(&hardware, &runtime_model(), [RuntimeKind::Cpu]).unwrap_err();
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
                runtime_kind_for(&hardware, &runtime_model(), [RuntimeKind::AmdRocm]).unwrap(),
                RuntimeKind::AmdRocm
            );
            let mut missing_driver = hardware;
            missing_driver.accelerators = vec![Accelerator::Amd {
                vram_mib: 16 * 1024,
                rocm: None,
            }];
            let error = runtime_kind_for(&missing_driver, &runtime_model(), [RuntimeKind::Cpu])
                .unwrap_err();
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
                runtime_kind_for(&hardware, &runtime_model(), [RuntimeKind::Cpu]).unwrap(),
                RuntimeKind::Cpu
            );
        }
    }
}
