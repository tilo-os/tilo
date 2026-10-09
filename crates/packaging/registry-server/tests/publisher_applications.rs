//! A key applies to publish under namespaces it proves, over HTTP, and only
//! the owner's approval — which checks the proofs again — allowlists it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use registry_server::applications::{self, ApplicationFailure, ProofSource};
use registry_server::serve::ServeOptions;
use registry_server::{AppState, ServerIdentity, Storage, bind_and_run};
use registry_types::application::{NamespaceClaim, PublisherApplication, proof_token};
use tilo_clock::try_unix_now;

#[derive(Clone, Default)]
struct Proofs(Arc<Mutex<BTreeMap<String, Vec<String>>>>);

impl Proofs {
    fn set_txt(&self, name: &str, records: Vec<String>) {
        self.0.lock().unwrap().insert(name.into(), records);
    }
}

impl ProofSource for Proofs {
    fn txt_records(&self, name: &str) -> Result<Vec<String>, String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default())
    }

    fn https_text(&self, _url: &str) -> Result<String, String> {
        Err("404".into())
    }
}

fn application(key: &SigningKey) -> PublisherApplication {
    let mut application = PublisherApplication {
        pubkey_hex: String::new(),
        claims: vec![NamespaceClaim::Domain {
            domain: "example.com".into(),
        }],
        contact: "ada@example.com".into(),
        note: "Ada's apps".into(),
        submitted_at_unix: try_unix_now().unwrap(),
        signature_hex: String::new(),
    };
    application.sign(key);
    application
}

fn owner_approves_only_while_proven(storage: &Storage, pubkey: &str, proofs: &Proofs) {
    proofs.set_txt("_tilo-publisher.example.com", Vec::new());
    let withdrawn =
        applications::approve(storage, pubkey, "owner", try_unix_now().unwrap(), proofs);
    assert!(
        matches!(withdrawn, Err(ApplicationFailure::Rejected(_))),
        "{withdrawn:?}"
    );
    assert!(storage.load_allowlist().unwrap().publishers.is_empty());

    proofs.set_txt("_tilo-publisher.example.com", vec![proof_token(pubkey)]);
    let (_, row) =
        applications::approve(storage, pubkey, "owner", try_unix_now().unwrap(), proofs).unwrap();
    assert!(row.can_add_new);
    assert_eq!(row.namespaces, ["com.example"]);
    assert!(row.owns_package_id("com.example.notes"));
    assert!(!row.owns_package_id("so.tilo.notes"));
    assert_eq!(storage.load_allowlist().unwrap().publishers, [row]);
    assert!(matches!(
        applications::approve(storage, pubkey, "owner", try_unix_now().unwrap(), proofs),
        Err(ApplicationFailure::NotPending(_))
    ));
}

async fn start(proofs: &Proofs) -> (tempfile::TempDir, Storage, SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open(tmp.path()).unwrap();
    let identity = ServerIdentity {
        owner_pubkey_hex: hex::encode(SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes()),
        name: "test registry".into(),
        description: "for tests".into(),
        base_url: "http://127.0.0.1".into(),
    };
    let state = AppState::with_proof_source(
        storage.clone(),
        identity,
        ServeOptions::default(),
        Arc::new(proofs.clone()),
    );
    let addr = bind_and_run(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), state)
        .await
        .unwrap();
    (tmp, storage, addr)
}

#[tokio::test]
async fn applications_queue_for_the_owner_who_approves_only_what_is_still_proven() {
    let proofs = Proofs::default();
    let (_tmp, storage, addr) = start(&proofs).await;
    let client = tilo_net::http::client().build().unwrap();
    let applications_url = format!("http://{addr}/publishers/applications");

    let key = SigningKey::from_bytes(&[7; 32]);
    let pubkey = hex::encode(key.verifying_key().to_bytes());
    let signed = application(&key);

    let unproven = client
        .post(&applications_url)
        .json(&signed)
        .send()
        .await
        .unwrap();
    assert_eq!(unproven.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let reason = unproven.text().await.unwrap();
    assert!(
        reason.contains("DNS TXT _tilo-publisher.example.com"),
        "{reason}"
    );
    assert!(reason.contains(&proof_token(&pubkey)), "{reason}");

    proofs.set_txt("_tilo-publisher.example.com", vec![proof_token(&pubkey)]);
    let mut tampered = signed.clone();
    tampered.claims = vec![NamespaceClaim::Domain {
        domain: "example.org".into(),
    }];
    let forged = client
        .post(&applications_url)
        .json(&tampered)
        .send()
        .await
        .unwrap();
    assert_eq!(forged.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);

    let accepted = client
        .post(&applications_url)
        .json(&signed)
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), reqwest::StatusCode::ACCEPTED);
    let status: serde_json::Value = client
        .get(format!("{applications_url}/{pubkey}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["status"], "pending");
    assert_eq!(status["namespaces"], serde_json::json!(["com.example"]));
    assert!(status.get("contact").is_none(), "{status}");
    assert!(storage.load_allowlist().unwrap().publishers.is_empty());

    let storage_for_review = storage.clone();
    let reviewer_proofs = proofs.clone();
    tokio::task::spawn_blocking(move || {
        owner_approves_only_while_proven(&storage_for_review, &pubkey, &reviewer_proofs);
    })
    .await
    .unwrap();

    let decided: serde_json::Value = client
        .get(format!(
            "{applications_url}/{}",
            hex::encode(key.verifying_key().to_bytes())
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(decided["status"], "approved");
}
