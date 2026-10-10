use super::*;
use automerge::{ReadDoc, transaction::Transactable};
use collab_types::{Ed25519PublicKey, OpClass, SignedOp};
use tilo_user_identity::DeviceCapabilities;

const APP: &str = "dev.tilo.notes";
const RECORD: &str = "tab:0000000000000001";
const KIND: &str = "notes.notebook";

struct Profile {
    dir: tempfile::TempDir,
    workspace: Arc<ProfileWorkspace>,
    room: String,
    root_key: SigningKey,
    key: SigningKey,
    device: DeviceCert,
}

/// A profile with its own workspace, and a foreign device certified by a
/// root key the profile never sees.
fn profile() -> Profile {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Arc::new(ProfileWorkspace::open(dir.path()).unwrap());
    workspace.adopt("person-1").unwrap();
    let room = workspace.room();
    let root_key = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[2; 32]);
    let mut device = DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "runtime".into(),
        capabilities: DeviceCapabilities {
            foreign: true,
            ..DeviceCapabilities::member()
        },
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut device, &root_key).unwrap();
    Profile {
        dir,
        workspace,
        room,
        root_key,
        key,
        device,
    }
}

/// `APP` declares `KIND` when installed; `dev` serves builds that are not.
fn creation(installed: bool, dev: bool) -> RecordCreation {
    RecordCreation {
        declared_kinds: Box::new(move |app| (installed && app == APP).then(|| vec![KIND.into()])),
        dev,
    }
}

fn open(profile: &Profile, creation: RecordCreation) -> RoomOwner {
    open_with(profile, creation, ChannelKeyProvider::default())
}

fn open_with(profile: &Profile, creation: RecordCreation, keys: ChannelKeyProvider) -> RoomOwner {
    RoomOwner::open(
        profile.dir.path(),
        &profile.room,
        (profile.key.clone(), profile.device.clone()),
        creation,
        Arc::clone(&profile.workspace),
        keys,
    )
    .unwrap()
}

/// A write as the SDK signs one: `text` put in the app's document, sent as
/// a snapshot signed by the device and sequenced by the clock.
fn write(profile: &Profile, doc: &mut automerge::AutoCommit, text: &str) -> OwnerCommitRequest {
    signed_write(&profile.room, &profile.key, doc, text)
}

fn signed_write(
    room: &str,
    key: &SigningKey,
    doc: &mut automerge::AutoCommit,
    text: &str,
) -> OwnerCommitRequest {
    doc.put(automerge::ROOT, "text", text).unwrap();
    doc.commit();
    let now = SystemTime::now();
    let sequence = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_micros();
    let mut envelope = SignedOp::new_unsigned(
        collab_types::DocumentMutation::Snapshot(doc.save())
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(APP, room, RECORD),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(APP, room, RECORD),
        OpClass::Splice,
        now,
        u64::try_from(sequence).unwrap(),
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    OwnerCommitRequest {
        room: room.to_owned(),
        operation: CommitOperation::Document {
            app: APP.into(),
            record: RECORD.into(),
            envelope: Box::new(envelope),
        },
    }
}

fn commit(
    owner: &RoomOwner,
    request: &OwnerCommitRequest,
    kind: Option<&str>,
) -> Result<OwnerCommitReceipt, String> {
    tilo_rt::try_app()
        .unwrap()
        .block_on(owner.commit_with_record_kind(request.clone(), kind.map(str::to_owned)))
        .unwrap()
}

fn text(owner: &RoomOwner) -> Option<String> {
    let bytes = owner.committed.lock().unwrap().peer.document(APP, RECORD)?;
    let doc = automerge::AutoCommit::load(&bytes).unwrap();
    doc.get(automerge::ROOT, "text")
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
}

#[test]
fn a_new_room_is_governed_by_the_device_key_alone_and_holds_no_records() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    assert!(owner.requires_room(&profile.room));
    assert!(!owner.requires_room(&tilo_desktop_model::RoomId::new().to_string()));
    let peer = owner.committed.lock().unwrap().peer.clone();
    assert!(peer.rows().is_empty(), "apps start empty");
    assert_eq!(peer.authority(), &profile.device);
    // No root private key reaches the profile, in any file.
    let root_key = profile.root_key.to_bytes();
    let mut files = vec![profile.dir.path().to_path_buf()];
    while let Some(path) = files.pop() {
        if path.is_dir() {
            files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes
                    .windows(root_key.len())
                    .any(|window| window == root_key),
                "{} holds the root private key",
                path.display()
            );
        }
    }
    assert!(profile.dir.path().join(ROOM_OWNER).is_file());
}

#[test]
fn a_first_write_in_a_declared_kind_creates_the_record_durably() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let mut doc = automerge::AutoCommit::new();
    let request = write(&profile, &mut doc, "first");
    let receipt = commit(&owner, &request, Some(KIND)).unwrap();
    receipt.validate_for(&request).unwrap();
    assert_eq!(receipt.generation, 2);
    assert_eq!(text(&owner).as_deref(), Some("first"));

    // An exact retry is the same commit.
    let again = commit(&owner, &request, Some(KIND)).unwrap();
    assert_eq!(
        (again.generation, again.aggregate_hash),
        (receipt.generation, receipt.aggregate_hash)
    );
    // A later write needs no kind: the record exists.
    let later = write(&profile, &mut doc, "second");
    assert_eq!(commit(&owner, &later, None).unwrap().generation, 3);
    drop(owner);

    let reopened = open(&profile, creation(true, false));
    assert_eq!(text(&reopened).as_deref(), Some("second"));
    let retried = commit(&reopened, &request, None).unwrap();
    assert_eq!(
        retried.generation, 3,
        "the first write survived the restart"
    );
}

#[test]
fn a_missing_record_in_an_undeclared_or_unnamed_kind_is_refused() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let stored = std::fs::read(profile.dir.path().join(ROOM_OWNER)).unwrap();
    let request = write(&profile, &mut automerge::AutoCommit::new(), "first");
    let error = commit(&owner, &request, Some("notes.other")).unwrap_err();
    assert!(error.contains("does not declare"), "{error}");
    let error = commit(&owner, &request, None).unwrap_err();
    assert!(error.contains("named no kind"), "{error}");
    assert_eq!(text(&owner), None);
    assert_eq!(
        std::fs::read(profile.dir.path().join(ROOM_OWNER)).unwrap(),
        stored,
        "nothing refused is written"
    );
}

#[test]
fn only_dev_mode_trusts_a_build_that_is_not_installed() {
    let profile = profile();
    let request = write(&profile, &mut automerge::AutoCommit::new(), "first");
    let installed_only = open(&profile, creation(false, false));
    assert!(commit(&installed_only, &request, Some(KIND)).is_err());
    drop(installed_only);
    let dev = open(&profile, creation(false, true));
    commit(&dev, &request, Some("dev.any-kind")).unwrap();
    assert_eq!(text(&dev).as_deref(), Some("first"));
    drop(dev);
    // Dev mode never widens what an installed app declared.
    let profile = self::profile();
    let installed = open(&profile, creation(true, true));
    let request = write(&profile, &mut automerge::AutoCommit::new(), "first");
    assert!(commit(&installed, &request, Some("dev.any-kind")).is_err());
}

#[test]
fn a_write_signed_by_another_key_is_refused() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let other = SigningKey::from_bytes(&[9; 32]);
    let error = commit(
        &owner,
        &signed_write(
            &profile.room,
            &other,
            &mut automerge::AutoCommit::new(),
            "x",
        ),
        Some(KIND),
    )
    .unwrap_err();
    assert!(error.contains("signer"), "{error}");
}

#[test]
fn another_device_key_cannot_open_the_room() {
    let profile = profile();
    drop(open(&profile, creation(true, false)));
    let error = RoomOwner::open(
        profile.dir.path(),
        &profile.room,
        (SigningKey::from_bytes(&[9; 32]), profile.device.clone()),
        creation(true, false),
        Arc::clone(&profile.workspace),
        ChannelKeyProvider::default(),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("another device key"), "{error}");
}

#[test]
fn effect_receipts_are_kept_in_the_workspace() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(5));
    let receipt = tilo_action_broker::policy::redacted_effect_receipt(
        invocation,
        "person-1",
        tilo_actions::ActionContext::default(),
        tilo_actions::EffectReceiptStatus::Succeeded,
    );
    let request = OwnerCommitRequest {
        room: profile.room.clone(),
        operation: CommitOperation::EffectReceipt {
            invocation: invocation.0,
            redacted: serde_json::to_vec(&receipt).unwrap(),
        },
    };
    for _ in 0..2 {
        commit(&owner, &request, None)
            .unwrap()
            .validate_for(&request)
            .unwrap();
    }
    let reopened = ProfileWorkspace::open(profile.dir.path()).unwrap();
    let tree = tilo_action_broker::services::WorkspaceAuthority::snapshot(&reopened).unwrap();
    assert_eq!(tree.workspaces[0].effect_receipts, [receipt]);
}

#[test]
fn an_adopted_room_refuses_every_commit() {
    let profile = profile();
    let adopted = AdoptedRoom {
        room: profile.room.clone(),
    };
    assert!(adopted.requires_room(&profile.room));
    assert!(adopted.health().is_err());
    let result = tilo_rt::try_app()
        .unwrap()
        .block_on(adopted.commit(write(&profile, &mut automerge::AutoCommit::new(), "x")))
        .unwrap();
    assert!(result.is_err());
}

const PROVISIONED_TEST: &str =
    "standalone::owner::tests::a_provisioned_profile_governs_its_room_with_its_trusted_device";

/// The owner the runtime installs, in a provisioned profile: its trusted
/// device key signs the room, an app that is not installed creates a record
/// only in dev mode, and the record outlives the owner.
#[test]
fn a_provisioned_profile_governs_its_room_with_its_trusted_device() {
    let Some(profile) = super::super::tests::isolated_root(PROVISIONED_TEST) else {
        return;
    };
    drop(super::super::Standalone::start(&profile, &mut |_| {}, false).unwrap());
    let workspace = Arc::new(ProfileWorkspace::open(&profile).unwrap());
    let room = workspace.room();
    let apps = Arc::new(crate::apps::Apps::from_env(&profile, &room));
    let (key, _) = tilo_identity_host::broker::trusted_current_device(&room).unwrap();
    let request = signed_write(&room, &key, &mut automerge::AutoCommit::new(), "first");
    let committed = |dev: bool| {
        let owner = super::super::room_owner(
            &profile,
            &room,
            &workspace,
            &apps,
            dev,
            ChannelKeyProvider::default(),
        )
        .unwrap();
        tilo_rt::try_app()
            .unwrap()
            .block_on(owner.commit_with_record_kind(request.clone(), Some("test.memo".into())))
            .unwrap()
    };
    assert!(
        committed(false).is_err(),
        "no installed app declares test.memo"
    );
    let first = committed(true).unwrap();
    first.validate_for(&request).unwrap();
    let retried = committed(false).unwrap();
    assert_eq!(
        (retried.generation, retried.aggregate_hash),
        (first.generation, first.aggregate_hash),
        "the record the dev build created survived its owner"
    );
}

#[test]
fn a_profile_adopted_from_the_desktop_session_refuses_every_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut tree = super::super::workspace::bootstrap();
    tree.workspaces[0].set_owner("desktop-owner".to_owned());
    std::fs::write(
        dir.path().join(super::super::workspace::DESKTOP_WORKSPACE),
        serde_json::to_vec(&tree).unwrap(),
    )
    .unwrap();
    let workspace = Arc::new(ProfileWorkspace::open(dir.path()).unwrap());
    let room = workspace.room();
    let apps = Arc::new(crate::apps::Apps::new(
        None,
        dir.path(),
        &room,
        PathBuf::new(),
        Vec::new(),
    ));
    let owner = super::super::room_owner(
        dir.path(),
        &room,
        &workspace,
        &apps,
        true,
        ChannelKeyProvider::default(),
    )
    .unwrap();
    assert!(owner.requires_room(&room));
    assert!(owner.health().is_err());
    assert!(!dir.path().join(ROOM_OWNER).exists(), "no owner is created");
}

/// Channel keys as this device holds them stewarding the room, with
/// `readers` allowed to read its channels, kept under `state`.
fn steward_keys(profile: &Profile, state: &str, readers: &[SigningKey]) -> ChannelKeyProvider {
    use tilo_action_broker::channel_encryption::{
        ChannelEncryption, ChannelEncryptionConfig, ChannelKeyAccess,
        testing::{Directory, Policy},
    };
    let readers = readers
        .iter()
        .map(|key| Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()))
        .collect();
    let service = ChannelEncryption::open(
        ChannelEncryptionConfig {
            root: profile.dir.path().join(state),
            vault: channel_crypto::VaultKey::new([7; 32]),
            device_key: profile.key.clone(),
        },
        Box::new(Directory {
            governs: true,
            steward: Ed25519PublicKey::from_bytes(profile.key.verifying_key().to_bytes()),
            policy: Policy {
                allowed: Arc::new(Mutex::new(readers)),
                ..Policy::default()
            },
        }),
        Arc::new(Mutex::new(
            tilo_action_broker::collab_core::BrokerChannels::default(),
        )),
    )
    .unwrap();
    let keys = ChannelKeyProvider::default();
    keys.set(Some(ChannelKeyAccess::new(service)));
    keys
}

/// `write` as an SDK app seals it: the payload sealed under the channel's
/// current content key, then signed.
fn seal(
    profile: &Profile,
    keys: &ChannelKeyProvider,
    mut request: OwnerCommitRequest,
) -> OwnerCommitRequest {
    use tilo_action_broker::channel_encryption::ChannelAccess;
    let CommitOperation::Document { envelope, .. } = &mut request.operation else {
        panic!("a document write")
    };
    let channel = collab_types::document_channel_id(APP, &profile.room, RECORD);
    let room = RoomId::parse(&profile.room).unwrap();
    let (ChannelAccess::Ready(content), _) = keys.get().unwrap().access(room, channel) else {
        panic!("the steward holds the room's channel keys")
    };
    let (metadata, sealed) = content
        .seal_content(
            envelope.artifact,
            collab_types::ContentRetention::Checkpoint,
            &envelope.payload,
        )
        .unwrap();
    envelope.payload = sealed;
    envelope.encryption = Some(metadata);
    collab_types::sign_op(envelope, &profile.key).unwrap();
    request
}

#[test]
fn a_sealed_write_is_opened_after_its_checks_and_reads_back_after_reopen() {
    let profile = profile();
    let keys = steward_keys(&profile, "channels", std::slice::from_ref(&profile.key));
    let owner = open_with(&profile, creation(true, false), keys.clone());
    let mut doc = automerge::AutoCommit::new();
    let request = seal(
        &profile,
        &keys,
        write(&profile, &mut doc, "sealed by the app"),
    );
    commit(&owner, &request, Some(KIND)).unwrap();
    assert_eq!(text(&owner).as_deref(), Some("sealed by the app"));
    drop(owner);
    let reopened = open_with(&profile, creation(true, false), keys);
    assert_eq!(text(&reopened).as_deref(), Some("sealed by the app"));
    let plain = write(&profile, &mut doc, "a plaintext downgrade");
    assert!(
        commit(&reopened, &plain, Some(KIND)).is_err(),
        "the encrypted record refuses plaintext"
    );
}

#[test]
fn a_sealed_write_the_owner_cannot_open_under_the_current_epoch_is_refused() {
    let profile = profile();
    let keys = steward_keys(&profile, "channels", std::slice::from_ref(&profile.key));
    let owner = open_with(&profile, creation(true, false), keys.clone());
    let mut doc = automerge::AutoCommit::new();
    let mut stale = seal(&profile, &keys, write(&profile, &mut doc, "stale"));
    if let CommitOperation::Document { envelope, .. } = &mut stale.operation {
        let metadata = envelope.encryption.as_mut().unwrap();
        metadata.epoch = metadata.epoch.wrapping_add(1);
        collab_types::sign_op(envelope, &profile.key).unwrap();
    }
    assert!(
        commit(&owner, &stale, Some(KIND)).is_err(),
        "another epoch is refused"
    );
    let mut tampered = seal(&profile, &keys, write(&profile, &mut doc, "tampered"));
    if let CommitOperation::Document { envelope, .. } = &mut tampered.operation {
        envelope.payload[0] ^= 1;
    }
    assert!(
        commit(&owner, &tampered, Some(KIND)).is_err(),
        "a tampered write is refused"
    );
    assert_eq!(
        text(&owner),
        None,
        "nothing was admitted, not even the record"
    );
}

#[test]
fn without_a_current_key_the_owner_admits_no_sealed_write() {
    let profile = profile();
    let keys = steward_keys(&profile, "channels", std::slice::from_ref(&profile.key));
    let mut doc = automerge::AutoCommit::new();
    let request = seal(&profile, &keys, write(&profile, &mut doc, "sealed"));
    let unkeyed = open_with(
        &profile,
        creation(true, false),
        ChannelKeyProvider::default(),
    );
    assert!(
        commit(&unkeyed, &request, Some(KIND)).is_err(),
        "no channel service"
    );
    drop(unkeyed);
    let revoked = steward_keys(&profile, "unread-channels", &[]);
    let owner = open_with(&profile, creation(true, false), revoked);
    assert!(
        commit(&owner, &request, Some(KIND)).is_err(),
        "a device that no longer reads the channel opens nothing"
    );
    assert_eq!(text(&owner), None);
}
