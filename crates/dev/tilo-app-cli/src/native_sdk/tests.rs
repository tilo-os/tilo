#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use super::*;

#[test]
fn versioned_imports_must_resolve_and_weak_imports_remain_optional() {
    let rows = "Symbol table '.dynsym' contains 5 entries:\n 1: 0 0 FUNC GLOBAL DEFAULT UND required@GLIBC_2.34 (2)\n 2: 1 8 FUNC GLOBAL DEFAULT 10 required@@GLIBC_2.34\n 3: 0 0 NOTYPE WEAK DEFAULT UND optional\n 4: 1 8 FUNC GLOBAL DEFAULT 10 other@@GLIBC_2.33\n";
    let mut exports = BTreeSet::new();
    let mut imports = BTreeSet::new();
    parse_symbols(rows.as_bytes(), &mut exports, &mut imports).unwrap();
    assert_eq!(imports, BTreeSet::from(["required@GLIBC_2.34".to_owned()]));
    assert!(imports.is_subset(&exports));
    assert!(exports.contains("required"));
    assert!(!exports.contains("required@GLIBC_2.35"));
    assert!(parse_symbols("unobservable".as_bytes(), &mut exports, &mut imports).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn actual_elf_identity_and_symbols_are_observed_without_running_the_subject() {
    let exe = std::env::current_exe().unwrap();
    native(&exe, std::env::consts::ARCH).unwrap();
    let foreign = if std::env::consts::ARCH == "x86_64" {
        "aarch64"
    } else {
        "x86_64"
    };
    assert!(native(&exe, foreign).is_err());
    let mut exports = BTreeSet::new();
    let mut imports = BTreeSet::new();
    symbols(&exe, &mut exports, &mut imports).unwrap();
    assert!(
        !imports.is_empty(),
        "actual native executable imports system symbols"
    );
    let temp = tempfile::tempdir().unwrap();
    let opaque = temp.path().join("opaque.so");
    std::fs::write(&opaque, b"not an ELF").unwrap();
    assert!(native(&opaque, std::env::consts::ARCH).is_err());
    assert!(symbols(&opaque, &mut exports, &mut imports).is_err());
}

#[test]
fn sdk_graph_reuses_the_full_cohort_and_selective_production_wrapper() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let mut command = Command::new("cargo");
    let features = configure_graph(&mut command, &root, "thin-app", "calculator", &[]).unwrap();
    let args = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(features.contains(&"calculator/thin".to_owned()));
    assert!(features.contains(&"settings/thin".to_owned()));
    assert!(args.iter().any(|a| a.contains("rustc-thin-app-wrapper.sh")));
    assert!(args.iter().any(|a| a == "runtime-dylib"));
    assert!(args.iter().any(|a| a == "--locked"));
    assert!(
        configure_graph(
            &mut Command::new("cargo"),
            &root,
            "thin-app",
            "foreign",
            &[]
        )
        .is_err()
    );
}

#[cfg(target_os = "linux")]
#[test]
fn verified_snapshot_cannot_follow_later_mutation_of_the_selected_sdk() {
    let temp = tempfile::tempdir().unwrap();
    let identity = runtime_bundle::Identity::generate();
    let key = temp.path().join("key.json");
    std::fs::write(&key, identity.to_json_bytes()).unwrap();
    let extra = temp.path().join("extra.so");
    std::fs::write(&extra, b"signed fixture payload").unwrap();
    let (manifest, bundle) = runtime_bundle::build_abi_bundle(
        &temp.path().join("bundles"),
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: None,
            arch: Some(std::env::consts::ARCH),
            identity: Some(&key),
            extra_libs: &[extra],
        },
    )
    .unwrap();
    let selected = SdkSelection {
        compiler: None,
        bundle: bundle.clone(),
        publisher: identity.public_key_hex(),
        root: manifest.root_hash,
    };
    let (snapshot, copied) = verified_snapshot(&selected, &bundle, std::env::consts::ARCH).unwrap();
    std::fs::write(
        bundle.join("lib/extra.so"),
        b"replaced while inspection is pending",
    )
    .unwrap();
    assert_eq!(
        std::fs::read(snapshot.path().join("lib/extra.so")).unwrap(),
        b"signed fixture payload"
    );
    assert_eq!(
        runtime_bundle::verify_bundle_with(
            snapshot.path(),
            &runtime_bundle::VerifyOptions {
                require_publisher: Some(selected.publisher.clone()),
                require_signature: true,
                require_arch: Some(std::env::consts::ARCH.to_owned())
            }
        )
        .unwrap()
        .root_hash,
        copied.root_hash
    );
    assert!(verified_snapshot(&selected, &bundle, std::env::consts::ARCH).is_err());
}

#[test]
fn ambiguous_and_oversized_symbol_names_fail_observation() {
    let mut exports = BTreeSet::new();
    let mut imports = BTreeSet::new();
    let ambiguous = "Symbol table '.dynsym' contains 1 entry:\n 1: 0 0 FUNC GLOBAL DEFAULT UND name with-space\n";
    assert!(parse_symbols(ambiguous.as_bytes(), &mut exports, &mut imports).is_err());
    let oversized = "x".repeat(16 * 1024 + 1);
    assert!(parse_symbols(oversized.as_bytes(), &mut exports, &mut imports).is_err());
}
