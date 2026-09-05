//! End-to-end HTTP tests — spin up a real registry-server bound
//! to an ephemeral port, drive it with reqwest exactly as an
//! on-device client would, and assert the wire behaviour.
//!
//! These tests verify the whole loop: signature construction ↔
//! JSON round-trip ↔ server-side verify ↔ disk write ↔ GET.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;
use registry_server::{
    AppState, ModerationService, PublisherAllowlist, PublisherRow, ServerIdentity, Storage,
    bind_and_run,
};
use registry_types::{
    EntryAction, ModerationActorRole, ModerationCase, ModerationMutation, RegistryEntry,
    RegistryHandshake, RegistryVersion, SignedModerationMutation, SignedRegistrySnapshot,
    TransparencyPage, sign_change_request, sign_moderation_mutation,
};

/// Start a server against a fresh tempdir. Returns the bound
/// address + the tempdir handle (kept alive by the caller so the
/// dir isn't cleaned up mid-test) + the owner + first publisher
/// key.
struct Harness {
    _tmp: tempfile::TempDir,
    addr: SocketAddr,
    storage: Storage,
    allowlist: PublisherAllowlist,
    publisher: SigningKey,
    owner: SigningKey,
    /// Second publisher who is NOT allowlisted — used for the
    /// negative test.
    outsider: SigningKey,
}

async fn spawn_server(publisher_can_add_new: bool) -> Harness {
    let _ = tracing_subscriber::fmt::try_init();

    let tmp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(tmp.path()).expect("open storage");

    let owner_key = SigningKey::generate(&mut OsRng);
    let identity = ServerIdentity {
        owner_pubkey_hex: hex::encode(owner_key.verifying_key().to_bytes()),
        name: "test registry".into(),
        description: "for tests".into(),
        base_url: "http://127.0.0.1".into(),
    };
    storage.save_identity(&identity).expect("save identity");

    let publisher = SigningKey::generate(&mut OsRng);
    let outsider = SigningKey::generate(&mut OsRng);
    let mut allow = PublisherAllowlist::empty();
    allow.upsert(PublisherRow {
        pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        can_add_new: publisher_can_add_new,
        note: "test publisher".into(),
    });
    storage.save_allowlist(&allow).expect("save allowlist");

    publish_snapshot(&storage, &allow, &owner_key);

    let state = AppState::new(storage.clone(), identity);
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let bound = bind_and_run(addr, state).await.expect("bind");

    // Give the tokio::spawn'd accept loop a beat to actually be
    // ready to accept. On a busy loopback stack the first
    // `TcpStream::connect` occasionally beats the accept.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    Harness {
        _tmp: tmp,
        addr: bound,
        storage,
        allowlist: allow,
        publisher,
        owner: owner_key,
        outsider,
    }
}

fn signed_report(key: &SigningKey, request_id: &str, now: u64) -> SignedModerationMutation {
    sign_moderation_mutation(
        request_id.into(),
        now,
        now.saturating_add(300),
        ModerationActorRole::Reporter,
        ModerationMutation::FileReport {
            package_id: "dev.qos.editor".into(),
            category: "security".into(),
            summary: "Unexpected outbound connection during offline editing".into(),
            evidence_digest_hex: "ab".repeat(32),
        },
        key,
    )
    .unwrap()
}

#[tokio::test]
async fn moderation_http_is_signed_idempotent_authorized_and_verifiable() {
    let h = spawn_server(true).await;
    let entry = sample_entry(&h.publisher, 100);
    let client = reqwest::Client::new();
    client
        .post(url(h.addr, "/entries"))
        .json(&sign_change_request(EntryAction::Add, entry, &h.publisher))
        .send()
        .await
        .unwrap();
    approve(&h, "dev.qos.editor");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let report = signed_report(&h.outsider, "http-report-one", now);
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

#[tokio::test]
async fn oversized_moderation_body_is_rejected_before_parse_or_disk_write() {
    let h = spawn_server(true).await;
    let response = reqwest::Client::new()
        .post(url(h.addr, "/moderation/mutations"))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body("x".repeat(70 * 1024))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    let page: TransparencyPage = reqwest::Client::new()
        .get(url(h.addr, "/moderation/transparency?from=1&limit=10"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(page.events.is_empty());
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
    let previous = storage.load_snapshot().unwrap();
    let mut entries = storage.load_entries().unwrap();
    entries.sort_by(|left, right| left.package_id.cmp(&right.package_id));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut snapshot = SignedRegistrySnapshot {
        protocol: registry_types::SNAPSHOT_PROTOCOL,
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
    };
    registry_types::sign_snapshot(&mut snapshot, owner).unwrap();
    storage.save_snapshot(&snapshot).unwrap();
}

fn sample_entry(publisher: &SigningKey, updated_at: u64) -> RegistryEntry {
    RegistryEntry {
        package_id: "dev.qos.editor".into(),
        name: "Editor".into(),
        description: "CRDT-native text editor".into(),
        categories: vec!["Productivity".into()],
        icon_url: None,
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: "2026.06".into(),
            qpkg_url: "https://apps.qos.dev/editor/2026.06.qpkg".into(),
            qpkg_root_hash_hex: "cd".repeat(32),
            qpkg_size_bytes: 1024,
            released_at_unix: 1_800_000_000,
        }],
        updated_at_unix: updated_at,
        signature_hex: String::new(),
    }
}

fn url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let h = spawn_server(true).await;
    let client = reqwest::Client::new();
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

#[tokio::test]
async fn server_refuses_a_tampered_or_expired_published_snapshot() {
    let h = spawn_server(true).await;
    let mut snapshot = h.storage.load_snapshot().unwrap().unwrap();
    snapshot.expires_at_unix = 1;
    h.storage.save_snapshot(&snapshot).unwrap();
    let response = reqwest::Client::new()
        .get(url(h.addr, "/entries"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn registry_handshake_uses_stored_identity() {
    let h = spawn_server(true).await;
    let client = reqwest::Client::new();
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

#[tokio::test]
async fn signed_post_stays_private_until_approval() {
    let h = spawn_server(true).await;
    let entry = sample_entry(&h.publisher, 100);
    let req = sign_change_request(EntryAction::Add, entry.clone(), &h.publisher);

    let client = reqwest::Client::new();
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
        .get(url(h.addr, "/entries/dev.qos.editor"))
        .send()
        .await
        .expect("get single")
        .json()
        .await
        .expect("decode");
    assert_eq!(single, list);
}

#[tokio::test]
async fn add_then_update_bumps_stored_entry() {
    let h = spawn_server(true).await;

    let entry_v1 = sample_entry(&h.publisher, 100);
    let req_v1 = sign_change_request(EntryAction::Add, entry_v1, &h.publisher);
    let client = reqwest::Client::new();
    client
        .post(url(h.addr, "/entries"))
        .json(&req_v1)
        .send()
        .await
        .expect("v1")
        .error_for_status()
        .expect("v1 accepted");
    approve(&h, "dev.qos.editor");

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
        .get(url(h.addr, "/entries/dev.qos.editor"))
        .send()
        .await
        .expect("get still-public v1")
        .json()
        .await
        .expect("decode still-public v1");
    assert_eq!(still_v1.entries[0].updated_at_unix, 100);
    assert_ne!(still_v1.entries[0].description, "improved editor");

    approve(&h, "dev.qos.editor");

    let stored: SignedRegistrySnapshot = client
        .get(url(h.addr, "/entries/dev.qos.editor"))
        .send()
        .await
        .expect("get")
        .json()
        .await
        .expect("decode");
    assert_eq!(stored.entries[0].description, "improved editor");
    assert_eq!(stored.entries[0].updated_at_unix, 200);
}

#[tokio::test]
async fn update_rejects_when_updated_at_not_strictly_greater() {
    let h = spawn_server(true).await;
    let client = reqwest::Client::new();

    let v1 = sample_entry(&h.publisher, 100);
    client
        .post(url(h.addr, "/entries"))
        .json(&sign_change_request(EntryAction::Add, v1, &h.publisher))
        .send()
        .await
        .expect("v1")
        .error_for_status()
        .expect("v1 accepted");
    approve(&h, "dev.qos.editor");

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

#[tokio::test]
async fn delete_keeps_entry_public_until_approval() {
    let h = spawn_server(true).await;
    let client = reqwest::Client::new();

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
    approve(&h, "dev.qos.editor");

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
        .get(url(h.addr, "/entries/dev.qos.editor"))
        .send()
        .await
        .expect("get before approval");
    assert_eq!(before.status(), reqwest::StatusCode::OK);

    approve(&h, "dev.qos.editor");

    let resp = client
        .get(url(h.addr, "/entries/dev.qos.editor"))
        .send()
        .await
        .expect("get");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unauthorised_publisher_gets_403() {
    let h = spawn_server(true).await;
    let client = reqwest::Client::new();

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

#[tokio::test]
async fn tampered_signature_gets_403() {
    let h = spawn_server(true).await;
    let client = reqwest::Client::new();

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

#[tokio::test]
async fn add_denied_when_publisher_cannot_add_new() {
    // Same publisher as normal, but the allowlist says
    // can_add_new = false — they may only edit their own rows.
    let h = spawn_server(false).await;
    let client = reqwest::Client::new();

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

#[tokio::test]
async fn publisher_cannot_hijack_another_publishers_entry() {
    let h = spawn_server(true).await;
    let client = reqwest::Client::new();

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
    let second = SigningKey::generate(&mut OsRng);
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

#[tokio::test]
async fn bad_json_returns_400() {
    let h = spawn_server(true).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(url(h.addr, "/entries"))
        .header("content-type", "application/json")
        .body("{not really JSON")
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn oversized_change_request_returns_413_without_touching_disk() {
    let h = spawn_server(true).await;
    let response = reqwest::Client::new()
        .post(url(h.addr, "/entries"))
        .body(vec![b'x'; 1024 * 1024 + 1])
        .send()
        .await
        .expect("send oversized request");

    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert!(h.storage.load_pending_submissions().unwrap().is_empty());
}
