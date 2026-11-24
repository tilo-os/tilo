//! A server applies the action of an app packaged by another release: the
//! model runtime the package's module links against is not in this server's
//! image, so the server fetches it from the registry that offers the runtime
//! bundle the package names, verifies it, keeps it, and links against it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a fixture that cannot be built, and an assertion that does not hold, fail the test"
)]

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryVersion, SignedRegistrySnapshot, sign_entry,
    sign_snapshot,
};
use tilo_app_package::{Manifest, SigningIdentity, pack, tapp};
use tilo_model_engine::{RUNTIME_DIR, RUNTIME_SECTION, Runtime};
use tilo_resource::Resource as _;

use super::fixtures::{governor, test_host};
use super::packaged_model::PackagedAction;
use super::packaged_model_tests::{
    APP, COLLECTION, NOW_MS, add_connection, model_module, models_dir, package_on,
};

/// The release the registry's runtime bundle is built for.
const RELEASE: &str = "2026.07";

/// The machine the registry offers its runtime package for: architecture,
/// the ISA level a package names, and the one its entry names.
fn native_target() -> (&'static str, &'static str, &'static str) {
    #[cfg(target_arch = "x86_64")]
    {
        ("x86_64", "baseline", "x86_64")
    }
    #[cfg(target_arch = "aarch64")]
    {
        ("aarch64", "armv8-a", "aarch64")
    }
}

/// An unsigned LEB128 encoding of `value`.
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

/// An unsigned LEB128 number at the start of `bytes`, and what follows it.
fn read_leb(bytes: &[u8]) -> (usize, &[u8]) {
    let mut value = 0_usize;
    for (index, byte) in bytes.iter().enumerate() {
        value |= usize::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return (value, &bytes[index + 1..]);
        }
    }
    panic!("a LEB128 number goes past the module");
}

/// A custom section `name` holding `data`, as a module carries it.
fn custom_section(name: &str, data: &[u8]) -> Vec<u8> {
    let mut contents = leb(name.len());
    contents.extend_from_slice(name.as_bytes());
    contents.extend_from_slice(data);
    let mut section = vec![0];
    section.extend(leb(contents.len()));
    section.extend(contents);
    section
}

/// `module` with its [`RUNTIME_SECTION`] naming `runtime` instead, and the
/// same entry: the app's code, as another release's split would name it.
fn relinked(module: &[u8], runtime: &str) -> Vec<u8> {
    let mut out = module[..8].to_vec();
    let mut at = &module[8..];
    while let Some((&id, rest)) = at.split_first() {
        let (size, rest) = read_leb(rest);
        let (contents, after) = rest.split_at(size);
        let (name_len, named) = read_leb(contents);
        if id == 0 && &named[..name_len] == RUNTIME_SECTION.as_bytes() {
            let mut section: serde_json::Value =
                serde_json::from_slice(&named[name_len..]).unwrap();
            section["runtime"] = runtime.into();
            out.extend(custom_section(
                RUNTIME_SECTION,
                serde_json::to_vec(&section).unwrap().as_slice(),
            ));
        } else {
            out.push(id);
            out.extend(leb(size));
            out.extend_from_slice(contents);
        }
        at = after;
    }
    out
}

/// Another release's model runtime, written to `to`: the same code as the
/// runtime at `from`, whose start module carries one more custom section, so
/// its bytes and its hash differ. Returns its hash.
fn another_release(from: &Path, to: &Path) -> String {
    std::fs::create_dir_all(to).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(from.join("runtime.json")).unwrap()).unwrap();
    let mut hasher = blake3::Hasher::new();
    for (index, file) in manifest["modules"].as_array().unwrap().iter().enumerate() {
        let file = file.as_str().unwrap();
        let mut module = std::fs::read(from.join(file)).unwrap();
        if index == 0 {
            module.extend(custom_section("tilo_release", RELEASE.as_bytes()));
        }
        hasher.update(&(module.len() as u64).to_le_bytes());
        hasher.update(&module);
        std::fs::write(to.join(file), module).unwrap();
    }
    let hash = hasher.finalize().to_hex().to_string();
    manifest["hash"] = hash.clone().into();
    std::fs::write(
        to.join("runtime.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    hash
}

/// A signed runtime bundle of [`RELEASE`] carrying the model runtime at
/// `model_runtime`, as the release pack bakes one; returns its version and
/// directory, and the BLAKE3 of the loader beside the model runtime.
fn runtime_bundle(root: &Path, model_runtime: &Path, identity: &Path) -> (String, PathBuf, String) {
    let machine = root.join("machine");
    let loader = runtime_bundle::host_minimal_paths_for(runtime_bundle::host_arch())
        .unwrap()
        .first()
        .unwrap()
        .trim_start_matches('/');
    std::fs::create_dir_all(machine.join(loader).parent().unwrap()).unwrap();
    let loader_bytes = b"the release's loader, which a model never needs".to_vec();
    std::fs::write(machine.join(loader), &loader_bytes).unwrap();
    let (manifest, dir) = runtime_bundle::build_abi_bundle(
        &root.join("bundles"),
        RELEASE,
        &runtime_bundle::BuildOptions {
            from: Some(&machine),
            arch: None,
            identity: Some(identity),
            extra_libs: &[model_runtime.to_owned()],
            extra_bins: &[],
        },
    )
    .unwrap();
    assert!(
        dir.join("lib")
            .join(RUNTIME_DIR)
            .join("runtime.json")
            .is_file(),
        "the bundle carries the model runtime under lib/{RUNTIME_DIR}"
    );
    (
        manifest.version,
        dir,
        blake3::hash(&loader_bytes).to_hex().to_string(),
    )
}

/// The registry's runtime package of `bundle`, providing `provides`, signed
/// by `publisher`.
fn runtime_package(bundle: &Path, provides: &str, publisher: &SigningKey) -> Vec<u8> {
    let manifest = Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{id}"
version = "0.1.7"
kind = "runtime"
name = "Tilo runtime"
provides = "{provides}"

[package.isa]
{arch} = "{isa}"

[publisher]
identity = "ed25519:{key}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#,
        id = registry_types::NATIVE_RUNTIME_PACKAGE_ID,
        arch = native_target().0,
        isa = native_target().1,
        key = hex::encode(publisher.verifying_key().to_bytes()),
    ))
    .unwrap();
    let packed = pack(
        bundle,
        &manifest,
        &SigningIdentity::from_signing_key(publisher.clone()),
    )
    .unwrap();
    tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

/// The publisher-signed entry offering the thin runtime package `thin`
/// (whose root is `root`) at `url`.
fn runtime_entry(
    url: &str,
    thin: &[u8],
    root: &str,
    provides: &str,
    publisher: &SigningKey,
) -> RegistryEntry {
    let mut entry = RegistryEntry {
        package_id: registry_types::NATIVE_RUNTIME_PACKAGE_ID.into(),
        kind: registry_types::EntryKind::App,
        name: "Tilo runtime".into(),
        description: "The runtime every app of a release links".into(),
        tagline: "Runtime".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec!["System".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: "0.1.7".into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: url.into(),
                tapp_root_hash_hex: root.into(),
                tapp_size_bytes: thin.len() as u64,
                tapp_blake3_hex: blake3::hash(thin).to_hex().to_string(),
                tapp_surface_hex: String::new(),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: native_target().0.into(),
                    isa: native_target().2.into(),
                    runtime: Some(provides.into()),
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, publisher);
    entry
}

/// The registry owner's signed snapshot listing `entries` by `publisher`.
fn snapshot(
    entries: Vec<RegistryEntry>,
    owner: &SigningKey,
    publisher: &SigningKey,
) -> SignedRegistrySnapshot {
    let now = tilo_clock::unix_now();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: 1,
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        previous_snapshot_hash_hex: String::new(),
        moderation_head_hex: String::new(),
        publishers: vec![PublisherAuthorization {
            pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
            can_add_new: true,
        }],
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    sign_snapshot(&mut snapshot, owner).unwrap();
    snapshot
}

/// The paths the registry was asked for since the last call.
fn asked(registry: &test_http::Responder) -> Vec<String> {
    registry
        .drain_requests()
        .into_iter()
        .map(|request| request.target)
        .collect()
}

#[tokio::test]
#[ignore = "reads the model module the app-models check built (t test test-packaged-model)"]
#[expect(
    clippy::too_many_lines,
    reason = "one server, one registry, the phases of one fetch"
)]
async fn a_server_fetches_the_model_runtime_another_releases_package_names() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();

    // This server's image carries its own release's runtime; the package was
    // built by another release, whose runtime is the same code in other bytes.
    let ours = Runtime::read(&models_dir().join(RUNTIME_DIR)).unwrap();
    let theirs = root.join("release").join(RUNTIME_DIR);
    let hash = another_release(ours.dir(), &theirs);
    assert_ne!(hash, ours.hash());
    let missing = Runtime::find(&hash).expect_err("the server lacks the other release's runtime");
    assert!(missing.to_string().contains(&hash), "{missing}");
    let module = relinked(&model_module("db-client"), &hash);

    // The registry offers that release's runtime bundle as a thin package,
    // its files as payload blobs, signed by a runtime publisher the server trusts.
    let runtime_identity = runtime_bundle::Identity::generate();
    let identity_path = root.join("runtime-publisher.json");
    std::fs::write(&identity_path, runtime_identity.to_json_bytes()).unwrap();
    let trust = root.join("trusted-runtime-publishers.txt");
    std::fs::write(&trust, format!("{}\n", runtime_identity.public_key_hex())).unwrap();
    let (version, bundle, loader) = runtime_bundle(root, &theirs, &identity_path);
    let provides = format!("tilo-runtime/{version}");
    let publisher = SigningKey::from_bytes(&[21_u8; 32]);
    let owner = SigningKey::from_bytes(&[22_u8; 32]);
    let full = runtime_package(&bundle, &provides, &publisher);
    let objects = root.join("objects");
    std::fs::create_dir_all(&objects).unwrap();
    let split = tilo_app_package::chunks::split(full.as_slice(), &objects).unwrap();
    let thin = tapp::to_bytes(&split.meta, None).unwrap();
    let mut files = std::collections::BTreeMap::new();
    for object in std::fs::read_dir(&objects).unwrap() {
        let object = object.unwrap().path();
        let name = object.file_name().unwrap().to_str().unwrap().to_owned();
        let served_as = name.strip_suffix(".blob").unwrap_or(&name).to_owned();
        files.insert(
            format!("/blobs/{served_as}"),
            std::fs::read(&object).unwrap(),
        );
    }
    files.insert("/runtime.tapp".to_owned(), thin.clone());
    // The entry names the package's URL, which needs the registry's address:
    // the registry answers from a map the entry is put into once it is known.
    let offered = std::sync::Arc::new(std::sync::Mutex::new(files));
    let answering = std::sync::Arc::clone(&offered);
    let registry = test_http::Responder::handle(move |request| {
        let path = request.target.split('?').next().unwrap_or_default();
        Some(match answering.lock().unwrap().get(path) {
            Some(body) => test_http::Reply::status(200).body(body.clone()),
            None => test_http::Reply::status(404),
        })
    });
    let base = registry.url();
    let entry = runtime_entry(
        &format!("{base}/runtime.tapp"),
        &thin,
        &split.meta.id.to_hex(),
        &provides,
        &publisher,
    );
    offered.lock().unwrap().insert(
        "/entries".to_owned(),
        serde_json::to_vec(&snapshot(vec![entry], &owner, &publisher)).unwrap(),
    );

    let config = root.join("store-config");
    let cache = root.join("store-cache");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("registries.toml"),
        format!(
            "[[registry]]\nname = \"local\"\nbase_url = \"{base}\"\nowner_pubkey_hex = \"{}\"\nenabled = true\n",
            hex::encode(owner.verifying_key().to_bytes())
        ),
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_STORE_CONFIG_DIR", &config)
        .set("TILO_STORE_CACHE_DIR", &cache)
        .set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trust);

    let (key, cert) = governor();
    let server = test_host(&root.join("server-state"), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Another release").await.unwrap();
    let package = package_on(APP, Some(&module), Some(&provides));
    assert!(
        workspaces
            .prepare_packaged_application(
                &created.workspace,
                &cert,
                APP,
                COLLECTION,
                "",
                &package
            )
            .await
            .unwrap(),
        "the server makes the app's resource with the runtime it fetched"
    );
    let fetched = asked(&registry);
    assert!(
        fetched.iter().any(|path| path.starts_with("/runtime.tapp")),
        "the server fetched the runtime package: {fetched:?}"
    );
    assert!(
        !fetched.iter().any(|path| path.contains(&loader)),
        "the server fetched only the model runtime, not the bundle's libraries: {fetched:?}"
    );
    let kept = cache.join(RUNTIME_DIR).join(&hash);
    assert_eq!(
        Runtime::read(&kept).unwrap().hash(),
        hash,
        "the runtime is kept on disk by its hash"
    );

    let apply = |package: Vec<u8>| {
        let member = tilo_actions::MemberId::person(hex::encode(cert.user_public_key));
        let workspace = created.workspace.clone();
        let cert = cert.clone();
        async move {
            workspaces
                .apply_packaged_action(
                    &workspace,
                    &cert,
                    PackagedAction {
                        app: APP,
                        kind: COLLECTION,
                        document: "",
                        package: &package,
                        action: "add-connection",
                        arguments: add_connection("Ship it"),
                        member: member.clone(),
                        acting: tilo_actions::Acting::member(member),
                        now_ms: NOW_MS,
                    },
                )
                .await
        }
    };
    apply(package.clone()).await.unwrap();
    assert!(
        asked(&registry).is_empty(),
        "an action of a runtime fetched before asks the registry nothing"
    );
    let after = workspaces
        .resource_document(&created.workspace, APP, "")
        .await
        .unwrap()
        .expect("the document exists");
    let workspace = db_client_model::Workspace::read(&automerge::AutoCommit::load(&after).unwrap()).unwrap();
    assert!(
        workspace
            .connections
            .values()
            .any(|connection| connection.name.as_str() == "Ship it"),
        "the app's code, linked after the fetched runtime, applied the action"
    );

    // A module naming a runtime the bundle it names does not carry is
    // refused, naming the runtime it wants and the one the bundle holds.
    let wanted = "0".repeat(64);
    let refused = apply(package_on(
        APP,
        Some(&relinked(&module, &wanted)),
        Some(&provides),
    ))
    .await
    .expect_err("a runtime that is not the one the module names is refused")
    .to_string();
    assert!(
        refused.contains(&wanted) && refused.contains(&hash),
        "{refused}"
    );
    assert!(
        !cache.join(RUNTIME_DIR).join(&wanted).exists(),
        "a refused runtime is not kept"
    );
    server.shutdown().await;
    drop(env);
}
