//! `qos-install run <pkg> -- <argv...>` forwards trailing args to the
//! spawned launcher verbatim.
//!
//! Uses a bash-script `qos-launcher` shim pinned via `QOS_LAUNCHER_PATH`
//! that records its own argv to a file, so the assertion can compare
//! the tail against the arguments the test passed in.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use pkg::blobs::{BlobHash, BlobStore, MemoryBlobStore};
use pkg::signed::SignedRoot;
use pkg::{Manifest, SigningIdentity, pack};
use rand::rngs::OsRng;
use serde::Serialize;

fn install_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_qos-install"))
}

const NOTES_LIKE_MANIFEST: &str = r#"
[package]
id          = "dev.qos.notes-cli"
version     = "0.1.0"
kind        = "app"

[publisher]
identity    = "ed25519:<HEX>"
signature   = ""

[sandbox]
namespaces  = ["user", "pid", "uts", "ipc", "mount", "net"]
seccomp     = "qos-default"
landlock    = "strict"

[sandbox.fs]
read  = []
write = []

[sandbox.net]
mode = "off"

[entrypoint]
bin  = "bin/notes"
type = "cli"

[cli]
commands = ["notes"]
"#;

fn write_payload(dir: &Path) {
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::write(dir.join("bin/notes"), b"#!/notreal").unwrap();
}

fn build_qpkg(qpkg_path: &Path, publisher_sk: &SigningKey, payload_dir: &Path) {
    let pub_hex = hex::encode(publisher_sk.verifying_key().to_bytes());
    let src = NOTES_LIKE_MANIFEST.replace("<HEX>", &pub_hex);
    let manifest = Manifest::from_toml_str(&src).expect("decode manifest");

    let mut store = MemoryBlobStore::new();
    let identity = SigningIdentity::from_signing_key(publisher_sk.clone());
    let id = pack(payload_dir, &manifest, &identity, &mut store).expect("pack");

    #[derive(Serialize)]
    struct QpkgHeader {
        package_id: String,
        format: u32,
    }
    let header = QpkgHeader {
        package_id: id.to_string(),
        format: 1,
    };
    let header_toml = toml::to_string(&header).unwrap();

    let signed_bytes = store.get(&BlobHash(*id.as_bytes())).unwrap().unwrap();
    let signed = SignedRoot::decode(&signed_bytes).unwrap();
    let manifest_bytes = store.get(&signed.manifest_hash).unwrap().unwrap();
    let collection_bytes = store.get(&signed.payload_root_hash).unwrap().unwrap();

    let file = fs::File::create(qpkg_path).unwrap();
    let mut tar = tar::Builder::new(file);
    append(&mut tar, "qpkg.toml", header_toml.as_bytes());
    append(&mut tar, "signed-root", &signed_bytes);
    append(&mut tar, "manifest.toml", &manifest_bytes);
    append(&mut tar, "collection.qcl", &collection_bytes);
    for (h, b) in store.iter() {
        let name = format!("blobs/{}", h);
        append(&mut tar, &name, b);
    }
    tar.finish().unwrap();
}

fn append(tar: &mut tar::Builder<fs::File>, name: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_path(name).unwrap();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append(&header, bytes).unwrap();
}

/// Write a bash shim that stashes its own argv (one line per arg) into
/// `record`, so the test can compare what qos-install actually forwards.
fn write_launcher_shim(shim_path: &Path, record: &Path) {
    let script = format!(
        "#!/bin/bash\nfor a in \"$@\"; do echo \"$a\" >> {}; done\nexit 0\n",
        record.display()
    );
    fs::write(shim_path, script).unwrap();
    let mut perm = fs::metadata(shim_path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(shim_path, perm).unwrap();
}

#[test]
fn run_forwards_trailing_argv_to_launcher() {
    let home = tempfile::tempdir().unwrap();
    let bin_dir = tempfile::tempdir().unwrap();
    let record = bin_dir.path().join("record.txt");
    let shim = bin_dir.path().join("qos-launcher");
    write_launcher_shim(&shim, &record);

    // Build + install a .qpkg — self-signed, no attestor.
    let mut rng = OsRng;
    let publisher_sk = SigningKey::generate(&mut rng);
    let payload_dir = tempfile::tempdir().unwrap();
    write_payload(payload_dir.path());
    let qpkg_path = home.path().join("app.qpkg");
    build_qpkg(&qpkg_path, &publisher_sk, payload_dir.path());

    let out = Command::new(install_bin())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .arg("install")
        .arg(&qpkg_path)
        .output()
        .expect("spawn qos-install install");
    assert!(
        out.status.success(),
        "install failed: {} / {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Now `qos-install run <pkg> -- create hello world` must reach the
    // shim's argv verbatim after the `--` (leading argv the shim also
    // sees is `run --manifest <p> --payload-root <p> --`).
    let out = Command::new(install_bin())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("QOS_LAUNCHER_PATH", &shim)
        .args(["run", "dev.qos.notes-cli", "--", "create", "hello world"])
        .output()
        .expect("spawn qos-install run");
    assert!(
        out.status.success(),
        "run failed: {} / {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let text = fs::read_to_string(&record).expect("record.txt written by shim");
    let lines: Vec<&str> = text.lines().collect();
    // Argv the shim received: run --manifest <p> --payload-root <p> -- create "hello world"
    assert!(
        lines.first() == Some(&"run"),
        "first arg to launcher must be 'run', got {:?}",
        lines.first(),
    );
    let dash_idx = lines
        .iter()
        .position(|l| *l == "--")
        .expect("`--` separator must be present in forwarded argv");
    let tail: Vec<&str> = lines[dash_idx + 1..].to_vec();
    assert_eq!(
        tail,
        vec!["create", "hello world"],
        "passthrough argv must reach the launcher verbatim after `--`",
    );
}
