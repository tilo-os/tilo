use super::*;
use ed25519_dalek::{Signer as _, SigningKey};
use package_store::{Catalog, InstalledPackage, Layout};
use tilo_app_package::{BlobHash, Collection, SignedRoot};

const ROOM: &str = "0b7c7a52-6f7e-4a55-9a51-1c2f4b3f7e10";
const VIEWER: &str = "so.tilo.viewer";

fn manifest(id: &str, entrypoint: &str, extra: &str) -> String {
    format!(
        r#"[package]
id = "{id}"
version = "0.1.0"
kind = "app"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "probe"
type = "{entrypoint}"
{extra}"#
    )
}

/// A user store at `<base>/store`, as an install leaves it: each package's
/// payload tree beside its signed record, and one registry row each.
struct Store {
    root: PathBuf,
    registry: String,
}

impl Store {
    fn new(base: &Path) -> Self {
        let root = base.join("store");
        std::fs::create_dir_all(root.join("pkgs")).unwrap();
        Self {
            root,
            registry: String::new(),
        }
    }

    /// Install `files` as package `id` with `manifest`, signed by a fixed
    /// publisher key.
    fn install(&mut self, id: &str, manifest: &str, files: &[(&str, &[u8])]) -> InstalledPackage {
        let manifest_bytes = manifest.as_bytes().to_vec();
        let collection = Collection::from_entries(
            files
                .iter()
                .map(|(path, bytes)| ((*path).to_string(), BlobHash::of(bytes))),
        );
        let collection_bytes = collection.encode();
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut signed = SignedRoot::unsigned(
            BlobHash::of(&manifest_bytes),
            BlobHash::of(&collection_bytes),
            key.verifying_key().to_bytes(),
        );
        // The has-signature bit is part of the signed preimage.
        signed.signature = Some([0; 64]);
        signed.signature = Some(key.sign(&signed.signing_bytes()).to_bytes());
        let signed_bytes = signed.encode().to_vec();
        let id_hex = BlobHash::of(&signed_bytes).to_hex();
        let payload = self.root.join("pkgs").join(&id_hex);
        for (path, bytes) in files {
            let file = payload.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, bytes).unwrap();
        }
        std::fs::create_dir_all(&payload).unwrap();
        let meta = self.root.join("pkgs").join(format!("{id_hex}.pkg"));
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("signed-root"), &signed_bytes).unwrap();
        std::fs::write(meta.join("manifest.toml"), &manifest_bytes).unwrap();
        std::fs::write(meta.join("collection.qcl"), &collection_bytes).unwrap();
        let package = InstalledPackage {
            id: id.into(),
            version: "0.1.0".into(),
            package_id_hex: id_hex,
            payload_root: payload,
            manifest_path: meta.join("manifest.toml"),
            publisher_key_hex: hex_key(&key),
            pinned: false,
            installed_at: 0,
            last_used: 0,
            attested_by: String::new(),
            cli_commands: Vec::new(),
            runtime: String::new(),
            source: String::new(),
            held: false,
            payload_bytes: 0,
            system: false,
        };
        self.registry.push_str(&registry_row(&package));
        std::fs::write(Layout::new(&self.root).registry(), &self.registry).unwrap();
        package
    }

    fn catalog(&self) -> Catalog {
        Catalog::at(&self.root, None)
    }
}

/// `package` as a `registry.toml` row.
fn registry_row(package: &InstalledPackage) -> String {
    format!(
        "[[entry]]\nid = {:?}\nversion = {:?}\npackage_id_hex = {:?}\npayload_root = {:?}\n\
         manifest_path = {:?}\npublisher_key_hex = {:?}\n\n",
        package.id,
        package.version,
        package.package_id_hex,
        package.payload_root.display().to_string(),
        package.manifest_path.display().to_string(),
        package.publisher_key_hex,
    )
}

fn hex_key(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_bytes())
}

fn canonical_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    // Seatbelt and the identity store see resolved paths (macOS `/var`).
    let root = std::fs::canonicalize(temp.path()).unwrap();
    (temp, root)
}

fn apps(store: &Store, root: &Path, candidates: Vec<PathBuf>) -> Apps {
    Apps::new(
        Some(store.catalog()),
        root,
        ROOM,
        root.join("support"),
        candidates,
    )
}

#[test]
fn the_catalogue_offers_installed_windows_and_their_declared_access() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        VIEWER,
        &manifest(VIEWER, "gui", "\n[actions]\nquery = true\ninvoke = true\n"),
        &[("probe", b"binary")],
    );
    store.install(
        "so.tilo.tool",
        &manifest("so.tilo.tool", "cli", ""),
        &[("probe", b"binary")],
    );
    let apps = Arc::new(apps(&store, &root, Vec::new()));

    let offered = apps.applications();
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].package_id, VIEWER);
    assert_eq!(offered[0].display_name, "Viewer");

    let resolver = apps.access_resolver();
    assert_eq!(
        resolver(VIEWER),
        AppActionAccess {
            query: true,
            invoke: true
        }
    );
    assert_eq!(resolver("so.tilo.tool"), AppActionAccess::default());
    assert_eq!(resolver("so.tilo.unknown"), AppActionAccess::default());
    assert_eq!(apps.display_name("so.tilo.unknown-app"), "Unknown App");

    let err = apps.start("so.tilo.tool", ROOM).map(|_| ()).unwrap_err();
    assert!(err.contains("no window"), "{err}");
    let err = apps.start("so.tilo.unknown", ROOM).map(|_| ()).unwrap_err();
    assert!(err.contains("not installed"), "{err}");

    let empty = Apps::new(None, &root, ROOM, PathBuf::new(), Vec::new());
    assert!(empty.applications().is_empty());
}

#[test]
fn a_launch_into_a_room_this_runtime_does_not_serve_is_refused() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        VIEWER,
        &manifest(VIEWER, "gui", ""),
        &[("probe", b"binary")],
    );
    let apps = apps(&store, &root, Vec::new());
    let err = apps
        .start(VIEWER, "4f1d0f7a-9d0e-4c43-8d3b-2f6f7b8f3a21")
        .map(|_| ())
        .unwrap_err();
    assert!(err.contains("not served by this runtime"), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_linux_launch_without_tilo_launcher_is_refused() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    store.install(
        VIEWER,
        &manifest(VIEWER, "gui", ""),
        &[("probe", b"binary")],
    );
    let apps = apps(&store, &root, vec![root.join("missing/tilo-launcher")]);
    let err = apps.start(VIEWER, ROOM).map(|_| ()).unwrap_err();
    assert!(err.contains("tilo-launcher was not found"), "{err}");
    assert!(apps.launched.0.lock().unwrap().is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn a_payload_that_does_not_verify_is_refused() {
    let (_temp, root) = canonical_temp();
    let mut store = Store::new(&root);
    let package = store.install(
        VIEWER,
        &manifest(VIEWER, "gui", ""),
        &[("probe", b"binary")],
    );
    let installed = || {
        tilo_installed_apps::read(&store.catalog())
            .unwrap()
            .into_iter()
            .find(|app| app.package_id == VIEWER)
            .unwrap()
    };
    macos::verify_installed(&store.catalog(), &installed()).unwrap();

    // phase: a_changed_payload_file
    std::fs::write(package.payload_root.join("probe"), b"tampered").unwrap();
    let err = macos::verify_installed(&store.catalog(), &installed()).unwrap_err();
    assert!(err.contains("payload changed"), "{err}");
    std::fs::write(package.payload_root.join("probe"), b"binary").unwrap();

    // phase: a_file_the_package_did_not_ship
    std::fs::write(package.payload_root.join("extra.dylib"), b"?").unwrap();
    let err = macos::verify_installed(&store.catalog(), &installed()).unwrap_err();
    assert!(err.contains("did not ship"), "{err}");
    std::fs::remove_file(package.payload_root.join("extra.dylib")).unwrap();

    // phase: a_missing_signed_record
    let record = package.manifest_path.parent().unwrap().join("signed-root");
    let signed = std::fs::read(&record).unwrap();
    std::fs::remove_file(&record).unwrap();
    let err = macos::verify_installed(&store.catalog(), &installed()).unwrap_err();
    assert!(err.contains("refusing to launch"), "{err}");
    let apps = apps(&store, &root, Vec::new());
    let err = apps.start(VIEWER, ROOM).map(|_| ()).unwrap_err();
    assert!(err.contains("refusing to launch"), "{err}");
    assert!(apps.launched.0.lock().unwrap().is_empty());
    std::fs::write(&record, signed).unwrap();
    macos::verify_installed(&store.catalog(), &installed()).unwrap();
}

#[test]
fn attestation_holds_only_for_the_launched_process_itself() {
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id();
    let launched = Launched::default();
    assert!(
        !launched.attest(pid, VIEWER, ROOM),
        "unknown until recorded"
    );
    assert!(launched.record(pid, VIEWER, ROOM));
    assert!(launched.attest(pid, VIEWER, ROOM));
    assert!(!launched.attest(pid, "so.tilo.other", ROOM));
    assert!(!launched.attest(pid, VIEWER, "another-room"));
    assert!(!launched.attest(pid.wrapping_add(100_000), VIEWER, ROOM));

    // A record of an earlier process that held this pid does not attest the
    // one holding it now.
    launched.0.lock().unwrap().get_mut(&pid).unwrap().start_time += 1;
    assert!(!launched.attest(pid, VIEWER, ROOM));
    launched.forget(pid);
    assert!(launched.record(pid, VIEWER, ROOM));

    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        !launched.attest(pid, VIEWER, ROOM),
        "a reaped process is no longer the one launched"
    );
    launched.forget(pid);
    assert!(!launched.attest(pid, VIEWER, ROOM));
}

#[test]
fn a_process_start_time_is_stable_and_absent_for_no_process() {
    let own = std::process::id();
    let first = process_start_time(own).unwrap();
    assert_eq!(process_start_time(own), Some(first));
    assert_eq!(process_start_time(u32::MAX), None);
}

/// A real launch through `launcher-macos`: a signed, verified package whose
/// probe checks it inherited its service descriptors and its file chooser,
/// then waits on the
/// collaboration connection until the host goes away.
#[cfg(target_os = "macos")]
#[test]
fn a_verified_app_launches_confined_with_its_services_on_macos() {
    use std::os::unix::net::UnixListener;
    use std::process::Command;

    const PROBE: &str = r#"
#include <fcntl.h>
#include <stdlib.h>
#include <unistd.h>
static int fd_of(const char *name) {
    const char *value = getenv(name);
    if (!value) return -1;
    int fd = atoi(value);
    return fcntl(fd, F_GETFD) < 0 ? -1 : fd;
}
int main(void) {
    int collab = fd_of("TILO_SVC_COLLAB_FD");
    if (collab < 0 || fd_of("TILO_SVC_IDENTITY_FD") < 0) return 3;
    if (fd_of("TILO_SVC_CAPTURE_FD") < 0) return 5;
    if (fd_of("TILO_FILE_PICKER_FD") < 0) return 4;
    char byte;
    (void)read(collab, &byte, 1);
    return 0;
}
"#;
    let (_temp, root) = canonical_temp();
    let source = root.join("probe.c");
    std::fs::write(&source, PROBE).unwrap();
    let binary = root.join("probe");
    for command in [
        Command::new("cc")
            .arg("-O0")
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .status(),
        Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&binary)
            .status(),
    ] {
        assert!(command.unwrap().success());
    }
    let bytes = std::fs::read(&binary).unwrap();
    let mut store = Store::new(&root);
    let package = store.install(VIEWER, &manifest(VIEWER, "gui", ""), &[("probe", &bytes)]);
    let probe = package.payload_root.join("probe");
    std::fs::set_permissions(&probe, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();

    let collab = UnixListener::bind(root.join(COLLAB_SOCKET)).unwrap();
    let _identity = UnixListener::bind(root.join(IDENTITY_SOCKET)).unwrap();
    let apps = apps(&store, &root, Vec::new());
    let launch = apps.start(VIEWER, ROOM).unwrap();
    assert!(apps.attest_process(launch.pid, VIEWER, ROOM));
    assert!(!apps.attest_process(launch.pid, "so.tilo.other", ROOM));
    // The app's connection is queued, never accepted; closing the listener
    // ends it, and the probe's read with it.
    drop(collab);
    assert_eq!(launch.wait(), Some(0));
}
