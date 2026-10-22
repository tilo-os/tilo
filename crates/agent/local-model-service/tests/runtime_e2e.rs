//! A signed llama.cpp runtime serving a downloaded GGUF model, end to end:
//! the catalog compiler signs a developer catalog naming the runtime archive
//! the release check built and a tiny pinned model, the service fetches and
//! verifies both through its offline transport, picks the runtime for this
//! machine's hardware and starts it, and a chat completion goes through the
//! real model gateway to it.
//!
//! The runtime's own `llama-server` and libraries run from the verified
//! archive's root through its own dynamic loader, standing in for podman,
//! which t's sandbox lacks. `tools/release/local-model-runtime-e2e.sh` runs it
//! with the archive, the model and the gateway it built.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use agent_provider_config::local::{HardwareSnapshot, RuntimeDecision, select_runtime_for_model};
use agent_provider_config::protocol::ModelResponse;
use agent_provider_config::{EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile};
use local_model_service::catalog::VerifiedCatalog;
use local_model_service::developer::OfflineTransport;
use local_model_service::{
    LocalModelService, ProcessHandle, ProcessSupervisor, ServiceError, VllmCommand,
};

/// How long a real server may take to load and answer before the test
/// counts it as hung.
const READY_WITHIN: Duration = Duration::from_secs(120);

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    let mut file = fs::File::open(path).unwrap();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hex::encode(hasher.finalize())
}

/// Runs an OCI image's entrypoint from the image's own root, the way the
/// production supervisor runs it in a container.
struct ExtractedImageSupervisor {
    root: PathBuf,
    entrypoint: Vec<String>,
    working_dir: String,
    children: BTreeMap<String, Child>,
}

impl ExtractedImageSupervisor {
    fn json(&self, digest: &str) -> serde_json::Value {
        let hex = digest.strip_prefix("sha256:").unwrap();
        let path = self.root.join("layout/blobs/sha256").join(hex);
        assert_eq!(
            sha256(&path),
            hex,
            "blob {digest} does not match its digest"
        );
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }
}

impl ProcessSupervisor for ExtractedImageSupervisor {
    fn install_runtime_image(&mut self, archive: &Path, signed: &str) -> Result<(), ServiceError> {
        let layout = self.root.join("layout");
        let rootfs = self.root.join("rootfs");
        fs::create_dir_all(&layout)?;
        fs::create_dir_all(&rootfs)?;
        tar(&[
            "-xf",
            &archive.to_string_lossy(),
            "-C",
            &layout.to_string_lossy(),
        ]);
        // What podman's RepoDigests check proves: the archive names the
        // signed repository and holds the signed manifest digest.
        let index: serde_json::Value =
            serde_json::from_slice(&fs::read(layout.join("index.json"))?).unwrap();
        let entry = &index["manifests"][0];
        let digest = entry["digest"].as_str().unwrap();
        let name = entry["annotations"]["org.opencontainers.image.ref.name"]
            .as_str()
            .unwrap();
        let (repository, signed_digest) = signed.split_once('@').unwrap();
        if signed_digest != digest
            || name.rsplit_once(':').map(|(repo, _)| repo) != Some(repository)
        {
            return Err(ServiceError::Backend(format!(
                "archive holds {name}@{digest}, not {signed}"
            )));
        }
        let manifest = self.json(digest);
        for layer in manifest["layers"].as_array().unwrap() {
            let hex = layer["digest"]
                .as_str()
                .unwrap()
                .strip_prefix("sha256:")
                .unwrap();
            let blob = layout.join("blobs/sha256").join(hex);
            assert_eq!(sha256(&blob), hex);
            unpack_layer(&blob, &rootfs);
        }
        let config = self.json(manifest["config"]["digest"].as_str().unwrap());
        self.entrypoint = config["config"]["Entrypoint"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| part.as_str().unwrap().to_owned())
            .collect();
        config["config"]["WorkingDir"]
            .as_str()
            .unwrap_or("/")
            .clone_into(&mut self.working_dir);
        Ok(())
    }

    fn prepare(&mut self, command: &VllmCommand) -> Result<ProcessHandle, ServiceError> {
        Ok(ProcessHandle {
            id: format!("e2e-{}", command.endpoint.model_id.replace('/', "-")),
            container: None,
        })
    }

    fn start(&mut self, command: &VllmCommand, handle: &ProcessHandle) -> Result<(), ServiceError> {
        let rootfs = self.root.join("rootfs");
        let inside = |path: &str| rootfs.join(path.trim_start_matches('/'));
        let args = command.args.iter().map(|arg| {
            arg.strip_prefix(&command.model_container_path).map_or_else(
                || arg.clone(),
                |rest| format!("{}{rest}", command.model_path.display()),
            )
        });
        let libraries = [inside(&self.working_dir), inside("/usr/lib64")]
            .map(|path| path.display().to_string())
            .join(":");
        let child = Command::new(inside("/usr/lib64/ld-linux-x86-64.so.2"))
            .arg("--library-path")
            .arg(libraries)
            .arg(inside(self.entrypoint.first().unwrap()))
            .args(self.entrypoint.iter().skip(1))
            .args(args)
            // llama.cpp starts a thread per core of the host and every token
            // waits for the slowest of them, which on a large shared build
            // machine is one descheduled thread. The tiny model needs two.
            .args(["--threads", "2"])
            .current_dir(inside(&self.working_dir))
            .env_clear()
            .stdin(Stdio::null())
            .spawn()?;
        self.children.insert(handle.id.clone(), child);
        let client = tilo_net::http::blocking_client()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let started = Instant::now();
        while started.elapsed() < READY_WITHIN {
            if local_model_service::supervision::model_endpoint_ready(&client, &command.endpoint) {
                return Ok(());
            }
            let child = self.children.get_mut(&handle.id).unwrap();
            if let Some(status) = child.try_wait()? {
                return Err(ServiceError::Backend(format!(
                    "llama-server exited: {status}"
                )));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(ServiceError::Backend(
            "llama-server did not become ready".into(),
        ))
    }

    fn stop(&mut self, handle: &ProcessHandle) -> Result<(), ServiceError> {
        if let Some(mut child) = self.children.remove(&handle.id) {
            child.kill()?;
            child.wait()?;
        }
        Ok(())
    }
}

/// Unpack one gzip layer into `rootfs` as this user: directories stay
/// writable for the layers above them, and only directories, files and
/// links are taken (a runtime's root needs nothing else).
fn unpack_layer(blob: &Path, rootfs: &Path) {
    let mut gunzip = Command::new("gzip")
        .arg("-dc")
        .arg(blob)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut archive = tar::Archive::new(gunzip.stdout.take().unwrap());
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let relative = entry.path().unwrap().into_owned();
        let target = rootfs.join(&relative);
        let mode = entry.header().mode().unwrap();
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                fs::create_dir_all(&target).unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(mode | 0o700)).unwrap();
            }
            tar::EntryType::Regular | tar::EntryType::Symlink | tar::EntryType::Link => {
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                let _ = fs::remove_file(&target);
                assert!(
                    entry.unpack_in(rootfs).unwrap(),
                    "{} escapes",
                    relative.display()
                );
                if entry.header().entry_type() == tar::EntryType::Regular {
                    fs::set_permissions(&target, fs::Permissions::from_mode(mode | 0o600)).unwrap();
                }
            }
            _ => {}
        }
    }
    assert!(gunzip.wait().unwrap().success());
}

fn tar(args: &[&str]) {
    let status = Command::new("tar").args(args).status().unwrap();
    assert!(status.success(), "tar {args:?}: {status}");
}

/// Stage `file` where the offline transport finds it, by its digest.
fn stage(artifacts: &Path, file: &Path) -> (String, u64) {
    let digest = sha256(file);
    let target = artifacts.join(&digest);
    if fs::hard_link(file, &target).is_err() {
        fs::copy(file, &target).unwrap();
    }
    (digest, fs::metadata(file).unwrap().len())
}

/// Sign a developer catalog of `runtime` (the release check's entry) and the
/// model with the real compiler and a throwaway key.
fn developer_catalog(
    work: &Path,
    runtime: &serde_json::Value,
    (digest, size): &(String, u64),
) -> VerifiedCatalog {
    let source = serde_json::json!({
        "schema": 1,
        "production": false,
        "key_id": "runtime-e2e",
        "recommendation": "stories-260k",
        "runtimes": [runtime],
        "models": [{
            "id": "stories-260k",
            "label": "Stories 260K",
            "description": "A tiny story model for testing.",
            "revision": "499bc8821c6b12b4e53c5bffcb21ec206f212d81",
            "artifact": {
                "file": "artifacts/model",
                "url": format!("offline:sha256:{digest}"),
                "sha256": digest,
                "size_bytes": size,
            },
            "artifact_format": "llama_cpp_gguf",
            "license_id": "MIT",
            "license_url": "https://huggingface.co/ggml-org/models-moved/tree/499bc8821c6b12b4e53c5bffcb21ec206f212d81",
            "requires_acceptance": false,
            "installed_size_bytes": size,
            "allowed_runtimes": ["cpu"],
            "required_memory_mib": 256,
            "required_accelerator_mib": 0,
            "context_window": 256,
        }],
    });
    fs::write(
        work.join("source.json"),
        serde_json::to_vec(&source).unwrap(),
    )
    .unwrap();
    let key = work.join("signing-key");
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).unwrap();
    fs::write(&key, hex::encode(seed)).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_tilo-model-catalog"))
        .arg("--source")
        .arg(work.join("source.json"))
        .arg("--signing-key")
        .arg(&key)
        .arg("--catalog-out")
        .arg(work.join("catalog.json"))
        .arg("--trust-out")
        .arg(work.join("keys.json"))
        .arg("--developer-offline")
        .status()
        .unwrap();
    assert!(status.success(), "the catalog compiler refused the source");
    VerifiedCatalog::load_developer_offline(&work.join("catalog.json"), &work.join("keys.json"))
        .unwrap()
}

/// A gateway serving `socket` for the local profile pointing at `url`.
fn gateway(work: &Path, url: &str, model: &str) -> (Child, PathBuf) {
    let mut profile = ProviderProfile::local_vllm();
    profile.id = "local".into();
    profile.endpoint = url.into();
    profile.location = EndpointLocation::ThisDevice;
    profile.models = vec![ModelDescriptor::new(model)];
    let settings = work.join("settings.json");
    ProviderConfig {
        profiles: vec![profile],
        default_profile: None,
        ..Default::default()
    }
    .save_at(&settings)
    .unwrap();
    let socket = work.join("gateway.sock");
    let child = Command::new(env_path("TILO_E2E_GATEWAY"))
        .arg("serve")
        .arg("--socket")
        .arg(&socket)
        .env_clear()
        .env("HOME", work)
        .env("XDG_CONFIG_HOME", work.join("config"))
        .env("XDG_RUNTIME_DIR", work)
        .env("TILO_SESSION_SETTINGS", &settings)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    (child, socket)
}

fn frame(stream: &mut UnixStream, value: &serde_json::Value) {
    let body = serde_json::to_vec(value).unwrap();
    stream
        .write_all(&u32::try_from(body.len()).unwrap().to_le_bytes())
        .unwrap();
    stream.write_all(&body).unwrap();
}

fn read_frame(stream: &mut UnixStream) -> serde_json::Value {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).unwrap();
    let mut body = vec![0_u8; usize::try_from(u32::from_le_bytes(length)).unwrap()];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn complete(socket: &Path, model: &str) -> ModelResponse {
    let started = Instant::now();
    let mut stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(error) if started.elapsed() > READY_WITHIN => panic!("gateway: {error}"),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    stream.set_read_timeout(Some(READY_WITHIN)).unwrap();
    frame(
        &mut stream,
        &serde_json::json!({"protocol": 1, "app_id": "so.tilo.agent"}),
    );
    frame(
        &mut stream,
        &serde_json::json!({
            "version": 3,
            "verb": "complete",
            "profile_id": "local",
            "model_id": model,
            "request": {
                "messages": [{"role": "user", "content": [{"type": "text", "text": "Tell me a story."}]}],
                "tools": [],
                "tool_choice": "none",
                "max_output_tokens": 24,
            },
            "timeout_ms": 60_000,
        }),
    );
    let response = read_frame(&mut stream);
    assert_eq!(response["ok"], true, "the gateway refused: {response}");
    serde_json::from_str(response["output"].as_str().unwrap()).unwrap()
}

#[test]
#[ignore = "needs a built runtime archive, a model and the gateway: tools/release/local-model-runtime-e2e.sh"]
fn a_downloaded_gguf_model_answers_through_the_gateway_on_the_signed_runtime() {
    let work = tempfile::tempdir().unwrap();
    let artifacts = work.path().join("artifacts");
    fs::create_dir_all(&artifacts).unwrap();
    let runtime_dir = env_path("TILO_E2E_RUNTIME");
    let mut runtime: serde_json::Value =
        serde_json::from_slice(&fs::read(runtime_dir.join("runtime.json")).unwrap()).unwrap();
    let archive = runtime_dir.join(format!("{}.oci.tar", runtime["id"].as_str().unwrap()));
    let (digest, size) = stage(&artifacts, &archive);
    assert_eq!(runtime["artifact"]["sha256"], digest.as_str());
    runtime["artifact"] = serde_json::json!({
        "file": archive,
        "url": format!("offline:sha256:{digest}"),
        "sha256": digest,
        "size_bytes": size,
    });
    let model = stage(&artifacts, &env_path("TILO_E2E_MODEL"));
    fs::hard_link(artifacts.join(&model.0), artifacts.join("model")).unwrap();
    let catalog = developer_catalog(work.path(), &runtime, &model).into_payload();
    let release = catalog.models.first().unwrap().clone();

    // The service picks the runtime this machine's hardware calls for: a
    // processor-only machine gets the CPU build of its architecture.
    let hardware = HardwareSnapshot {
        cpu_threads: 4,
        memory_mib: 8192,
        free_storage_mib: 1024,
        accelerators: Vec::new(),
    };
    let serving: Vec<_> = catalog
        .runtimes
        .iter()
        .filter(|runtime| runtime.serves(&release, std::env::consts::ARCH))
        .collect();
    let RuntimeDecision::Ready(kind) = select_runtime_for_model(
        &hardware,
        &release,
        serving.iter().map(|runtime| runtime.release.kind),
    ) else {
        panic!("no runtime for a processor-only machine");
    };
    let runtime = serving
        .into_iter()
        .find(|runtime| runtime.release.kind == kind)
        .unwrap();

    let supervisor = ExtractedImageSupervisor {
        root: work.path().join("image"),
        entrypoint: Vec::new(),
        working_dir: String::new(),
        children: BTreeMap::new(),
    };
    let mut service = LocalModelService::open(
        work.path().join("store"),
        OfflineTransport::new(&artifacts, &catalog).unwrap(),
        supervisor,
    )
    .unwrap();
    service.install_with_runtime(runtime, &release).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = service.start(runtime, &release, port).unwrap();
    assert_eq!(endpoint.model_id, "stories-260k");

    let (mut gateway, socket) = gateway(work.path(), &endpoint.url, &endpoint.model_id);
    let answer = complete(&socket, &endpoint.model_id);
    gateway.kill().unwrap();
    gateway.wait().unwrap();
    service.stop(&release.id).unwrap();
    let text: String = answer
        .message
        .content
        .iter()
        .filter_map(|content| match content {
            agent_provider_config::protocol::Content::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        !text.trim().is_empty(),
        "the model answered nothing: {answer:?}"
    );
    println!("stories-260k answered through the gateway: {text}");
}
