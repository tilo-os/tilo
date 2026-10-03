//! Web builds are ordinary signed packages whose payload is content
//! addressed, so what a browser downloads per release is the metadata plus
//! the blobs it has never seen.

use std::collections::BTreeMap;

use tilo_app_package::blobs::DiscardBlobs;
use tilo_app_package::web::{
    RUNTIME_ABI_SECTION, WebBuildOptions, inspect_wasm_module, web_manifest,
};
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp, verify};

use crate::fixtures::SAMPLE_APP_MANIFEST_TEMPLATE;

const ABI: &str = "tilo-runtime/2025.06+wasm.0f1e2d3c4b5a69788796a5b4c3d2e1f0";

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

/// A side module of roughly `code_bytes`, built against [`ABI`].
fn side_module(code_bytes: usize, seed: u8) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    out.extend(custom(RUNTIME_ABI_SECTION, ABI.as_bytes()));
    let mut features = leb(2);
    for name in ["mutable-globals", "sign-ext"] {
        features.push(b'+');
        features.extend(leb(name.len()));
        features.extend_from_slice(name.as_bytes());
    }
    out.extend(custom("target_features", &features));
    let code: Vec<u8> = (0..code_bytes)
        .map(|i| u8::try_from(i % 251).unwrap() ^ seed)
        .collect();
    out.extend(custom("filler", &code));
    out
}

struct Built {
    thin: Vec<u8>,
    full: Vec<u8>,
    payload_bytes: u64,
    meta: tilo_app_package::PackageMeta,
}

fn build(identity: &SigningIdentity, version: &str, module: &[u8], font: &[u8]) -> Built {
    let authored = Manifest::from_toml_str(
        &SAMPLE_APP_MANIFEST_TEMPLATE
            .replace("<HEX>", &hex::encode(identity.public_key_bytes()))
            .replace(
                "version     = \"1.4.2\"",
                &format!("version = \"{version}\""),
            ),
    )
    .unwrap();
    let info = inspect_wasm_module(module).unwrap();
    let manifest =
        web_manifest(&authored, "editor.wasm", &info, &WebBuildOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("editor.wasm"), module).unwrap();
    std::fs::create_dir_all(dir.path().join("assets/fonts")).unwrap();
    std::fs::write(dir.path().join("assets/fonts/body.ttf"), font).unwrap();
    let packed = pack(dir.path(), &manifest, identity).unwrap();
    let thin = tapp::to_bytes(&packed.meta, None).unwrap();
    let full = tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    Built {
        thin,
        full,
        payload_bytes: module.len().saturating_add(font.len()) as u64,
        meta: packed.meta,
    }
}

fn payload(meta: &tilo_app_package::PackageMeta) -> BTreeMap<String, String> {
    meta.collection
        .entries
        .iter()
        .map(|(path, hash)| (path.clone(), hash.to_hex()))
        .collect()
}

#[test]
fn a_web_build_signs_and_verifies_like_a_native_package() {
    let identity = SigningIdentity::generate();
    let built = build(
        &identity,
        "1.0.0",
        &side_module(64 * 1024, 1),
        &vec![7; 20_000],
    );
    let decoded = tapp::read(built.full.as_slice(), &mut DiscardBlobs).unwrap();
    assert!(decoded.is_complete());
    let verification = verify(&decoded.meta, &[identity.public_key_bytes()]);
    let manifest = verification.manifest().expect("signature verifies");
    let target = manifest.target.as_ref().unwrap();
    assert_eq!(target.platform, "web");
    assert_eq!(target.isa, "wasm32-mvp+mutable-globals+sign-ext");
    assert_eq!(target.runtime, ABI);
    assert_eq!(
        payload(&decoded.meta).keys().collect::<Vec<_>>(),
        ["assets/fonts/body.ttf", "editor.wasm"]
    );
}

#[test]
fn the_format_adds_kilobytes_not_a_copy_of_the_payload() {
    let identity = SigningIdentity::generate();
    let built = build(
        &identity,
        "1.0.0",
        &side_module(900 * 1024, 1),
        &vec![7; 300_000],
    );
    let blobs = built.meta.payload_hashes().len() as u64;
    let overhead = (built.full.len() as u64).saturating_sub(built.payload_bytes);
    // Tar framing: one 512-byte header per entry, padding to 512, and the
    // 1 KiB end marker around the four metadata entries.
    assert!(
        overhead <= blobs.saturating_mul(1024).saturating_add(6 * 1024),
        "full web tapp adds {overhead} bytes to {} payload bytes",
        built.payload_bytes
    );
    assert!(
        built.thin.len() <= 6 * 1024,
        "thin web tapp is {} bytes",
        built.thin.len()
    );
}

#[test]
fn unchanged_blobs_keep_their_address_across_releases() {
    let identity = SigningIdentity::generate();
    let font = vec![9; 120_000];
    let first = build(&identity, "1.0.0", &side_module(200 * 1024, 1), &font);
    let second = build(&identity, "1.0.1", &side_module(200 * 1024, 2), &font);
    let (first, second) = (payload(&first.meta), payload(&second.meta));
    assert_eq!(
        first["assets/fonts/body.ttf"], second["assets/fonts/body.ttf"],
        "an unchanged asset is the same blob, so a client never fetches it twice"
    );
    assert_ne!(first["editor.wasm"], second["editor.wasm"]);
}
