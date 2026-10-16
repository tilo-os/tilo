//! Authentication of a separate offline installed image (formerly
//! `os/tests/test_installer_offline_target.py`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::arithmetic_side_effects,
    reason = "a failed fixture call fails the test; fixtures are small known values"
)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Map, Value, json};
use tilo_install_helpers::offline_target::{
    BUILD_TYPE, BUILDER, Host, checked_provenance, import_image, read_target, verify,
};
use tilo_install_helpers::{Error, Result};

const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn update() -> String {
    format!("code.tilo.so/tilo/tilo:lenovo-83ed-{SOURCE}")
}

fn sha256(bytes: &[u8]) -> String {
    tilo_install_helpers::py::sha256_hex(bytes)
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Answers one tool call.
type Responder = Box<dyn Fn(&[String]) -> Result<String>>;

/// A fixture installer: paths in a temporary directory, tool calls answered
/// by `respond` or a queue, and every call recorded.
struct Fixture {
    dir: PathBuf,
    calls: RefCell<Vec<Vec<String>>>,
    queue: RefCell<VecDeque<String>>,
    respond: Responder,
    proof_port: bool,
}

impl Fixture {
    fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            calls: RefCell::new(Vec::new()),
            queue: RefCell::new(VecDeque::new()),
            respond: Box::new(|argv| panic!("unexpected {argv:?}")),
            proof_port: false,
        }
    }
}

impl Host for Fixture {
    fn root(&self) -> PathBuf {
        self.dir.join("target-oci")
    }

    fn key(&self) -> PathBuf {
        self.dir.join("cosign.pub")
    }

    fn cosign(&self) -> PathBuf {
        self.dir.join("cosign")
    }

    fn receipt(&self) -> PathBuf {
        self.dir.join("run/offline-target.json")
    }

    fn dev_vm_marker(&self) -> PathBuf {
        self.dir.join("vm-marker")
    }

    fn dev_flavour(&self) -> PathBuf {
        self.dir.join("flavour")
    }

    fn proof_port(&self) -> bool {
        self.proof_port
    }

    fn executable(&self, _path: &Path) -> bool {
        true
    }

    fn run(&self, argv: &[&str], _timeout: Duration) -> Result<String> {
        let argv: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
        self.calls.borrow_mut().push(argv.clone());
        if let Some(next) = self.queue.borrow_mut().pop_front() {
            return Ok(next);
        }
        (self.respond)(&argv)
    }
}

struct Target {
    root: PathBuf,
    metadata: Map<String, Value>,
    signature: String,
    attestation: String,
}

fn statement(digest: &str) -> Value {
    json!({"predicateType": "https://slsa.dev/provenance/v1",
        "subject": [{"digest": {"sha256": &digest[7..]}}],
        "predicate": {"buildDefinition": {
            "buildType": BUILD_TYPE,
            "externalParameters": {"generation": 1, "architecture": "aarch64",
                                   "hardwareProfile": "lenovo-83ed",
                                   "firmwareManifestSha256": "b".repeat(64)},
            "internalParameters": {"expectedReleaseDigest": digest},
            "resolvedDependencies": [
                {"uri": format!("git+ssh://code.tilo.so/tilo/tilo@{SOURCE}")},
                {"uri": format!("code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:{}", "c".repeat(64))}]},
            "runDetails": {"builder": {"id": BUILDER},
                           "metadata": {"invocationId": "12345"},
                           "byproducts": [{"name": "independent-rebuild",
                                           "uri": "urn:tilo:verification:manifest-digest-equal"}]}}})
}

fn write_metadata(root: &Path, metadata: &Map<String, Value>) {
    fs::write(
        root.join("target.json"),
        Value::Object(metadata.clone()).to_string(),
    )
    .unwrap();
}

fn fixture(dir: &Path) -> Target {
    let root = dir.join("target-oci");
    let layout = root.join("oci");
    let blobs = layout.join("blobs/sha256");
    fs::create_dir_all(&blobs).unwrap();
    let config =
        br#"{"architecture":"arm64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}"#;
    let config_digest = sha256(config);
    fs::write(blobs.join(&config_digest), config).unwrap();
    let manifest = serde_json::to_vec(&json!({"schemaVersion": 2, "config": {
        "mediaType": "application/vnd.oci.image.config.v1+json",
        "digest": format!("sha256:{config_digest}"), "size": config.len()},
        "layers": []}))
    .unwrap();
    let digest = format!("sha256:{}", sha256(&manifest));
    fs::write(blobs.join(&digest[7..]), &manifest).unwrap();
    fs::write(
        layout.join("oci-layout"),
        r#"{"imageLayoutVersion":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        layout.join("index.json"),
        json!({"schemaVersion": 2, "manifests": [{"mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": digest, "size": manifest.len(),
            "annotations": {"kind": "dev.cosignproject.cosign/image"}}]})
        .to_string(),
    )
    .unwrap();
    let metadata = json!({"schema": "tilo.installer-offline-target.v1",
        "target_ref": format!("code.tilo.so/tilo/tilo@{digest}"),
        "target_digest": digest, "architecture": "aarch64",
        "hardware_profile": "lenovo-83ed", "update_origin": update()});
    let metadata = metadata.as_object().unwrap().clone();
    write_metadata(&root, &metadata);
    let signature = json!([{"critical": {
        "identity": {"docker-reference": "code.tilo.so/tilo/tilo"},
        "image": {"docker-manifest-digest": digest}}}])
    .to_string();
    let attestation =
        json!([{"payload": b64(statement(&digest).to_string().as_bytes())}]).to_string();
    Target {
        root,
        metadata,
        signature,
        attestation,
    }
}

fn digest_of(metadata: &Map<String, Value>) -> String {
    metadata["target_digest"].as_str().unwrap().to_owned()
}

#[test]
fn direct_import_preserves_digest_and_uses_immutable_id() {
    let dir = tempfile::tempdir().unwrap();
    let target = fixture(dir.path());
    let mut host = Fixture::new(dir.path());
    fs::create_dir(dir.path().join("run")).unwrap();
    fs::write(
        host.receipt(),
        Value::Object(target.metadata.clone()).to_string(),
    )
    .unwrap();
    let image_id = format!("sha256:{}", "e".repeat(64));
    let digest = digest_of(&target.metadata);
    let (id, expected_digest) = (image_id.clone(), digest);
    host.respond = Box::new(move |argv| {
        if argv[0] == "skopeo" {
            assert_eq!(
                argv[1..4],
                ["--insecure-policy", "copy", "--preserve-digests"]
            );
            let view = PathBuf::from(
                argv[4]
                    .strip_prefix("oci:")
                    .unwrap()
                    .strip_suffix(":target")
                    .unwrap(),
            );
            assert!(view.join("blobs").is_symlink());
            let index: Value =
                serde_json::from_str(&fs::read_to_string(view.join("index.json")).unwrap())
                    .unwrap();
            assert_eq!(index["manifests"].as_array().unwrap().len(), 1);
            assert_eq!(
                index["manifests"][0]["annotations"]["org.opencontainers.image.ref.name"],
                "target"
            );
            assert_eq!(
                argv[5],
                format!(
                    "containers-storage:localhost/tilo-offline-target:{}",
                    &expected_digest[7..]
                )
            );
            return Ok(String::new());
        }
        if argv.iter().any(|word| word == "{{.Id}}") {
            return Ok(id.clone());
        }
        if argv.iter().any(|word| word == "{{.Digest}}") {
            assert_eq!(argv.last().unwrap(), &id);
            return Ok(expected_digest.clone());
        }
        assert_eq!(argv[0..2], ["podman", "run"]);
        assert!(argv.contains(&id));
        assert!(argv.iter().any(|word| word == "--network=none"));
        Ok(String::new())
    });
    assert_eq!(import_image(&host).unwrap(), image_id);
    assert!(
        !host
            .calls
            .borrow()
            .iter()
            .any(|argv| argv.iter().any(|w| w == "load"))
    );

    let host = Fixture {
        queue: RefCell::new(
            [
                String::new(),
                image_id,
                format!("sha256:{}", "f".repeat(64)),
            ]
            .into(),
        ),
        ..Fixture::new(dir.path())
    };
    let error = import_image(&host).unwrap_err();
    assert!(error.0.contains("manifest differs"), "{error}");
}

fn development_transport(dir: &Path, target: &mut Target) {
    let blobs = target.root.join("oci/blobs/sha256");
    let blob = |data: Value| {
        let raw = serde_json::to_vec(&data).unwrap();
        let digest = format!("sha256:{}", sha256(&raw));
        fs::write(blobs.join(&digest[7..]), &raw).unwrap();
        json!({"digest": digest, "size": raw.len()})
    };
    let config = blob(
        json!({"architecture": "arm64", "os": "linux", "config": {"Labels": {
        "sh.tilo.image.flavour": "dev", "org.opencontainers.image.revision": SOURCE}}}),
    );
    let layer = blob(json!({"fixture": "payload"}));
    let mut manifest = blob(json!({"schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": config, "layers": [layer]}));
    let keep: Vec<String> = [&config, &layer, &manifest]
        .iter()
        .map(|item| item["digest"].as_str().unwrap()[7..].to_owned())
        .collect();
    for entry in fs::read_dir(&blobs).unwrap() {
        let entry = entry.unwrap();
        if !keep.contains(&entry.file_name().to_string_lossy().into_owned()) {
            fs::remove_file(entry.path()).unwrap();
        }
    }
    manifest["mediaType"] = json!("application/vnd.oci.image.manifest.v1+json");
    manifest["annotations"] = json!({"org.opencontainers.image.ref.name": "target"});
    let digest = manifest["digest"].as_str().unwrap().to_owned();
    fs::write(
        target.root.join("oci/index.json"),
        json!({"schemaVersion": 2, "manifests": [manifest]}).to_string(),
    )
    .unwrap();
    let metadata = &mut target.metadata;
    metadata.insert("schema".into(), json!("tilo.installer-dev-target.v1"));
    metadata.insert("target_digest".into(), json!(digest));
    metadata.insert(
        "target_ref".into(),
        json!(format!("registry.tilo.so/tilo/tilo-vm@{digest}")),
    );
    metadata.insert(
        "update_origin".into(),
        json!("registry.tilo.so/tilo/tilo-vm:fixture-lenovo-83ed-ci"),
    );
    metadata.insert("source_revision".into(), json!(SOURCE));
    metadata.insert("ci_run_id".into(), json!("75356"));
    write_metadata(&target.root, metadata);
    let _ = dir;
}

#[test]
fn development_transport_requires_the_vm_gate() {
    let dir = tempfile::tempdir().unwrap();
    let mut target = fixture(dir.path());
    development_transport(dir.path(), &mut target);
    let host = Fixture::new(dir.path());
    let error = read_target(&host, &target.root).unwrap_err();
    assert!(error.0.contains("development VM installer"), "{error}");
    fs::write(dir.path().join("vm-marker"), "VM-only").unwrap();
    fs::write(dir.path().join("flavour"), "dev\n").unwrap();
    let mut host = Fixture::new(dir.path());
    host.proof_port = true;
    host.respond = Box::new(|_| Ok(String::new()));
    let (selected, _) = read_target(&host, &target.root).unwrap();
    assert_eq!(selected, target.metadata);
    assert_eq!(
        *host.calls.borrow(),
        [vec!["systemd-detect-virt", "--vm", "--quiet"]]
    );
    fs::write(dir.path().join("flavour"), "production\n").unwrap();
    let error = read_target(&host, &target.root).unwrap_err();
    assert!(error.0.contains("development VM installer"), "{error}");
}

#[test]
fn development_import_validates_boot_layout_without_changing_image() {
    let dir = tempfile::tempdir().unwrap();
    let mut target = fixture(dir.path());
    development_transport(dir.path(), &mut target);
    fs::write(dir.path().join("vm-marker"), "VM-only").unwrap();
    fs::write(dir.path().join("flavour"), "dev\n").unwrap();
    fs::create_dir(dir.path().join("run")).unwrap();
    let mounted = dir.path().join("mounted");
    fs::create_dir(&mounted).unwrap();
    let image_id = format!("sha256:{}", "e".repeat(64));
    let digest = digest_of(&target.metadata);
    let mut host = Fixture::new(dir.path());
    host.proof_port = true;
    fs::write(
        host.receipt(),
        Value::Object(target.metadata.clone()).to_string(),
    )
    .unwrap();
    let (id, mount) = (image_id.clone(), mounted.to_string_lossy().into_owned());
    host.respond = Box::new(move |argv| {
        if argv.iter().any(|word| word == "{{.Id}}") {
            return Ok(id.clone());
        }
        if argv.iter().any(|word| word == "{{.Digest}}") {
            return Ok(digest.clone());
        }
        if argv[..3] == ["podman", "image", "mount"] {
            return Ok(mount.clone());
        }
        Ok(String::new())
    });
    assert_eq!(import_image(&host).unwrap(), image_id);
    let calls = host.calls.borrow();
    let validate: Vec<String> = [
        "python3",
        "/usr/libexec/tilo-installer-dev-target-uki.py",
        "validate-image",
        "--image-root",
        &mounted.to_string_lossy(),
    ]
    .iter()
    .map(|word| (*word).to_owned())
    .collect();
    assert!(calls.contains(&validate), "{calls:?}");
    assert!(calls.contains(&vec![
        "podman".to_owned(),
        "image".into(),
        "unmount".into(),
        image_id
    ]));
    assert!(!calls.iter().any(|argv| {
        argv.iter()
            .any(|word| word == "commit" || word == "build" || word == "push")
    }));
}

#[test]
fn command_failure_retains_exit_status_without_arguments() {
    struct Real;
    impl Host for Real {}
    let failed = Real
        .run(
            &["sh", "-c", "exit 125", "private-argument"],
            Duration::from_secs(5),
        )
        .unwrap_err();
    assert_eq!(failed.0, "sh failed (exit 125): no diagnostic stderr");
    let signalled = Real
        .run(
            &["sh", "-c", "kill -9 $$", "private-argument"],
            Duration::from_secs(5),
        )
        .unwrap_err();
    assert!(signalled.0.starts_with("sh failed (exit -"), "{signalled}");
    assert!(!signalled.0.contains("private-argument"));
    let stderr = Real
        .run(
            &["sh", "-c", "echo '  why  ' >&2; exit 3"],
            Duration::from_secs(5),
        )
        .unwrap_err();
    assert_eq!(stderr.0, "sh failed (exit 3): why");
}

#[test]
fn manifest_signature_and_slsa_bind_selected_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = fixture(dir.path());
    fs::write(dir.path().join("cosign.pub"), "test public key").unwrap();
    fs::write(dir.path().join("cosign"), "test").unwrap();
    let host = Fixture {
        queue: RefCell::new([target.signature.clone(), target.attestation.clone()].into()),
        ..Fixture::new(dir.path())
    };
    let result = verify(&host).unwrap();
    assert_eq!(result["target_digest"], target.metadata["target_digest"]);
    assert_eq!(result["provenance"]["source_revision"], SOURCE);
    assert_eq!(result["provenance"]["invocation_id"], "12345");
    assert_eq!(result["provenance"]["generation"], 1);
    assert_eq!(result["verifier_key_sha256"], sha256(b"test public key"));
    let receipt: Value =
        serde_json::from_str(&fs::read_to_string(host.receipt()).unwrap()).unwrap();
    assert_eq!(receipt["update_origin"], update());
    let calls = host.calls.borrow();
    assert_eq!(calls[0][1..4], ["verify", "--offline", "--key"]);
    assert_eq!(calls[1][1..3], ["verify-attestation", "--offline"]);
}

#[test]
fn generic_child_binds_signed_generation_origin() {
    let dir = tempfile::tempdir().unwrap();
    let mut target = fixture(dir.path());
    let digest = digest_of(&target.metadata);
    target
        .metadata
        .insert("hardware_profile".into(), json!("generic"));
    target.metadata.insert(
        "target_ref".into(),
        json!(format!("registry.tilo.so/tilo/tilo@{digest}")),
    );
    target.metadata.insert(
        "update_origin".into(),
        json!("registry.tilo.so/tilo/tilo:gen2"),
    );
    write_metadata(&target.root, &target.metadata);
    read_target(&Fixture::new(dir.path()), &target.root).unwrap();
    let mut statement = statement(&digest);
    let definition = &mut statement["predicate"]["buildDefinition"];
    definition["externalParameters"] = json!({"generation": 2});
    definition["resolvedDependencies"]
        .as_array_mut()
        .unwrap()
        .pop();
    let signed = json!([{"payload": b64(statement.to_string().as_bytes())}]).to_string();
    let provenance = checked_provenance(&signed, &target.metadata).unwrap();
    assert_eq!(provenance["generation"], 2);
    assert_eq!(provenance["firmware_candidate"], Value::Null);
    target.metadata.insert(
        "update_origin".into(),
        json!("registry.tilo.so/tilo/tilo:gen3"),
    );
    let error = checked_provenance(&signed, &target.metadata).unwrap_err();
    assert!(error.0.contains("SLSA"), "{error}");
}

#[test]
fn tampered_manifest_or_origin_fails() {
    let dir = tempfile::tempdir().unwrap();
    let mut target = fixture(dir.path());
    let host = Fixture::new(dir.path());
    let blob = target
        .root
        .join("oci/blobs/sha256")
        .join(&digest_of(&target.metadata)[7..]);
    let original = fs::read(&blob).unwrap();
    fs::write(&blob, [original.as_slice(), b"x"].concat()).unwrap();
    let error = read_target(&host, &target.root).unwrap_err();
    assert!(error.0.contains("manifest differs"), "{error}");
    fs::write(&blob, &original).unwrap();
    let mut origin = update();
    origin.pop();
    origin.push('b');
    target
        .metadata
        .insert("update_origin".into(), json!(origin));
    let error = checked_provenance(&target.attestation, &target.metadata).unwrap_err();
    assert!(error.0.contains("SLSA"), "{error}");
}

#[test]
fn ambiguous_or_unsigned_target_fails() {
    let dir = tempfile::tempdir().unwrap();
    let target = fixture(dir.path());
    let index_path = target.root.join("oci/index.json");
    let mut index: Value = serde_json::from_str(&fs::read_to_string(&index_path).unwrap()).unwrap();
    let first = index["manifests"][0].clone();
    index["manifests"].as_array_mut().unwrap().push(first);
    fs::write(&index_path, index.to_string()).unwrap();
    let error = read_target(&Fixture::new(dir.path()), &target.root).unwrap_err();
    assert!(error.0.contains("unique target"), "{error}");
    index["manifests"].as_array_mut().unwrap().pop();
    fs::write(&index_path, index.to_string()).unwrap();
    fs::write(dir.path().join("cosign.pub"), "test public key").unwrap();
    fs::write(dir.path().join("cosign"), "test").unwrap();
    let wrong = json!([{"critical": {"image": {
        "docker-manifest-digest": format!("sha256:{}", "f".repeat(64))}}}])
    .to_string();
    let mut host = Fixture::new(dir.path());
    host.respond = Box::new(move |_| Ok(wrong.clone()));
    let error = verify(&host).unwrap_err();
    assert!(error.0.contains("signature does not bind"), "{error}");
    assert!(!host.receipt().exists());
}

#[test]
fn tampered_config_fails_before_disk_work() {
    let dir = tempfile::tempdir().unwrap();
    let target = fixture(dir.path());
    let blobs = target.root.join("oci/blobs/sha256");
    let manifest: Value =
        serde_json::from_slice(&fs::read(blobs.join(&digest_of(&target.metadata)[7..])).unwrap())
            .unwrap();
    let config_path = blobs.join(&manifest["config"]["digest"].as_str().unwrap()[7..]);
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["architecture"] = json!("amd64");
    fs::write(&config_path, config.to_string()).unwrap();
    let error = read_target(&Fixture::new(dir.path()), &target.root).unwrap_err();
    assert!(error.0.contains("config differs"), "{error}");
}

#[test]
fn import_rejects_metadata_changed_after_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let mut target = fixture(dir.path());
    let host = Fixture::new(dir.path());
    fs::create_dir(dir.path().join("run")).unwrap();
    fs::write(
        host.receipt(),
        Value::Object(target.metadata.clone()).to_string(),
    )
    .unwrap();
    let mut origin = update();
    origin.pop();
    origin.push('b');
    target
        .metadata
        .insert("update_origin".into(), json!(origin));
    write_metadata(&target.root, &target.metadata);
    // The changed origin no longer names a supported child, or differs from
    // the receipt: either way nothing runs.
    let error = import_image(&host).unwrap_err();
    assert!(
        error.0.contains("changed after verified preflight"),
        "{error}"
    );
    assert!(host.calls.borrow().is_empty());
}

#[test]
fn the_installer_runs_the_compiled_helper() {
    let backend = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../os/usr/libexec/tilo-install"),
    )
    .unwrap();
    assert!(backend.contains("LOCAL_IMG=\"$(/usr/libexec/tilo-installer-offline-target import)\""));
    assert!(backend.contains("/usr/libexec/tilo-installer-offline-target verify >/dev/null"));
    assert!(!backend.contains("tilo-installer-offline-target.py"));
    let _ = Error::new("");
}
