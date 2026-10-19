//! Signed layers through the production activation: a development machine's
//! tilo-vm layers reach `/usr` only when an authority the owner enrolled
//! signed them and the tree on disk is exactly what it signed.
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "a test: any I/O error fails it"
)]

mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use serde_json::Value;
use support::Machine;
use tilo_extension::signed;
use tilo_layer_trust::manifest::{content_sha256, tilo_vm_extension_release};
use tilo_layer_trust::{LayerManifest, MANIFEST_TYPE, SCHEMA, SecretKey, describe};

const DEV: [u8; 32] = [3; 32];
const STRANGER: [u8; 32] = [4; 32];

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn mkdir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    chmod(path, 0o755);
}

fn write(path: &Path, bytes: &[u8], mode: u32) {
    mkdir(path.parent().unwrap());
    fs::write(path, bytes).unwrap();
    chmod(path, mode);
}

/// Enroll `key` as the authority `name`, as `tilo-vm trust enroll` does.
fn enroll(machine: &Machine, name: &str, key: &SecretKey) {
    let dir = signed::authorities_dir(&machine.layout);
    mkdir(&dir);
    write(
        &dir.join(format!("{name}.pub")),
        key.public().to_pem().as_bytes(),
        0o644,
    );
}

/// A tilo-vm layer in tilo-vmd's store, with the extension-release it writes.
fn layer(machine: &Machine, component: &str, binary: &[u8]) -> (PathBuf, LayerManifest) {
    let staging = machine.root.join("var/lib/tilo-vm/staging/one");
    write(&staging.join("usr/bin").join(component), binary, 0o755);
    write(
        &staging
            .join("usr/share/tilo")
            .join(format!("{component}.toml")),
        b"k = 1\n",
        0o644,
    );
    write(&staging.join("provenance.json"), b"{}\n", 0o644);
    let entries = describe(&staging).unwrap();
    let hash = content_sha256(&entries);
    let release = tilo_vm_extension_release(component);
    write(
        &staging.join(format!(
            "usr/lib/extension-release.d/extension-release.{hash}"
        )),
        release.as_bytes(),
        0o644,
    );
    let store = machine.root.join("var/lib/tilo-vm/layers");
    mkdir(&store);
    let path = store.join(&hash);
    fs::rename(&staging, &path).unwrap();
    let manifest = LayerManifest {
        kind: MANIFEST_TYPE.into(),
        schema: SCHEMA,
        name: hash.clone(),
        component: component.into(),
        revision: "1a2b3c4".into(),
        content_sha256: hash,
        generation: 7,
        base_image_digest: Some(format!("sha256:{}", "e".repeat(64))),
        extension_release: release,
        entries,
    };
    (path, manifest)
}

/// Put the layer in the desired set, signed by `key` (unsigned without one).
fn desire(machine: &Machine, path: &Path, manifest: &LayerManifest, key: Option<&SecretKey>) {
    let entry = signed::desired_dir(&machine.layout).join(&manifest.name);
    mkdir(&entry);
    let bytes = manifest.to_bytes().unwrap();
    write(&entry.join("manifest.json"), &bytes, 0o644);
    if let Some(key) = key {
        write(
            &entry.join("manifest.json.sig"),
            key.sign(&bytes).as_bytes(),
            0o644,
        );
    }
    symlink(path, entry.join("payload")).unwrap();
}

fn status(machine: &Machine) -> Value {
    Value::Object(signed::read_status(&machine.layout))
}

fn refusal(machine: &Machine, name: &str) -> String {
    status(machine)["refused"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .map_or_else(
            || panic!("{name} was not refused: {}", status(machine)),
            |r| r["reason"].as_str().unwrap().to_owned(),
        )
}

fn merged(machine: &Machine) -> Vec<String> {
    machine.host.merged.borrow().clone()
}

/// Boot: /run is a fresh tmpfs.
fn reboot(machine: &Machine) {
    let _ = fs::remove_dir_all(machine.root.join("run"));
    machine.host.merged.borrow_mut().clear();
}

#[test]
fn a_signed_layer_by_an_enrolled_authority_merges_and_stays_merged_across_a_reboot() {
    let machine = Machine::new();
    let dev = SecretKey::from_seed(&DEV);
    enroll(&machine, "dev", &dev);
    let (path, manifest) = layer(&machine, "shell", b"shell v1");
    desire(&machine, &path, &manifest, Some(&dev));

    assert_eq!(machine.run("activate"), 0, "{}", status(&machine));
    assert_eq!(merged(&machine), std::slice::from_ref(&manifest.name));
    let link = signed::link(&machine.layout, &manifest.name);
    assert_eq!(
        fs::read_link(&link).unwrap(),
        fs::canonicalize(&path).unwrap()
    );
    let record = &status(&machine)["merged"][0];
    assert_eq!(record["component"], "shell");
    assert_eq!(record["authority"], "dev");
    assert_eq!(record["fingerprint"], dev.public().fingerprint());

    // Nothing in /run survives a boot; the activation at boot verifies the
    // desired set again and merges it.
    reboot(&machine);
    assert!(fs::symlink_metadata(&link).is_err());
    assert_eq!(machine.run("activate"), 0, "{}", status(&machine));
    assert_eq!(merged(&machine), [manifest.name]);
}

#[test]
fn a_tampered_layer_is_refused_and_the_others_still_merge() {
    let machine = Machine::new();
    let dev = SecretKey::from_seed(&DEV);
    enroll(&machine, "dev", &dev);
    let (shell, shell_manifest) = layer(&machine, "shell", b"shell v1");
    desire(&machine, &shell, &shell_manifest, Some(&dev));
    let (launcher, launcher_manifest) = layer(&machine, "launcher", b"launcher v1");
    desire(&machine, &launcher, &launcher_manifest, Some(&dev));
    assert_eq!(machine.run("activate"), 0, "{}", status(&machine));

    // The binary replaced behind the activation's back (a sealed file cannot
    // be written in place; replacing it is what tampering looks like).
    let binary = shell.join("usr/bin/shell");
    fs::remove_file(&binary).unwrap();
    write(&binary, b"shell, changed", 0o755);
    reboot(&machine);

    assert_eq!(machine.run("activate"), 1, "a refusal fails the unit");
    let reason = refusal(&machine, &shell_manifest.name);
    assert!(reason.contains("usr/bin/shell"), "{reason}");
    assert_eq!(
        merged(&machine),
        std::slice::from_ref(&launcher_manifest.name),
        "the untouched layer merges"
    );
    assert!(fs::symlink_metadata(signed::link(&machine.layout, &shell_manifest.name)).is_err());

    // A file added to a layer is as much a change.
    write(&launcher.join("usr/bin/extra"), b"x", 0o755);
    assert_eq!(machine.run("activate"), 1);
    assert!(refusal(&machine, &launcher_manifest.name).contains("usr/bin/extra"));
    assert!(merged(&machine).is_empty());
}

#[test]
fn an_unsigned_layer_is_refused() {
    let machine = Machine::new();
    let dev = SecretKey::from_seed(&DEV);
    enroll(&machine, "dev", &dev);
    let (path, manifest) = layer(&machine, "shell", b"shell v1");
    desire(&machine, &path, &manifest, None);

    assert_eq!(machine.run("activate"), 1);
    assert!(refusal(&machine, &manifest.name).contains("unsigned"));
    assert!(merged(&machine).is_empty());

    // Signed, but the manifest changed afterwards.
    let entry = signed::desired_dir(&machine.layout).join(&manifest.name);
    write(
        &entry.join("manifest.json.sig"),
        dev.sign(&manifest.to_bytes().unwrap()).as_bytes(),
        0o644,
    );
    let mut changed = manifest.clone();
    changed.revision = "something else".into();
    write(
        &entry.join("manifest.json"),
        &changed.to_bytes().unwrap(),
        0o644,
    );
    assert_eq!(machine.run("activate"), 1);
    assert!(refusal(&machine, &manifest.name).contains("not signed by an enrolled"));
    assert!(merged(&machine).is_empty());
}

#[test]
fn a_layer_signed_by_an_unenrolled_key_is_refused() {
    let machine = Machine::new();
    let (path, manifest) = layer(&machine, "shell", b"shell v1");
    let stranger = SecretKey::from_seed(&STRANGER);
    desire(&machine, &path, &manifest, Some(&stranger));

    // No authority at all, as on a production device.
    assert_eq!(machine.run("activate"), 1);
    assert!(refusal(&machine, &manifest.name).contains("no extension authority is enrolled"));
    assert!(merged(&machine).is_empty());

    // Another key enrolled: still not this signer.
    let dev = SecretKey::from_seed(&DEV);
    enroll(&machine, "dev", &dev);
    assert_eq!(machine.run("activate"), 1);
    let reason = refusal(&machine, &manifest.name);
    assert!(
        reason.contains("not signed by an enrolled")
            && reason.contains(&dev.public().fingerprint()),
        "{reason}"
    );
    assert!(merged(&machine).is_empty());

    // A key file anyone can write is no authority either.
    let loose = signed::authorities_dir(&machine.layout).join("stranger.pub");
    write(&loose, stranger.public().to_pem().as_bytes(), 0o666);
    assert_eq!(machine.run("activate"), 1);
    assert!(merged(&machine).is_empty());
    assert!(
        status(&machine)["problems"][0]
            .as_str()
            .unwrap()
            .contains("stranger.pub")
    );

    // Enrolled properly, the same signature is accepted.
    chmod(&loose, 0o644);
    assert_eq!(machine.run("activate"), 0, "{}", status(&machine));
    assert_eq!(merged(&machine), [manifest.name]);
}

#[test]
fn an_unexpected_extension_in_the_search_path_refuses_every_signed_layer() {
    let machine = Machine::new();
    let dev = SecretKey::from_seed(&DEV);
    enroll(&machine, "dev", &dev);
    let (path, manifest) = layer(&machine, "shell", b"shell v1");
    desire(&machine, &path, &manifest, Some(&dev));
    // What tilo-vmd linked before activation verified its layers.
    mkdir(&machine.layout.extensions());
    symlink(&path, machine.layout.extensions().join(&manifest.name)).unwrap();

    assert_eq!(machine.run("activate"), 1);
    assert!(refusal(&machine, &manifest.name).contains("unexpected system extensions"));
    assert!(fs::symlink_metadata(signed::link(&machine.layout, &manifest.name)).is_err());
    assert!(merged(&machine).is_empty());
    assert!(machine.host.unmerges.get() > 0);
}

#[test]
fn a_layer_no_longer_desired_is_unmerged() {
    let machine = Machine::new();
    let dev = SecretKey::from_seed(&DEV);
    enroll(&machine, "dev", &dev);
    let (path, manifest) = layer(&machine, "shell", b"shell v1");
    desire(&machine, &path, &manifest, Some(&dev));
    assert_eq!(machine.run("activate"), 0);
    assert_eq!(merged(&machine), std::slice::from_ref(&manifest.name));

    fs::remove_dir_all(signed::desired_dir(&machine.layout).join(&manifest.name)).unwrap();
    assert_eq!(machine.run("activate"), 0);
    assert!(merged(&machine).is_empty());
    assert!(fs::symlink_metadata(signed::link(&machine.layout, &manifest.name)).is_err());
    assert!(
        !signed::status_path(&machine.layout).exists(),
        "no record of nothing"
    );
}
