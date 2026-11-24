//! The devices, endpoints and workspaces the two-device tests run on.
#![allow(
    dead_code,
    reason = "each test binary uses a different subset of these fixtures"
)]

use automerge::AutoCommit;
use collab_types::{ContentRetention, SignedOp, document_artifact_id};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;

use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

/// A member device key `n` certified by `root`.
pub fn device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
    device_with(root, n, DeviceCapabilities::member())
}

/// A device key `n` certified by `root`, with `capabilities`.
pub fn device_with(
    root: &SigningKey,
    n: u8,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

pub use tilo_peer_state::loopback::{addr, endpoint as localhost_endpoint};

/// The workspace `workspace`, made by `a` with `roster` in it, held as an
/// empty peer.
pub fn workspace(
    workspace: &str,
    a: &(SigningKey, DeviceCert),
    roster: &[DeviceCert],
) -> StatePeer {
    let now = tilo_clock::wall();
    let mut peer = StatePeer::new(
        workspace,
        a.1.clone(),
        roster.to_vec(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_exact_admissions().unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(workspace, &a.1, roster).unwrap(),
            documents: Vec::new(),
        },
        &a.0,
        now,
    )
    .unwrap();
    peer
}

/// `sync`, keeping what it holds and is sent in a store in `directory`, as
/// every device holding a workspace does.
pub fn stored(sync: DirectSync, directory: &tempfile::TempDir) -> DirectSync {
    let store = tilo_peer_state::store::Store::open(
        directory.path(),
        tilo_peer_state::store::Appetite::StoreAll,
    )
    .unwrap();
    sync.with_store(std::sync::Arc::new(store))
}

/// The keys every member of `workspace` seals its records under.
pub fn members_keys(workspace: &str) -> key_group::ContentKeys {
    key_group::KeyGroup::create(
        collab_types::WorkspaceId::parse(workspace).unwrap(),
        key_group::MemberKey::generate("member").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap()
}

/// `record` of `app` as a holder of `keys` reads it: every sealed write
/// merged over its base. `None` while `peer` holds none of it.
pub fn opened(
    peer: &StatePeer,
    keys: &key_group::ContentKeys,
    (app, record): (&str, &str),
) -> Option<Vec<u8>> {
    let mut open = |op: &SignedOp| {
        let metadata = op.encryption.ok_or("not sealed")?;
        let ciphertext = collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
        keys.open_content(op.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    };
    peer.publication()
        .opened_document(app, record, &mut open)
        .unwrap()
        .map(|opened| opened.document)
}

/// The heads of `record` of `app` as a holder of `keys` reads it.
pub fn opened_heads(
    peer: &StatePeer,
    keys: &key_group::ContentKeys,
    route: (&str, &str),
) -> Option<Vec<automerge::ChangeHash>> {
    opened(peer, keys, route).map(|bytes| AutoCommit::load(&bytes).unwrap().get_heads())
}

/// Write `record` of `app` on `sync` as `key`'s device, sealed under `keys`
/// ([`sealed_write`]), and publish it.
pub fn write_sealed(
    sync: &DirectSync,
    keys: &key_group::ContentKeys,
    key: &SigningKey,
    route: (&str, &str),
    edit: impl FnOnce(&mut AutoCommit),
) -> NeighbourMsg {
    sync.publish(|peer| sealed_write(peer, keys, key, route, edit))
        .unwrap()
}

/// Write `record` of `app` on `peer` as `key`'s device, sealed under `keys`:
/// the whole record `edit` makes of nothing when `peer` holds none of it, the
/// change `edit` makes of it otherwise.
pub fn sealed_write(
    peer: &mut StatePeer,
    keys: &key_group::ContentKeys,
    key: &SigningKey,
    (app, record): (&str, &str),
    edit: impl FnOnce(&mut AutoCommit),
) -> Result<NeighbourMsg, String> {
    let artifact = document_artifact_id(app, peer.workspace_id(), record);
    let mut seal = |plaintext: &[u8]| {
        keys.seal_content(artifact, ContentRetention::of_frame(plaintext), plaintext)
            .map_err(|error| error.to_string())
    };
    let now = tilo_clock::wall();
    let held = opened(peer, keys, (app, record));
    let starts = held.is_none();
    let mut document = held
        .map_or_else(AutoCommit::new, |base| AutoCommit::load(&base).unwrap())
        .with_actor(automerge::ActorId::from(
            key.verifying_key().to_bytes().to_vec(),
        ));
    let heads = document.get_heads();
    edit(&mut document);
    if starts {
        return peer.publication_mut().publish_sealed_checkpoint(
            app,
            record,
            document.save(),
            &mut seal,
            key,
            now,
        );
    }
    peer.publication_mut().publish_sealed_incremental(
        app,
        record,
        document.save_after(&heads),
        &mut seal,
        key,
        now,
    )
}
