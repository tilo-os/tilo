#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
//! Opt-in genuine authored app → canonical SDK graph → signed SDK → package.
//! All native work runs on the admitted builder. No VM or app executes while signing.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use tilo_app_cli::{PackagePlatform, PackageRequest, native_sdk::SdkSelection};

fn copy_sources(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if matches!(
            entry.file_name().to_str(),
            Some("target" | ".git" | ".claude")
        ) {
            continue;
        }
        let target = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_sources(&entry.path(), &target);
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target).unwrap();
            std::fs::set_permissions(&target, entry.metadata().unwrap().permissions()).unwrap();
        } else {
            assert!(
                !kind.is_symlink() && kind.is_file(),
                "fixture source contains an unsupported file type"
            );
        }
    }
}

fn request(selection: SdkSelection, key: &Path) -> PackageRequest {
    PackageRequest {
        sdk: Some(selection),
        output: None,
        identity: Some(key.to_path_buf()),
        profile: None,
        static_link: false,
        target: None,
        platform: PackagePlatform::Tilo,
        arch: None,
        features: Vec::new(),
        no_default_features: false,
        force: true,
    }
}

fn dependencies(root: &Path, binary: &Path, toolkit: &Path, std: &Path) -> Vec<PathBuf> {
    let program = "import importlib.util,json,os,pathlib,sys; s=importlib.util.spec_from_file_location('assembler',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); e=os.environ.copy(); e['LD_LIBRARY_PATH']=sys.argv[4]+':'+sys.argv[5]; print(json.dumps([str(p) for p in m.native_dependencies([pathlib.Path(sys.argv[2]),pathlib.Path(sys.argv[3])],e)]))";
    let output = Command::new("python3")
        .args(["-c", program])
        .arg(root.join("os/dev/assemble-app-runtime.py"))
        .arg(binary)
        .arg(toolkit)
        .arg(toolkit.parent().unwrap())
        .arg(std)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct NativeFixture {
    temp: PathBuf,
    app: PathBuf,
    binary: PathBuf,
    key: PathBuf,
    selection: SdkSelection,
    manifest: runtime_bundle::BundleManifest,
    extra: Vec<PathBuf>,
}

fn prepare_fixture(temp: &Path) -> NativeFixture {
    let sources = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let root = temp.join("sdk");
    copy_sources(&sources, &root);
    let created = Command::new(tilo_test_env::package_binary!("tilo-app"))
        .args([
            "new",
            "sdk-authoring-proof",
            "--id",
            "so.example.sdk-authoring-proof",
            "--template",
            "starter",
        ])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    // Only add this fixture's package node; every resolved dependency/version
    // remains the committed SDK lock. There is no fresh dependency resolution.
    let lock = root.join("Cargo.lock");
    let mut bytes = std::fs::read_to_string(&lock).unwrap();
    bytes.push_str("\n[[package]]\nname = \"sdk-authoring-proof\"\nversion = \"0.1.0\"\ndependencies = [\n \"runtime-dylib\",\n \"tilo-app\",\n \"workspace-hack\",\n]\n");
    std::fs::write(&lock, bytes).unwrap();
    let app = root.join("apps/sdk-authoring-proof");
    let identity = runtime_bundle::Identity::generate();
    let key = temp.join("sdk-identity.json");
    std::fs::write(&key, identity.to_json_bytes()).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let placeholder = SdkSelection {
        compiler: None,
        bundle: temp.join("bundles"),
        publisher: identity.public_key_hex(),
        root: "0".repeat(64),
    };
    let build = tilo_app_cli::build_native(&app, &request(placeholder, &key)).unwrap();
    let toolkit = build
        .sdk_toolkit
        .as_ref()
        .expect("actual Cargo toolkit artifact");
    let binary = &build.executables.first().unwrap().1;
    let std_output = Command::new("rustc")
        .args(["--print", "target-libdir"])
        .current_dir(&app)
        .output()
        .unwrap();
    assert!(std_output.status.success());
    let std = PathBuf::from(std::str::from_utf8(&std_output.stdout).unwrap().trim());
    let mut extra = dependencies(&root, binary, toolkit, &std);
    extra.push(toolkit.clone());
    let (manifest, bundle) = runtime_bundle::build_abi_bundle(
        &temp.join("bundles"),
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: None,
            arch: Some(std::env::consts::ARCH),
            identity: Some(&key),
            extra_libs: &extra,
            extra_bins: &[],
        },
    )
    .unwrap();
    let selection = SdkSelection {
        compiler: None,
        bundle,
        publisher: identity.public_key_hex(),
        root: manifest.root_hash.clone(),
    };
    NativeFixture {
        temp: temp.to_path_buf(),
        app,
        binary: binary.clone(),
        key,
        selection,
        manifest,
        extra,
    }
}

fn assert_refusals(fixture: &NativeFixture) {
    let mut foreign = fixture.selection.clone();
    foreign.root = "1".repeat(64);
    assert!(
        tilo_app_cli::package_app(&fixture.app, &request(foreign, &fixture.key))
            .unwrap_err()
            .to_string()
            .contains("root")
    );
    let mut foreign = fixture.selection.clone();
    foreign.publisher = runtime_bundle::Identity::generate().public_key_hex();
    assert!(tilo_app_cli::package_app(&fixture.app, &request(foreign, &fixture.key)).is_err());
    let incompatible_dir = fixture.temp.join("incompatible");
    std::fs::create_dir(&incompatible_dir).unwrap();
    let incompatible_toolkit = incompatible_dir.join("libruntime_dylib.so");
    std::fs::copy("/usr/bin/true", &incompatible_toolkit).unwrap();
    let wrong_extra = fixture
        .extra
        .iter()
        .map(|p| {
            if p.file_name().is_some_and(|n| n == "libruntime_dylib.so") {
                incompatible_toolkit.clone()
            } else {
                p.clone()
            }
        })
        .collect::<Vec<_>>();
    let (wrong_manifest, wrong_bundle) = runtime_bundle::build_abi_bundle(
        &fixture.temp.join("other-bundles"),
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: None,
            arch: Some(std::env::consts::ARCH),
            identity: Some(&fixture.key),
            extra_libs: &wrong_extra,
            extra_bins: &[],
        },
    )
    .unwrap();
    let wrong = SdkSelection {
        compiler: None,
        bundle: wrong_bundle,
        publisher: fixture.selection.publisher.clone(),
        root: wrong_manifest.root_hash,
    };
    assert!(
        format!(
            "{:#}",
            tilo_app_cli::package_app(&fixture.app, &request(wrong, &fixture.key)).unwrap_err()
        )
        .contains("compiled toolkit differs"),
        "a valid signed incompatible SDK is refused, even when its publisher/root match the selected receipt"
    );
    let original = std::fs::read(fixture.selection.bundle.join("lib/libruntime_dylib.so")).unwrap();
    std::fs::write(
        fixture.selection.bundle.join("lib/libruntime_dylib.so"),
        b"tampered",
    )
    .unwrap();
    assert!(
        tilo_app_cli::package_app(
            &fixture.app,
            &request(fixture.selection.clone(), &fixture.key)
        )
        .is_err()
    );
    std::fs::write(
        fixture.selection.bundle.join("lib/libruntime_dylib.so"),
        original,
    )
    .unwrap();
}

#[test]
fn generated_app_packages_only_with_its_actual_signed_native_sdk() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = prepare_fixture(temp.path());
    let package = tilo_app_cli::package_app(
        &fixture.app,
        &request(fixture.selection.clone(), &fixture.key),
    )
    .unwrap();
    let read = tilo_app_package::tapp::read(
        std::io::BufReader::new(std::fs::File::open(&package.output).unwrap()),
        &mut tilo_app_package::DiscardBlobs,
    )
    .unwrap();
    assert_eq!(
        read.meta.manifest.package.id,
        "so.example.sdk-authoring-proof"
    );
    assert_eq!(
        read.meta.manifest.package.runtime,
        Some(format!("tilo-runtime/{}", fixture.manifest.version))
    );
    let inspection = fixture.temp.join("authored-app-inspect.json");
    let actions: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.app.join("tests/headless-actions.json")).unwrap(),
    )
    .unwrap();
    let drive = serde_json::json!({"inspect": inspection, "actions": actions});
    let behavior = Command::new(&fixture.binary)
        .args(["--headless-drive", &drive.to_string()])
        .current_dir(&fixture.app)
        .env("LD_LIBRARY_PATH", fixture.selection.bundle.join("lib"))
        .output()
        .unwrap();
    assert!(
        behavior.status.success(),
        "{}",
        String::from_utf8_lossy(&behavior.stderr)
    );
    assert!(
        inspection.is_file(),
        "actual authored app executed its shared semantic assertions after signing"
    );
    assert_refusals(&fixture);
}
