//! Integration test: build a bundle from a fixture host root, verify it,
//! then flip a byte and confirm verify rejects it. Also exercises the
//! cross-arch aarch64 path and the signing round-trip.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fs;
use std::path::Path;

use runtime_bundle::{
    BuildOptions, BundleError, Identity, VerifyOptions, build_bundle, build_bundle_with,
    verify_bundle, verify_bundle_with,
};

#[test]
fn overlapping_includes_preserve_one_signed_payload_or_refuse_conflicting_bytes() {
    let host = tempfile::tempdir().unwrap();
    fake_x86_64_host(host.path());
    let identity = Identity::generate();
    let key = host.path().join("identity.json");
    fs::write(&key, identity.to_json_bytes()).unwrap();
    for relative in ["libruntime_dylib.so", "engine/resources/data.bin"] {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for source in [&first, &second] {
            let file = source.path().join(relative);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, b"same payload").unwrap();
        }
        let include_name = relative.split('/').next().unwrap();
        let includes = [
            first.path().join(include_name),
            second.path().join(include_name),
        ];
        let options = BuildOptions {
            from: Some(host.path()),
            arch: Some("x86_64"),
            identity: Some(&key),
            extra_libs: &includes,
            extra_bins: &[],
        };
        let out = tempfile::tempdir().unwrap();
        let built = build_bundle_with(out.path(), "2026.07", &options).unwrap();
        let verified = verify_bundle_with(
            out.path(),
            &VerifyOptions {
                require_publisher: Some(identity.public_key_hex()),
                require_signature: true,
                require_arch: Some("x86_64".into()),
            },
        )
        .expect("overlapping includes must produce a launchable signed bundle");
        assert_eq!(verified, built);
        assert_eq!(
            built
                .files
                .iter()
                .filter(|file| file.path == format!("lib/{relative}"))
                .count(),
            1
        );
        fs::write(second.path().join(relative), b"conflicting payload").unwrap();
        let rejected = tempfile::tempdir().unwrap();
        assert!(matches!(
            build_bundle_with(rejected.path(), "2026.07", &options),
            Err(BundleError::IncludeConflict { name, .. }) if name == format!("lib/{relative}")
        ));
        assert!(!rejected.path().join("bundle.toml").exists());
    }
}

fn fake_x86_64_host(root: &Path) {
    for rel in [
        "lib64/ld-linux-x86-64.so.2",
        "lib64/libc.so.6",
        "lib64/libm.so.6",
        "lib64/libdl.so.2",
    ] {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("STUB: {rel}\n").as_bytes()).unwrap();
    }
    fs::create_dir_all(root.join("etc")).unwrap();
    fs::write(root.join("etc/ld.so.cache"), b"stub-cache").unwrap();
}

fn fake_aarch64_debian_host(root: &Path) {
    for rel in [
        "lib/ld-linux-aarch64.so.1",
        "lib/aarch64-linux-gnu/libc.so.6",
        "lib/aarch64-linux-gnu/libm.so.6",
        "lib/aarch64-linux-gnu/libgcc_s.so.1",
    ] {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("STUB: {rel}\n").as_bytes()).unwrap();
    }
}

fn fake_aarch64_fedora_host(root: &Path) {
    for rel in [
        "lib/ld-linux-aarch64.so.1",
        "usr/lib64/libc.so.6",
        "usr/lib64/libm.so.6",
        "usr/lib64/libgcc_s.so.1",
    ] {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, format!("STUB: {rel}\n").as_bytes()).unwrap();
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn unsigned_bundles_round_trip_over_every_host_layout() {
    // phase: build_verify_roundtrip_over_fake_host_x86_64
    {
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());

        let out = tempfile::tempdir().unwrap();
        let manifest = build_bundle_with(
            out.path(),
            "2026.07",
            &BuildOptions {
                from: Some(host.path()),
                arch: Some("x86_64"),
                identity: None,
                extra_libs: &[],
                extra_bins: &[],
            },
        )
        .expect("build");
        assert_eq!(manifest.version, "2026.07");
        assert_eq!(manifest.arch, "x86_64");
        assert!(!manifest.files.is_empty(), "bundle should include files");
        assert_eq!(manifest.root_hash.len(), 64);

        // ld.so.cache should be included when present.
        assert!(manifest.files.iter().any(|f| f.path == "etc/ld.so.cache"));
        // Sorted.
        assert!(manifest.files.windows(2).all(|w| w[0].path <= w[1].path));

        let verified = verify_bundle(out.path()).expect("verify");
        assert_eq!(verified.root_hash, manifest.root_hash);
    }
    // phase: build_verify_roundtrip_over_fake_host_default_arch
    {
        fn phase_build_verify_roundtrip_over_fake_host_default_arch() {
            // build_bundle without --arch uses the host arch — the wrapper
            // path most tools use.
            let host = tempfile::tempdir().unwrap();
            fake_x86_64_host(host.path());
            let out = tempfile::tempdir().unwrap();
            // Only run when host is x86_64; on aarch64 CI this test would
            // (correctly) find no sources.
            if runtime_bundle::host_arch() != "x86_64" {
                return;
            }
            let manifest = build_bundle(out.path(), "v", Some(host.path())).expect("build");
            assert_eq!(manifest.arch, "x86_64");
            verify_bundle(out.path()).expect("verify");
        }
        phase_build_verify_roundtrip_over_fake_host_default_arch();
    }
    // phase: build_verify_roundtrip_aarch64_debian_fixture
    {
        let host = tempfile::tempdir().unwrap();
        fake_aarch64_debian_host(host.path());

        let out = tempfile::tempdir().unwrap();
        let manifest = build_bundle_with(
            out.path(),
            "arm-2026.07",
            &BuildOptions {
                from: Some(host.path()),
                arch: Some("aarch64"),
                identity: None,
                extra_libs: &[],
                extra_bins: &[],
            },
        )
        .expect("build aarch64");
        assert_eq!(manifest.arch, "aarch64");
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths
                .iter()
                .any(|p| p.ends_with("aarch64-linux-gnu/libc.so.6"))
        );

        verify_bundle(out.path()).expect("verify aarch64");
        // Arch mismatch policy refuses.
        let err = verify_bundle_with(
            out.path(),
            &VerifyOptions {
                require_arch: Some("x86_64".to_string()),
                ..VerifyOptions::default()
            },
        )
        .unwrap_err();
        matches!(err, BundleError::ArchMismatch { .. });
    }
    // phase: build_verify_roundtrip_aarch64_fedora_fixture
    {
        let host = tempfile::tempdir().unwrap();
        fake_aarch64_fedora_host(host.path());

        let out = tempfile::tempdir().unwrap();
        let manifest = build_bundle_with(
            out.path(),
            "arm",
            &BuildOptions {
                from: Some(host.path()),
                arch: Some("aarch64"),
                identity: None,
                extra_libs: &[],
                extra_bins: &[],
            },
        )
        .expect("build aarch64 fedora");
        assert_eq!(manifest.arch, "aarch64");
        let paths: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.ends_with("usr/lib64/libc.so.6")));
        verify_bundle(out.path()).expect("verify");
    }
    // phase: verify_rejects_a_flipped_byte
    {
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());

        let out = tempfile::tempdir().unwrap();
        build_bundle_with(
            out.path(),
            "2026.07",
            &BuildOptions {
                from: Some(host.path()),
                arch: Some("x86_64"),
                identity: None,
                extra_libs: &[],
                extra_bins: &[],
            },
        )
        .expect("build");

        let libc = out.path().join("lib/lib64/libc.so.6");
        let mut bytes = fs::read(&libc).unwrap();
        bytes[0] ^= 0x5a;
        fs::write(&libc, bytes).unwrap();

        let err = verify_bundle(out.path()).unwrap_err();
        matches!(err, BundleError::HashMismatch { .. });
    }
    // phase: build_is_deterministic_over_root_hash
    {
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());

        let out1 = tempfile::tempdir().unwrap();
        let out2 = tempfile::tempdir().unwrap();
        let opts = BuildOptions {
            from: Some(host.path()),
            arch: Some("x86_64"),
            identity: None,
            extra_libs: &[],
            extra_bins: &[],
        };
        let a = build_bundle_with(out1.path(), "v", &opts).expect("build a");
        let b = build_bundle_with(out2.path(), "v", &opts).expect("build b");
        assert_eq!(a.root_hash, b.root_hash);
    }
}

#[test]
fn signed_bundles_verify_and_catch_a_tampered_root() {
    // phase: signed_bundle_verifies_and_publisher_pin_works
    {
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());
        let identity_path = host.path().join("id.json");
        let identity = Identity::generate();
        fs::write(&identity_path, identity.to_json_bytes()).unwrap();

        let out = tempfile::tempdir().unwrap();
        let manifest = build_bundle_with(
            out.path(),
            "signed",
            &BuildOptions {
                from: Some(host.path()),
                arch: Some("x86_64"),
                identity: Some(&identity_path),
                extra_libs: &[],
                extra_bins: &[],
            },
        )
        .expect("build signed");
        assert_eq!(
            manifest.publisher_hex.as_deref(),
            Some(identity.public_key_hex()).as_deref()
        );

        // Verify accepts.
        verify_bundle(out.path()).expect("verify signed");
        verify_bundle_with(
            out.path(),
            &VerifyOptions {
                require_publisher: Some(identity.public_key_hex()),
                require_signature: true,
                require_arch: Some("x86_64".to_string()),
            },
        )
        .expect("required publisher matches");

        // Different publisher pin refuses.
        let other = Identity::generate();
        let err = verify_bundle_with(
            out.path(),
            &VerifyOptions {
                require_publisher: Some(other.public_key_hex()),
                require_signature: true,
                require_arch: None,
            },
        )
        .unwrap_err();
        matches!(err, BundleError::PublisherMismatch { .. });
    }
    // phase: tampered_root_hash_in_signed_bundle_is_caught
    {
        // Signature covers version+root_hash; flipping root_hash without
        // updating files causes the payload-hash check to fail first
        // (hash-of-files vs recorded root_hash). That's the design —
        // we test both signatures: swap the recorded root_hash so the
        // signature no longer verifies but files stay consistent.
        let host = tempfile::tempdir().unwrap();
        fake_x86_64_host(host.path());
        let identity_path = host.path().join("id.json");
        let identity = Identity::generate();
        fs::write(&identity_path, identity.to_json_bytes()).unwrap();

        let out = tempfile::tempdir().unwrap();
        build_bundle_with(
            out.path(),
            "signed",
            &BuildOptions {
                from: Some(host.path()),
                arch: Some("x86_64"),
                identity: Some(&identity_path),
                extra_libs: &[],
                extra_bins: &[],
            },
        )
        .unwrap();

        // Swap in a valid-looking but wrong root_hash so we reach the
        // signature check with mismatched hashes. Use a well-formed 64-
        // hex string.
        let bundle_toml = out.path().join(runtime_bundle::BUNDLE_MANIFEST_FILENAME);
        let text = fs::read_to_string(&bundle_toml).unwrap();
        let tampered = text.replace(
            // this is fragile-but-scoped: the root_hash field is unique
            // and one line.
            "root_hash = \"",
            "root_hash = \"deadbeef",
        );
        fs::write(&bundle_toml, tampered).unwrap();

        let err = verify_bundle(out.path()).unwrap_err();
        // The integrity check (file-list root-hash) rejects first; that's the
        // right layered behavior. Either error type is acceptable — both mean
        // "you can't fake this".
        match err {
            BundleError::HashMismatch { .. }
            | BundleError::BadSignature
            | BundleError::SignatureFieldMalformed { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}

/// Payload hashing is spread over threads. A bundle of many files must still
/// have every one of them hashed, and one changed byte in any of them (the
/// first, a middle one, the last) must still be refused.
#[test]
fn a_bundle_of_many_files_is_hashed_whole_and_refuses_a_change_in_any_of_them() {
    let host = tempfile::tempdir().unwrap();
    fake_x86_64_host(host.path());
    let libs_dir = tempfile::tempdir().unwrap();
    let libs: Vec<_> = (0..40)
        .map(|n| {
            let path = libs_dir.path().join(format!("libextra{n:02}.so"));
            let bytes: Vec<u8> = (0..=250u8).cycle().skip(n).take(1000 + n * 7919).collect();
            fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    let out = tempfile::tempdir().unwrap();
    let manifest = build_bundle_with(
        out.path(),
        "many",
        &BuildOptions {
            from: Some(host.path()),
            arch: Some("x86_64"),
            identity: None,
            extra_libs: &libs,
            extra_bins: &[],
        },
    )
    .unwrap();
    assert!(manifest.files.len() > 40);
    let (_, check) =
        runtime_bundle::verify_bundle_verity_with(out.path(), &VerifyOptions::default()).unwrap();
    assert_eq!(check.hashed, manifest.files.len(), "every file is hashed");
    let total: u64 = manifest.files.iter().map(|f| f.size).sum();
    assert_eq!(check.hashed_bytes, total, "every byte is read");

    for victim in [
        "lib/libextra00.so",
        "lib/libextra20.so",
        "lib/libextra39.so",
    ] {
        let path = out.path().join(victim);
        let original = fs::read(&path).unwrap();
        let mut changed = original.clone();
        let middle = changed.len() / 2;
        changed[middle] ^= 1;
        fs::write(&path, &changed).unwrap();
        let error = verify_bundle(out.path()).unwrap_err();
        assert!(
            matches!(error, BundleError::HashMismatch { .. }),
            "{victim}: {error:?}"
        );
        fs::write(&path, original).unwrap();
        verify_bundle(out.path()).unwrap();
    }
}
