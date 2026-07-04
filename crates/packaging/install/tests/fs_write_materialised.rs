//! `qos-install install` materialises the directories declared in
//! `[sandbox.fs].write` on the host, so an app doesn't hit ENOENT on
//! first launch when it tries to write to (say)
//! `~/.local/share/dev.qos.demo`.

use std::fs;
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

const DEMO_MANIFEST: &str = r#"
[package]
id          = "dev.qos.demo"
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
write = ["~/.local/share/dev.qos.demo", "$XDG_DATA_HOME/dev.qos.demo/cache", "<scope=current>/should-be-skipped"]

[sandbox.net]
mode = "off"

[entrypoint]
bin  = "bin/app"
"#;

fn write_payload(dir: &Path) {
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::write(dir.join("bin/app"), b"#!/notreal").unwrap();
}

fn build_qpkg(qpkg_path: &Path, publisher_sk: &SigningKey, payload_dir: &Path) {
    let pub_hex = hex::encode(publisher_sk.verifying_key().to_bytes());
    let src = DEMO_MANIFEST.replace("<HEX>", &pub_hex);
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

#[test]
fn install_creates_sandbox_write_paths() {
    let home = tempfile::tempdir().unwrap();
    let xdg_data = tempfile::tempdir().unwrap();

    let mut rng = OsRng;
    let publisher_sk = SigningKey::generate(&mut rng);
    let payload_dir = tempfile::tempdir().unwrap();
    write_payload(payload_dir.path());
    let qpkg_path = home.path().join("app.qpkg");
    build_qpkg(&qpkg_path, &publisher_sk, payload_dir.path());

    let target_a = home.path().join(".local/share/dev.qos.demo");
    let target_b = xdg_data.path().join("dev.qos.demo/cache");
    // Neither should exist before install.
    assert!(!target_a.exists());
    assert!(!target_b.exists());

    let out = Command::new(install_bin())
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", xdg_data.path())
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

    // Both paths that expand cleanly must have been mkdir_p'd on the host.
    assert!(
        target_a.is_dir(),
        "sandbox.fs.write path {} should have been created",
        target_a.display()
    );
    assert!(
        target_b.is_dir(),
        "sandbox.fs.write path {} should have been created",
        target_b.display()
    );

    // The <scope=current> entry should have been left alone — nothing
    // matching that literal path should live under any of the tempdirs.
    // (The most likely "leak" would be the CWD; we're not going to sweep
    // that broadly, but if `<scope=current>` had been resolved by
    // qos-install itself the create_dir_all would have named it verbatim
    // — that's what we check for.)
    let cwd = std::env::current_dir().unwrap();
    assert!(
        !cwd.join("<scope=current>").exists(),
        "should not have created a literal `<scope=current>` directory"
    );
}
