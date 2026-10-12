//! `registry-client`, as `tilo-app apply` and `tilo-app publish` drive it,
//! against a running registry: apply for a namespace, get approved, publish
//! a build into it, and find the upload where review reads it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use registry_client::RegistryClient;
use registry_server::applications::{self, ProofSource};
use registry_server::serve::ServeOptions;
use registry_server::{AppState, ServerIdentity, Storage, bind_and_run};
use registry_types::application::{NamespaceClaim, proof_token};

#[derive(Clone, Default)]
struct Proofs(Arc<Mutex<BTreeMap<String, Vec<String>>>>);

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

fn package(dir: &std::path::Path, key: &SigningKey, id: &str, isa: &str) -> Vec<u8> {
    release(dir, key, id, isa, "1.0.0")
}

fn release(dir: &std::path::Path, key: &SigningKey, id: &str, isa: &str, version: &str) -> Vec<u8> {
    let identity = tilo_app_package::SigningIdentity::from_signing_key(key.clone());
    let payload = dir.join(format!("{id}-{isa}-{version}"));
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(
        payload.join("bin/editor"),
        format!("published binary {version}"),
    )
    .unwrap();
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        "[package]\nid = \"{id}\"\nversion = \"{version}\"\nkind = \"app\"\n\
         [package.isa]\n{isa}\n\
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

#[tokio::test]
async fn an_outside_publisher_applies_is_approved_and_publishes() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open(tmp.path().join("registry")).unwrap();
    let identity = ServerIdentity {
        owner_pubkey_hex: hex::encode(SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes()),
        name: "test registry".into(),
        description: "for tests".into(),
        base_url: "https://apps.example".into(),
    };
    let proofs = Proofs::default();
    let state = AppState::with_proof_source(
        storage.clone(),
        identity,
        ServeOptions::default(),
        Arc::new(proofs.clone()),
    );
    let addr = bind_and_run(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), state)
        .await
        .unwrap();

    let key = SigningKey::from_bytes(&[9; 32]);
    let pubkey = hex::encode(key.verifying_key().to_bytes());
    proofs.0.lock().unwrap().insert(
        "_tilo-publisher.example.com".into(),
        vec![proof_token(&pubkey)],
    );
    let dir = tmp.path().to_owned();
    tokio::task::spawn_blocking(move || {
        let client = RegistryClient::new(&format!("http://{addr}")).unwrap();
        let editor = package(&dir, &key, "com.example.editor", "x86_64 = \"v3\"");
        let editor_arm = package(&dir, &key, "com.example.editor", "aarch64 = \"aarch64\"");
        let builds = [editor.clone(), editor_arm];
        let refused = publish(&client, &key, &builds).unwrap_err();
        assert!(format!("{refused:#}").contains("403"), "{refused:#}");

        let status = registry_client::apply(
            &client,
            &key,
            vec![NamespaceClaim::Domain {
                domain: "example.com".into(),
            }],
            "ada@example.com".into(),
            String::new(),
        )
        .unwrap();
        assert_eq!(status["status"], "pending");
        applications::approve(&storage, &pubkey, "owner", tilo_clock::unix_now(), &proofs).unwrap();
        assert_eq!(
            client.application(&pubkey).unwrap().unwrap()["status"],
            "approved"
        );

        let outside = package(&dir, &key, "org.example.editor", "x86_64 = \"v3\"");
        let rejected = publish(&client, &key, &[outside]).unwrap_err();
        assert!(
            format!("{rejected:#}").contains("outside this publisher's verified namespaces"),
            "{rejected:#}"
        );

        let published = publish(&client, &key, &builds).unwrap();
        assert_eq!(published.len(), 2);
        assert!(
            published
                .iter()
                .all(|build| build.package_id == "com.example.editor"
                    && build.action == Some(registry_types::EntryAction::Add))
        );
        let digest = blake3::hash(&editor).to_hex().to_string();
        assert_eq!(
            published.first().map(|build| build.url.as_str()),
            Some(format!("https://apps.tilo.so/artifacts/{digest}.tapp").as_str())
        );
        let pending = storage.load_pending_submissions().unwrap();
        let [submission] = pending.as_slice() else {
            panic!("expected one pending submission, got {pending:?}");
        };
        let mut architectures: Vec<&str> = submission
            .change
            .entry
            .versions
            .iter()
            .map(|version| version.target.architecture.as_str())
            .collect();
        architectures.sort_unstable();
        assert_eq!(
            architectures,
            ["aarch64", "x86_64"],
            "one request lists every build"
        );
        let reviewed = registry_server::review_entry_bytes(
            &storage,
            &submission.change.entry,
            registry_server::InspectionPolicy::default(),
        )
        .unwrap();
        assert_eq!(reviewed.len(), 2);
        assert!(
            reviewed
                .iter()
                .any(|(_, bytes)| bytes.as_slice() == editor.as_slice())
        );

        a_release_before_review_extends_the_waiting_request(&client, &key, &dir);
    })
    .await
    .unwrap();
}

/// A second release while the first waits for review: the new request keeps
/// every version the waiting one lists, and nobody else can read it.
fn a_release_before_review_extends_the_waiting_request(
    client: &RegistryClient,
    key: &SigningKey,
    dir: &std::path::Path,
) {
    let stranger = SigningKey::from_bytes(&[5; 32]);
    assert_eq!(
        client.pending(&stranger, "com.example.editor").unwrap(),
        None,
        "only the publisher reads its pending entry"
    );
    let next = release(dir, key, "com.example.editor", "x86_64 = \"v3\"", "1.1.0");
    let again = publish(client, key, &[next]).unwrap();
    assert_eq!(
        again.first().and_then(|build| build.action),
        Some(registry_types::EntryAction::Add),
        "a release before review extends the waiting request"
    );
    let waiting = client.pending(key, "com.example.editor").unwrap().unwrap();
    let mut versions: Vec<(&str, &str)> = waiting
        .versions
        .iter()
        .map(|version| {
            (
                version.version.as_str(),
                version.target.architecture.as_str(),
            )
        })
        .collect();
    versions.sort_unstable();
    assert_eq!(
        versions,
        [
            ("1.0.0", "aarch64"),
            ("1.0.0", "x86_64"),
            ("1.1.0", "x86_64")
        ]
    );
}

fn publish(
    client: &RegistryClient,
    key: &SigningKey,
    builds: &[Vec<u8>],
) -> anyhow::Result<Vec<registry_client::Published>> {
    let retention = registry_types::retention::RetentionPolicy::default();
    registry_client::publish(client, key, builds, None, None, &retention)
}
