//! Authentication of a separate offline installed image.

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
use tilo_install_helpers::Result;
use tilo_install_helpers::offline_target::{
    BUILD_TYPE, BUILDER, Host, checked_provenance, import_image, read_target, receipt_facts, verify,
};

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

type Responder = Box<dyn Fn(&[String]) -> Result<String>>;

/// A fixture installer: paths in a temporary directory, tool calls answered by
/// `respond` or a queue, and every call recorded.
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
        "repository": "code.tilo.so/tilo/tilo",
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

fn development_transport(dir: &Path, target: &mut Target) {
    development_transport_of(dir, target, "dev", SOURCE, SOURCE);
}

/// A development transport of an image of FLAVOUR whose revision label is LABEL, its
/// target.json naming SOURCE.
fn development_transport_of(
    dir: &Path,
    target: &mut Target,
    flavour: &str,
    label: &str,
    source: &str,
) {
    let blobs = target.root.join("oci/blobs/sha256");
    let blob = |data: Value| {
        let raw = serde_json::to_vec(&data).unwrap();
        let digest = format!("sha256:{}", sha256(&raw));
        fs::write(blobs.join(&digest[7..]), &raw).unwrap();
        json!({"digest": digest, "size": raw.len()})
    };
    let config = blob(
        json!({"architecture": "arm64", "os": "linux", "config": {"Labels": {
        "sh.tilo.image.flavour": flavour, "org.opencontainers.image.revision": label}}}),
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
    metadata.insert("repository".into(), json!("registry.tilo.so/tilo/tilo-vm"));
    metadata.insert(
        "update_origin".into(),
        json!("registry.tilo.so/tilo/tilo-vm:fixture-lenovo-83ed-ci"),
    );
    metadata.insert("source_revision".into(), json!(source));
    metadata.insert("ci_run_id".into(), json!("75356"));
    write_metadata(&target.root, metadata);
    let _ = dir;
}

// The preflight a person's installer runs: the signature, the SLSA attestation and
// the OCI layout must bind the selected target, or nothing is written.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the verified target and every refusal it has are one flow"
)]
fn an_offline_target_is_authenticated() {
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

    // A generic child binds the signed generation and origin, and only that origin.
    let dir = tempfile::tempdir().unwrap();
    let mut target = fixture(dir.path());
    let digest = digest_of(&target.metadata);
    target
        .metadata
        .insert("hardware_profile".into(), json!("generic"));
    target
        .metadata
        .insert("repository".into(), json!("registry.tilo.so/tilo/tilo"));
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

    // A tampered manifest, then an origin the attestation does not name.
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

    // An ambiguous index, then a signature over another digest.
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

    // A tampered config is refused before any disk work.
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

// The import a person's installer runs: a verified target is copied without
// changing its digest, named by its immutable ID and pointed at its origin.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the verified target and every refusal it has are one flow"
)]
fn an_offline_target_is_imported() {
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
        assert!(argv.iter().any(|word| word == "--network=none"));
        let origin = update();
        assert_eq!(
            argv[argv.len() - 5..],
            [
                "--entrypoint",
                "/usr/libexec/tilo-board-successor",
                id.as_str(),
                "install-update-origin",
                origin.as_str(),
            ]
        );
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
    // The digest the copied image reports must be the verified one.
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

    // The receipt names the verified origin and target kind; anything else is refused.
    let dir = tempfile::tempdir().unwrap();
    let host = Fixture::new(dir.path());
    let error = receipt_facts(&host).unwrap_err();
    assert!(error.0.contains("no verified preflight"), "{error}");
    fs::create_dir(dir.path().join("run")).unwrap();
    let mut target = fixture(dir.path());
    let write = |metadata: &Map<String, Value>| {
        fs::write(host.receipt(), Value::Object(metadata.clone()).to_string()).unwrap();
    };
    write(&target.metadata);
    assert_eq!(
        receipt_facts(&host).unwrap(),
        format!("UPDATE_ORIGIN={}\nDEVELOPMENT=0", update())
    );
    development_transport(dir.path(), &mut target);
    write(&target.metadata);
    assert!(receipt_facts(&host).unwrap().ends_with("\nDEVELOPMENT=1"));
    for origin in [Value::Null, json!(""), json!("a\nDEVELOPMENT=1")] {
        let mut metadata = target.metadata.clone();
        metadata.insert("update_origin".into(), origin);
        write(&metadata);
        assert!(receipt_facts(&host).is_err());
    }

    // Metadata changed after the verified preflight stops the import before any tool.
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
    let error = import_image(&host).unwrap_err();
    assert!(
        error.0.contains("changed after verified preflight"),
        "{error}"
    );
    assert!(host.calls.borrow().is_empty());
}

// The development target must be the opted-in scenario VM's, and its transport
// must name the commit the image was built from.
#[test]
fn the_development_transport_requires_the_vm_check() {
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

    // A server image names no commit; a development image must name its own.
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("vm-marker"), "VM-only").unwrap();
    fs::write(dir.path().join("flavour"), "dev\n").unwrap();
    let mut installer = Fixture::new(dir.path());
    installer.proof_port = true;
    installer.respond = Box::new(|_| Ok(String::new()));
    let mut target = fixture(dir.path());
    development_transport_of(dir.path(), &mut target, "server", "", "");
    let (selected, _) = read_target(&installer, &target.root).unwrap();
    assert_eq!(selected["source_revision"], "");
    let mut target = fixture(dir.path());
    development_transport_of(dir.path(), &mut target, "dev", "", "");
    let error = read_target(&installer, &target.root).unwrap_err();
    assert!(error.0.contains("flavour label differs"), "{error}");
    let mut target = fixture(dir.path());
    development_transport_of(dir.path(), &mut target, "dev", SOURCE, "");
    let error = read_target(&installer, &target.root).unwrap_err();
    assert!(error.0.contains("flavour label differs"), "{error}");
}

// A development import validates the image's boot layout and changes nothing in it.
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
        "/usr/libexec/tilo-installer-offline-target",
        "dev-target-uki",
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

// A failed tool reports its status without repeating caller arguments.
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
