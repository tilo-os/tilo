//! `tilo-model-manager` — the small production CLI Settings invokes.
//!
//! It accepts only signed catalog selections, writes only its private store,
//! downloads only pinned HTTPS URLs, and delegates process lifetime to a
//! fixed-argv systemd user service. No subcommand accepts a URL, command, tag,
//! model path, or arbitrary process argument; a configuration is named by
//! catalogue and probe identities and re-derived here before anything runs.

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
    Accelerator, AcceleratorState, AcceleratorVendor, Configuration, HardwareSnapshot,
    LocalModelCatalog, LocalModelRelease, LocalModelStatus, Placement, RestartReason,
    RuntimeEngine, RuntimeKind,
};
use agent_provider_config::manager::{
    AcceleratorSetup, AcceleratorSetupState, AvailableModel, HardwareFit, HardwareRecommendation,
    InstalledLocalModel, LocalModelPackage, LocalModelPhase, LocalModelSnapshot,
};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use local_model_service::{
    DeviceAccess, HttpsTransport, LocalModelService, ProcessHandle, ProcessSupervisor,
    RuntimeCommand, ServiceError, SignedRuntime,
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

/// A processor as the command line names it (`vulkan`, `nvidia_cuda`, ...).
fn parse_kind(value: &str) -> Result<RuntimeKind, String> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| format!("unknown processor {value}"))
}

#[derive(Subcommand)]
enum Action {
    Snapshot {
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Set up one configuration the snapshot offers: a model, the runtime
    /// pack, the processor and, on a GPU, the device's PCI slot.
    Install {
        #[arg(long)]
        model: String,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long)]
        runtime: String,
        #[arg(long, value_parser = parse_kind)]
        on: RuntimeKind,
        #[arg(long)]
        device: Option<String>,
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
        placement: Placement,
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
/// No release is a default: which configuration is recommended is decided
/// on each device from its hardware.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct CatalogPayload {
    schema: u16,
    models: Vec<LocalModelRelease>,
    runtimes: Vec<SignedRuntime>,
}
#[derive(Default, Deserialize)]
struct KeyFile {
    keys: BTreeMap<String, String>,
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
    model_catalog(&signed.payload)
        .validate()
        .map_err(|error| error.to_string())?;
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
                // Vendors serve pinned files through their CDN (a Hugging Face
                // resolve URL redirects to it). Follow only HTTPS hops; the
                // pinned digest, not the host, decides what is accepted.
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.url().scheme() == "https" && attempt.previous().len() < 5 {
                        attempt.follow()
                    } else {
                        attempt.stop()
                    }
                }))
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

/// User-systemd supervision. The executable and every engine argument come
/// from [`RuntimeCommand`], itself constructed only from a verified signed
/// catalog and the probed device.
struct SystemdSupervisor;
/// The container has no network at all: the only way in or out is the API
/// socket in its private socket directory, so what a model is sent cannot
/// leave the device through it. It is given its one configuration's device
/// and nothing else.
fn podman_runtime_argv(command: &RuntimeCommand) -> Vec<String> {
    let mut argv: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--pull".into(),
        "never".into(),
        "--network".into(),
        "none".into(),
    ];
    match command.engine {
        // Nothing inside should try to reach a hub or report usage; with no
        // network it would only fail slowly.
        RuntimeEngine::Vllm => argv.extend([
            "--env".into(),
            "HF_HUB_OFFLINE=1".into(),
            "--env".into(),
            "VLLM_NO_USAGE_STATS=1".into(),
        ]),
        // `--offline` in its argv keeps llama-server off the network.
        RuntimeEngine::LlamaCpp => {}
    }
    match &command.device {
        DeviceAccess::None => {}
        DeviceAccess::NvidiaCdi => {
            argv.extend(["--device".into(), "nvidia.com/gpu=all".into()]);
        }
        DeviceAccess::AmdCompute => {
            argv.extend([
                "--device".into(),
                "/dev/kfd".into(),
                "--device".into(),
                "/dev/dri".into(),
            ]);
        }
        DeviceAccess::RenderNode(node) => {
            argv.extend(["--device".into(), node.display().to_string()]);
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
        "--mount".into(),
        format!(
            "type=bind,src={},dst={}",
            command.socket_dir.display(),
            local_model_service::CONTAINER_SOCKET_DIR
        ),
        "--security-opt".into(),
        "label=disable".into(),
        command.runtime_image.clone(),
    ]);
    argv.extend(command.args.iter().cloned());
    argv
}

/// Whether the runtime's API answers `GET /health` with 200 on `socket`.
/// A listening socket alone is not readiness: the server binds before the
/// weights are loaded.
fn api_ready(socket: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut status = [0_u8; 12];
    stream.read_exact(&mut status).is_ok()
        && status.starts_with(b"HTTP/1.")
        && status.get(9..12) == Some(b"200".as_slice())
}

impl ProcessSupervisor for SystemdSupervisor {
    fn pull_runtime_image(&mut self, signed_image: &str) -> Result<(), ServiceError> {
        // The image string is the signed `repository@sha256:` reference; the
        // container engine verifies the manifest digest while pulling.
        let mut pull = Command::new("podman");
        pull.args(["pull", "--quiet", signed_image]);
        let output =
            bounded_command::run_prepared(pull, None, 1024 * 1024, Duration::from_secs(4 * 3600))
                .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !output.status.success() {
            return Err(ServiceError::Backend(
                "podman could not pull the signed runtime image".into(),
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
                "podman cannot inspect the pulled runtime image".into(),
            ));
        }
        let digests: Vec<String> = serde_json::from_slice(&output.stdout).map_err(|_| {
            ServiceError::Backend("podman returned invalid image digest metadata".into())
        })?;
        if !digests.iter().any(|digest| digest == signed_image) {
            return Err(ServiceError::Backend(
                "the pulled image does not carry the exact signed digest".into(),
            ));
        }
        Ok(())
    }
    fn start(&mut self, command: &RuntimeCommand) -> Result<ProcessHandle, ServiceError> {
        let digest = hex::encode(Sha256::digest(command.endpoint.model_id.as_bytes()));
        let unit = format!("tilo-model-{}", digest.get(..16).unwrap_or(&digest));
        // A private directory per model holds nothing but the API socket.
        std::fs::create_dir_all(&command.socket_dir)?;
        std::fs::set_permissions(&command.socket_dir, std::fs::Permissions::from_mode(0o700))?;
        match std::fs::remove_file(&command.endpoint.socket) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ServiceError::Io(error)),
        }
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
            .args(podman_runtime_argv(command));
        let output =
            bounded_command::run_prepared(launch, None, 1024 * 1024, Duration::from_secs(30))
                .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !output.status.success() {
            return Err(ServiceError::Backend(
                "systemd-run rejected the verified runtime command".into(),
            ));
        }
        // Loading model weights routinely takes minutes. This worker is
        // already detached from Settings, so favour a truthful bounded wait
        // over reporting a healthy large model as failed after 20 seconds.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(10 * 60) {
            if api_ready(&command.endpoint.socket) {
                return Ok(ProcessHandle { id: unit });
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        let mut stop = Command::new("systemctl");
        stop.args(["--user", "stop", &unit]);
        let _ = bounded_command::run_prepared(stop, None, 64 * 1024, Duration::from_secs(30));
        Err(ServiceError::Backend(
            "verified model runtime did not become ready".into(),
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

/// Where the probe reads the machine: sysfs's PCI devices and the vendor
/// name tables the graphics stack ships.
struct ProbeSources<'a> {
    pci_devices: &'a Path,
    amdgpu_ids: &'a Path,
    pci_ids: &'a Path,
}

fn system_probe() -> ProbeSources<'static> {
    ProbeSources {
        pci_devices: Path::new("/sys/bus/pci/devices"),
        amdgpu_ids: Path::new("/usr/share/libdrm/amdgpu.ids"),
        pci_ids: Path::new("/usr/share/hwdata/pci.ids"),
    }
}

/// One card as `nvidia-smi` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NvidiaCard {
    pci_slot: String,
    name: String,
    vram_mib: u64,
    driver: String,
}

/// `nvidia-smi`'s `pci.bus_id` (`00000000:01:00.0`) as sysfs spells the slot
/// (`0000:01:00.0`).
fn sysfs_slot(bus_id: &str) -> Option<String> {
    let (domain, rest) = bus_id.trim().split_once(':')?;
    let domain = domain.get(domain.len().checked_sub(4)?..)?;
    Some(format!("{domain}:{rest}").to_ascii_lowercase())
}

/// The cards in `nvidia-smi --query-gpu=pci.bus_id,name,memory.total,
/// driver_version --format=csv,noheader,nounits` output.
fn nvidia_cards(stdout: &[u8]) -> Vec<NvidiaCard> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter_map(|line| {
            let mut values = line.split(',').map(str::trim);
            let pci_slot = sysfs_slot(values.next()?)?;
            let name = values.next()?.to_owned();
            let vram_mib = values.next()?.parse().ok()?;
            let driver = values.next()?.to_owned();
            Some(NvidiaCard {
                pci_slot,
                name,
                vram_mib,
                driver,
            })
        })
        .collect()
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
}

fn read_mib(path: &Path) -> u64 {
    read_trimmed(path)
        .and_then(|bytes| bytes.parse::<u64>().ok())
        .map_or(0, |bytes| bytes / 1024 / 1024)
}

/// The marketing name libdrm's `amdgpu.ids` lists for a device id and
/// revision (`15BF,\tC1,\tAMD Radeon 780M Graphics`).
fn amdgpu_name(table: &str, device: &str, revision: &str) -> Option<String> {
    table.lines().find_map(|line| {
        let mut fields = line.split(',').map(str::trim);
        let id = fields.next()?;
        let rev = fields.next()?;
        let name = fields.next()?;
        (id.eq_ignore_ascii_case(device) && rev.eq_ignore_ascii_case(revision))
            .then(|| name.to_owned())
    })
}

/// The device name hwdata's `pci.ids` lists under a vendor.
fn pci_name(table: &str, vendor: &str, device: &str) -> Option<String> {
    let mut in_vendor = false;
    for line in table.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            in_vendor = line
                .get(..4)
                .is_some_and(|id| id.eq_ignore_ascii_case(vendor));
            continue;
        }
        if in_vendor && !line.starts_with("\t\t") {
            let entry = line.trim_start_matches('\t');
            if entry
                .get(..4)
                .is_some_and(|id| id.eq_ignore_ascii_case(device))
            {
                return entry.get(4..).map(|name| name.trim().to_owned());
            }
        }
    }
    None
}

/// Every GPU on the PCI bus, with what runs it. Sysfs names the device, its
/// kernel driver and render node; AMD's memory counters give its VRAM and
/// the system memory it can map (an APU keeps no VRAM vendor record, which
/// marks its memory as shared); Intel's integrated GPU sits at `00:02.0`;
/// `nvidia-smi` gives an NVIDIA card's memory and driver, and without it the
/// card needs its driver.
fn probe_accelerators(
    sources: &ProbeSources<'_>,
    nvidia: &[NvidiaCard],
    nvml_mismatch: bool,
    rocm: Option<&str>,
    memory_mib: u64,
) -> Vec<Accelerator> {
    let amdgpu_ids = std::fs::read_to_string(sources.amdgpu_ids).unwrap_or_default();
    let pci_ids = std::fs::read_to_string(sources.pci_ids).unwrap_or_default();
    let mut slots: Vec<PathBuf> = std::fs::read_dir(sources.pci_devices)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    slots.sort();
    slots
        .into_iter()
        .filter_map(|device| {
            // Display controllers only: VGA, 3D and other display classes.
            if !read_trimmed(&device.join("class"))?.starts_with("0x03") {
                return None;
            }
            let vendor = match read_trimmed(&device.join("vendor"))?.as_str() {
                "0x10de" => AcceleratorVendor::Nvidia,
                "0x1002" => AcceleratorVendor::Amd,
                "0x8086" => AcceleratorVendor::Intel,
                _ => return None,
            };
            let pci_slot = device.file_name()?.to_str()?.to_owned();
            let device_id = read_trimmed(&device.join("device"))?;
            let device_id = device_id.trim_start_matches("0x");
            let kernel_driver = std::fs::read_link(device.join("driver"))
                .ok()
                .and_then(|driver| driver.file_name()?.to_str().map(str::to_owned))
                .unwrap_or_default();
            let render_node = std::fs::read_dir(device.join("drm"))
                .ok()
                .and_then(|nodes| {
                    nodes.flatten().find_map(|node| {
                        let name = node.file_name().to_str()?.to_owned();
                        name.starts_with("renderD").then_some(name)
                    })
                });
            let mut accelerator = Accelerator {
                vendor,
                name: String::new(),
                pci_slot,
                render_node,
                kernel_driver,
                integrated: false,
                dedicated_mib: 0,
                shared_mib: 0,
                compute: None,
                restart: None,
            };
            match vendor {
                AcceleratorVendor::Nvidia => {
                    match nvidia
                        .iter()
                        .find(|card| card.pci_slot == accelerator.pci_slot)
                    {
                        Some(card) => {
                            accelerator.name.clone_from(&card.name);
                            accelerator.dedicated_mib = card.vram_mib;
                            accelerator.compute = Some(card.driver.clone());
                        }
                        // The card and a driver are present, but the loaded
                        // kernel module and the installed userspace library
                        // differ: usable after a restart.
                        None if nvml_mismatch && accelerator.kernel_driver == "nvidia" => {
                            accelerator.restart = Some(RestartReason::DriverLibraryMismatch);
                        }
                        None => {}
                    }
                    if accelerator.name.is_empty() {
                        accelerator.name =
                            pci_name(&pci_ids, "10de", device_id).unwrap_or_default();
                    }
                }
                AcceleratorVendor::Amd => {
                    let revision = read_trimmed(&device.join("revision")).unwrap_or_default();
                    accelerator.name =
                        amdgpu_name(&amdgpu_ids, device_id, revision.trim_start_matches("0x"))
                            .or_else(|| pci_name(&pci_ids, "1002", device_id))
                            .unwrap_or_default();
                    accelerator.dedicated_mib = read_mib(&device.join("mem_info_vram_total"));
                    accelerator.shared_mib = read_mib(&device.join("mem_info_gtt_total"));
                    accelerator.integrated = !device.join("mem_info_vram_vendor").exists();
                    accelerator.compute = rocm.map(str::to_owned);
                }
                AcceleratorVendor::Intel => {
                    accelerator.name = pci_name(&pci_ids, "8086", device_id).unwrap_or_default();
                    accelerator.integrated = accelerator.pci_slot.ends_with(":00:02.0");
                    if accelerator.integrated {
                        accelerator.shared_mib = memory_mib;
                    }
                }
            }
            Some(accelerator)
        })
        .collect()
}

/// What this machine offers. Free storage is measured where the model store
/// actually lives, so a per-user store is not judged by a system path.
fn local_hardware(store: &Path) -> HardwareSnapshot {
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let meminfo_mib = |field: &str| {
        meminfo
            .lines()
            .find_map(|line| {
                line.strip_prefix(field)?
                    .split_whitespace()
                    .next()?
                    .parse::<u64>()
                    .ok()
            })
            .map_or(0, |kib| kib / 1024)
    };
    let memory_mib = meminfo_mib("MemTotal:");
    let available_memory_mib = meminfo_mib("MemAvailable:");
    let free_storage_mib = free_storage_mib(store).unwrap_or(0);
    // NVIDIA's vendor tool is called with a fixed query/format, never a
    // caller-provided argument.
    let mut nvidia = Command::new("nvidia-smi");
    nvidia.args([
        "--query-gpu=pci.bus_id,name,memory.total,driver_version",
        "--format=csv,noheader,nounits",
    ]);
    let nvidia = bounded_command::run_prepared(nvidia, None, 1024 * 1024, Duration::from_secs(5));
    let (cards, mismatch) = match &nvidia {
        Ok(output) if output.status.success() => (nvidia_cards(&output.stdout), false),
        Ok(output) => (
            Vec::new(),
            nvml_reports_version_mismatch(&output.stdout, &output.stderr),
        ),
        Err(_) => (Vec::new(), false),
    };
    let rocm = std::fs::read_to_string("/opt/rocm/.info/version")
        .ok()
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty());
    HardwareSnapshot {
        cpu_threads: std::thread::available_parallelism()
            .map_or(1, |count| u16::try_from(count.get()).unwrap_or(u16::MAX)),
        memory_mib,
        available_memory_mib,
        free_storage_mib,
        accelerators: probe_accelerators(
            &system_probe(),
            &cards,
            mismatch,
            rocm.as_deref(),
            memory_mib,
        ),
    }
}

/// NVML's own words when the loaded kernel module and the userspace library
/// differ in version, which only a restart resolves.
fn nvml_reports_version_mismatch(stdout: &[u8], stderr: &[u8]) -> bool {
    [stdout, stderr].iter().any(|stream| {
        String::from_utf8_lossy(stream)
            .to_ascii_lowercase()
            .contains("driver/library version mismatch")
    })
}

/// A staged driver deployment boots next time: the driverless card it is for
/// is waiting for a restart, not missing a driver.
fn with_staged_driver(
    mut hardware: HardwareSnapshot,
    setup: Option<&AcceleratorSetup>,
) -> HardwareSnapshot {
    if setup.is_some_and(|setup| setup.status == AcceleratorSetupState::RestartRequired) {
        for accelerator in &mut hardware.accelerators {
            if accelerator.needs_driver() {
                accelerator.restart = Some(RestartReason::DriverStaged);
            }
        }
    }
    hardware
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

/// The signed pack a configuration runs in.
fn signed_runtime<'a>(catalog: &'a CatalogPayload, id: &str) -> Result<&'a SignedRuntime, String> {
    catalog
        .runtimes
        .iter()
        .find(|runtime| runtime.release.id == id)
        .ok_or_else(|| "the runtime pack is not in the signed catalog".to_string())
}

/// The configuration `placement` names for `model`, re-derived from the
/// signed catalogue and this machine's probe; never taken on the caller's
/// word.
fn placed<'a>(
    catalog: &'a LocalModelCatalog,
    hardware: &'a HardwareSnapshot,
    model: &str,
    placement: &Placement,
) -> Result<Configuration<'a>, String> {
    catalog
        .configuration(hardware, model, placement)
        .ok_or_else(|| {
            "This configuration does not run on this device now: it needs more free memory, a device that has gone, or a driver.".to_string()
        })
}

/// The device's summary: how well the best configuration fits, or why
/// nothing is offered.
fn recommendation(
    hardware: &HardwareSnapshot,
    best: Option<&Configuration<'_>>,
) -> HardwareRecommendation {
    let state = hardware.accelerator_state();
    let (fit, reason) = match (state, best) {
        (AcceleratorState::RestartRequired { vendor, .. }, _) => (
            HardwareFit::RestartRequired,
            format!(
                "Restart to finish setting up the {} graphics driver.",
                vendor.label()
            ),
        ),
        (_, Some(best)) => (
            best.fit.into(),
            format!(
                "Ranked from this device's probed processors, memory and storage: {} with {} {}.",
                best.model.label,
                best.runtime.engine.label(),
                best.device.map_or_else(
                    || "on the CPU".to_string(),
                    |device| format!("on {}", device.label())
                ),
            ),
        ),
        (AcceleratorState::NeedsDriver { vendor }, None) => (
            HardwareFit::Unsupported,
            format!(
                "The {} graphics card has no working driver yet; nothing else here runs a signed model.",
                vendor.label()
            ),
        ),
        (_, None) => (
            HardwareFit::Unsupported,
            "No signed configuration fits this device's free memory, graphics memory and storage."
                .into(),
        ),
    };
    HardwareRecommendation {
        fit,
        memory_gib: u16::try_from(hardware.memory_mib / 1024).unwrap_or(u16::MAX),
        free_storage_gib: Some(u16::try_from(hardware.free_storage_mib / 1024).unwrap_or(u16::MAX)),
        reason,
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
        Action::Worker { .. } | Action::WorkerStart { .. } => run_local(cli),
        _ if service_fd().is_some() => forward_to_service(&cli),
        _ => run_local(cli),
    };
    match result {
        Ok(Some(output)) => println!("{output}"),
        Ok(None) => {}
        Err(error) => {
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
            runtime,
            on,
            device,
            accept_license,
        } => Ok(RpcAction::Install {
            model: model.clone(),
            revision: revision.clone(),
            placement: Placement {
                runtime: runtime.clone(),
                kind: *on,
                device: device.clone(),
            },
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
            placement,
            accept_license,
        } => Action::Install {
            model,
            revision,
            runtime: placement.runtime,
            on: placement.kind,
            device: placement.device,
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

/// The private directory a model's runtime serves its API socket in, under
/// the user's runtime directory so it disappears with the session.
fn runtime_socket_dir(model_id: &str) -> Result<PathBuf, String> {
    let runtime = xdg_paths::xdg::runtime_dir().ok_or_else(|| {
        "XDG_RUNTIME_DIR is not set; the model runtime has no socket directory".to_string()
    })?;
    let digest = hex::encode(Sha256::digest(model_id.as_bytes()));
    Ok(runtime
        .join("tilo/models")
        .join(digest.get(..16).unwrap_or(&digest)))
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
    let result = run_command(cli, &root, &catalog_path, &key_path);
    // Every start, stop, removal, install step and failure changes what this
    // device can serve. The manager owns that record, so the gateway resolves
    // a model started without Settings the moment it is up.
    if let Err(error) = publish_runtime(&root, &catalog_path, &key_path) {
        eprintln!("tilo-model-manager: could not publish the runtime record: {error}");
    }
    result
}

/// Rewrite this device's runtime record from the manager's own state.
fn publish_runtime(
    root: &PathBuf,
    catalog_path: &PathBuf,
    key_path: &PathBuf,
) -> Result<(), String> {
    snapshot_of(root, catalog_path, key_path)?
        .runtime()
        .save_at(&root.join(agent_provider_config::RUNTIME_FILE))
        .map_err(|error| error.to_string())
}

fn run_command(
    cli: Cli,
    root: &Path,
    catalog_path: &Path,
    key_path: &Path,
) -> Result<Option<String>, String> {
    let (root, catalog_path, key_path) = (
        root.to_path_buf(),
        catalog_path.to_path_buf(),
        key_path.to_path_buf(),
    );
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
            runtime,
            on,
            device,
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
            let placement = Placement {
                runtime,
                kind: on,
                device,
            };
            // Only a configuration this device offers now is set up.
            let local = model_catalog(&catalog);
            let hardware = local_hardware(&root);
            placed(&local, &hardware, &model, &placement)?;
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
                .queue_install(release, &placement)
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
            if let Err(error) = start_placed(&mut service, &catalog, &model) {
                let _ = service.record_failure(&model, &error);
                return Err(error);
            }
        }
        // The dispatcher answers these itself and never hands them over.
        Action::Snapshot { .. } | Action::Serve { .. } => {
            return Err("not a local model action".into());
        }
    }
    Ok(None)
}

/// Start `model` in the configuration it was set up for, once it is
/// re-derived from the signed catalogue and a fresh probe.
fn start_placed(
    service: &mut LocalModelService<ReqwestTransport, SystemdSupervisor>,
    catalog: &CatalogPayload,
    model: &str,
) -> Result<(), String> {
    let placement = service
        .state()
        .models
        .get(model)
        .map(|managed| managed.placement.clone())
        .ok_or_else(|| format!("model is not installed: {model}"))?;
    let local = model_catalog(catalog);
    let hardware = local_hardware(service.root());
    let configuration = placed(&local, &hardware, model, &placement)?;
    let runtime = signed_runtime(catalog, &placement.runtime)?;
    service
        .start(runtime, &configuration, &runtime_socket_dir(model)?)
        .map(|_| ())
        .map_err(|error| error.to_string())
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
    let payload = snapshot_of(root, catalog_path, key_path)?;
    // A snapshot also refreshes the record, so hardware facts stay current.
    let _ = payload
        .runtime()
        .save_at(&root.join(agent_provider_config::RUNTIME_FILE));
    serde_json::to_string(&payload).map_err(|error| error.to_string())
}

/// This device's snapshot from the signed catalogue, its probed hardware and
/// the manager's own state.
fn snapshot_for(
    catalog: &CatalogPayload,
    hardware: &HardwareSnapshot,
    state: &local_model_service::ServiceState,
    accelerator_setup: Option<AcceleratorSetup>,
) -> LocalModelSnapshot {
    let local = model_catalog(catalog);
    let configurations = local.configurations(hardware);
    let models = state
        .models
        .iter()
        .filter_map(|(id, managed)| {
            let model = catalog.models.iter().find(|model| &model.id == id)?;
            let engine = catalog
                .runtimes
                .iter()
                .find(|runtime| runtime.release.id == managed.placement.runtime)?
                .release
                .engine;
            Some(InstalledLocalModel {
                package: LocalModelPackage::of(model),
                placement: managed.placement.clone(),
                engine,
                device: managed
                    .placement
                    .device
                    .as_deref()
                    .and_then(|slot| hardware.accelerator(slot))
                    .map(Accelerator::label),
                phase: phase(&managed.status),
            })
        })
        .collect();
    LocalModelSnapshot {
        hardware: recommendation(hardware, configurations.first()),
        available: configurations.iter().map(AvailableModel::from).collect(),
        facts: hardware.clone(),
        models,
        accelerator_setup,
    }
}

/// The signed catalogue, this machine's recommendation and installed models.
fn snapshot_of(
    root: &PathBuf,
    catalog_path: &PathBuf,
    key_path: &PathBuf,
) -> Result<LocalModelSnapshot, String> {
    let catalog = load_catalog(catalog_path, key_path)?;
    let service = LocalModelService::open(
        root,
        ReqwestTransport::new().map_err(|error| error.to_string())?,
        SystemdSupervisor,
    )
    .map_err(|error| error.to_string())?;
    let accelerator_setup = accelerator_setup_snapshot();
    let hardware = with_staged_driver(local_hardware(root), accelerator_setup.as_ref());
    Ok(snapshot_for(
        &catalog,
        &hardware,
        service.state(),
        accelerator_setup,
    ))
}

/// Install then start one signed release in the configuration it was queued
/// for, in the detached worker, recording any failure in the service state
/// Settings observes.
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
    let outcome = service
        .state()
        .models
        .get(model)
        .map(|managed| managed.placement.clone())
        .ok_or_else(|| format!("model was not queued: {model}"))
        .and_then(|placement| {
            let runtime = signed_runtime(catalog, &placement.runtime)?;
            service
                .install_with_runtime_progress(runtime, release, &placement)
                .map_err(|error| error.to_string())
        })
        .and_then(|_| start_placed(service, catalog, model));
    if let Err(error) = outcome {
        let _ = service.record_failure(model, &error);
        return Err(error);
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

    fn command(engine: RuntimeEngine, kind: RuntimeKind, device: DeviceAccess) -> RuntimeCommand {
        RuntimeCommand {
            runtime_image: "registry.example/tilo/runtime@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            kind,
            device,
            engine,
            args: vec!["serve".into(), "/models/model".into()],
            model_path: PathBuf::from("/home/ada/.local/share/tilo/models/sha256/abc"),
            model_container_path: "/models/model".into(),
            socket_dir: PathBuf::from("/run/user/1000/tilo/models/0123456789abcdef"),
            endpoint: agent_provider_config::local::LocalEndpoint {
                socket: PathBuf::from("/run/user/1000/tilo/models/0123456789abcdef/api.sock"),
                model_id: "tiny".into(),
            },
            runtime_extension_id: "runtime".into(),
        }
    }

    fn devices(argv: &[String]) -> Vec<&str> {
        argv.windows(2)
            .filter(|pair| pair[0] == "--device")
            .map(|pair| pair[1].as_str())
            .collect()
    }

    /// Podman runs every engine without network, mounting only the verified model path at its
    /// container path and the socket directory, and gives the container its configuration's device
    /// alone: llama-server on the iGPU sees one render node, never the NVIDIA card.
    #[test]
    fn the_runtime_container_has_no_network_and_only_its_own_device() {
        let vllm = podman_runtime_argv(&command(
            RuntimeEngine::Vllm,
            RuntimeKind::NvidiaCuda,
            DeviceAccess::NvidiaCdi,
        ));
        let llama = podman_runtime_argv(&command(
            RuntimeEngine::LlamaCpp,
            RuntimeKind::Vulkan,
            DeviceAccess::RenderNode("/dev/dri/renderD128".into()),
        ));
        let cpu = podman_runtime_argv(&command(
            RuntimeEngine::LlamaCpp,
            RuntimeKind::Cpu,
            DeviceAccess::None,
        ));
        for argv in [&vllm, &llama, &cpu] {
            assert!(
                argv.windows(2).any(|pair| pair == ["--network", "none"]),
                "the runtime must have no network egress"
            );
            assert!(argv.windows(2).any(|pair| pair == ["--pull", "never"]));
            assert!(!argv.iter().any(|arg| arg == "host"));
            assert!(argv.windows(2).any(|pair| pair
                == [
                    "--mount",
                    "type=bind,src=/run/user/1000/tilo/models/0123456789abcdef,dst=/run/tilo-model"
                ]));
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
        assert_eq!(devices(&vllm), ["nvidia.com/gpu=all"]);
        assert!(vllm.iter().any(|arg| arg == "VLLM_NO_USAGE_STATS=1"));
        assert_eq!(devices(&llama), ["/dev/dri/renderD128"]);
        assert!(!llama.iter().any(|arg| arg.starts_with("VLLM_")));
        assert!(devices(&cpu).is_empty());
    }

    const AMDGPU_IDS: &str = "# amdgpu.ids\n1.0.0\n15BF,\tC1,\tAMD Radeon 780M Graphics\n15BF,\tC3,\tAMD Radeon 760M Graphics\n";
    const PCI_IDS: &str = "10de  NVIDIA Corporation\n\t2860  AD106M [GeForce RTX 4070 Max-Q / Mobile]\n8086  Intel Corporation\n\ta7a0  Raptor Lake-P [Iris Xe Graphics]\n\t\t1043 16d3  subsystem\n";

    /// One fake GPU: its slot, vendor, device, kernel driver and sysfs files.
    type Gpu<'a> = (&'a str, &'a str, &'a str, &'a str, &'a [(&'a str, &'a str)]);

    /// A fake `/sys/bus/pci/devices` with `gpus` in it.
    fn sysfs(root: &Path, gpus: &[Gpu<'_>]) -> PathBuf {
        let devices = root.join("devices");
        let drivers = root.join("drivers");
        std::fs::create_dir_all(&drivers).unwrap();
        for (slot, vendor, device, driver, files) in gpus {
            let dir = devices.join(slot);
            std::fs::create_dir_all(dir.join("drm")).unwrap();
            for (name, value) in [
                ("class", "0x030000"),
                ("vendor", *vendor),
                ("device", *device),
            ]
            .iter()
            .chain(files.iter())
            {
                if name.starts_with("renderD") {
                    std::fs::create_dir_all(dir.join("drm").join(name)).unwrap();
                } else {
                    std::fs::write(dir.join(name), format!("{value}\n")).unwrap();
                }
            }
            if !driver.is_empty() {
                std::fs::create_dir_all(drivers.join(driver)).unwrap();
                std::os::unix::fs::symlink(drivers.join(driver), dir.join("driver")).unwrap();
            }
        }
        // Not a display controller: never a GPU.
        let bridge = devices.join("0000:00:00.0");
        std::fs::create_dir_all(&bridge).unwrap();
        std::fs::write(bridge.join("class"), "0x060000\n").unwrap();
        std::fs::write(bridge.join("vendor"), "0x1022\n").unwrap();
        devices
    }

    fn probe(
        root: &Path,
        gpus: &[Gpu<'_>],
        nvidia: &[NvidiaCard],
        mismatch: bool,
    ) -> Vec<Accelerator> {
        let devices = sysfs(root, gpus);
        std::fs::write(root.join("amdgpu.ids"), AMDGPU_IDS).unwrap();
        std::fs::write(root.join("pci.ids"), PCI_IDS).unwrap();
        probe_accelerators(
            &ProbeSources {
                pci_devices: &devices,
                amdgpu_ids: &root.join("amdgpu.ids"),
                pci_ids: &root.join("pci.ids"),
            },
            nvidia,
            mismatch,
            None,
            32_038,
        )
    }

    const RADEON_780M: Gpu<'static> = (
        "0000:66:00.0",
        "0x1002",
        "0x15bf",
        "amdgpu",
        &[
            ("revision", "0xc1"),
            ("mem_info_vram_total", "536870912"),
            ("mem_info_gtt_total", "16403759104"),
            ("renderD128", ""),
        ],
    );
    const RTX_4070: Gpu<'static> = (
        "0000:01:00.0",
        "0x10de",
        "0x2860",
        "nvidia",
        &[("renderD129", "")],
    );

    /// The probe finds every GPU on the PCI bus: an AMD APU with its carve-out, the system memory
    /// it maps and its libdrm name; an NVIDIA card with what `nvidia-smi` reports; an Intel iGPU
    /// sharing all RAM.
    #[test]
    fn the_probe_reports_every_gpu_with_its_memory_and_driver() {
        let smi = nvidia_cards(
            b"00000000:01:00.0, NVIDIA GeForce RTX 4070 Laptop GPU, 8188, 615.71.09\n",
        );
        assert_eq!(
            smi,
            [NvidiaCard {
                pci_slot: "0000:01:00.0".into(),
                name: "NVIDIA GeForce RTX 4070 Laptop GPU".into(),
                vram_mib: 8188,
                driver: "615.71.09".into(),
            }]
        );
        let temp = tempfile::tempdir().unwrap();
        let found = probe(temp.path(), &[RADEON_780M, RTX_4070], &smi, false);
        assert_eq!(
            found,
            [
                Accelerator {
                    vendor: AcceleratorVendor::Nvidia,
                    name: "NVIDIA GeForce RTX 4070 Laptop GPU".into(),
                    pci_slot: "0000:01:00.0".into(),
                    render_node: Some("renderD129".into()),
                    kernel_driver: "nvidia".into(),
                    integrated: false,
                    dedicated_mib: 8188,
                    shared_mib: 0,
                    compute: Some("615.71.09".into()),
                    restart: None,
                },
                Accelerator {
                    vendor: AcceleratorVendor::Amd,
                    name: "AMD Radeon 780M Graphics".into(),
                    pci_slot: "0000:66:00.0".into(),
                    render_node: Some("renderD128".into()),
                    kernel_driver: "amdgpu".into(),
                    integrated: true,
                    dedicated_mib: 512,
                    shared_mib: 15_643,
                    compute: None,
                    restart: None,
                },
            ]
        );
        assert!(found.iter().all(|gpu| gpu.drives(RuntimeKind::Vulkan)));

        // A discrete AMD card keeps a VRAM vendor record.
        let temp = tempfile::tempdir().unwrap();
        let discrete = probe(
            temp.path(),
            &[(
                "0000:03:00.0",
                "0x1002",
                "0x744c",
                "amdgpu",
                &[
                    ("mem_info_vram_total", "25753026560"),
                    ("mem_info_vram_vendor", "samsung"),
                    ("renderD128", ""),
                ],
            )],
            &[],
            false,
        );
        assert!(!discrete[0].integrated);
        assert_eq!(discrete[0].dedicated_mib, 24_560);
        assert_eq!(discrete[0].label(), "AMD graphics");

        let temp = tempfile::tempdir().unwrap();
        let intel = probe(
            temp.path(),
            &[(
                "0000:00:02.0",
                "0x8086",
                "0xa7a0",
                "xe",
                &[("renderD128", "")],
            )],
            &[],
            false,
        );
        assert!(intel[0].integrated);
        assert_eq!(intel[0].shared_mib, 32_038);
        assert_eq!(intel[0].name, "Raptor Lake-P [Iris Xe Graphics]");
    }

    /// A card without its driver needs one, a driver/library mismatch waits for a restart, and a
    /// staged driver turns a driverless card into one waiting for a restart.
    #[test]
    fn a_card_without_its_driver_needs_one_or_waits_for_a_restart() {
        // No `nvidia-smi` answer: the card needs its driver, named from pci.ids.
        let temp = tempfile::tempdir().unwrap();
        let driverless = probe(
            temp.path(),
            &[("0000:01:00.0", "0x10de", "0x2860", "", &[])],
            &[],
            false,
        );
        assert_eq!(
            driverless[0].name,
            "AD106M [GeForce RTX 4070 Max-Q / Mobile]"
        );
        assert!(driverless[0].needs_driver());
        let hardware = HardwareSnapshot {
            accelerators: driverless,
            ..HardwareSnapshot::default()
        };
        assert_eq!(
            hardware.accelerator_state(),
            AcceleratorState::NeedsDriver {
                vendor: AcceleratorVendor::Nvidia
            }
        );
        let staged = with_staged_driver(
            hardware.clone(),
            Some(&AcceleratorSetup {
                boot_pack_id: "nvidia".into(),
                status: AcceleratorSetupState::RestartRequired,
            }),
        );
        assert_eq!(
            staged.accelerator_state(),
            AcceleratorState::RestartRequired {
                vendor: AcceleratorVendor::Nvidia,
                reason: RestartReason::DriverStaged,
            }
        );
        assert_eq!(
            recommendation(&staged, None).fit,
            HardwareFit::RestartRequired
        );
        assert_eq!(with_staged_driver(hardware.clone(), None), hardware);

        // The kernel module and the library disagree: a restart, not no GPU.
        assert!(nvml_reports_version_mismatch(
            b"Failed to initialize NVML: Driver/library version mismatch\n",
            b""
        ));
        assert!(!nvml_reports_version_mismatch(
            b"NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver.",
            b""
        ));
        let temp = tempfile::tempdir().unwrap();
        let mismatched = probe(temp.path(), &[RTX_4070], &[], true);
        assert_eq!(
            mismatched[0].restart,
            Some(RestartReason::DriverLibraryMismatch)
        );
    }

    fn signed_runtime_fixture(
        id: &str,
        engine: RuntimeEngine,
        kinds: &[RuntimeKind],
    ) -> SignedRuntime {
        let manifest = format!("{{\"schemaVersion\":2,\"id\":\"{id}\"}}");
        let digest = hex::encode(Sha256::digest(manifest.as_bytes()));
        let artifact = agent_provider_config::local::PinnedArtifact {
            url: format!("https://registry.example.invalid/v2/{id}/manifests/sha256:{digest}"),
            sha256: digest.clone(),
            size_bytes: manifest.len() as u64,
        };
        let signed = system_extension::SignedExtensionManifest::sign(
            system_extension::ExtensionManifest {
                schema_version: system_extension::SCHEMA_VERSION,
                id: id.into(),
                kind: system_extension::ExtensionKind::LocalModelRuntime,
                version: "1".into(),
                artifact: system_extension::PinnedArtifact {
                    source: artifact.url.clone(),
                    sha256: artifact.sha256.clone(),
                    size_bytes: artifact.size_bytes,
                },
                target: system_extension::ImageConstraint {
                    base_image_digest: format!("sha256:{}", "c".repeat(64)),
                    architecture: None,
                    kernel_abi: None,
                },
                requires_reboot: false,
            },
            "test",
            &ed25519_dalek::SigningKey::from_bytes(&[5; 32]),
        )
        .unwrap();
        SignedRuntime {
            accepted: system_extension::AcceptedExtension {
                signed,
                artifact: system_extension::ArtifactReceipt {
                    sha256: artifact.sha256.clone(),
                    size_bytes: artifact.size_bytes,
                },
                committed_revision: 1,
            },
            release: agent_provider_config::local::RuntimeRelease {
                id: id.into(),
                kinds: kinds.iter().copied().collect(),
                version: "1".into(),
                engine,
                oci_image: format!("registry.example.invalid/{id}@sha256:{digest}"),
                artifact,
            },
        }
    }

    /// The review model as vLLM's AWQ snapshot and llama.cpp's GGUF, with both runtime packs.
    fn review_catalog() -> CatalogPayload {
        use agent_provider_config::local::{ModelArtifact, PinnedArtifact, SnapshotFile};
        let pin = |name: &str, size_bytes| PinnedArtifact {
            url: format!("https://huggingface.co/Qwen/{name}"),
            sha256: "a".repeat(64),
            size_bytes,
        };
        let release = |id: &str, label: &str, artifact: ModelArtifact, engine| LocalModelRelease {
            id: id.into(),
            label: label.into(),
            description: "Reviews feedback recordings on this device.".into(),
            version: "b".repeat(40),
            installed_size_bytes: artifact.download_bytes(),
            artifact,
            license_id: "Apache-2.0".into(),
            license_url: "https://huggingface.co/Qwen/LICENSE".into(),
            requires_acceptance: false,
            engines: [engine].into(),
            allowed_runtimes: match engine {
                RuntimeEngine::Vllm => [RuntimeKind::NvidiaCuda].into(),
                RuntimeEngine::LlamaCpp => [RuntimeKind::Vulkan, RuntimeKind::Cpu].into(),
            },
            required_memory_mib: 12_288,
            required_accelerator_mib: 6_144,
            context_window: 8_192,
            capabilities: [agent_provider_config::ModelCapability::TraceReview].into(),
            accelerator_memory_percent: None,
        };
        let awq = release(
            "qwen/qwen3-4b-awq",
            "Qwen3 4B (AWQ, 4-bit)",
            ModelArtifact::HfSnapshot {
                files: vec![
                    SnapshotFile {
                        path: "config.json".into(),
                        artifact: pin("config.json", 1_000),
                    },
                    SnapshotFile {
                        path: "model.safetensors".into(),
                        artifact: pin("model.safetensors", 2_669_000_000),
                    },
                ],
            },
            RuntimeEngine::Vllm,
        );
        let mut gguf = release(
            "qwen/qwen3-4b-gguf",
            "Qwen3 4B (GGUF, Q4_K_M)",
            ModelArtifact::Gguf(pin("Qwen3-4B-Q4_K_M.gguf", 2_497_280_256)),
            RuntimeEngine::LlamaCpp,
        );
        gguf.required_memory_mib = 5_120;
        gguf.required_accelerator_mib = 4_096;
        CatalogPayload {
            schema: 1,
            models: vec![awq, gguf],
            runtimes: vec![
                signed_runtime_fixture(
                    "vllm-cuda",
                    RuntimeEngine::Vllm,
                    &[RuntimeKind::NvidiaCuda],
                ),
                signed_runtime_fixture(
                    "llama-cpp-vulkan",
                    RuntimeEngine::LlamaCpp,
                    &[RuntimeKind::Vulkan, RuntimeKind::Cpu],
                ),
            ],
        }
    }

    /// On this machine's hardware (an RTX 4070 Mobile beside a Radeon 780M), the snapshot
    /// recommends vLLM on the card and lists llama.cpp on the card, on the iGPU and on the CPU as
    /// the alternatives, each with its device and memory; an installed model names where it runs;
    /// a placement the probe does not support is refused.
    #[test]
    fn the_snapshot_ranks_every_configuration_this_machine_supports() {
        let catalog = review_catalog();
        assert!(model_catalog(&catalog).validate().is_ok());
        let temp = tempfile::tempdir().unwrap();
        let smi = nvidia_cards(
            b"00000000:01:00.0, NVIDIA GeForce RTX 4070 Laptop GPU, 8188, 615.71.09\n",
        );
        let hardware = HardwareSnapshot {
            cpu_threads: 16,
            memory_mib: 31_287,
            available_memory_mib: 22_528,
            free_storage_mib: 100 * 1024,
            accelerators: probe(temp.path(), &[RADEON_780M, RTX_4070], &smi, false),
        };
        let mut state = local_model_service::ServiceState::default();
        let igpu = Placement {
            runtime: "llama-cpp-vulkan".into(),
            kind: RuntimeKind::Vulkan,
            device: Some("0000:66:00.0".into()),
        };
        state.models.insert(
            "qwen/qwen3-4b-gguf".into(),
            local_model_service::ManagedModel {
                identity: "a".repeat(64),
                placement: igpu.clone(),
                objects: std::collections::BTreeSet::new(),
                status: LocalModelStatus::Installed,
                process: None,
            },
        );
        let snapshot = snapshot_for(&catalog, &hardware, &state, None);
        let ranked: Vec<(RuntimeEngine, Option<&str>, &str)> = snapshot
            .available
            .iter()
            .map(|available| {
                (
                    available.engine,
                    available.device.as_deref(),
                    available.package.id.as_str(),
                )
            })
            .collect();
        assert_eq!(
            ranked,
            [
                (
                    RuntimeEngine::Vllm,
                    Some("NVIDIA GeForce RTX 4070 Laptop GPU"),
                    "qwen/qwen3-4b-awq"
                ),
                (
                    RuntimeEngine::LlamaCpp,
                    Some("AMD Radeon 780M Graphics"),
                    "qwen/qwen3-4b-gguf"
                ),
                (RuntimeEngine::LlamaCpp, None, "qwen/qwen3-4b-gguf"),
                (
                    RuntimeEngine::LlamaCpp,
                    Some("NVIDIA GeForce RTX 4070 Laptop GPU"),
                    "qwen/qwen3-4b-gguf"
                ),
            ]
        );
        assert_eq!(snapshot.hardware.fit, HardwareFit::Good);
        assert!(
            snapshot.hardware.reason.contains("vLLM"),
            "{}",
            snapshot.hardware.reason
        );
        assert!(snapshot.available[1].shared_memory);
        assert_eq!(snapshot.models[0].placement, igpu);
        assert_eq!(snapshot.models[0].engine, RuntimeEngine::LlamaCpp);
        assert_eq!(
            snapshot.models[0].device.as_deref(),
            Some("AMD Radeon 780M Graphics")
        );

        let local = model_catalog(&catalog);
        assert!(placed(&local, &hardware, "qwen/qwen3-4b-gguf", &igpu).is_ok());
        let gone = Placement {
            device: Some("0000:07:00.0".into()),
            ..igpu
        };
        assert!(placed(&local, &hardware, "qwen/qwen3-4b-gguf", &gone).is_err());

        // A CPU-only machine is offered llama.cpp on the CPU, labelled slow.
        let cpu_only = HardwareSnapshot {
            accelerators: Vec::new(),
            ..hardware
        };
        let snapshot = snapshot_for(&catalog, &cpu_only, &state, None);
        assert_eq!(snapshot.available.len(), 1);
        assert_eq!(snapshot.available[0].placement.kind, RuntimeKind::Cpu);
        assert_eq!(
            snapshot.available[0].speed.class,
            agent_provider_config::local::SpeedClass::Slow
        );
        assert_eq!(snapshot.models[0].device, None, "the iGPU is gone");
    }

    /// The install verb carries the configuration, and the RPC maps it back unchanged.
    #[test]
    fn an_install_names_its_configuration_end_to_end() {
        let cli = Cli::try_parse_from([
            "tilo-model-manager",
            "install",
            "--model",
            "qwen/qwen3-4b-gguf",
            "--runtime",
            "llama-cpp-vulkan",
            "--on",
            "vulkan",
            "--device",
            "0000:66:00.0",
            "--accept-license",
        ])
        .unwrap();
        let action = rpc_action(&cli.command).unwrap();
        let RpcAction::Install { placement, .. } = &action else {
            panic!("an install");
        };
        assert_eq!(placement.kind, RuntimeKind::Vulkan);
        assert_eq!(placement.device.as_deref(), Some("0000:66:00.0"));
        let back = cli_for_rpc(action);
        assert!(matches!(
            back.command,
            Action::Install {
                on: RuntimeKind::Vulkan,
                ..
            }
        ));
        assert!(
            Cli::try_parse_from([
                "tilo-model-manager",
                "install",
                "--model",
                "m",
                "--runtime",
                "r",
                "--on",
                "gpu"
            ])
            .is_err()
        );
    }
}
