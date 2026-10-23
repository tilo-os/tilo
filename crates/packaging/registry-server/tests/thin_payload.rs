//! Thin packages whose payload the publisher hosts as objects beside the
//! `.tapp`: review fetches the objects (whole blobs, or chunk lists and
//! their chunks) into the registry, verified and within the payload limit,
//! and installers fetch them back from `/blobs/` chunk by chunk.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use registry_server::{
    AppState, BundleReviewError, InspectionPolicy, ServerIdentity, Storage, bind_and_run,
};
use registry_types::{RegistryEntry, RegistryTarget, RegistryVersion};
use tilo_app_package::chunks::{self, ChunkList};
use tilo_app_package::{BlobFetcher, BlobHash, Manifest, SigningIdentity, pack};

const CHUNK: u64 = 64 * 1024;
const PROVIDES: &str = "tilo-runtime/2026.07";

/// Serve the files of `dir` at `http://<addr>/pkg/<name>`, like the
/// artifact proxy serves a release directory.
fn serve_dir(dir: PathBuf) -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let body = path
                .strip_prefix("/pkg/")
                .filter(|name| !name.contains('/'))
                .and_then(|name| std::fs::read(dir.join(name)).ok());
            let _ = match body {
                Some(body) => stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .and_then(|()| stream.write_all(&body)),
                None => stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
            };
        }
    });
    addr
}

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

struct Published {
    entry: RegistryEntry,
    objects: PathBuf,
    payload: BTreeMap<String, Vec<u8>>,
}

/// Pack `files` under `manifest`, split it into `<root>/objects` with
/// `CHUNK`-sized chunks, and describe the thin form at `base_url`.
fn publish(
    root: &Path,
    identity: &SigningIdentity,
    manifest: &str,
    files: &[(&str, Vec<u8>)],
    base_url: &str,
) -> Published {
    let payload_dir = root.join("payload");
    for (path, bytes) in files {
        let path = payload_dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let manifest = Manifest::from_toml_str(manifest).unwrap();
    let packed = pack(&payload_dir, &manifest, identity).unwrap();
    let full = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    let objects = root.join("objects");
    std::fs::create_dir_all(&objects).unwrap();
    let split = chunks::split_with_chunk_bytes(full.as_slice(), &objects, CHUNK).unwrap();
    let thin = tilo_app_package::tapp::to_bytes(&split.meta, None).unwrap();
    std::fs::write(objects.join("package.tapp"), &thin).unwrap();
    let entry = RegistryEntry {
        package_id: manifest.package.id.clone(),
        kind: registry_types::EntryKind::App,
        name: "Runtime".into(),
        description: "Shared runtime".into(),
        tagline: "Shared".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Developer Tools".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(identity.public_key_bytes()),
        versions: vec![RegistryVersion {
            version: manifest.package.version.clone(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: format!("{base_url}/package.tapp"),
                tapp_root_hash_hex: split.meta.id.to_hex(),
                tapp_size_bytes: thin.len() as u64,
                tapp_blake3_hex: blake3::hash(&thin).to_hex().to_string(),
                target: RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: manifest.package.native_runtime_binding().map(str::to_owned),
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 1,
        signature_hex: String::new(),
    };
    Published {
        entry,
        objects,
        payload: files
            .iter()
            .map(|(path, bytes)| ((*path).to_owned(), bytes.clone()))
            .collect(),
    }
}

fn runtime_manifest(identity: &SigningIdentity) -> String {
    format!(
        r#"
[package]
id = "so.tilo.runtime"
version = "2026.7.0"
kind = "runtime"
provides = "{PROVIDES}"

[package.isa]
x86_64 = "v3"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#,
        hex::encode(identity.public_key_bytes())
    )
}

fn app_manifest(identity: &SigningIdentity) -> String {
    format!(
        r#"
[package]
id = "so.tilo.editor"
version = "1.0.0"
kind = "app"

[package.isa]
x86_64 = "v3"

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
bin = "bin/editor"
type = "gui"
"#,
        hex::encode(identity.public_key_bytes())
    )
}

fn local_policy() -> InspectionPolicy {
    InspectionPolicy {
        allow_insecure_local: true,
        ..InspectionPolicy::default()
    }
}

fn review(
    storage: &Storage,
    entry: &RegistryEntry,
) -> Result<registry_server::InspectedVersion, BundleReviewError> {
    registry_server::review_version_bytes(storage, entry, &entry.versions[0], local_policy())
        .map(|(inspection, _bytes)| inspection)
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn thin_packages_review_from_hosted_objects_and_install_from_chunks() {
    // phase: a_thin_runtime_reviews_from_chunked_objects_and_serves_them
    {
        let tmp = tempfile::tempdir().unwrap();
        let identity = SigningIdentity::generate();
        let objects_dir = tmp.path().join("objects");
        let host = serve_dir(objects_dir.clone());
        let library = noise(5 * 64 * 1024 + 123, 1);
        let published = publish(
            tmp.path(),
            &identity,
            &runtime_manifest(&identity),
            &[
                ("bundle.toml", b"abi = \"test\"\n".to_vec()),
                ("lib/libruntime_dylib.so", library.clone()),
            ],
            &format!("http://{host}/pkg"),
        );
        let library_hash = BlobHash::of(&library);
        assert!(
            objects_dir
                .join(chunks::chunk_list_name(&library_hash))
                .exists()
        );

        let storage = Storage::open(tmp.path().join("data")).unwrap();
        let entry = published.entry.clone();
        let review_storage = storage.clone();
        let inspection = tokio::task::spawn_blocking(move || review(&review_storage, &entry))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            inspection.payload_bytes,
            library.len() as u64 + b"abi = \"test\"\n".len() as u64
        );
        assert_eq!(
            storage.payload_blob_len(&library_hash).unwrap(),
            Some(library.len() as u64)
        );
        // Stored as chunks, never reassembled.
        assert_eq!(
            storage.load_payload_blob(&library_hash.to_hex()).unwrap(),
            None
        );
        let list = storage
            .load_chunk_list(&library_hash.to_hex())
            .unwrap()
            .unwrap();
        assert_eq!(list.chunks.len(), 6);

        // Publication inspects what collection stored while it holds the
        // registry lock, and must not take it again.
        let thin = std::fs::read(published.objects.join("package.tapp")).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let (held_storage, held_entry) = (storage.clone(), published.entry.clone());
        std::thread::spawn(move || {
            let _registry = held_storage.lock_snapshot().unwrap();
            let inspected = registry_server::inspect_stored_bytes(
                &held_storage,
                &held_entry,
                &held_entry.versions[0],
                &thin,
                None,
            );
            let _ = done_tx.send(inspected.map(|inspection| inspection.payload_bytes));
        });
        let under_lock = tilo_test_env::recv(
            "inspecting a stored thin package under the registry lock to return",
            &done_rx,
        );
        assert_eq!(under_lock.unwrap(), inspection.payload_bytes);

        storage
            .save_payload_blobs(&std::fs::read(published.objects.join("package.tapp")).unwrap())
            .unwrap();
        let identity_record = ServerIdentity {
            owner_pubkey_hex: "00".repeat(32),
            name: "test".into(),
            description: "test".into(),
            base_url: "http://127.0.0.1".into(),
        };
        let addr = bind_and_run(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            AppState::new(storage.clone(), identity_record),
        )
        .await
        .unwrap();
        let client = tilo_net::http::client().build().unwrap();
        let served = client
            .get(format!("http://{addr}/blobs/{library_hash}.chunks"))
            .send()
            .await
            .unwrap();
        assert_eq!(served.status(), reqwest::StatusCode::OK);
        assert_eq!(served.headers()["content-type"], "application/json");
        assert_eq!(
            served.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        let served = ChunkList::decode(&served.bytes().await.unwrap(), &library_hash).unwrap();
        assert_eq!(served, list);
        let whole = client
            .get(format!("http://{addr}/blobs/{library_hash}"))
            .send()
            .await
            .unwrap();
        assert_eq!(whole.status(), reqwest::StatusCode::NOT_FOUND);
        let chunk = client
            .get(format!("http://{addr}/blobs/{}", list.chunks[0].hash))
            .send()
            .await
            .unwrap();
        assert_eq!(chunk.bytes().await.unwrap().len() as u64, CHUNK);

        // An installer fetches the thin package and its payload, chunk by
        // verified chunk, and materializes the tree.
        let registry = app_registry::HttpBlobs::new(format!("http://{addr}"));
        let payload = published.payload.clone();
        // Installers are synchronous: no async runtime on the fetching thread.
        let install = std::thread::spawn(move || {
            let thin = tilo_app_package::PackageFetcher::fetch_package(
                &registry,
                &tilo_app_package::PackageId::from_bytes(list_root(&published)),
            )
            .unwrap()
            .unwrap();
            let tapp =
                tilo_app_package::tapp::read(thin.as_slice(), &mut tilo_app_package::DiscardBlobs)
                    .unwrap();
            let mut blobs = BTreeMap::new();
            registry.fetch(&tapp.missing(), &mut blobs).unwrap();
            let tree = tempfile::tempdir().unwrap();
            let dest = tree.path().join("runtime");
            tilo_app_package::unpack(&tapp.meta, &blobs, &dest).unwrap();
            for (path, bytes) in &payload {
                assert_eq!(&std::fs::read(dest.join(path)).unwrap(), bytes, "{path}");
            }
            let absent = BlobHash::of(b"never reviewed");
            let error = registry
                .fetch(&[absent], &mut BTreeMap::new())
                .unwrap_err()
                .to_string();
            assert!(error.contains(&absent.to_hex()), "{error}");
        });
        tilo_test_env::within("the install to finish", async {
            while !install.is_finished() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        install.join().unwrap();
    }
    // phase: a_tampered_chunk_or_missing_object_fails_review_and_publishes_no_list
    {
        let tmp = tempfile::tempdir().unwrap();
        let identity = SigningIdentity::generate();
        let objects_dir = tmp.path().join("objects");
        let host = serve_dir(objects_dir.clone());
        let library = noise(3 * 64 * 1024 + 5, 2);
        let published = publish(
            tmp.path(),
            &identity,
            &runtime_manifest(&identity),
            &[
                ("bundle.toml", b"abi = \"x\"\n".to_vec()),
                ("lib/libruntime_dylib.so", library.clone()),
            ],
            &format!("http://{host}/pkg"),
        );
        let hash = BlobHash::of(&library);
        let list = ChunkList::decode(
            &std::fs::read(objects_dir.join(chunks::chunk_list_name(&hash))).unwrap(),
            &hash,
        )
        .unwrap();
        let chunk_path = objects_dir.join(chunks::blob_object_name(&list.chunks[1].hash));
        let original = std::fs::read(&chunk_path).unwrap();
        let mut tampered = original.clone();
        tampered[7] ^= 0xff;
        std::fs::write(&chunk_path, &tampered).unwrap();

        let storage = Storage::open(tmp.path().join("data")).unwrap();
        let (entry, review_storage) = (published.entry.clone(), storage.clone());
        let error = tokio::task::spawn_blocking(move || review(&review_storage, &entry))
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error, BundleReviewError::PayloadObject { .. }),
            "{error}"
        );
        assert_eq!(storage.payload_blob_len(&hash).unwrap(), None);
        assert_eq!(storage.load_chunk_list(&hash.to_hex()).unwrap(), None);

        std::fs::write(&chunk_path, &original).unwrap();
        std::fs::remove_file(objects_dir.join(chunks::chunk_list_name(&hash))).unwrap();
        let (entry, review_storage) = (published.entry.clone(), storage.clone());
        let error = tokio::task::spawn_blocking(move || review(&review_storage, &entry))
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(&error, BundleReviewError::MissingPayload { blob, .. } if *blob == hash.to_hex()),
            "{error}"
        );
    }
    // phase: a_thin_app_is_held_to_the_application_budget
    {
        let tmp = tempfile::tempdir().unwrap();
        let identity = SigningIdentity::generate();
        let objects_dir = tmp.path().join("objects");
        let host = serve_dir(objects_dir.clone());
        let oversized = usize::try_from(tilo_app_package::MAX_APP_TAPP_BYTES).unwrap() + 1;
        let published = publish(
            tmp.path(),
            &identity,
            &app_manifest(&identity),
            &[
                ("bin/editor", b"#!/bin/true\n".to_vec()),
                ("share/data", noise(oversized, 3)),
            ],
            &format!("http://{host}/pkg"),
        );
        let storage = Storage::open(tmp.path().join("data")).unwrap();
        let (entry, review_storage) = (published.entry.clone(), storage.clone());
        let error = tokio::task::spawn_blocking(move || review(&review_storage, &entry))
            .await
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error, BundleReviewError::TooLarge { limit, .. } if limit == tilo_app_package::MAX_APP_TAPP_BYTES),
            "{error}"
        );
    }
}

fn list_root(published: &Published) -> [u8; 32] {
    let mut root = [0u8; 32];
    hex::decode_to_slice(
        &published.entry.versions[0]
            .tapp()
            .unwrap()
            .tapp_root_hash_hex,
        &mut root,
    )
    .unwrap();
    root
}
