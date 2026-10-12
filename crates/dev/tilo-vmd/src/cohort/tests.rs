use super::*;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;
use tilo_app_package::{Manifest, SigningIdentity};

fn elf(needed: &[&str], foreign: bool) -> Vec<u8> {
    let mut bytes = launcher::loader_preflight::elf_fixture(needed, None);
    let machine = if (std::env::consts::ARCH == "aarch64") == foreign {
        62_u16
    } else {
        183_u16
    };
    bytes
        .get_mut(18..20)
        .unwrap()
        .copy_from_slice(&machine.to_le_bytes());
    bytes
}

fn app_elf(foreign: bool) -> Vec<u8> {
    let mut bytes = elf(&["libruntime_dylib.so"], foreign);
    let old_headers = bytes.get(64..176).unwrap().to_vec();
    let table = bytes.len();
    let offset = table.checked_add(168).unwrap();
    let interpreter = if std::env::consts::ARCH == "aarch64" {
        "/lib/ld-linux-aarch64.so.1"
    } else {
        "/lib64/ld-linux-x86-64.so.2"
    };
    bytes
        .get_mut(32..40)
        .unwrap()
        .copy_from_slice(&(table as u64).to_le_bytes());
    bytes
        .get_mut(56..58)
        .unwrap()
        .copy_from_slice(&3_u16.to_le_bytes());
    bytes.extend(old_headers);
    let mut header = [0_u8; 56];
    header
        .get_mut(..4)
        .unwrap()
        .copy_from_slice(&3_u32.to_le_bytes());
    header
        .get_mut(8..16)
        .unwrap()
        .copy_from_slice(&(offset as u64).to_le_bytes());
    header
        .get_mut(32..40)
        .unwrap()
        .copy_from_slice(&(interpreter.len().checked_add(1).unwrap() as u64).to_le_bytes());
    bytes.extend(header);
    bytes.extend(interpreter.as_bytes());
    bytes.push(0);
    bytes
}

fn runtime(layer: &Path, work: &Path, missing: bool) -> String {
    let host = work.join("host");
    let needed: &[&str] = if missing { &["missing.so"] } else { &[] };
    let library = work.join("libruntime_dylib.so");
    fs::write(&library, elf(needed, false)).unwrap();
    for path in runtime_bundle::host_minimal_paths_for(std::env::consts::ARCH).unwrap() {
        let dest = host.join(path.trim_start_matches('/'));
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(dest, elf(&[], false)).unwrap();
    }
    let identity = work.join("identity.json");
    fs::write(
        &identity,
        serde_json::json!({"secret_key_hex": "21".repeat(32)}).to_string(),
    )
    .unwrap();
    let key = runtime_bundle::Identity::load(&identity)
        .unwrap()
        .public_key_hex();
    let (bundle, _) = runtime_bundle::build_abi_bundle(
        &layer.join("usr/lib/tilo-runtime"),
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: Some(&host),
            arch: Some(std::env::consts::ARCH),
            identity: Some(&identity),
            extra_libs: &[library],
        },
    )
    .unwrap();
    pin(layer, "runtime-bundle-publisher.hex", &key);
    bundle.version
}

fn pin(layer: &Path, name: &str, key: &str) {
    let path = layer.join("usr/share/tilo").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, key).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}

fn package(layer: &Path, work: &Path, version: &str, case: &str) -> InstalledPackage {
    let payload = work.join("payload");
    fs::create_dir_all(payload.join("bin")).unwrap();
    let binary = payload.join("bin/fixture");
    fs::write(
        &binary,
        app_elf(matches!(case, "arch" | "empty_isa_foreign")),
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let dual = matches!(case, "dual_isa" | "service");
    let isa = if matches!(case, "empty_isa" | "empty_isa_foreign") {
        ""
    } else if dual {
        "x86_64 = \"baseline\"\naarch64 = \"armv8-a\""
    } else if (std::env::consts::ARCH == "aarch64") != (case == "missing_isa") {
        "aarch64 = \"aarch64\""
    } else {
        "x86_64 = \"baseline\""
    };
    let (kind, entry_type) = if case == "service" {
        ("service", "cli")
    } else {
        ("app", "gui")
    };
    let platform = if case == "platform" {
        "platforms = [\"macos\"]\nplatform-reason = \"An explicit non-Linux fixture.\""
    } else {
        ""
    };
    let source = format!(
        r#"
[package]
id = "so.tilo.fixture"
kind = "{kind}"
version = "0.1.0"
runtime = "tilo-runtime/{version}"
{platform}
[package.isa]
{isa}
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[sandbox.portals]
[entrypoint]
bin = "bin/fixture"
type = "{entry_type}"
"#,
        "00".repeat(32)
    );
    let identity = SigningIdentity::from_seed(&[23; 32]);
    let mut manifest = Manifest::from_toml_str(&source).unwrap();
    if matches!(case, "empty_isa" | "empty_isa_foreign") {
        tilo_app_package::desktop::validate_native_host(&manifest).unwrap();
    }
    let publisher = hex::encode(identity.public_key_bytes());
    manifest.publisher.identity = format!("ed25519:{publisher}");
    let packed = tilo_app_package::pack(&payload, &manifest, &identity).unwrap();
    let store = layer.join("usr/share/tilo-apps");
    // The archive the publisher signed and the store installed. The layer does
    // not carry it: the daemon must rebuild exactly these bytes.
    tilo_app_package::tapp::write(
        fs::File::create(work.join("signed.tapp")).unwrap(),
        &packed.meta,
        Some(&packed),
    )
    .unwrap();
    let actual = store.join("payload/fixture");
    let record = store.join("packages/fixture");
    fs::create_dir_all(actual.join("bin")).unwrap();
    fs::create_dir_all(&record).unwrap();
    fs::copy(binary, actual.join("bin/fixture")).unwrap();
    fs::write(
        record.join(package_store::layout::SIGNED_ROOT_FILE),
        packed.meta.signed_root_bytes(),
    )
    .unwrap();
    fs::write(
        record.join(package_store::layout::MANIFEST_FILE),
        &packed.meta.manifest_bytes,
    )
    .unwrap();
    fs::write(
        record.join(package_store::layout::COLLECTION_FILE),
        &packed.meta.collection_bytes,
    )
    .unwrap();
    pin(layer, "app-publisher.hex", &publisher);
    serde_json::from_value(serde_json::json!({
        "id":"so.tilo.fixture", "version":"0.1.0", "package_id_hex":packed.meta.id.to_hex(),
        "manifest_path":"/usr/share/tilo-apps/packages/fixture/manifest.toml",
        "payload_root":"/usr/share/tilo-apps/payload/fixture", "publisher_key_hex":publisher,
        "runtime":format!("tilo-runtime/{version}")
    }))
    .unwrap()
}

fn fixture(root: &Path, case: &str) {
    let layer = root.join("layer");
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    let version = runtime(&layer, &work, case == "closure");
    let claimed = if case == "runtime" {
        "2026.07+native.00000000000000000000000000000000"
    } else {
        &version
    };
    let row = package(&layer, &work, claimed, case);
    let mut registry = Registry::default();
    registry.upsert(row.clone());
    save(&registry, &layer.join("usr/share/tilo-apps/registry.toml"));
    let base = root.join("base");
    fs::create_dir_all(&base).unwrap();
    if case == "incomplete" {
        let mut missing = row;
        missing.id = "so.tilo.missing".into();
        registry.upsert(missing);
    }
    save(&registry, &base.join("registry.toml"));
    match case {
        "publisher" => pin(&layer, "runtime-bundle-publisher.hex", &"11".repeat(32)),
        "payload" => fs::write(
            layer.join("usr/share/tilo-apps/payload/fixture/bin/fixture"),
            b"corrupt",
        )
        .unwrap(),
        "signature" => fs::write(
            layer.join("usr/share/tilo-apps/packages/fixture/signed-root"),
            b"corrupt",
        )
        .unwrap(),
        "executable" => fs::set_permissions(
            layer.join("usr/share/tilo-apps/payload/fixture/bin/fixture"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap(),
        "escape" => {
            fs::remove_file(layer.join("usr/share/tilo/app-publisher.hex")).unwrap();
            std::os::unix::fs::symlink(
                "/etc/passwd",
                layer.join("usr/share/tilo/app-publisher.hex"),
            )
            .unwrap();
        }
        // Another key in the app publisher pin: the installed packages are
        // signed, but not by the publisher this layer names.
        "app_publisher" => pin(&layer, "app-publisher.hex", &"11".repeat(32)),
        // A payload file swapped for a link out of the store: the archive must
        // never be rebuilt from bytes outside the authenticated layer.
        "payload_escape" => {
            let binary = layer.join("usr/share/tilo-apps/payload/fixture/bin/fixture");
            fs::copy(&binary, work.join("outside")).unwrap();
            fs::remove_file(&binary).unwrap();
            std::os::unix::fs::symlink(work.join("outside"), binary).unwrap();
        }
        // A cohort archive left in the layer is not what the daemon installs.
        "stale_archive" => {
            let archives = layer.join("usr/share/tilo-apps/cohort");
            fs::create_dir_all(&archives).unwrap();
            fs::write(archives.join("so.tilo.fixture.tapp"), b"stale").unwrap();
        }
        "record_escape" => {
            let path = layer.join("usr/share/tilo-apps/packages/fixture/manifest.toml");
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(work.join("outside"), path).unwrap();
            fs::write(work.join("outside"), b"outside").unwrap();
        }
        _ => {}
    }
}

fn save(registry: &Registry, path: &Path) {
    #[derive(serde::Serialize)]
    struct Rows<'a> {
        entry: &'a [InstalledPackage],
    }
    fs::write(
        path,
        toml::to_string(&Rows {
            entry: registry.entries(),
        })
        .unwrap(),
    )
    .unwrap();
}

fn verify_subject(root: &Path, case: &str) {
    let staging = root.join("staging");
    let result = verify(
        &root.join("layer"),
        &root.join("base"),
        std::env::consts::ARCH,
        &staging,
    );
    if matches!(
        case,
        "valid" | "dual_isa" | "service" | "empty_isa" | "stale_archive"
    ) {
        assert!(result.is_ok(), "{}", result.err().unwrap());
        let cohort = result.unwrap();
        assert_derived_archive(root, &cohort);
        if case == "valid" {
            assert_selection_receipts(&cohort);
        }
        assert_cohort_abi(&cohort);
        drop(cohort);
        assert_eq!(
            fs::read_dir(&staging).unwrap().count(),
            0,
            "the derived archives outlived their cohort"
        );
    } else {
        assert!(result.is_err(), "an invalid cohort was admitted");
        assert_eq!(
            fs::read_dir(&staging).unwrap().count(),
            0,
            "a refused cohort left archives behind"
        );
    }
}

fn assert_derived_archive(root: &Path, cohort: &VerifiedCohort) {
    // The derived archive is the publisher's signed archive, byte for
    // byte: complete, the same package id, trusted under the layer's pin.
    let derived = fs::read(&cohort.packages.first().unwrap().archive).unwrap();
    assert_eq!(derived, fs::read(root.join("work/signed.tapp")).unwrap());
    let tapp =
        tilo_app_package::tapp::read(derived.as_slice(), &mut tilo_app_package::DiscardBlobs)
            .unwrap();
    assert!(tapp.is_complete());
    assert_eq!(
        tapp.meta.id.to_hex(),
        cohort.packages.first().unwrap().row.package_id_hex
    );
}

fn assert_selection_receipts(cohort: &VerifiedCohort) {
    let session = crate::apply::Session {
        slot: "device".into(),
        unit: "session-3.scope".into(),
        user: "owner".into(),
        units: Vec::new(),
        control_socket: PathBuf::from("/run/owner/control"),
        stand_in: false,
        login: None,
    };
    let row = &cohort.packages.first().unwrap().row;
    for variant in ["selected", "wrong-root", "missing", "forged-held", "held"] {
        let mut receipt = serde_json::json!({
            "id": row.id, "version": row.version, "root": row.package_id_hex,
            "runtime": row.runtime, "preserved": false, "pinned": true, "held": false,
        });
        match variant {
            "wrong-root" => receipt["root"] = "11".repeat(32).into(),
            "forged-held" => receipt["preserved"] = true.into(),
            "held" => {
                receipt["root"] = "22".repeat(32).into();
                receipt["preserved"] = true.into();
                receipt["held"] = true.into();
            }
            _ => {}
        }
        let output = if variant == "missing" {
            String::new()
        } else {
            format!("cohort-selection: {receipt}\n")
        };
        let mut apply = crate::apply::RecordingApply::default();
        apply.cohort_replies.push_back(output);
        let selected = super::select(&mut apply, &session, cohort);
        if matches!(variant, "selected" | "held") {
            let actions = selected.unwrap();
            assert!(
                matches!(actions.as_slice(), [tilo_vm_protocol::CohortSelection { id, pinned: true, preserved, .. }]
                if id == &row.id && *preserved == (variant == "held"))
            );
        } else {
            assert!(
                selected.is_err(),
                "{variant} must not acknowledge selection"
            );
        }
        assert_eq!(apply.calls.len(), 1);
        assert!(
            apply
                .calls
                .first()
                .unwrap()
                .starts_with("install_cohort_app owner ")
        );
    }
}

fn assert_cohort_abi(cohort: &VerifiedCohort) {
    let mut provenance = tilo_vm_protocol::Provenance::local("apps+runtime");
    provenance.cargo_profile = Some("thin-app".into());
    provenance.cargo_lock_hash = Some("new graph".into());
    provenance.target = Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH));
    let base = tilo_vm_protocol::BaseInfo {
        cargo_lock_hash: Some("old graph".into()),
        target: provenance.target.clone(),
        ..Default::default()
    };
    assert!(crate::daemon::abi_check(&provenance, &base, std::env::consts::ARCH).is_err());
    assert!(
        crate::daemon::abi_check_verified(&provenance, &base, std::env::consts::ARCH, Some(cohort))
            .is_ok()
    );
    provenance.target = Some("wrong-architecture".into());
    assert!(
        crate::daemon::abi_check_verified(&provenance, &base, std::env::consts::ARCH, Some(cohort))
            .is_err()
    );
}

#[test]
fn complete_cohort_replays_real_signatures_payload_and_native_closure() {
    const SUBJECT: &str = "TILO_VMD_COHORT_SUBJECT";
    if let Ok(subject) = std::env::var(SUBJECT) {
        verify_subject(
            Path::new(&subject),
            &std::env::var("TILO_VMD_COHORT_CASE").unwrap(),
        );
        return;
    }
    for case in [
        "valid",
        "dual_isa",
        "service",
        "empty_isa",
        "empty_isa_foreign",
        "missing_isa",
        "platform",
        "publisher",
        "runtime",
        "payload",
        "signature",
        "arch",
        "closure",
        "incomplete",
        "executable",
        "escape",
        "record_escape",
        "app_publisher",
        "payload_escape",
        "stale_archive",
    ] {
        let root = tempfile::tempdir().unwrap();
        fixture(root.path(), case);
        // The one directory the verifier may write: everything else is
        // read-only in the sandbox below.
        let staging = root.path().join("staging");
        fs::create_dir_all(&staging).unwrap();
        let output = Command::new("timeout")
            .args([
                "30",
                "bwrap",
                "--unshare-all",
                "--uid",
                "0",
                "--gid",
                "0",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
            ])
            .arg("--bind")
            .args([&staging, &staging])
            .args(["--setenv", SUBJECT])
            .arg(root.path())
            .args(["--setenv", "TILO_VMD_COHORT_CASE", case])
            .arg(std::env::current_exe().unwrap())
            .args([
                "cohort::tests::complete_cohort_replays_real_signatures_payload_and_native_closure",
                "--exact",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
