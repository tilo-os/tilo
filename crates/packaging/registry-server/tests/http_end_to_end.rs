//! End-to-end HTTP tests — start the registry server on an ephemeral
//! port, drive it with reqwest as an
//! on-device client would, and assert the wire behaviour.
//!
//! These tests verify the whole loop: signature construction ↔
//! JSON round-trip ↔ server-side verify ↔ disk write ↔ GET.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use registry_server::serve::ServeOptions;
use registry_server::{
    AppState, ModerationService, PublisherAllowlist, PublisherRow, ServerIdentity, Storage,
    bind_and_run,
};
use registry_types::{
    EntryAction, ExternalModerationOperation, ModerationActorRole, ModerationCase,
    ModerationDestination, ModerationMutation, RegistryEntry, RegistryHandshake, RegistryVersion,
    SignedModerationMutation, SignedRegistrySnapshot, TransparencyPage, sign_change_request,
    sign_external_moderation,
};

/// Start a server against a fresh tempdir. Returns the bound
/// address + the tempdir handle (kept alive by the caller so the
/// dir isn't cleaned up mid-test) + the owner + first publisher
/// key.
struct Harness {
    tmp: tempfile::TempDir,
    addr: SocketAddr,
    state: AppState,
    storage: Storage,
    allowlist: PublisherAllowlist,
    publisher: SigningKey,
    owner: SigningKey,
    /// Second publisher who is NOT allowlisted — used for the
    /// negative test.
    outsider: SigningKey,
}

async fn spawn_server(publisher_can_add_new: bool) -> Harness {
    spawn_server_with(publisher_can_add_new, ServeOptions::default()).await
}

async fn spawn_server_with(publisher_can_add_new: bool, options: ServeOptions) -> Harness {
    let _ = tracing_subscriber::fmt::try_init();

    let tmp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(tmp.path()).expect("open storage");

    let owner_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let identity = ServerIdentity {
        owner_pubkey_hex: hex::encode(owner_key.verifying_key().to_bytes()),
        name: "test registry".into(),
        description: "for tests".into(),
        base_url: "http://127.0.0.1".into(),
    };
    storage.save_identity(&identity).expect("save identity");

    let publisher = SigningKey::generate(&mut UnwrapErr(SysRng));
    let outsider = SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut allow = PublisherAllowlist::empty();
    allow.upsert(PublisherRow {
        pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        can_add_new: publisher_can_add_new,
        note: "test publisher".into(),
        namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
    });
    storage.save_allowlist(&allow).expect("save allowlist");

    publish_snapshot(&storage, &allow, &owner_key);

    let state = AppState::with_options(storage.clone(), identity, options);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let bound = bind_and_run(addr, state.clone()).await.expect("bind");

    // Give the tokio::spawn'd accept loop a beat to actually be
    // ready to accept. On a busy loopback stack the first
    // `TcpStream::connect` occasionally beats the accept.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    Harness {
        tmp,
        addr: bound,
        state,
        storage,
        allowlist: allow,
        publisher,
        owner: owner_key,
        outsider,
    }
}

fn sign_moderation_mutation(
    request_id: String,
    issued_at_unix: u64,
    expires_at_unix: u64,
    role: ModerationActorRole,
    mutation: ModerationMutation,
    key: &SigningKey,
    owner: &SigningKey,
) -> Result<SignedModerationMutation, registry_types::ModerationWireError> {
    let request = sign_external_moderation(
        &ExternalModerationOperation {
            destination: ModerationDestination {
                base_url: "http://127.0.0.1".into(),
                owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
            },
            request_id,
            issued_at_unix,
            expires_at_unix,
            mutation,
        },
        key,
    )?;
    assert_eq!(request.actor_role, role);
    Ok(request)
}

fn signed_report(
    key: &SigningKey,
    owner: &SigningKey,
    request_id: &str,
    now: u64,
) -> SignedModerationMutation {
    sign_moderation_mutation(
        request_id.into(),
        now,
        now.saturating_add(300),
        ModerationActorRole::Reporter,
        ModerationMutation::FileReport {
            package_id: "so.tilo.editor".into(),
            category: "security".into(),
            summary: "Unexpected outbound connection during offline editing".into(),
            evidence_digest_hex: "ab".repeat(32),
        },
        key,
        owner,
    )
    .unwrap()
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn moderation_http_is_signed_authorised_and_bounded() {
    // phase: moderation_http_is_signed_idempotent_authorized_and_verifiable
    {
        let h = spawn_server(true).await;
        let entry = sample_entry(&h.publisher, 100);
        let client = tilo_net::http::client().build().expect("http client");
        client
            .post(url(h.addr, "/entries"))
            .json(&sign_change_request(EntryAction::Add, entry, &h.publisher))
            .send()
            .await
            .unwrap();
        approve(&h, "so.tilo.editor");

        let now = tilo_clock::try_unix_now().unwrap();
        let report = signed_report(&h.outsider, &h.owner, "http-report-one", now);
        let response = client
            .post(url(h.addr, "/moderation/mutations"))
            .json(&report)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let case: ModerationCase = response.json().await.unwrap();

        let retry: ModerationCase = client
            .post(url(h.addr, "/moderation/mutations"))
            .json(&report)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(retry, case);
        let fetched: ModerationCase = client
            .get(url(h.addr, &format!("/moderation/cases/{}", case.case_id)))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(fetched, case);

        let page: TransparencyPage = client
            .get(url(h.addr, "/moderation/transparency?from=1&limit=20"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
        registry_types::verify_transparency_chain(&page.events, "").unwrap();

        let forged = sign_moderation_mutation(
            "forged-decision".into(),
            now,
            now.saturating_add(300),
            ModerationActorRole::Reviewer,
            ModerationMutation::ReviewerDecision {
                case_id: case.case_id.clone(),
                decision: registry_types::ModerationDecision::NoAction,
                reason: "Not enough evidence to establish the reported behavior".into(),
            },
            &h.outsider,
            &h.owner,
        )
        .unwrap();
        assert_eq!(
            client
                .post(url(h.addr, "/moderation/mutations"))
                .json(&forged)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        let owner_decision = sign_moderation_mutation(
            "owner-decision".into(),
            now,
            now.saturating_add(300),
            ModerationActorRole::Reviewer,
            ModerationMutation::ReviewerDecision {
                case_id: case.case_id,
                decision: registry_types::ModerationDecision::NoAction,
                reason: "Not enough evidence to establish the reported behavior".into(),
            },
            &h.owner,
            &h.owner,
        )
        .unwrap();
        assert_eq!(
            client
                .post(url(h.addr, "/moderation/mutations"))
                .json(&owner_decision)
                .send()
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::OK
        );
    }
    // phase: oversized_moderation_body_is_rejected_before_parse_or_disk_write
    {
        let h = spawn_server(true).await;
        let response = tilo_net::http::client()
            .build()
            .expect("http client")
            .post(url(h.addr, "/moderation/mutations"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body("x".repeat(70 * 1024))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
        let page: TransparencyPage = tilo_net::http::client()
            .build()
            .expect("http client")
            .get(url(h.addr, "/moderation/transparency?from=1&limit=10"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(page.events.is_empty());
    }
}

fn approve(h: &Harness, package_id: &str) {
    let moderation = ModerationService::new(h.storage.clone());
    let pending = moderation.pending(package_id).unwrap().unwrap();
    moderation
        .approve(&pending, "test-owner", "reviewed", 500, &h.allowlist)
        .expect("approve queued submission");
    publish_snapshot(&h.storage, &h.allowlist, &h.owner);
}

fn publish_snapshot(storage: &Storage, allowlist: &PublisherAllowlist, owner: &SigningKey) {
    publish_snapshot_of(storage, allowlist, owner, storage.load_entries().unwrap());
}

fn publish_snapshot_of(
    storage: &Storage,
    allowlist: &PublisherAllowlist,
    owner: &SigningKey,
    mut entries: Vec<RegistryEntry>,
) {
    let previous = storage.load_snapshot().unwrap();
    entries.sort_by(|left, right| left.package_id.cmp(&right.package_id));
    let now = tilo_clock::try_unix_now().unwrap();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: previous
            .as_ref()
            .map_or(1, |old| old.sequence.saturating_add(1)),
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(86_400),
        previous_snapshot_hash_hex: previous
            .as_ref()
            .map(|old| registry_types::snapshot_hash(old).unwrap())
            .unwrap_or_default(),
        moderation_head_hex: storage.moderation_head().unwrap(),
        publishers: allowlist
            .publishers
            .iter()
            .map(|row| registry_types::PublisherAuthorization {
                pubkey_hex: row.pubkey_hex.clone(),
                can_add_new: row.can_add_new,
            })
            .collect(),
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
    };
    registry_types::sign_snapshot(&mut snapshot, owner).unwrap();
    storage.save_snapshot(&snapshot).unwrap();
}

fn sample_entry(publisher: &SigningKey, updated_at: u64) -> RegistryEntry {
    RegistryEntry {
        package_id: "so.tilo.editor".into(),
        kind: registry_types::EntryKind::App,
        name: "Editor".into(),
        description: "CRDT-native text editor".into(),
        tagline: "Edit together".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Productivity".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: "2026.06".into(),
            released_at_unix: 1_800_000_000,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: "https://apps.tilo.so/artifacts/editor/2026.06.tapp".into(),
                tapp_root_hash_hex: "cd".repeat(32),
                tapp_size_bytes: 1024,
                tapp_blake3_hex: "ef".repeat(32),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: updated_at,
        signature_hex: String::new(),
    }
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn server_serves_identity_snapshot_and_staged_blobs() {
    // phase: health_endpoint_returns_ok
    {
        let h = spawn_server(true).await;
        let client = tilo_net::http::client().build().expect("http client");
        let body: serde_json::Value = client
            .get(url(h.addr, "/health"))
            .send()
            .await
            .expect("send")
            .json()
            .await
            .expect("decode");
        assert_eq!(body, serde_json::json!({"ok": true}));
    }
    // phase: server_refuses_a_tampered_or_expired_published_snapshot
    {
        let h = spawn_server(true).await;
        let mut snapshot = h.storage.load_snapshot().unwrap().unwrap();
        snapshot.expires_at_unix = 1;
        std::fs::write(
            h.storage.snapshot_path(),
            serde_json::to_vec_pretty(&snapshot).unwrap(),
        )
        .unwrap();
        let response = tilo_net::http::client()
            .build()
            .expect("http client")
            .get(url(h.addr, "/entries"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    }
    // phase: registry_handshake_uses_stored_identity
    {
        let h = spawn_server(true).await;
        let client = tilo_net::http::client().build().expect("http client");
        let body: RegistryHandshake = client
            .get(url(h.addr, "/registry"))
            .send()
            .await
            .expect("send")
            .json()
            .await
            .expect("decode");
        assert_eq!(body.name, "test registry");
        assert_eq!(body.description, "for tests");
        assert_eq!(body.protocol_version, registry_types::PROTOCOL_VERSION);
    }
    // phase: payload_blobs_of_a_staged_package_are_served_by_hash
    // A reviewed package's payload blobs are served by hash, so a client that
    // downloaded the thin package fetches only what its store lacks — and a
    // blob the registry never reviewed is not there to fetch.
    {
        let h = spawn_server(true).await;
        let identity = tilo_app_package::SigningIdentity::generate();
        let payload = h.tmp.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/editor"), b"reviewed binary").unwrap();
        let side_module = b"\0asm\x01\0\0\0reviewed module";
        std::fs::write(payload.join("editor.wasm"), side_module).unwrap();
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            "[package]\nid = \"so.tilo.editor\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
         [publisher]\nidentity = \"ed25519:{}\"\nsignature = \"\"\n\
         [sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         [sandbox.net]\nmode = \"off\"\n\
         [entrypoint]\nbin = \"bin/editor\"\ntype = \"gui\"\n",
            hex::encode(identity.public_key_bytes())
        ))
        .unwrap();
        let packed = tilo_app_package::pack(&payload, &manifest, &identity).unwrap();
        let full = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        let digest = blake3::hash(&full).to_hex().to_string();
        h.storage.save_staged_artifact(&digest, &full).unwrap();

        let client = tilo_net::http::client().build().expect("http client");
        let thin = client
            .get(url(
                h.addr,
                &format!("/thin/{}.tapp", packed.meta.id.to_hex()),
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(thin.status(), reqwest::StatusCode::OK);
        assert_eq!(
            thin.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            thin.bytes().await.unwrap().as_ref(),
            tilo_app_package::tapp::to_bytes(&packed.meta, None)
                .unwrap()
                .as_slice()
        );
        for missing in [
            format!("/thin/{}.tapp", "0".repeat(64)),
            format!("/thin/{}", packed.meta.id.to_hex()),
            format!("/artifacts/{digest}.tapp"),
        ] {
            let response = client.get(url(h.addr, &missing)).send().await.unwrap();
            assert_eq!(
                response.status(),
                reqwest::StatusCode::NOT_FOUND,
                "{missing}"
            );
        }
        h.storage.import_artifact(&full).unwrap();
        let artifact = client
            .get(url(h.addr, &format!("/artifacts/{digest}.tapp")))
            .send()
            .await
            .unwrap();
        assert_eq!(artifact.status(), reqwest::StatusCode::OK);
        assert_eq!(artifact.bytes().await.unwrap().as_ref(), full.as_slice());

        let blobs = app_registry::HttpBlobs::new(format!("http://{}", h.addr));
        let binary = tilo_app_package::BlobHash::of(b"reviewed binary");
        let fetched = blobs.fetch_all(&[binary]).await.unwrap();
        assert_eq!(fetched[0].as_ref(), b"reviewed binary");
        for (bytes, content_type) in [
            (b"reviewed binary".as_slice(), "application/octet-stream"),
            (side_module.as_slice(), "application/wasm"),
        ] {
            let blob = client
                .get(url(
                    h.addr,
                    &format!("/blobs/{}", tilo_app_package::BlobHash::of(bytes).to_hex()),
                ))
                .send()
                .await
                .unwrap();
            assert_eq!(blob.headers()["content-type"], content_type);
            assert_eq!(blob.headers()["x-content-type-options"], "nosniff");
        }
        assert!(
            blobs
                .fetch_all(&[tilo_app_package::BlobHash::of(b"never reviewed")])
                .await
                .is_err()
        );

        // phase: stored_objects_are_served_as_checked_compressed_twins
        // Storing an object writes its zstd twin; the twin is served as an
        // immutable `application/zstd` body with no Content-Encoding, and
        // only while it decompresses to the object it is named for.
        let binary_hex = binary.to_hex();
        let decompress = |body: &[u8], limit: usize| {
            let mut out = Vec::new();
            tilo_app_package::compressed::decompress_bounded(body, limit as u64, &mut out)
                .expect("twin decompresses");
            out
        };
        for (path, raw) in [
            (
                format!("/blobs/{binary_hex}.zst"),
                b"reviewed binary".to_vec(),
            ),
            (format!("/artifacts/{digest}.tapp.zst"), full.clone()),
        ] {
            let served = client.get(url(h.addr, &path)).send().await.unwrap();
            assert_eq!(served.status(), reqwest::StatusCode::OK, "{path}");
            assert_eq!(served.headers()["content-type"], "application/zstd");
            assert_eq!(
                served
                    .headers()
                    .get_all("cache-control")
                    .iter()
                    .collect::<Vec<_>>(),
                ["public, max-age=31536000, immutable"],
                "exactly one Cache-Control, so an edge cache may keep it"
            );
            assert!(served.headers().get("content-encoding").is_none());
            let body = served.bytes().await.unwrap();
            assert_eq!(decompress(&body, raw.len()), raw, "{path}");
        }
        let data = h.tmp.path();
        let twin_file = data.join("payload-blobs").join(format!("{binary_hex}.zst"));
        assert!(twin_file.exists(), "storing the blob wrote its twin");
        // A twin of other bytes is never served under this name.
        std::fs::write(
            &twin_file,
            tilo_app_package::compressed::compress(b"reviewed binarY").unwrap(),
        )
        .unwrap();
        let tampered = client
            .get(url(h.addr, &format!("/blobs/{binary_hex}.zst")))
            .send()
            .await
            .unwrap();
        assert_eq!(
            tampered.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );
        // Without a twin the route is a 404 and clients take the raw blob.
        std::fs::remove_file(&twin_file).unwrap();
        let missing = client
            .get(url(h.addr, &format!("/blobs/{binary_hex}.zst")))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
        assert_eq!(
            blobs.fetch_all(&[binary]).await.unwrap()[0].as_ref(),
            b"reviewed binary"
        );
        // The backfill writes exactly the missing twin.
        let report = h.storage.compress_stored_objects().unwrap();
        assert_eq!(report.written, 1, "{report:?}");
        assert!(report.present >= 2, "{report:?}");
        assert!(twin_file.exists());
        assert_eq!(h.storage.compress_stored_objects().unwrap().written, 0);
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn change_requests_stay_private_until_approval_and_are_validated() {
    // phase: signed_post_stays_private_until_approval
    {
        let h = spawn_server(true).await;
        let entry = sample_entry(&h.publisher, 100);
        let req = sign_change_request(EntryAction::Add, entry.clone(), &h.publisher);

        let client = tilo_net::http::client().build().expect("http client");
        let resp = client
            .post(url(h.addr, "/entries"))
            .json(&req)
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

        let pending: serde_json::Value = resp.json().await.expect("pending response");
        assert_eq!(pending["status"], "pending_review");
        assert_eq!(pending["id"], entry.package_id);

        let retry = client
            .post(url(h.addr, "/entries"))
            .json(&req)
            .send()
            .await
            .expect("retry submission");
        assert_eq!(retry.status(), reqwest::StatusCode::ACCEPTED);
        assert_eq!(
            h.storage.load_pending_submissions().unwrap().len(),
            1,
            "network retries must not create duplicate review work"
        );

        let before: SignedRegistrySnapshot = client
            .get(url(h.addr, "/entries"))
            .send()
            .await
            .expect("list")
            .json()
            .await
            .expect("decode");
        assert!(
            before.entries.is_empty(),
            "pending apps must not enter search"
        );
        assert!(h.storage.load_entry(&entry.package_id).unwrap().is_none());
        assert!(h.storage.load_pending(&entry.package_id).unwrap().is_some());

        approve(&h, &entry.package_id);
        let list: SignedRegistrySnapshot = client
            .get(url(h.addr, "/entries"))
            .send()
            .await
            .expect("list approved")
            .json()
            .await
            .expect("decode approved");
        assert_eq!(list.entries.len(), 1);
        assert_eq!(list.entries[0].package_id, entry.package_id);
        assert_eq!(
            list.entries[0].signature_hex.len(),
            128,
            "server preserves entry sig"
        );

        // GET /entries/<id> returns the same row.
        let single: SignedRegistrySnapshot = client
            .get(url(h.addr, "/entries/so.tilo.editor"))
            .send()
            .await
            .expect("get single")
            .json()
            .await
            .expect("decode");
        assert_eq!(single, list);
    }
    // phase: add_then_update_bumps_stored_entry
    {
        let h = spawn_server(true).await;

        let entry_v1 = sample_entry(&h.publisher, 100);
        let req_v1 = sign_change_request(EntryAction::Add, entry_v1, &h.publisher);
        let client = tilo_net::http::client().build().expect("http client");
        client
            .post(url(h.addr, "/entries"))
            .json(&req_v1)
            .send()
            .await
            .expect("v1")
            .error_for_status()
            .expect("v1 accepted");
        approve(&h, "so.tilo.editor");

        // updated_at bumped, description changed.
        let mut entry_v2 = sample_entry(&h.publisher, 200);
        entry_v2.description = "improved editor".into();
        let req_v2 = sign_change_request(EntryAction::Update, entry_v2.clone(), &h.publisher);
        let resp = client
            .post(url(h.addr, "/entries"))
            .json(&req_v2)
            .send()
            .await
            .expect("v2 send");
        assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);

        let still_v1: SignedRegistrySnapshot = client
            .get(url(h.addr, "/entries/so.tilo.editor"))
            .send()
            .await
            .expect("get still-public v1")
            .json()
            .await
            .expect("decode still-public v1");
        assert_eq!(still_v1.entries[0].updated_at_unix, 100);
        assert_ne!(
            still_v1.entries[0].description.resolve("en"),
            Some("improved editor")
        );

        approve(&h, "so.tilo.editor");

        let stored: SignedRegistrySnapshot = client
            .get(url(h.addr, "/entries/so.tilo.editor"))
            .send()
            .await
            .expect("get")
            .json()
            .await
            .expect("decode");
        assert_eq!(
            stored.entries[0].description.resolve("en"),
            Some("improved editor")
        );
        assert_eq!(stored.entries[0].updated_at_unix, 200);
    }
    // phase: update_rejects_when_updated_at_not_strictly_greater
    {
        let h = spawn_server(true).await;
        let client = tilo_net::http::client().build().expect("http client");

        let v1 = sample_entry(&h.publisher, 100);
        client
            .post(url(h.addr, "/entries"))
            .json(&sign_change_request(EntryAction::Add, v1, &h.publisher))
            .send()
            .await
            .expect("v1")
            .error_for_status()
            .expect("v1 accepted");
        approve(&h, "so.tilo.editor");

        // Same timestamp — server must reject with 409.
        let v_same = sample_entry(&h.publisher, 100);
        let resp = client
            .post(url(h.addr, "/entries"))
            .json(&sign_change_request(
                EntryAction::Update,
                v_same,
                &h.publisher,
            ))
            .send()
            .await
            .expect("v_same");
        assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    }
    // phase: delete_keeps_entry_public_until_approval
    {
        let h = spawn_server(true).await;
        let client = tilo_net::http::client().build().expect("http client");

        let entry = sample_entry(&h.publisher, 100);
        client
            .post(url(h.addr, "/entries"))
            .json(&sign_change_request(
                EntryAction::Add,
                entry.clone(),
                &h.publisher,
            ))
            .send()
            .await
            .expect("add")
            .error_for_status()
            .expect("add accepted");
        approve(&h, "so.tilo.editor");

        let delete = client
            .post(url(h.addr, "/entries"))
            .json(&sign_change_request(
                EntryAction::Delete,
                entry.clone(),
                &h.publisher,
            ))
            .send()
            .await
            .expect("delete");
        assert_eq!(delete.status(), reqwest::StatusCode::ACCEPTED);

        let before = client
            .get(url(h.addr, "/entries/so.tilo.editor"))
            .send()
            .await
            .expect("get before approval");
        assert_eq!(before.status(), reqwest::StatusCode::OK);

        approve(&h, "so.tilo.editor");

        let resp = client
            .get(url(h.addr, "/entries/so.tilo.editor"))
            .send()
            .await
            .expect("get");
        assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    }
    // phase: bad_json_returns_400
    {
        let h = spawn_server(true).await;
        let client = tilo_net::http::client().build().expect("http client");
        let resp = client
            .post(url(h.addr, "/entries"))
            .header("content-type", "application/json")
            .body("{not really JSON")
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    // phase: oversized_change_request_returns_413_without_touching_disk
    {
        let h = spawn_server(true).await;
        let response = tilo_net::http::client()
            .build()
            .expect("http client")
            .post(url(h.addr, "/entries"))
            .body(vec![b'x'; 1024 * 1024 + 1])
            .send()
            .await
            .expect("send oversized request");

        assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
        assert!(h.storage.load_pending_submissions().unwrap().is_empty());
    }
}

#[tokio::test]
async fn unauthorised_change_requests_are_refused_with_403() {
    // phase: unauthorised_publisher_gets_403
    {
        let h = spawn_server(true).await;
        let client = tilo_net::http::client().build().expect("http client");

        // Build a request signed by the outsider — a valid Ed25519
        // signature, but the pubkey is not in the allowlist.
        let entry = sample_entry(&h.outsider, 100);
        let req = sign_change_request(EntryAction::Add, entry, &h.outsider);

        let resp = client
            .post(url(h.addr, "/entries"))
            .json(&req)
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
        assert!(h.storage.load_pending_submissions().unwrap().is_empty());
    }
    // phase: tampered_signature_gets_403
    {
        let h = spawn_server(true).await;
        let client = tilo_net::http::client().build().expect("http client");

        let entry = sample_entry(&h.publisher, 100);
        let mut req = sign_change_request(EntryAction::Add, entry, &h.publisher);
        // Flip a byte in the change-level signature.
        let mut bytes = hex::decode(&req.publisher_signature_hex).unwrap();
        bytes[0] ^= 1;
        req.publisher_signature_hex = hex::encode(bytes);

        let resp = client
            .post(url(h.addr, "/entries"))
            .json(&req)
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
        assert!(h.storage.load_pending_submissions().unwrap().is_empty());
    }
    // phase: add_denied_when_publisher_cannot_add_new
    {
        // Same publisher as normal, but the allowlist says
        // can_add_new = false — they may only edit their own rows.
        let h = spawn_server(false).await;
        let client = tilo_net::http::client().build().expect("http client");

        let entry = sample_entry(&h.publisher, 100);
        let req = sign_change_request(EntryAction::Add, entry, &h.publisher);
        let resp = client
            .post(url(h.addr, "/entries"))
            .json(&req)
            .send()
            .await
            .expect("send");
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    }
    // phase: publisher_cannot_hijack_another_publishers_entry
    {
        let h = spawn_server(true).await;
        let client = tilo_net::http::client().build().expect("http client");

        // The allowlisted publisher creates an entry.
        let entry_v1 = sample_entry(&h.publisher, 100);
        client
            .post(url(h.addr, "/entries"))
            .json(&sign_change_request(
                EntryAction::Add,
                entry_v1.clone(),
                &h.publisher,
            ))
            .send()
            .await
            .expect("v1")
            .error_for_status()
            .expect("v1 accepted");

        // A different allowlisted publisher (we'll add one on the fly)
        // tries to update the same package_id. Simulate by asking the
        // storage layer to add the outsider as a full member.
        let second = SigningKey::generate(&mut UnwrapErr(SysRng));
        // Use the CLI-equivalent path: modify allowlist via server
        // restart is heavy; instead sign a change with a mismatched
        // publisher pubkey — the entry-level sig now doesn't verify
        // because the entry declares publisher_pubkey_hex = first
        // publisher but we sign with second's key. Server must reject.
        let mut hijack = sample_entry(&h.publisher, 200); // note: still claims first publisher
        hijack.description = "hijacked".into();
        let req = sign_change_request(EntryAction::Update, hijack, &second);
        let resp = client
            .post(url(h.addr, "/entries"))
            .json(&req)
            .send()
            .await
            .expect("send");
        // 403 — the change-level sig doesn't match the declared publisher
        // pubkey, so verify_change_request refuses before touching disk.
        assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    }
}

/// A publisher may name packages only inside its verified namespaces.
#[tokio::test]
async fn package_ids_must_lie_in_a_verified_namespace() {
    let h = spawn_server(true).await;
    let client = tilo_net::http::client().build().expect("http client");
    let mut outside = sample_entry(&h.publisher, 100);
    outside.package_id = "com.acme.editor".into();
    registry_types::sign_entry(&mut outside, &h.publisher);
    let resp = client
        .post(url(h.addr, "/entries"))
        .json(&sign_change_request(
            EntryAction::Add,
            outside,
            &h.publisher,
        ))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(
        resp.text()
            .await
            .unwrap()
            .contains("outside this publisher's verified namespaces")
    );
    assert!(h.storage.load_pending_submissions().unwrap().is_empty());

    let mut allow = h.allowlist.clone();
    allow.publishers[0].namespaces = vec!["com.acme".into()];
    h.storage.save_allowlist(&allow).unwrap();
    let mut acme = sample_entry(&h.publisher, 100);
    acme.package_id = "com.acme.editor".into();
    registry_types::sign_entry(&mut acme, &h.publisher);
    let resp = client
        .post(url(h.addr, "/entries"))
        .json(&sign_change_request(EntryAction::Add, acme, &h.publisher))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let tilo = sample_entry(&h.publisher, 101);
    let resp = client
        .post(url(h.addr, "/entries"))
        .json(&sign_change_request(EntryAction::Add, tilo, &h.publisher))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
}

/// An allowlisted publisher whose row names no namespace — a hand-written or
/// pre-namespace `publishers.toml` row — owns nothing, so it cannot claim Tilo's
/// own `so.tilo.store`.
#[tokio::test]
async fn publisher_without_namespace_cannot_add_so_tilo_entry() {
    let h = spawn_server(true).await;
    std::fs::write(
        h.storage.publishers_path(),
        format!(
            "[[publisher]]\npubkey_hex = \"{}\"\ncan_add_new = true\nnote = \"community\"\n",
            hex::encode(h.publisher.verifying_key().to_bytes())
        ),
    )
    .unwrap();
    let row = h.storage.load_allowlist().unwrap().publishers[0].clone();
    assert!(row.can_add_new);
    assert!(row.namespaces.is_empty());

    let client = tilo_net::http::client().build().expect("http client");
    let mut store = sample_entry(&h.publisher, 100);
    store.package_id = "so.tilo.store".into();
    registry_types::sign_entry(&mut store, &h.publisher);
    let resp = client
        .post(url(h.addr, "/entries"))
        .json(&sign_change_request(EntryAction::Add, store, &h.publisher))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("package id so.tilo.store is outside this publisher's verified namespaces"),
        "{body}"
    );
    assert!(h.storage.load_pending_submissions().unwrap().is_empty());
}

/// A package signed by `key`, in full.
fn signed_package(dir: &std::path::Path, key: &SigningKey) -> Vec<u8> {
    let identity = tilo_app_package::SigningIdentity::from_signing_key(key.clone());
    let payload = dir.join(hex::encode(key.verifying_key().to_bytes()));
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/editor"), b"uploaded binary").unwrap();
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        "[package]\nid = \"so.tilo.editor\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
         [package.isa]\nx86_64 = \"v3\"\n\
         [publisher]\nidentity = \"ed25519:{}\"\nsignature = \"\"\n\
         [sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         [sandbox.net]\nmode = \"off\"\n\
         [entrypoint]\nbin = \"bin/editor\"\ntype = \"gui\"\n",
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let packed = tilo_app_package::pack(&payload, &manifest, &identity).unwrap();
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

/// An allowlisted publisher uploads its own signed package; the registry keeps
/// it privately, and review reads it from there without a network fetch.
#[tokio::test]
async fn publishers_upload_their_own_packages_for_review() {
    let h = spawn_server(true).await;
    let client = tilo_net::http::client().build().expect("http client");
    let bytes = signed_package(h.tmp.path(), &h.publisher);
    let digest = blake3::hash(&bytes).to_hex().to_string();
    let path = format!("/uploads/{digest}.tapp");
    let put = |key: &SigningKey, signed: &str, body: Vec<u8>| {
        client
            .put(url(h.addr, &path))
            .header(
                registry_types::upload::UPLOAD_PUBLISHER_HEADER,
                hex::encode(key.verifying_key().to_bytes()),
            )
            .header(
                registry_types::upload::UPLOAD_SIGNATURE_HEADER,
                registry_types::upload::sign_upload(key, signed),
            )
            .body(body)
            .send()
    };

    let unsigned = client
        .put(url(h.addr, &path))
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), reqwest::StatusCode::UNAUTHORIZED);
    let wrong_digest = put(&h.publisher, &"00".repeat(32), bytes.clone())
        .await
        .unwrap();
    assert_eq!(wrong_digest.status(), reqwest::StatusCode::FORBIDDEN);
    let stranger = put(&h.outsider, &digest, bytes.clone()).await.unwrap();
    assert_eq!(stranger.status(), reqwest::StatusCode::FORBIDDEN);
    let foreign = signed_package(h.tmp.path(), &h.outsider);
    let foreign_digest = blake3::hash(&foreign).to_hex().to_string();
    let foreign_upload = client
        .put(url(h.addr, &format!("/uploads/{foreign_digest}.tapp")))
        .header(
            registry_types::upload::UPLOAD_PUBLISHER_HEADER,
            hex::encode(h.publisher.verifying_key().to_bytes()),
        )
        .header(
            registry_types::upload::UPLOAD_SIGNATURE_HEADER,
            registry_types::upload::sign_upload(&h.publisher, &foreign_digest),
        )
        .body(foreign)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign_upload.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(h.storage.load_upload(&foreign_digest).unwrap(), None);

    let accepted = put(&h.publisher, &digest, bytes.clone()).await.unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::CREATED);
    let answer: serde_json::Value = accepted.json().await.unwrap();
    let artifact_url = format!("https://apps.tilo.so/artifacts/{digest}.tapp");
    assert_eq!(answer["url"], artifact_url.as_str());
    assert_eq!(h.storage.load_upload(&digest).unwrap(), Some(bytes.clone()));
    let public = client
        .get(url(h.addr, &format!("/artifacts/{digest}.tapp")))
        .send()
        .await
        .unwrap();
    assert_eq!(public.status(), reqwest::StatusCode::NOT_FOUND);

    let package = app_registry::tapp::load(&bytes).unwrap();
    let mut entry = sample_entry(&h.publisher, 100);
    entry.versions[0].version = "1.0.0".into();
    entry.versions[0].tapp_mut().unwrap().tapp_url = artifact_url;
    entry.versions[0].tapp_mut().unwrap().tapp_root_hash_hex = package.package_id.to_hex();
    entry.versions[0].tapp_mut().unwrap().tapp_size_bytes = bytes.len() as u64;
    entry.versions[0].tapp_mut().unwrap().tapp_blake3_hex = digest.clone();
    let storage = h.storage.clone();
    let reviewed = tokio::task::spawn_blocking(move || {
        registry_server::review_entry_bytes(
            &storage,
            &entry,
            registry_server::InspectionPolicy::default(),
        )
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reviewed.len(), 1);
    assert_eq!(reviewed[0].1, bytes);
}

/// A browser page on a configured origin may read the catalogue and blobs;
/// any other origin, and every POST, gets no CORS grant. An unchanged
/// catalogue revalidates to an empty 304.
#[tokio::test]
async fn cors_grants_exact_origins_get_only_and_snapshots_revalidate() {
    let page = "http://localhost:4321";
    let h = spawn_server_with(
        true,
        ServeOptions {
            cors_origins: vec![page.into()],
        },
    )
    .await;
    let client = tilo_net::http::client().build().expect("http client");

    let entries = client
        .get(url(h.addr, "/entries"))
        .header("origin", page)
        .send()
        .await
        .unwrap();
    assert_eq!(entries.status(), reqwest::StatusCode::OK);
    assert_eq!(entries.headers()["access-control-allow-origin"], page);
    assert_eq!(entries.headers()["vary"], "origin");
    assert_eq!(entries.headers()["cache-control"], "no-cache");
    assert!(
        entries
            .headers()
            .get("access-control-allow-credentials")
            .is_none()
    );
    let etag = entries.headers()["etag"].to_str().unwrap().to_string();
    let snapshot: SignedRegistrySnapshot = entries.json().await.unwrap();
    assert_eq!(
        etag,
        format!("\"{}\"", registry_types::snapshot_hash(&snapshot).unwrap())
    );

    let revalidated = client
        .get(url(h.addr, "/entries"))
        .header("origin", page)
        .header("if-none-match", format!("W/\"x\", {etag}"))
        .send()
        .await
        .unwrap();
    assert_eq!(revalidated.status(), reqwest::StatusCode::NOT_MODIFIED);
    assert_eq!(revalidated.headers()["etag"], etag.as_str());
    assert!(revalidated.bytes().await.unwrap().is_empty());

    let foreign = client
        .get(url(h.addr, "/entries"))
        .header("origin", "http://localhost:4322")
        .send()
        .await
        .unwrap();
    assert!(
        foreign
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );

    let post = client
        .post(url(h.addr, "/entries"))
        .header("origin", page)
        .json(&sign_change_request(
            EntryAction::Add,
            sample_entry(&h.publisher, 100),
            &h.publisher,
        ))
        .send()
        .await
        .unwrap();
    assert!(post.headers().get("access-control-allow-origin").is_none());
    let preflight = client
        .request(reqwest::Method::OPTIONS, url(h.addr, "/entries"))
        .header("origin", page)
        .header("access-control-request-method", "POST")
        .send()
        .await
        .unwrap();
    assert!(
        preflight
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    assert!(
        preflight
            .headers()
            .get("access-control-allow-methods")
            .is_none()
    );
}

#[test]
fn cors_origins_must_be_exact_serialized_origins() {
    use registry_server::serve::valid_cors_origin;
    for origin in [
        "http://localhost:4321",
        "https://tilo.so",
        "https://app.tilo.so:8443",
        "http://127.0.0.1:4321",
    ] {
        assert!(valid_cors_origin(origin), "{origin}");
    }
    for origin in [
        "*",
        "null",
        "https://tilo.so/",
        "https://tilo.so/app",
        "https://Tilo.so",
        "ftp://tilo.so",
        "https://",
        "https://tilo.so:",
        "https://tilo.so:123456",
        "https://user@tilo.so",
        "https://tilo.so, https://evil.example",
    ] {
        assert!(!valid_cors_origin(origin), "{origin}");
    }
}

/// A catalogue the size of a public store's: `count` signed entries.
fn large_catalogue(h: &Harness, count: usize) {
    let entries = (0..count)
        .map(|index| {
            let mut entry = sample_entry(&h.publisher, 100);
            entry.package_id = format!("so.tilo.app{index}");
            registry_types::sign_entry(&mut entry, &h.publisher);
            entry
        })
        .collect();
    publish_snapshot_of(&h.storage, &h.allowlist, &h.owner, entries);
}

fn p50(samples: &mut [std::time::Duration]) -> std::time::Duration {
    samples.sort_unstable();
    samples.get(samples.len() / 2).copied().unwrap_or_default()
}

/// What a client of a busy store waits for: the catalogue, and a missing
/// blob asked for while other clients fetch the catalogue. Prints the
/// numbers; the regression checks are the counters in the next test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entries_and_missing_blob_latency_with_a_large_catalogue() {
    let h = spawn_server(true).await;
    large_catalogue(&h, 1_000);
    let client = tilo_net::http::client().build().expect("http client");
    let entries = url(h.addr, "/entries");
    let missing = url(h.addr, &format!("/blobs/{}", "0".repeat(64)));

    let started = std::time::Instant::now();
    let first = client.get(&entries).send().await.unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let body = first.bytes().await.unwrap();
    let cold = started.elapsed();

    let mut warm = Vec::new();
    for _ in 0..20 {
        let started = std::time::Instant::now();
        let response = client.get(&entries).send().await.unwrap();
        assert_eq!(response.bytes().await.unwrap(), body);
        warm.push(started.elapsed());
    }

    // Eight clients fetch the catalogue while a ninth asks for a blob the
    // store does not have.
    let mut busy = Vec::new();
    for _ in 0..10 {
        let fetches: Vec<_> = (0..8)
            .map(|_| {
                let request = client.get(&entries).send();
                tokio::spawn(async move { request.await.unwrap().bytes().await.unwrap().len() })
            })
            .collect();
        // Let the catalogue requests reach the server first.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let started = std::time::Instant::now();
        let response = client.get(&missing).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        busy.push(started.elapsed());
        for fetch in fetches {
            assert_eq!(fetch.await.unwrap(), body.len());
        }
    }
    eprintln!(
        "perf registry /entries ({} KB, 1000 entries): first {:?}, then p50 {:?}; \
         missing blob beside 8 catalogue fetches p50 {:?}",
        body.len() / 1024,
        cold,
        p50(&mut warm),
        p50(&mut busy),
    );
}

/// The catalogue is read, verified and encoded once per published revision,
/// however many clients ask and in whichever form, and every client gets
/// the whole body.
#[tokio::test]
async fn the_catalogue_is_verified_once_per_published_revision() {
    let h = spawn_server(true).await;
    large_catalogue(&h, 300);
    let client = tilo_net::http::client().build().expect("http client");
    let entries = url(h.addr, "/entries");

    let fetches: Vec<_> = (0..24)
        .map(|index| {
            let path = match index % 3 {
                0 => "/entries",
                1 => "/entries?format=compact",
                _ => "/entries/so.tilo.app7",
            };
            let request = client.get(url(h.addr, path)).send();
            tokio::spawn(async move {
                let response = request.await.unwrap();
                assert_eq!(response.status(), reqwest::StatusCode::OK);
                let length = response.content_length().unwrap();
                let body = response.bytes().await.unwrap();
                assert_eq!(body.len() as u64, length, "the whole body arrives");
                serde_json::from_slice::<SignedRegistrySnapshot>(&body).unwrap()
            })
        })
        .collect();
    for fetch in fetches {
        let snapshot = fetch.await.unwrap();
        assert_eq!(snapshot.entries.len(), 300);
        assert_eq!(snapshot.sequence, 2);
    }
    let unknown = client
        .get(url(h.addr, "/entries/so.tilo.nothing"))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(h.state.catalogue_loads(), 1);

    // A newly published snapshot is served from the next request on.
    large_catalogue(&h, 301);
    let next: SignedRegistrySnapshot = client
        .get(&entries)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!((next.sequence, next.entries.len()), (3, 301));
    let _ = client.get(&entries).send().await.unwrap();
    assert_eq!(h.state.catalogue_loads(), 2);
}

/// A client that stops reading its catalogue holds up no one else.
#[tokio::test]
async fn a_client_that_stops_reading_holds_up_no_one() {
    use tokio::io::AsyncWriteExt as _;
    let h = spawn_server(true).await;
    large_catalogue(&h, 1_000);
    let mut stalled = Vec::new();
    for _ in 0..8 {
        let mut stream = tokio::net::TcpStream::connect(h.addr).await.unwrap();
        stream
            .write_all(b"GET /entries HTTP/1.1\r\nHost: registry\r\n\r\n")
            .await
            .unwrap();
        // Never read: the server's writes to it fill the socket and wait.
        stalled.push(stream);
    }
    let client = tilo_net::http::client().build().expect("http client");
    let answered = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for _ in 0..8 {
            let entries = client.get(url(h.addr, "/entries")).send().await.unwrap();
            assert_eq!(entries.status(), reqwest::StatusCode::OK);
            entries.bytes().await.unwrap();
            let missing = client
                .get(url(h.addr, &format!("/blobs/{}", "0".repeat(64))))
                .send()
                .await
                .unwrap();
            assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
        }
    })
    .await;
    assert!(answered.is_ok(), "stalled readers held up other clients");
    drop(stalled);
}
