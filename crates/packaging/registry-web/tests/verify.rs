//! End to end against the real producers: packages packed and signed by
//! `tilo-app-package`, entries and snapshots signed by `registry-types`.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use ed25519_dalek::SigningKey;
use registry_types::{
    LocalizedImages, PublisherAuthorization, RegistryEntry, RegistryTarget, RegistryVersion,
    SNAPSHOT_PROTOCOL, SignedRegistrySnapshot, sign_entry, sign_snapshot,
};
use registry_web::{
    Checkpoint, DEFAULT_WASM_FEATURES, Host, PackageKind, Selection, VerifyError, blake3_hex,
    verify_blob, verify_package, verify_snapshot,
};
use tilo_app_package::web::{
    RUNTIME_ABI_SECTION, WebBuildOptions, inspect_wasm_module, web_manifest,
};
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp};

const ABI: &str = "tilo-runtime/2026.07+wasm.fe8836616de68a65d59c02eb73caa005";
const OTHER_ABI: &str = "tilo-runtime/2026.07+wasm.00112233445566778899aabbccddeeff";
const NOW: u64 = 1_000_015;

fn publisher() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn owner() -> SigningKey {
    SigningKey::from_bytes(&[8; 32])
}

fn owner_hex() -> String {
    hex::encode(owner().verifying_key().to_bytes())
}

fn leb(mut value: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = u8::try_from(value & 0x7f).unwrap();
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn custom(name: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = leb(name.len());
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(payload);
    let mut out = vec![0];
    out.extend(leb(body.len()));
    out.extend(body);
    out
}

fn module(abi: &str, seed: u8) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    out.extend(custom(RUNTIME_ABI_SECTION, abi.as_bytes()));
    let mut features = leb(DEFAULT_WASM_FEATURES.len());
    for name in DEFAULT_WASM_FEATURES {
        features.push(b'+');
        features.extend(leb(name.len()));
        features.extend_from_slice(name.as_bytes());
    }
    out.extend(custom("target_features", &features));
    out.extend(custom("filler", &vec![seed; 4096]));
    out
}

struct Built {
    thin: Vec<u8>,
    full: Vec<u8>,
    version: RegistryVersion,
    module: Vec<u8>,
    meta: tilo_app_package::PackageMeta,
}

fn build(id: &str, kind: &str, version: &str, abi: &str, assets: &[(&str, &[u8])]) -> Built {
    let identity = SigningIdentity::from_signing_key(publisher());
    let runtime = if kind == "app" {
        "runtime = \"tilo-runtime/2026.07\""
    } else {
        ""
    };
    let authored = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{id}"
version = "{version}"
kind = "{kind}"
{runtime}

[publisher]
identity = "ed25519:{key}"
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
"#,
        key = hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let wasm = module(abi, version.len().try_into().unwrap());
    let info = inspect_wasm_module(&wasm).unwrap();
    let manifest = web_manifest(&authored, "app.wasm", &info, &WebBuildOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("app.wasm"), &wasm).unwrap();
    for (name, bytes) in assets {
        let path = dir.path().join("assets").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let packed = pack(dir.path(), &manifest, &identity).unwrap();
    let thin = tapp::to_bytes(&packed.meta, None).unwrap();
    let full = tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    let target = manifest.target.as_ref().unwrap();
    let version = RegistryVersion {
        version: version.into(),
        tapp_url: format!("https://apps.tilo.so/artifacts/{}.tapp", blake3_hex(&full)),
        tapp_root_hash_hex: packed.meta.id.to_hex(),
        tapp_size_bytes: full.len() as u64,
        released_at_unix: 1,
        tapp_blake3_hex: blake3_hex(&full),
        target: RegistryTarget {
            platform: target.platform.clone(),
            architecture: target.architecture.clone(),
            isa: target.isa.clone(),
            runtime: Some(target.runtime.clone()),
        },
    };
    Built {
        thin,
        full,
        version,
        module: wasm,
        meta: packed.meta,
    }
}

fn entry(id: &str, versions: Vec<RegistryVersion>) -> RegistryEntry {
    let mut entry = RegistryEntry {
        package_id: id.into(),
        name: "Books".into(),
        description: "Reads books".into(),
        tagline: "Read".into(),
        images: LocalizedImages::english_icon("https://apps.tilo.so/images/icon.svg", "Icon"),
        supported_languages: vec!["en".into()],
        categories: vec!["Office".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher().verifying_key().to_bytes()),
        versions,
        updated_at_unix: 1,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &publisher());
    entry
}

fn snapshot_bytes(sequence: u64, previous: &str, entries: Vec<RegistryEntry>) -> Vec<u8> {
    let mut snapshot = SignedRegistrySnapshot {
        protocol: SNAPSHOT_PROTOCOL,
        sequence,
        issued_at_unix: NOW - 5,
        expires_at_unix: NOW + 5,
        previous_snapshot_hash_hex: previous.into(),
        moderation_head_hex: String::new(),
        publishers: vec![PublisherAuthorization {
            pubkey_hex: hex::encode(publisher().verifying_key().to_bytes()),
            can_add_new: true,
        }],
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    sign_snapshot(&mut snapshot, &owner()).unwrap();
    serde_json::to_vec(&snapshot).unwrap()
}

fn host(runtime: Option<&str>) -> Host<'_> {
    Host {
        features: DEFAULT_WASM_FEATURES,
        runtime,
    }
}

#[test]
fn an_app_verifies_down_to_its_module_and_asset_blobs() {
    let books = build(
        "so.tilo.ebook",
        "app",
        "0.1.0",
        ABI,
        &[("fonts/body.ttf", b"font bytes"), ("icon.svg", b"<svg/>")],
    );
    let bytes = snapshot_bytes(
        1,
        "",
        vec![entry("so.tilo.ebook", vec![books.version.clone()])],
    );
    let snapshot = verify_snapshot(&bytes, &owner_hex(), None, NOW).unwrap();
    let selection = snapshot.select("so.tilo.ebook", &host(Some(ABI))).unwrap();
    assert_eq!(selection.package_root_hex, books.meta.id.to_hex());

    for archive in [&books.thin, &books.full] {
        let package = verify_package(archive, &selection).unwrap();
        assert_eq!(package.kind, PackageKind::App);
        assert_eq!(package.runtime, ABI);
        assert_eq!(package.module.path, "app.wasm");
        assert_eq!(package.module.blake3_hex, blake3_hex(&books.module));
        let assets: Vec<_> = package.assets.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(assets, ["fonts/body.ttf", "icon.svg"]);
        let expected: Vec<(String, String)> = books
            .meta
            .collection
            .entries
            .iter()
            .map(|(path, hash)| (path.clone(), hash.to_hex()))
            .collect();
        let found: Vec<(String, String)> = package
            .files
            .iter()
            .map(|file| (file.path.clone(), file.blake3_hex.clone()))
            .collect();
        assert_eq!(found, expected, "same collection as tilo-app-package reads");
    }
    verify_blob(&books.module, &selection_module_hash(&books, &selection)).unwrap();
    let mut tampered = books.module.clone();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(matches!(
        verify_blob(&tampered, &blake3_hex(&books.module)),
        Err(VerifyError::BlobMismatch(_))
    ));
}

fn selection_module_hash(built: &Built, selection: &Selection) -> String {
    verify_package(&built.thin, selection)
        .unwrap()
        .module
        .blake3_hex
}

#[test]
fn the_toolkit_core_is_a_runtime_selected_by_its_abi() {
    let core = build("so.tilo.runtime.web", "runtime", "2026.7.0", ABI, &[]);
    let bytes = snapshot_bytes(
        1,
        "",
        vec![entry("so.tilo.runtime.web", vec![core.version.clone()])],
    );
    let snapshot = verify_snapshot(&bytes, &owner_hex(), None, NOW).unwrap();
    let selection = snapshot
        .select("so.tilo.runtime.web", &host(Some(ABI)))
        .unwrap();
    let package = verify_package(&core.thin, &selection).unwrap();
    assert_eq!(package.kind, PackageKind::Runtime);
    assert_eq!(package.runtime, ABI);
    assert!(matches!(
        snapshot.select("so.tilo.runtime.web", &host(Some(OTHER_ABI))),
        Err(VerifyError::NoCompatibleVersion { .. })
    ));
}

fn fake_version(version: &str, isa: &str, runtime: &str, root: u8) -> RegistryVersion {
    RegistryVersion {
        version: version.into(),
        tapp_url: "https://apps.tilo.so/artifacts/x.tapp".into(),
        tapp_root_hash_hex: hex::encode([root; 32]),
        tapp_size_bytes: 1,
        released_at_unix: 1,
        tapp_blake3_hex: hex::encode([root; 32]),
        target: RegistryTarget {
            platform: "web".into(),
            architecture: "wasm32".into(),
            isa: isa.into(),
            runtime: Some(runtime.into()),
        },
    }
}

#[test]
fn selection_takes_the_newest_version_the_host_runs() {
    let mvp = "wasm32-mvp+mutable-globals";
    let simd = "wasm32-mvp+mutable-globals+simd128";
    let mut native = fake_version("9.0.0", "x86_64-v3", ABI, 9);
    native.target = RegistryTarget {
        platform: "tilo-os".into(),
        architecture: "x86_64".into(),
        isa: "x86_64-v3".into(),
        runtime: None,
    };
    let versions = vec![
        fake_version("1.0.0", mvp, ABI, 1),
        fake_version("1.10.0", mvp, OTHER_ABI, 2),
        fake_version("1.2.0", mvp, ABI, 3),
        fake_version("2.0.0", simd, ABI, 4),
        native,
    ];
    let bytes = snapshot_bytes(1, "", vec![entry("so.tilo.ebook", versions)]);
    // The compact form keeps only the newest version per target, yet every
    // host selects exactly what it selects from the full snapshot.
    for bytes in [compact_bytes(&bytes), bytes] {
        let snapshot = verify_snapshot(&bytes, &owner_hex(), None, NOW).unwrap();
        let pick = |runtime, features: &[&str]| {
            snapshot
                .select("so.tilo.ebook", &Host { features, runtime })
                .map(|selection| selection.version)
        };
        assert_eq!(pick(Some(ABI), DEFAULT_WASM_FEATURES).unwrap(), "1.2.0");
        assert_eq!(
            pick(Some(OTHER_ABI), DEFAULT_WASM_FEATURES).unwrap(),
            "1.10.0"
        );
        assert_eq!(pick(None, DEFAULT_WASM_FEATURES).unwrap(), "1.10.0");
        assert_eq!(
            pick(Some(ABI), &["mutable-globals", "simd128"]).unwrap(),
            "2.0.0"
        );
        assert!(matches!(
            pick(Some(ABI), &[]),
            Err(VerifyError::NoCompatibleVersion { .. })
        ));
        assert_eq!(
            snapshot.select("so.tilo.missing", &host(None)).unwrap_err(),
            VerifyError::NotListed("so.tilo.missing".into())
        );
    }
}

/// The compact form of a full snapshot, signed by the same owner.
fn compact_bytes(full: &[u8]) -> Vec<u8> {
    let full: SignedRegistrySnapshot = serde_json::from_slice(full).unwrap();
    let (mut compact, _) = registry_types::compact_snapshot(&full).unwrap();
    registry_types::sign_compact_snapshot(&mut compact, &owner()).unwrap();
    serde_json::to_vec(&compact).unwrap()
}

#[test]
fn the_checkpoint_refuses_rollback_and_forks() {
    let first = snapshot_bytes(1, "", vec![]);
    let verified = verify_snapshot(&first, &owner_hex(), None, NOW).unwrap();
    let checkpoint = Checkpoint::from_json(&verified.checkpoint().to_json()).unwrap();
    assert_eq!(checkpoint.sequence, 1);
    // The same snapshot again (a 304 served from cache) is fine.
    verify_snapshot(&first, &owner_hex(), Some(&checkpoint), NOW).unwrap();

    let second = snapshot_bytes(2, &checkpoint.snapshot_hash_hex, vec![]);
    let advanced = verify_snapshot(&second, &owner_hex(), Some(&checkpoint), NOW).unwrap();
    assert_eq!(advanced.sequence(), 2);
    assert_eq!(
        verify_snapshot(&first, &owner_hex(), Some(advanced.checkpoint()), NOW).unwrap_err(),
        VerifyError::Rollback {
            checkpoint: 2,
            sequence: 1
        }
    );
    let fork = snapshot_bytes(2, &"ee".repeat(32), vec![]);
    assert!(matches!(
        verify_snapshot(&fork, &owner_hex(), Some(advanced.checkpoint()), NOW),
        Err(VerifyError::Rollback { .. })
    ));
    // Both forms of one snapshot share its checkpoint; the compact form of a
    // replayed or forked snapshot is refused like the full one.
    verify_snapshot(
        &compact_bytes(&second),
        &owner_hex(),
        Some(advanced.checkpoint()),
        NOW,
    )
    .unwrap();
    for refused in [compact_bytes(&first), compact_bytes(&fork)] {
        assert!(matches!(
            verify_snapshot(&refused, &owner_hex(), Some(advanced.checkpoint()), NOW),
            Err(VerifyError::Rollback { .. })
        ));
    }
    assert!(matches!(
        Checkpoint::from_json(b"{\"sequence\":\"one\"}"),
        Err(VerifyError::Checkpoint(_))
    ));
}

#[test]
fn a_snapshot_must_be_signed_by_the_pinned_owner_and_fresh() {
    let bytes = snapshot_bytes(1, "", vec![]);
    let stranger = hex::encode(SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes());
    assert!(matches!(
        verify_snapshot(&bytes, &stranger, None, NOW),
        Err(VerifyError::Snapshot(_))
    ));
    assert!(matches!(
        verify_snapshot(&bytes, &"00".repeat(32), None, NOW),
        Err(VerifyError::Snapshot(_))
    ));
    assert!(matches!(
        verify_snapshot(&bytes, &owner_hex(), None, NOW + 3600),
        Err(VerifyError::Snapshot(_))
    ));
    let mut tampered = bytes.clone();
    let at = tampered.windows(3).position(|w| w == b"\"se").unwrap();
    tampered.insert(at, b' ');
    verify_snapshot(&tampered, &owner_hex(), None, NOW).expect("whitespace is not signed");
    let text = String::from_utf8(bytes)
        .unwrap()
        .replace("\"sequence\":1", "\"sequence\":3");
    assert!(verify_snapshot(text.as_bytes(), &owner_hex(), None, NOW).is_err());
    assert!(matches!(
        verify_snapshot(b"<html>", &owner_hex(), None, NOW),
        Err(VerifyError::SnapshotDecode(_))
    ));
}

#[test]
fn a_package_is_bound_to_the_selected_version() {
    let books = build("so.tilo.ebook", "app", "0.1.0", ABI, &[]);
    let newer = build("so.tilo.ebook", "app", "0.2.0", ABI, &[]);
    let bytes = snapshot_bytes(
        1,
        "",
        vec![entry(
            "so.tilo.ebook",
            vec![books.version.clone(), newer.version.clone()],
        )],
    );
    let snapshot = verify_snapshot(&bytes, &owner_hex(), None, NOW).unwrap();
    let selection = snapshot.select("so.tilo.ebook", &host(Some(ABI))).unwrap();
    assert_eq!(selection.version, "0.2.0");
    assert!(matches!(
        verify_package(&books.thin, &selection),
        Err(VerifyError::RootMismatch { .. })
    ));

    let mut other_publisher = selection.clone();
    other_publisher.publisher_pubkey_hex = "11".repeat(32);
    assert!(matches!(
        verify_package(&newer.thin, &other_publisher),
        Err(VerifyError::PublisherMismatch { .. })
    ));
    let mut other_target = selection.clone();
    other_target.target.runtime = Some(OTHER_ABI.into());
    assert_eq!(
        verify_package(&newer.thin, &other_target),
        Err(VerifyError::ManifestMismatch("[target]"))
    );
    let mut other_version = selection.clone();
    other_version.version = "0.3.0".into();
    assert_eq!(
        verify_package(&newer.thin, &other_version),
        Err(VerifyError::ManifestMismatch("version"))
    );

    // A forged signature changes the root, so it is caught even when the
    // attacker also controls the selection.
    let root = newer.meta.signed_root_bytes();
    let at = newer
        .thin
        .windows(root.len())
        .position(|window| window == root.as_slice())
        .unwrap();
    let mut forged = newer.thin;
    forged[at + root.len() - 1] ^= 1;
    let mut forged_selection = selection;
    forged_selection.package_root_hex = blake3_hex(&forged[at..at + root.len()]);
    let error = verify_package(&forged, &forged_selection).unwrap_err();
    assert!(
        matches!(error, VerifyError::Archive(_) | VerifyError::Signature),
        "{error}"
    );
}

#[test]
fn malformed_archives_fail_closed() {
    let books = build("so.tilo.ebook", "app", "0.1.0", ABI, &[]);
    let bytes = snapshot_bytes(
        1,
        "",
        vec![entry("so.tilo.ebook", vec![books.version.clone()])],
    );
    let snapshot = verify_snapshot(&bytes, &owner_hex(), None, NOW).unwrap();
    let selection = snapshot.select("so.tilo.ebook", &host(Some(ABI))).unwrap();
    for cut in [0, 100, 511, 700, books.thin.len() / 2] {
        assert!(
            verify_package(books.thin.get(..cut).unwrap(), &selection).is_err(),
            "{cut} bytes"
        );
    }
    let mut bad_checksum = books.thin;
    bad_checksum[0] ^= 1;
    assert!(verify_package(&bad_checksum, &selection).is_err());
    assert!(verify_package(&vec![0; 1024], &selection).is_err());
}
