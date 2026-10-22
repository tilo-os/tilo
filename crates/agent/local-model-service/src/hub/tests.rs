use std::collections::BTreeMap;
use std::fs::File;

use agent_provider_config::local::HardwareSnapshot;
use agent_provider_config::local::hub::{self as picks, HubModelOffer, HubRef};

use super::*;
use crate::tests::{Supervisor, signed_fixture_runtime};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// The Hub, answering API calls and downloads from memory.
#[derive(Default)]
struct FakeHub {
    json: BTreeMap<String, Vec<u8>>,
    files: BTreeMap<String, Vec<u8>>,
    /// Every download asked: its URL and first byte.
    ranges: Vec<(String, u64)>,
    /// Deliver at most this many bytes per download, as a dropped connection.
    cut: Option<usize>,
}

impl HubApi for FakeHub {
    fn get(&mut self, url: &str, limit: usize) -> Result<Vec<u8>, ServiceError> {
        let body = self
            .json
            .get(url)
            .cloned()
            .ok_or_else(|| ServiceError::Backend(format!("404 {url}")))?;
        if body.len() > limit {
            return Err(ServiceError::PartialTooLarge);
        }
        Ok(body)
    }
}

impl HttpsTransport for FakeHub {
    fn resume(
        &mut self,
        url: &str,
        start: u64,
        maximum: u64,
        progress: &mut dyn FnMut(u64),
        destination: &mut File,
    ) -> Result<u64, ServiceError> {
        self.ranges.push((url.to_owned(), start));
        let bytes = self
            .files
            .get(url)
            .ok_or_else(|| ServiceError::Backend(format!("404 {url}")))?;
        let tail = &bytes[usize::try_from(start).unwrap()..];
        if tail.len() as u64 > maximum {
            return Err(ServiceError::PartialTooLarge);
        }
        let sent = &tail[..tail.len().min(self.cut.unwrap_or(usize::MAX))];
        destination.write_all(sent)?;
        progress(sent.len() as u64);
        Ok(sent.len() as u64)
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn blob_id(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("blob {}\0", bytes.len()).as_bytes());
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// A repository of `(path, bytes, large)` files, published on `hub`:
/// large files carry an LFS hash, small ones only their blob id.
fn publish(hub: &mut FakeHub, repo: &str, files: &[(&str, &[u8], bool)]) -> ApiModelInfo {
    let siblings: Vec<serde_json::Value> = files
        .iter()
        .map(|(path, bytes, large)| {
            hub.files
                .insert(picks::file_url(repo, COMMIT, path), bytes.to_vec());
            let mut sibling = serde_json::json!({
                "rfilename": path,
                "size": bytes.len(),
                "blobId": blob_id(bytes),
            });
            if *large {
                sibling["lfs"] = serde_json::json!({"sha256": sha256(bytes), "size": bytes.len()});
            }
            sibling
        })
        .collect();
    let info = serde_json::json!({
        "id": repo,
        "sha": COMMIT,
        "cardData": {"license": "apache-2.0"},
        "siblings": siblings,
    });
    hub.json.insert(
        picks::model_info_url(repo, None),
        serde_json::to_vec(&info).unwrap(),
    );
    serde_json::from_value(info).unwrap()
}

fn machine() -> HardwareSnapshot {
    HardwareSnapshot {
        cpu_threads: 8,
        memory_mib: 32 * 1024,
        free_storage_mib: 1024 * 1024,
        accelerators: vec![],
    }
}

fn pick(info: &ApiModelInfo, variant: &str) -> UserModel {
    let offer = HubModelOffer::of(info, &machine(), |_| true, false).unwrap();
    UserModel::from_offer(&offer, variant).unwrap()
}

fn service(root: &Path, hub: FakeHub) -> LocalModelService<FakeHub, Supervisor> {
    LocalModelService::open(
        root,
        hub,
        Supervisor {
            command: None,
            stopped: vec![],
        },
    )
    .unwrap()
}

#[test]
fn search_and_model_info_read_the_hubs_answers() {
    let mut hub = FakeHub::default();
    hub.json.insert(
        picks::search_url("chat", HubFormat::Gguf),
        br#"[{"id":"acme/Chat-GGUF","downloads":9,"tags":["gguf"]},{"id":"bad id"}]"#.to_vec(),
    );
    let results = search(&mut hub, " chat ", HubFormat::Gguf).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].repo, "acme/Chat-GGUF");
    assert!(search(&mut hub, "  ", HubFormat::Gguf).is_err());

    publish(
        &mut hub,
        "acme/Chat-GGUF",
        &[("chat.Q4_0.gguf", b"GGUF0", true)],
    );
    let reference = HubRef::parse("https://huggingface.co/acme/chat-gguf").unwrap();
    // The Hub answers with the canonical spelling under any case.
    hub.json.insert(
        picks::model_info_url("acme/chat-gguf", None),
        hub.json[&picks::model_info_url("acme/Chat-GGUF", None)].clone(),
    );
    assert_eq!(
        model_info(&mut hub, &reference).unwrap().id,
        "acme/Chat-GGUF"
    );
    hub.json.insert(
        picks::model_info_url("acme/other", None),
        hub.json[&picks::model_info_url("acme/Chat-GGUF", None)].clone(),
    );
    assert!(model_info(&mut hub, &HubRef::parse("acme/other").unwrap()).is_err());
}

#[test]
fn a_picked_gguf_resumes_verifies_and_runs_on_the_signed_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let weights = b"GGUF weights of a tiny chat model";
    let mut hub = FakeHub::default();
    let info = publish(
        &mut hub,
        "acme/Chat-GGUF",
        &[
            ("README.md", b"# Chat", false),
            ("chat.Q4_K_M.gguf", weights, true),
        ],
    );
    hub.cut = Some(5);
    let mut service = service(dir.path(), hub);
    let model = pick(&info, "Q4_K_M");
    let release = service.queue_user_model(model.clone()).unwrap();
    assert_eq!(release.id, "hf/acme/chat-gguf/q4_k_m");

    // A dropped connection leaves what arrived, published for the reader.
    assert!(matches!(
        service.install_user_model(&model.id, None),
        Err(ServiceError::IncompleteDownload { .. })
    ));
    let published = crate::load_state(&dir.path().join(crate::STATE_FILE)).unwrap();
    assert_eq!(
        published.models[&model.id].status,
        LocalModelStatus::Downloading {
            received_bytes: 5,
            total_bytes: weights.len() as u64
        }
    );
    service.transport.cut = None;
    assert_eq!(
        service.install_user_model(&model.id, None).unwrap(),
        LocalModelStatus::Installed
    );
    let url = picks::file_url("acme/Chat-GGUF", COMMIT, "chat.Q4_K_M.gguf");
    assert_eq!(service.transport.ranges, [(url.clone(), 0), (url, 5)]);

    // The signed llama.cpp runtime mounts the assembled directory and is
    // told the file by its repository name.
    let runtime = signed_fixture_runtime(b"runtime");
    fs::write(
        dir.path()
            .join("sha256")
            .join(&runtime.release.artifact.sha256),
        b"runtime",
    )
    .unwrap();
    let release = service.user_release(&model.id).unwrap();
    service.start(&runtime, &release, 9000).unwrap();
    let command = service.supervisor.command.clone().unwrap();
    assert_eq!(command.model_container_path, "/models/model");
    assert_eq!(
        fs::read(command.model_path.join("chat.Q4_K_M.gguf")).unwrap(),
        weights
    );
    let arguments = command.args.join(" ");
    assert!(
        arguments
            .contains("--model /models/model/chat.Q4_K_M.gguf --alias hf/acme/chat-gguf/q4_k_m"),
        "{arguments}"
    );
    assert!(!arguments.contains("trust-remote-code"));

    // Removal stops it and leaves nothing of it behind.
    service.remove(&model.id).unwrap();
    assert_eq!(service.supervisor.stopped, ["svc-1"]);
    assert!(service.state().models.is_empty());
    assert!(service.state().user_models.is_empty());
    assert!(service.state().accepted_licenses.is_empty());
    assert!(!dir.path().join("sha256").join(sha256(weights)).exists());
    assert!(!command.model_path.exists());
}

#[test]
fn snapshot_files_are_checked_against_the_published_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let config = br#"{"architectures":["LlamaForCausalLM"]}"#;
    let mut hub = FakeHub::default();
    let info = publish(
        &mut hub,
        "acme/Chat",
        &[
            ("config.json", config, false),
            ("tokenizer.json", b"{}", false),
            ("model.safetensors", b"safetensors weights", true),
            ("modeling_chat.py", b"import os", false),
        ],
    );
    let model = pick(&info, "safetensors");
    assert_eq!(model.format, ModelArtifactFormat::HfSnapshotFiles);
    assert!(
        model
            .files
            .iter()
            .all(|file| file.path != "modeling_chat.py")
    );
    // The config is served altered: its blob id no longer matches.
    let config_url = picks::file_url("acme/Chat", COMMIT, "config.json");
    hub.files.insert(
        config_url.clone(),
        br#"{"architectures":["EvilForCausalLM!"]}"#.to_vec(),
    );
    let mut service = service(dir.path(), hub);
    service.queue_user_model(model.clone()).unwrap();
    assert!(matches!(
        service.install_user_model(&model.id, None),
        Err(ServiceError::DigestMismatch)
    ));
    assert!(
        fs::read_dir(dir.path().join("partial"))
            .unwrap()
            .next()
            .is_none()
    );
    assert!(service.user_release(&model.id).is_some());

    service.transport.files.insert(config_url, config.to_vec());
    service.install_user_model(&model.id, None).unwrap();
    let release = service.user_release(&model.id).unwrap();
    let directory = dir.path().join("snapshots").join(&release.artifact.sha256);
    validate_snapshot_layout(&directory).unwrap();
    assert_eq!(fs::read(directory.join("config.json")).unwrap(), config);
    assert!(!directory.join("modeling_chat.py").exists());
}

#[test]
fn removing_one_pick_keeps_the_files_another_still_uses() {
    let dir = tempfile::tempdir().unwrap();
    let shared = b"GGUF shared weights";
    let mut hub = FakeHub::default();
    let first = publish(&mut hub, "acme/One", &[("one.Q4_0.gguf", shared, true)]);
    let second = publish(&mut hub, "acme/Two", &[("two.Q4_0.gguf", shared, true)]);
    let mut service = service(dir.path(), hub);
    let (one, two) = (pick(&first, "Q4_0"), pick(&second, "Q4_0"));
    for model in [&one, &two] {
        service.queue_user_model(model.clone()).unwrap();
        service.install_user_model(&model.id, None).unwrap();
    }
    let object = dir.path().join("sha256").join(sha256(shared));
    service.remove(&one.id).unwrap();
    assert!(object.exists());
    service.remove(&two.id).unwrap();
    assert!(!object.exists());
}

#[test]
fn another_revision_waits_until_the_added_one_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let mut hub = FakeHub::default();
    let info = publish(
        &mut hub,
        "acme/Chat-GGUF",
        &[("chat.Q4_0.gguf", b"GGUF0", true)],
    );
    let mut service = service(dir.path(), hub);
    let model = pick(&info, "Q4_0");
    let first = service.queue_user_model(model.clone()).unwrap();
    // A retry of the same pick keeps its place in the store.
    assert_eq!(
        service
            .queue_user_model(model.clone())
            .unwrap()
            .artifact
            .sha256,
        first.artifact.sha256
    );
    let mut newer = model.clone();
    newer.source = UserModelSource::Hub {
        repo: "acme/Chat-GGUF".into(),
        revision: "f".repeat(40),
    };
    assert!(service.queue_user_model(newer.clone()).is_err());
    service.remove(&model.id).unwrap();
    service.queue_user_model(newer).unwrap();
}

#[test]
fn a_local_gguf_file_is_copied_hashed_and_independent_of_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("My Tune.Q4_K_M.gguf");
    let bytes = b"GGUF a fine-tune of my own";
    fs::write(&path, bytes).unwrap();
    let mut service = service(dir.path(), FakeHub::default());
    let model = UserModel::from_file("My Tune.Q4_K_M.gguf", bytes.len() as u64, "").unwrap();
    let release = service.queue_user_model(model.clone()).unwrap();
    assert!(!release.requires_acceptance);
    // Without the handle the file chooser gave, nothing is copied.
    assert!(service.install_user_model(&model.id, None).is_err());
    service
        .install_user_model(&model.id, Some(fs::File::open(&path).unwrap()))
        .unwrap();
    let installed = &service.state().user_models[&model.id];
    assert_eq!(installed.files[0].sha256(), Some(sha256(bytes).as_str()));
    fs::write(&path, b"GGUF changed afterwards").unwrap();
    let release = service.user_release(&model.id).unwrap();
    let copy = dir
        .path()
        .join("snapshots")
        .join(&release.artifact.sha256)
        .join("My Tune.Q4_K_M.gguf");
    assert_eq!(fs::read(copy).unwrap(), bytes);
    assert!(service.transport.ranges.is_empty());

    let other = files.path().join("notes.gguf");
    fs::write(&other, b"not a model").unwrap();
    let wrong = UserModel::from_file("notes.gguf", 11, "").unwrap();
    service.queue_user_model(wrong.clone()).unwrap();
    assert!(
        service
            .install_user_model(&wrong.id, Some(fs::File::open(&other).unwrap()))
            .is_err()
    );
}
