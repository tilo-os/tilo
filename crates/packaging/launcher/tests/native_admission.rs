//! Real signed packages and native SDK bytes on the production admission path.
use launcher::run::{IdentitySource, RunOptions, probe_native_admission};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

struct Fixture {
    home: tilo_test_env::TestHome,
    installed: package_store::InstalledPackage,
    runtime: PathBuf,
    options: RunOptions,
}

impl Fixture {
    fn new(required_service: bool) -> Self {
        let mut home = tilo_test_env::TestHome::new().unwrap();
        let store_root = home.data().join("tilo-apps");
        home.env()
            .set("TILO_APPS_DATA_DIR", &store_root)
            .remove("TILO_APPS_SYSTEM_DIR");
        let key = runtime_bundle::Identity::generate();
        let key_path = home.home().join("sdk-key.json");
        std::fs::write(&key_path, key.to_json_bytes()).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let trust = home.home().join("sdk-publishers.txt");
        std::fs::write(&trust, key.public_key_hex()).unwrap();
        home.env()
            .set(launcher::dev_env::TRUSTED_PUBLISHERS_FILE_ENV, &trust);
        let extras = native_libraries();
        let (sdk, runtime) = runtime_bundle::build_abi_bundle(
            home.data(),
            "2026.10",
            &runtime_bundle::BuildOptions {
                from: None,
                arch: Some(std::env::consts::ARCH),
                identity: Some(&key_path),
                extra_libs: &extras,
            },
        )
        .unwrap();
        let publisher = tilo_app_package::SigningIdentity::generate();
        let service = if required_service {
            "[sandbox.services]\ncollab = \"required@1\""
        } else {
            ""
        };
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.admission-test"
version = "1.0.0"
kind = "app"
runtime = "tilo-runtime/{}"
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/app"
type = "gui"
{}
"#,
            sdk.version,
            hex::encode(publisher.public_key_bytes()),
            service
        ))
        .unwrap();
        let payload = home.home().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::copy("/usr/bin/true", payload.join("bin/app")).unwrap();
        let packed = tilo_app_package::pack(&payload, &manifest, &publisher).unwrap();
        let archive = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        let store = package_store::PackageStore::at(&store_root);
        store
            .install(
                archive.as_slice(),
                |_| Ok(package_store::Admission::default()),
                None,
            )
            .unwrap();
        let installed = package_store::Catalog::at(store_root, None)
            .find("so.tilo.admission-test")
            .unwrap()
            .unwrap();
        let options = RunOptions {
            expected_package_hash: Some(installed.package_id_hex.clone()),
            app_id: Some(installed.id.clone()),
            workspace_id: Some("admission-room".into()),
            runtime_root: Some(runtime.clone()),
            require_runtime_publisher: Some(key.public_key_hex()),
            identity: IdentitySource::None,
            data_home: Some(home.data().into()),
            ..Default::default()
        };
        Self {
            home,
            installed,
            runtime,
            options,
        }
    }
    fn probe(&self) -> Result<launcher::run::NativeAdmission, launcher::run::RunError> {
        probe_native_admission(
            &self.installed.manifest_path,
            &self.installed.payload_root,
            &self.options,
        )
    }
    fn assert_no_app_data(&self) {
        let room = launcher::appdir::room_slug(self.options.workspace_id.as_deref());
        assert!(
            !launcher::appdir::host_app_dir(self.home.data(), &room, &self.installed.id).exists()
        );
        assert!(!launcher::appdir::host_person_dir(self.home.data(), &self.installed.id).exists());
    }
}

fn native_libraries() -> Vec<PathBuf> {
    let directories = [
        "/usr/lib64",
        "/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/lib/aarch64-linux-gnu",
    ];
    let mut pending = vec![PathBuf::from("/usr/bin/true")];
    let mut found = std::collections::BTreeSet::new();
    while let Some(object) = pending.pop() {
        let dynamic = launcher::loader_preflight::read_dynamic_section(&object)
            .unwrap()
            .unwrap();
        for name in dynamic.needed {
            let path = directories
                .iter()
                .map(|directory| Path::new(directory).join(&name))
                .find(|path| path.is_file())
                .expect("the actual native dependency must be observable");
            if found.insert(path.clone()) {
                pending.push(path);
            }
        }
    }
    found.into_iter().collect()
}

#[test]
fn signed_native_admission_reports_actual_roots_without_app_data() {
    let fixture = Fixture::new(false);
    let evidence = fixture.probe().unwrap();
    assert_eq!(evidence.package_root, fixture.installed.package_id_hex);
    assert_eq!(evidence.package_id, fixture.installed.id);
    assert_eq!(evidence.workspace_id, "admission-room");
    assert_eq!(
        evidence.runtime,
        runtime_bundle::verify_bundle(&fixture.runtime).unwrap()
    );
    assert_eq!(
        evidence.launcher_blake3,
        blake3::hash(&std::fs::read(std::env::current_exe().unwrap()).unwrap())
            .to_hex()
            .to_string()
    );
    assert!(evidence.connected_services.is_empty());
    fixture.assert_no_app_data();
}

#[test]
fn required_service_is_not_bypassed_by_a_read_only_probe() {
    let fixture = Fixture::new(true);
    assert!(matches!(
        fixture.probe(),
        Err(launcher::run::RunError::Service(_))
    ));
    fixture.assert_no_app_data();
}

#[test]
fn native_admission_rejects_changed_signed_sdk_bytes() {
    let fixture = Fixture::new(false);
    let sdk = runtime_bundle::verify_bundle(&fixture.runtime).unwrap();
    let file = fixture.runtime.join(&sdk.files[0].path);
    let original = std::fs::read(&file).unwrap();
    std::fs::write(&file, vec![0; original.len()]).unwrap();
    assert!(fixture.probe().is_err());
    fixture.assert_no_app_data();
}

#[test]
fn native_admission_rejects_changed_package_and_launch_bypasses() {
    let mut fixture = Fixture::new(false);
    fixture.options.no_services = true;
    assert!(fixture.probe().is_err());
    fixture.options.no_services = false;
    fixture.options.allow_unsigned_runtime = true;
    assert!(fixture.probe().is_err());
    fixture.options.allow_unsigned_runtime = false;
    fixture.options.expected_package_hash = Some("00".repeat(32));
    assert!(fixture.probe().is_err());
    fixture.options.expected_package_hash = Some(fixture.installed.package_id_hex.clone());
    let binary = fixture.installed.payload_root.join("bin/app");
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&binary, b"changed signed package").unwrap();
    assert!(fixture.probe().is_err());
    fixture.assert_no_app_data();
}

#[test]
fn supervised_native_probe_binds_actual_child_host_and_original_target() {
    let fixture = Fixture::new(false);
    let command = || {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_tilo-launcher"));
        command
            .args(["run", "--manifest"])
            .arg(&fixture.installed.manifest_path)
            .arg("--payload-root")
            .arg(&fixture.installed.payload_root)
            .arg("--runtime")
            .arg(&fixture.runtime)
            .arg("--app-id")
            .arg(&fixture.installed.id)
            .arg("--workspace-id")
            .arg("admission-room")
            .arg("--expected-package-hash")
            .arg(&fixture.installed.package_id_hex)
            .arg("--require-runtime-publisher")
            .arg(fixture.options.require_runtime_publisher.as_ref().unwrap());
        command
    };
    let observed = std::sync::atomic::AtomicU32::new(0);
    let retired = std::sync::atomic::AtomicU32::new(0);
    let scope = launcher::run::NativeAdmissionScope {
        package_id: &fixture.installed.id,
        package_root: &fixture.installed.package_id_hex,
        workspace_id: "admission-room",
    };
    let evidence = launcher::run::observe_registered_probe(
        command(),
        scope,
        |pid| {
            assert!(Path::new("/proc").join(pid.to_string()).exists());
            observed.store(pid, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        },
        |pid| retired.store(pid, std::sync::atomic::Ordering::SeqCst),
    )
    .unwrap();
    assert_ne!(observed.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        observed.load(std::sync::atomic::Ordering::SeqCst),
        retired.load(std::sync::atomic::Ordering::SeqCst)
    );
    assert_eq!(
        evidence.admission.package_root,
        fixture.installed.package_id_hex
    );
    assert_eq!(
        evidence.admission.launcher_blake3,
        blake3::hash(&std::fs::read(env!("CARGO_BIN_EXE_tilo-launcher")).unwrap())
            .to_hex()
            .to_string()
    );
    assert_eq!(
        evidence.native_host_blake3,
        blake3::hash(&std::fs::read("/proc/self/exe").unwrap())
            .to_hex()
            .to_string()
    );
    let foreign = launcher::run::NativeAdmissionScope {
        package_root: "foreign-root",
        ..scope
    };
    assert!(
        launcher::run::observe_registered_probe(command(), foreign, |_| Ok(()), |_| ()).is_err()
    );
    let refused = launcher::run::observe_registered_probe(
        command(),
        scope,
        |_| Err("original custody retired".into()),
        |pid| retired.store(pid, std::sync::atomic::Ordering::SeqCst),
    );
    assert!(refused.unwrap_err().contains("original custody retired"));
    fixture.assert_no_app_data();
}
