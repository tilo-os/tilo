//! Bot entries end to end: program uploads, submission checks, review and
//! serving the approved program as a content-addressed blob.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ed25519_dalek::SigningKey;
use registry_server::serve::ServeOptions;
use registry_server::{
    AppState, PublisherAllowlist, PublisherRow, ServerIdentity, Storage, bind_and_run,
};
use tilo_automation::program::canonical_bytes;

struct Bots {
    _tmp: tempfile::TempDir,
    base: String,
    storage: Storage,
    publisher: SigningKey,
    owner: SigningKey,
}

/// A registry on a loopback port whose base URL names that port, so the
/// program URLs it hands out are fetchable by the client under test.
async fn spawn() -> Bots {
    let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let base = format!("http://127.0.0.1:{port}");
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open(tmp.path()).unwrap();
    let owner = SigningKey::from_bytes(&[3; 32]);
    let identity = ServerIdentity {
        owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
        name: "bots".into(),
        description: "bot directory tests".into(),
        base_url: base.clone(),
    };
    storage.save_identity(&identity).unwrap();
    let publisher = SigningKey::from_bytes(&[4; 32]);
    let mut allowlist = PublisherAllowlist::empty();
    allowlist.upsert(PublisherRow {
        pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        can_add_new: true,
        note: "bot publisher".into(),
        namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
    });
    storage.save_allowlist(&allowlist).unwrap();
    let state = AppState::with_options(storage.clone(), identity, ServeOptions::default());
    bind_and_run(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        state,
    )
    .await
    .unwrap();
    Bots {
        _tmp: tmp,
        base,
        storage,
        publisher,
        owner,
    }
}

/// A one-step program: post a fixed text in Chat whenever a chat message
/// arrives.
fn echo_program() -> Vec<u8> {
    let shortcut: tilo_automation::Shortcut = serde_json::from_value(serde_json::json!({
        "id": "echo",
        "name": "Echo",
        "steps": [{
            "id": "post",
            "step": "invoke",
            "action": {"app": "so.tilo.chat", "kind": "chat.workspace", "action": "post"},
            "resource": {"first_of_kind": "chat.workspace"},
            "args": {"text": {"literal": "echo"}}
        }],
        "triggers": [{"trigger": "incoming_message", "app_id": "so.tilo.chat"}]
    }))
    .unwrap();
    canonical_bytes(&shortcut).unwrap()
}

fn http() -> reqwest::blocking::Client {
    tilo_net::http::blocking_client().build().unwrap()
}

fn put_program(bots: &Bots, key: &SigningKey, bytes: &[u8]) -> reqwest::blocking::Response {
    let digest = blake3::hash(bytes).to_hex().to_string();
    http()
        .put(format!("{}/uploads/{digest}.program", bots.base))
        .header(
            registry_types::upload::UPLOAD_PUBLISHER_HEADER,
            hex::encode(key.verifying_key().to_bytes()),
        )
        .header(
            registry_types::upload::UPLOAD_SIGNATURE_HEADER,
            registry_types::upload::sign_upload(key, &digest),
        )
        .body(bytes.to_vec())
        .send()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_allowlisted_publisher_uploads_a_program_privately() {
    let bots = spawn().await;
    let program = echo_program();
    let digest = blake3::hash(&program).to_hex().to_string();
    let response = tokio::task::block_in_place(|| put_program(&bots, &bots.publisher, &program));
    assert_eq!(response.status(), 201);
    let body: serde_json::Value = tokio::task::block_in_place(|| response.json().unwrap());
    assert_eq!(body["url"], format!("{}/blobs/{digest}", bots.base));
    assert_eq!(
        bots.storage
            .load_program_upload(&digest)
            .unwrap()
            .as_deref(),
        Some(program.as_slice())
    );
    // Not served before review approves a version that binds it.
    let get = tokio::task::block_in_place(|| {
        http()
            .get(format!("{}/blobs/{digest}", bots.base))
            .send()
            .unwrap()
    });
    assert_eq!(get.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_canonical_program_upload_is_refused() {
    let bots = spawn().await;
    let value: serde_json::Value = serde_json::from_slice(&echo_program()).unwrap();
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    let response = tokio::task::block_in_place(|| put_program(&bots, &bots.publisher, &pretty));
    assert_eq!(response.status(), 422);
    let oversized = vec![b'x'; tilo_automation::program::MAX_PROGRAM_BYTES + 1];
    let response = tokio::task::block_in_place(|| put_program(&bots, &bots.publisher, &oversized));
    assert_eq!(response.status(), 413);
    let stranger = SigningKey::from_bytes(&[9; 32]);
    let response = tokio::task::block_in_place(|| put_program(&bots, &stranger, &echo_program()));
    assert_eq!(response.status(), 403);
}

#[test]
fn an_unbound_program_blob_is_pruned() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Storage::open(tmp.path()).unwrap();
    let program = echo_program();
    let digest = storage.save_program_blob(&program).unwrap();
    assert_eq!(
        storage.load_payload_blob(&digest).unwrap().as_deref(),
        Some(program.as_slice())
    );
    let report = storage
        .lock_snapshot()
        .unwrap()
        .prune_unreferenced()
        .unwrap();
    assert_eq!(report.payload_blobs, 1, "no entry binds the program");
    assert!(storage.load_payload_blob(&digest).unwrap().is_none());
}

fn echo_requires() -> registry_types::BotRequirements {
    registry_types::BotRequirements {
        connectors: Vec::new(),
        apps: vec!["so.tilo.chat".into()],
        appearance: false,
        triggers: vec!["incoming_message".into()],
    }
}

fn bot_entry(
    bots: &Bots,
    program: &[u8],
    version: &str,
    updated_at: u64,
    requires: registry_types::BotRequirements,
) -> registry_types::RegistryEntry {
    let hash = blake3::hash(program).to_hex().to_string();
    registry_types::RegistryEntry {
        package_id: "so.tilo.bots.echo".into(),
        kind: registry_types::EntryKind::Bot,
        name: "Echo".into(),
        description: "Posts echo".into(),
        tagline: "Echo in chat".into(),
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: vec!["Bots".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(bots.publisher.verifying_key().to_bytes()),
        versions: vec![registry_types::RegistryVersion {
            version: version.into(),
            released_at_unix: updated_at,
            artifact: registry_types::VersionArtifact::Bot(registry_types::BotArtifact {
                program_url: registry_types::bot_program_url(&bots.base, &hash),
                program_blake3_hex: hash,
                program_size_bytes: program.len() as u64,
                requires,
            }),
        }],
        updated_at_unix: updated_at,
        signature_hex: String::new(),
    }
}

fn submit(
    bots: &Bots,
    action: registry_types::EntryAction,
    mut entry: registry_types::RegistryEntry,
) -> Result<registry_server::PendingSubmission, registry_server::ModerationError> {
    registry_types::sign_entry(&mut entry, &bots.publisher);
    let change = registry_types::sign_change_request(action, entry, &bots.publisher);
    registry_server::ModerationService::new(bots.storage.clone()).submit(change, 1_000)
}

fn approve(bots: &Bots, pending: &registry_server::PendingSubmission) {
    registry_server::submission_review::approve(
        &bots.storage,
        pending,
        "owner",
        "reviewed",
        2_000,
        registry_server::InspectionPolicy::default(),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_entry_needs_its_program_uploaded_and_honest_requirements() {
    use registry_types::EntryAction::Add;
    let bots = spawn().await;
    let program = echo_program();
    let early = submit(
        &bots,
        Add,
        bot_entry(&bots, &program, "1.0.0", 100, echo_requires()),
    );
    assert!(
        matches!(
            early,
            Err(registry_server::ModerationError::BotProgram { .. })
        ),
        "{early:?}"
    );
    tokio::task::block_in_place(|| {
        assert_eq!(put_program(&bots, &bots.publisher, &program).status(), 201);
    });
    let mut wrong = echo_requires();
    wrong.apps.push("so.tilo.notes".into());
    let dishonest = submit(&bots, Add, bot_entry(&bots, &program, "1.0.0", 100, wrong));
    assert!(
        matches!(
            dishonest,
            Err(registry_server::ModerationError::BotProgram { .. })
        ),
        "{dishonest:?}"
    );
    let mut elsewhere = bot_entry(&bots, &program, "1.0.0", 100, echo_requires());
    if let registry_types::VersionArtifact::Bot(bot) = &mut elsewhere.versions[0].artifact {
        bot.program_url = format!("https://evil.example/blobs/{}", bot.program_blake3_hex);
    }
    assert!(
        submit(&bots, Add, elsewhere).is_err(),
        "a program served by another host"
    );
    submit(
        &bots,
        Add,
        bot_entry(&bots, &program, "1.0.0", 100, echo_requires()),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn approving_a_bot_publishes_its_program_blob() {
    let bots = spawn().await;
    let program = echo_program();
    tokio::task::block_in_place(|| {
        assert_eq!(put_program(&bots, &bots.publisher, &program).status(), 201);
    });
    let pending = submit(
        &bots,
        registry_types::EntryAction::Add,
        bot_entry(&bots, &program, "1.0.0", 100, echo_requires()),
    )
    .unwrap();
    let inspected = registry_server::submission_review::inspect(
        &bots.storage,
        &pending,
        registry_server::InspectionPolicy::default(),
    )
    .unwrap();
    assert!(
        matches!(&inspected[0].0, registry_server::Inspection::Program(p) if p.steps == 1 && p.apps == ["so.tilo.chat"]),
        "{inspected:?}"
    );
    approve(&bots, &pending);
    let digest = blake3::hash(&program).to_hex().to_string();
    let served = tokio::task::block_in_place(|| {
        http()
            .get(format!("{}/blobs/{digest}", bots.base))
            .send()
            .unwrap()
            .bytes()
            .unwrap()
    });
    assert_eq!(served.as_ref(), program.as_slice());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_update_may_not_change_the_entry_kind() {
    let bots = spawn().await;
    let program = echo_program();
    tokio::task::block_in_place(|| {
        assert_eq!(put_program(&bots, &bots.publisher, &program).status(), 201);
    });
    let pending = submit(
        &bots,
        registry_types::EntryAction::Add,
        bot_entry(&bots, &program, "1.0.0", 100, echo_requires()),
    )
    .unwrap();
    approve(&bots, &pending);
    let mut as_app = bot_entry(&bots, &program, "1.0.0", 200, echo_requires());
    as_app.kind = registry_types::EntryKind::App;
    as_app.versions = vec![registry_types::RegistryVersion {
        version: "2.0.0".into(),
        released_at_unix: 200,
        artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
            tapp_url: format!("https://apps.tilo.so/artifacts/{}.tapp", "ef".repeat(32)),
            tapp_root_hash_hex: "cd".repeat(32),
            tapp_size_bytes: 1,
            tapp_blake3_hex: "ef".repeat(32),
            target: registry_types::RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: "x86_64".into(),
                isa: "x86_64-v3".into(),
                runtime: None,
                interfaces: Vec::new(),
            },
        }),
    }];
    let refused = submit(&bots, registry_types::EntryAction::Update, as_app);
    assert!(
        matches!(
            refused,
            Err(registry_server::ModerationError::KindChanged { .. })
        ),
        "{refused:?}"
    );
}

struct NoConnectors;

impl tilo_bot_directory::HostConnectors for NoConnectors {
    fn status(&self, _: &str, _: &str) -> tilo_bot_directory::ConnectorState {
        tilo_bot_directory::ConnectorState::NotInstalled
    }
}

/// Promote the approved entries into a snapshot the owner signs, as the
/// operator's publish step does.
fn publish_snapshot(bots: &Bots) {
    let storage = &bots.storage;
    let previous = storage.load_snapshot().unwrap();
    let mut entries = storage.load_entries().unwrap();
    entries.sort_by(|left, right| left.package_id.cmp(&right.package_id));
    let now = tilo_clock::try_unix_now().unwrap();
    let mut snapshot = registry_types::SignedRegistrySnapshot {
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
        publishers: storage
            .load_allowlist()
            .unwrap()
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
        bots_signature_hex: String::new(),
    };
    registry_types::sign_snapshot(&mut snapshot, &bots.owner).unwrap();
    storage.save_snapshot(&snapshot).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_published_bot_installs_from_the_directory() {
    let bots = spawn().await;
    let program = echo_program();
    let release = registry_client::BotRelease {
        program: &program,
        package_id: "so.tilo.bots.echo",
        version: "1.0.0",
        registry_origin: &bots.base,
        connectors: &[],
    };
    let entry =
        registry_client::build_bot_entry(&release, &bots.publisher, None, None, 100).unwrap();
    tokio::task::block_in_place(|| {
        let client = registry_client::RegistryClient::new(&bots.base).unwrap();
        let url = client.upload_program(&bots.publisher, &program).unwrap();
        assert_eq!(url, entry.versions[0].bot().unwrap().program_url);
    });
    let pending = submit(&bots, registry_types::EntryAction::Add, entry).unwrap();
    approve(&bots, &pending);
    publish_snapshot(&bots);

    let config = app_cli::config::RegistryConfig {
        name: "bots".into(),
        base_url: bots.base.clone(),
        owner_pubkey_hex: hex::encode(bots.owner.verifying_key().to_bytes()),
        enabled: true,
        channel: tilo_app_package::version::ReleaseChannel::default(),
    };
    let config_dir = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_STORE_CONFIG_DIR", config_dir.path());
    let (listed, fetched) = tokio::task::block_in_place(|| {
        let listed = app_cli::client::fetch_all_versions(&config, "so.tilo.bots.echo").unwrap();
        let entries = [listed];
        let (_, artifact) =
            app_cli::bots::listed_version(&entries, "so.tilo.bots.echo", "1.0.0").unwrap();
        let fetched = app_cli::bots::fetch_bot_program(artifact).unwrap();
        let [listed] = entries;
        (listed, fetched)
    });
    assert_eq!(fetched, program);
    assert_eq!(listed.entry.kind, registry_types::EntryKind::Bot);
    assert!(
        app_cli::bots::listed_version(std::slice::from_ref(&listed), "so.tilo.bots.echo", "0.9.0")
            .is_none(),
        "an unlisted version is never offered"
    );

    let plan = tokio::task::block_in_place(|| {
        let listed =
            tilo_bot_directory::fetch_listed_bot(&config, "so.tilo.bots.echo", "1.0.0").unwrap();
        assert_eq!(listed.program_text.as_bytes(), program.as_slice());
        tilo_bot_directory::plan_install(
            &listed,
            &tilo_automation::ActionCatalog::new(Vec::new()),
            &NoConnectors,
            &std::collections::BTreeSet::from(["so.tilo.chat".to_owned()]),
            &tilo_action_policy::Ids,
        )
    });
    let args =
        tilo_bot_directory::install_arguments(&plan, &tilo_bot_directory::InstallInputs::default())
            .unwrap();
    assert_eq!(args["source"]["version"], "1.0.0");
    let stale = tokio::task::block_in_place(|| {
        tilo_bot_directory::fetch_listed_bot(&config, "so.tilo.bots.echo", "0.9.0")
    });
    assert!(
        matches!(
            stale,
            Err(tilo_bot_directory::DirectoryError::NotListed { .. })
        ),
        "an unlisted version is refused, not installed from stale bytes"
    );

    // The approved program blob is bound, so pruning keeps it.
    bots.storage
        .lock_snapshot()
        .unwrap()
        .prune_unreferenced()
        .unwrap();
    let digest = blake3::hash(&program).to_hex().to_string();
    assert!(bots.storage.load_payload_blob(&digest).unwrap().is_some());
}
