//! A device that takes a workspace's records only as it opens them — a page,
//! a browser environment — is replayed the records every device is sent and
//! each chain's newest part, and a record's history once it opens it. The link
//! it holds is watched: it is sent one record when the host scopes it to one,
//! and it may send only its own frames and only the frames such a device takes
//! at all.

use crate::common::{
    addr, localhost_endpoint, members_keys, opened as read_opened, opened_heads, sealed_write,
    write_sealed,
};

use automerge::transaction::CommitOptions;
use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use ed25519_dalek::SigningKey;
use iroh::protocol::ProtocolHandler;
use neighbour_wire::{
    NeighbourMsg, PEER_PROTOCOL_VERSION, app_document_group, chain_head, document_part,
    on_demand_document,
};
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tilo_actions::{ActionContext, ActionInvoker, Did, EditOp, FieldEdit, PathStep};
use tilo_peer_state::StatePeer;
use tilo_peer_state::direct::{Admitting, DIRECT_ALPN, DirectSync, LinkScope, Replica, Serving};
use tilo_peer_state::store::{Appetite, Frame, Store};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use web_time::SystemTime;

use tilo_peer_state::fixture::certify;
const WORKSPACE: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeee0011";
const APP: &str = "test.app";
const EAGER: &str = "tab:1";
const OTHER: &str = "tab:2";
const ON_DEMAND: &str = "tab:3";
const CHAIN: &str = "chat";

/// The keys the workspace's members seal its records under.
static KEYS: LazyLock<key_group::ContentKeys> = LazyLock::new(|| members_keys(WORKSPACE));

/// The invoker a change names: the person whose device made it. It names the
/// person alone, since a change naming a device carries the invocation that
/// device signed, and a fixture commit has none to carry.
fn invoker(cert: &DeviceCert) -> ActionInvoker {
    ActionInvoker::person(hex::encode(cert.user_public_key))
}

/// Commit the edit already put on `document` as its claim, naming `invoker`.
fn commit(document: &mut AutoCommit, field: &str, invoker: ActionInvoker) {
    let context = ActionContext {
        what: Some(Did::Edit(FieldEdit {
            ty: "fixture".into(),
            field: field.into(),
            path: vec![PathStep::Field(field.into())],
            op: EditOp::Set,
            old: None,
            new: None,
            args: BTreeMap::new(),
        })),
        invoker: Some(invoker),
        ..ActionContext::for_resource("fixture", "fixture", "Fixture")
    };
    let mut options = CommitOptions::default();
    if let Some(message) = context.entry_in(&*document) {
        options.set_message(message);
    }
    document.commit_with(options);
}

/// The edit setting `field` to `value`, made for `invoker`.
fn edit(field: &str, value: &str, invoker: ActionInvoker) -> impl FnOnce(&mut AutoCommit) {
    move |document| {
        document.put(ROOT, field, value).unwrap();
        commit(document, field, invoker);
    }
}

/// The invoker of `sync`'s own device: a change it makes is its person's.
fn own_invoker(sync: &DirectSync) -> ActionInvoker {
    invoker(&certificate(sync))
}

/// `sync`'s open of `channel`, bounded: an open the host never answers is a
/// failure of the link, and the test says so instead of waiting on it.
async fn opened(sync: &DirectSync, channel: [u8; 32]) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(30), sync.open_record(channel))
        .await
        .expect("the host answers the open in time")
}

/// The workspace id the store keeps the frames of.
fn workspace_id() -> tilo_peer_state::join::WorkspaceId {
    tilo_peer_state::join::WorkspaceId::parse(WORKSPACE).unwrap()
}

/// The workspace's host, a device keeping its store, and a member that takes
/// its records only as it opens them: the records the host wrote, by their
/// ids, and the store's directory the caller keeps alive. `root`, `host` and
/// `member` pick the test's own keys: tests run side by side, and two
/// endpoints of one process are two devices only if their keys differ.
fn devices(
    root: u8,
    host: u8,
    member: u8,
    records: &[&str],
) -> (
    DirectSync,
    SigningKey,
    DirectSync,
    SigningKey,
    tempfile::TempDir,
) {
    let root = SigningKey::from_bytes(&[root; 32]);
    let (host_key, host_cert) = certify(&root, host, DeviceCapabilities::member());
    let (member_key, member_cert) = certify(&root, member, DeviceCapabilities::member());
    let members = vec![host_cert.clone(), member_cert];
    let now = SystemTime::now();
    let mut peer = StatePeer::new(
        WORKSPACE,
        host_cert.clone(),
        members.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_exact_admissions().unwrap();
    tilo_peer_state::fixture::found(
        &mut peer,
        (&host_key, &host_cert),
        &members,
        Vec::new(),
        now,
    );
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(directory.path(), Appetite::StoreAll).unwrap());
    let host = DirectSync::over(Replica::new(peer), host_key.clone()).with_store(store);
    for (index, record) in records.iter().enumerate() {
        let value = format!("record {index}");
        write_sealed(
            &host,
            &KEYS,
            &host_key,
            (APP, record),
            edit("value", &value, invoker(&host_cert)),
        );
    }
    let member_peer = StatePeer::from_join(WORKSPACE, host_cert, members, &[], now).unwrap();
    let member = DirectSync::new(member_peer, member_key.clone()).when_opened();
    (host, host_key, member, member_key, directory)
}

/// The endpoints, the host's router and the member's dial of a link a test
/// runs.
struct Linked {
    host: DirectSync,
    host_key: SigningKey,
    member: DirectSync,
    router: iroh::protocol::Router,
    member_endpoint: iroh::Endpoint,
    dialing: tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>,
    /// The store's directory, kept alive for the test.
    _directory: tempfile::TempDir,
}

impl Linked {
    /// Link `member` to `host` over iroh, the host answering `handler` as
    /// [`DIRECT_ALPN`], and wait for the member to catch up.
    async fn start<H: ProtocolHandler + 'static>(
        host: DirectSync,
        host_key: SigningKey,
        member: DirectSync,
        member_key: SigningKey,
        directory: tempfile::TempDir,
        handler: H,
    ) -> Self {
        let host_endpoint = localhost_endpoint(&host_key).await;
        let router = iroh::protocol::Router::builder(host_endpoint.clone())
            .accept(DIRECT_ALPN, handler)
            .spawn();
        let member_endpoint = localhost_endpoint(&member_key).await;
        let mut caught_up = member.caught_up();
        let dialing = tokio::spawn({
            let member = member.clone();
            let member_endpoint = member_endpoint.clone();
            let target = addr(&host_endpoint);
            async move { member.connect(&member_endpoint, target).await }
        });
        tokio::time::timeout(Duration::from_secs(30), caught_up.wait_for(|up| *up))
            .await
            .expect("the member catches up")
            .expect("the link is still up");
        Self {
            host,
            host_key,
            member,
            router,
            member_endpoint,
            dialing,
            _directory: directory,
        }
    }

    async fn stop(self) {
        self.dialing.abort();
        let _ = tokio::time::timeout(Duration::from_secs(10), self.router.shutdown()).await;
        let _ = tokio::time::timeout(Duration::from_secs(10), self.member_endpoint.close()).await;
    }
}

/// Wait until `done` holds of `sync`'s replica; `what` names the wait that gave up.
async fn until(sync: &DirectSync, what: &str, done: impl Fn(&StatePeer) -> bool + Sync) {
    let mut changes = sync.changes();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !sync.read(&done) {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

/// `record`'s heads in `sync`'s replica.
fn heads(sync: &DirectSync, record: &str) -> Option<Vec<automerge::ChangeHash>> {
    sync.read(|peer| opened_heads(peer, &KEYS, (APP, record)))
}

/// The certificate of `sync`'s own device, as its replica holds it.
fn certificate(sync: &DirectSync) -> tilo_user_identity::DeviceCert {
    let device = sync.device();
    sync.read(|peer| {
        peer.certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device)
            .expect("the device is certified")
    })
}

/// A record the workspace sends only to a device that opens it is not sent
/// before the open, its history comes with the open, and an open of a record
/// the host does not hold is answered rather than waited on.
#[tokio::test(flavor = "multi_thread")]
async fn an_on_demand_record_is_not_sent_until_it_is_opened() {
    let record = on_demand_document(ON_DEMAND);
    let (host, host_key, member, member_key, directory) = devices(0x10, 0x11, 0x12, &[EAGER]);
    let history = sealed_write(
        &mut host.read(Clone::clone),
        &KEYS,
        &host_key,
        (APP, &record),
        edit("value", "on demand", own_invoker(&host)),
    )
    .unwrap();
    host.store()
        .expect("the host keeps a store")
        .retain(workspace_id(), &Frame::of(&history).unwrap())
        .expect("the store keeps the record's history");
    let handler = host.clone();
    let linked = Linked::start(host, host_key, member, member_key, directory, handler).await;
    let member = &linked.member;
    let mut heard = member.heard();
    until(
        member,
        "the member holds the record every device is sent",
        |peer| opened_heads(peer, &KEYS, (APP, EAGER)).is_some(),
    )
    .await;
    assert_eq!(
        heads(member, &record),
        None,
        "a record the workspace sends only to a device that opens it was not replayed"
    );

    opened(member, app_document_group(APP, &record))
        .await
        .expect("the host answers the open");
    let taken = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let frame = heard.recv().await.expect("the member hears its link");
            if frame.message == history {
                return frame;
            }
        }
    })
    .await
    .expect("the record's history comes with the open");
    assert!(taken.retained, "the history the open brought is kept");

    // A record the host holds nothing of is answered with nothing, at once: a
    // page that opened it waits no further.
    let started = std::time::Instant::now();
    opened(member, app_document_group(APP, OTHER))
        .await
        .expect("the host answers an open of a record it holds nothing of");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "an open of a record the host holds nothing of is answered, not waited on"
    );
    assert_eq!(
        heads(member, OTHER),
        None,
        "a record the host holds nothing of is taken in not at all"
    );
    linked.stop().await;
}

/// A chain's newest part is replayed and its older parts are not, and a device
/// that opened the chain follows it to the part after.
#[tokio::test(flavor = "multi_thread")]
async fn a_chains_newest_part_is_sent_and_older_parts_are_not() {
    let (first, second) = (document_part(CHAIN, 0), document_part(CHAIN, 1));
    let (host, host_key, member, member_key, directory) =
        devices(0x20, 0x21, 0x22, &[&first, &second]);
    let handler = host.clone();
    let linked = Linked::start(host, host_key, member, member_key, directory, handler).await;
    let (host, member) = (&linked.host, &linked.member);
    until(member, "the member holds the chain's newest part", |peer| {
        opened_heads(peer, &KEYS, (APP, &second)).is_some()
    })
    .await;
    assert_eq!(
        heads(member, &first),
        None,
        "an older part of the chain is not replayed"
    );

    // Opening the chain's head is answered at once rather than waited on.
    let started = std::time::Instant::now();
    opened(member, app_document_group(APP, &chain_head(CHAIN)))
        .await
        .expect("the host answers the open of the chain's head");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "an open of a chain the store keeps nothing of is answered, not waited on"
    );
    assert!(
        heads(member, &second).is_some(),
        "the chain's newest part stays held, however the open was answered"
    );

    // The part after it becomes the chain's newest, and the device following
    // the chain takes it in with no further ask: a part published whole is a
    // checkpoint, and the coverage admission needs for it is the change the
    // host commits to the workspace's own document, which its links carry
    // live as the workspace's changes are eager.
    let third = document_part(CHAIN, 2);
    write_sealed(
        host,
        &KEYS,
        &linked.host_key,
        (APP, &third),
        edit("value", "third part", own_invoker(host)),
    );
    let written = heads(host, &third);
    assert!(written.is_some(), "the host holds the part it wrote");
    until(member, "the chain follows to its newest part", |peer| {
        opened_heads(peer, &KEYS, (APP, &third)) == written
    })
    .await;
    linked.stop().await;
}

/// A link a host scopes to one record is sent that record and no other, in its
/// replay and in what it is sent after.
#[tokio::test(flavor = "multi_thread")]
async fn an_environment_scope_reads_one_record_only() {
    let (host, host_key, member, member_key, directory) =
        devices(0x30, 0x31, 0x32, &[EAGER, OTHER]);
    let reaches = app_document_group(APP, EAGER);
    let hook: Arc<Admitting> = Arc::new(move |_remote, _certificate, _link| {
        Ok(LinkScope {
            reads: Some(vec![reaches]),
            own_frames_only: true,
            key_access: false,
            lists_itself: false,
        })
    });
    let serving = Serving::new().admitting(hook);
    serving
        .serve(host.clone())
        .expect("the host serves its workspace");
    let linked = Linked::start(host, host_key, member, member_key, directory, serving).await;
    let (host, member) = (&linked.host, &linked.member);
    until(member, "the member holds the record it reaches", |peer| {
        opened_heads(peer, &KEYS, (APP, EAGER)).is_some()
    })
    .await;
    assert_eq!(
        heads(member, OTHER),
        None,
        "a record outside the link's scope is not replayed"
    );

    // A write outside the scope, then one inside it: the second arrives, so the
    // first was not sent before it.
    let host_key = &linked.host_key;
    let invoker = own_invoker(host);
    let outside = edit("outside", "1", invoker.clone());
    write_sealed(host, &KEYS, host_key, (APP, OTHER), outside);
    write_sealed(
        host,
        &KEYS,
        host_key,
        (APP, EAGER),
        edit("inside", "1", invoker),
    );
    let inside = heads(host, EAGER);
    until(member, "the member holds the write it reaches", |peer| {
        opened_heads(peer, &KEYS, (APP, EAGER)) == inside
    })
    .await;
    assert_eq!(
        heads(member, OTHER),
        None,
        "a write outside the link's scope is not sent"
    );

    // Opening a record outside the scope is answered with nothing.
    opened(member, app_document_group(APP, OTHER))
        .await
        .expect("the host answers the open");
    assert_eq!(
        heads(member, OTHER),
        None,
        "a record outside the link's scope is not opened"
    );
    linked.stop().await;
}

/// A link from a device that does not keep the workspace carries only the
/// frames such a device sends: a permission grant is refused, and the link
/// still carries the frames it may take.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_without_the_workspace_cannot_send_a_permission_grant() {
    let (host, host_key, member, member_key, directory) = devices(0x40, 0x41, 0x42, &[EAGER]);
    let handler = host.clone();
    let linked = Linked::start(host, host_key, member, member_key, directory, handler).await;
    let (host, member) = (&linked.host, &linked.member);
    until(member, "the member holds the record", |peer| {
        opened_heads(peer, &KEYS, (APP, EAGER)).is_some()
    })
    .await;

    let mut heard = host.heard();
    member
        .send(NeighbourMsg::PermissionGrant {
            peer: 1,
            capability: "screen share".into(),
        })
        .expect("the member sends a frame");
    // A frame such a device may send, after it: when it is heard, the grant
    // that came before it was refused.
    member
        .send(NeighbourMsg::IdentityHello {
            protocol: PEER_PROTOCOL_VERSION,
            cert: Box::new(certificate(member)),
            revocations: Vec::new(),
        })
        .expect("the member sends its identity");
    let seen = tokio::time::timeout(Duration::from_secs(30), async {
        let mut seen = Vec::new();
        loop {
            let heard = heard.recv().await.expect("the host hears its link");
            if matches!(heard.message, NeighbourMsg::IdentityHello { .. }) {
                return seen;
            }
            seen.push(heard.message);
        }
    })
    .await
    .expect("the host hears the frame the link may carry");
    assert!(
        !seen
            .iter()
            .any(|message| matches!(message, NeighbourMsg::PermissionGrant { .. })),
        "a permission grant from a device that does not keep the workspace is refused"
    );
    linked.stop().await;
}

/// A link admitted without the workspace's channel keys is refused a channel's
/// commit, and still heard asking a channel to admit it.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_without_key_access_joins_no_channel_and_is_heard_asking() {
    let (host, host_key, member, member_key, directory) = devices(0x50, 0x51, 0x52, &[EAGER]);
    let hook: Arc<Admitting> = Arc::new(|_remote, _certificate, _link| {
        Ok(LinkScope {
            key_access: false,
            ..Default::default()
        })
    });
    let serving = Serving::new().admitting(hook);
    serving
        .serve(host.clone())
        .expect("the host serves its workspace");
    let linked = Linked::start(host, host_key, member, member_key, directory, serving).await;
    let (host, member) = (&linked.host, &linked.member);
    until(member, "the member holds the record", |peer| {
        opened_heads(peer, &KEYS, (APP, EAGER)).is_some()
    })
    .await;

    let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    let device = collab_types::Ed25519PublicKey::from_bytes(member.device());
    let mut heard = host.heard();
    member
        .send(NeighbourMsg::MlsGovernance {
            workspace: WORKSPACE.to_owned(),
            frame: Box::new(collab_types::MlsGovernanceFrame {
                version: 1,
                workspace,
                previous_frame_digest: [0; 32],
                epoch: 0,
                action: collab_types::MlsGovernanceAction::Genesis,
                signed_by: device,
                signature: collab_types::Signature([0; 64]),
            }),
            delivery: 1,
        })
        .expect("the member sends a channel's governance");
    member
        .send(NeighbourMsg::MlsAccessRequest {
            workspace: WORKSPACE.to_owned(),
            request: Box::new(collab_types::MlsAccessRequestV1 {
                version: 1,
                workspace,
                device,
                key_package: Vec::new(),
                issued_at: 0,
                signature: collab_types::Signature([0; 64]),
            }),
            delivery: 2,
        })
        .expect("the member asks a channel to admit it");
    let seen = tokio::time::timeout(Duration::from_secs(30), async {
        let mut seen = Vec::new();
        loop {
            let heard = heard.recv().await.expect("the host hears its link");
            if matches!(heard.message, NeighbourMsg::MlsAccessRequest { .. }) {
                return seen;
            }
            seen.push(heard.message);
        }
    })
    .await
    .expect("the host hears the ask the link may carry");
    assert!(
        !seen
            .iter()
            .any(|message| matches!(message, NeighbourMsg::MlsGovernance { .. })),
        "a channel's governance from a link without the channel keys is refused"
    );
    linked.stop().await;
}

/// A frame another device wrote, carried by a device that does not keep the
/// workspace, is refused: a link carries its own device's frames alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_from_a_device_without_the_workspace_refuses_another_devices_frame() {
    let root = SigningKey::from_bytes(&[0x61; 32]);
    let (host_key, host_cert) = certify(&root, 0x62, DeviceCapabilities::member());
    let (member_key, member_cert) = certify(&root, 0x63, DeviceCapabilities::member());
    // Another certified device of the same person, whose write the member
    // carries.
    let (other_key, other_cert) = certify(&root, 0x64, DeviceCapabilities::member());
    let members = vec![host_cert.clone(), member_cert, other_cert.clone()];
    let now = SystemTime::now();
    let mut peer = StatePeer::new(
        WORKSPACE,
        host_cert.clone(),
        members.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_exact_admissions().unwrap();
    tilo_peer_state::fixture::found(
        &mut peer,
        (&host_key, &host_cert),
        &members,
        Vec::new(),
        now,
    );
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(directory.path(), Appetite::StoreAll).unwrap());
    let host = DirectSync::over(Replica::new(peer), host_key.clone()).with_store(store);
    write_sealed(
        &host,
        &KEYS,
        &host_key,
        (APP, EAGER),
        edit("value", "eager", invoker(&host_cert)),
    );
    let member_peer = StatePeer::from_join(WORKSPACE, host_cert, members, &[], now).unwrap();
    let member = DirectSync::new(member_peer, member_key.clone()).when_opened();

    // Another device's write, made on the host's own copy of the record, and
    // carried by the member's link as the member's frame.
    let carried = sealed_write(
        &mut host.read(Clone::clone),
        &KEYS,
        &other_key,
        (APP, EAGER),
        edit("another device", "1", invoker(&other_cert)),
    )
    .unwrap();

    let handler = host.clone();
    let linked = Linked::start(host, host_key, member, member_key, directory, handler).await;
    let (host, member) = (&linked.host, &linked.member);
    until(member, "the member holds the record", |peer| {
        opened_heads(peer, &KEYS, (APP, EAGER)).is_some()
    })
    .await;

    let mut heard = host.heard();
    member.send(carried).expect("the member carries the write");
    // A frame of its own after it: when the host hears that, the carried write
    // was refused.
    member
        .send(NeighbourMsg::IdentityHello {
            protocol: PEER_PROTOCOL_VERSION,
            cert: Box::new(certificate(member)),
            revocations: Vec::new(),
        })
        .expect("the member sends its identity");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let heard = heard.recv().await.expect("the host hears its link");
            if matches!(heard.message, NeighbourMsg::IdentityHello { .. }) {
                return;
            }
            assert!(
                !matches!(
                    &heard.message,
                    NeighbourMsg::AppDocument { envelope, .. }
                        if envelope.author.as_bytes() == other_key.verifying_key().as_bytes()
                ),
                "a frame another device wrote is refused over a link that carries its own device's frames alone"
            );
        }
    })
    .await
    .expect("the host hears the frame the link may carry");
    let held = host
        .read(|peer| read_opened(peer, &KEYS, (APP, EAGER)))
        .unwrap();
    let held = AutoCommit::load(&held).unwrap();
    assert!(
        held.get(ROOT, "another device").unwrap().is_none(),
        "the workspace never took the carried write in"
    );
    linked.stop().await;
}

/// A page reaches its workspace over the browser's transport: the same link,
/// its streams carried by a WebTransport session instead of an iroh
/// connection.
#[tokio::test(flavor = "multi_thread")]
async fn a_page_reaches_its_workspace_over_a_webtransport_session() {
    use sha2::Digest as _;
    use tilo_net::webtransport::{Answer, Listener, Options, connect};
    use tilo_server_client::link::Link;

    let (host, host_key, member, member_key, _directory) = devices(0x70, 0x71, 0x72, &[EAGER]);
    let serving = Serving::new();
    serving
        .serve(host.clone())
        .expect("the host serves its workspace");

    // A listener with a certificate of its own, which the page pins.
    let params = rcgen::CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap();
    let signing = rcgen::KeyPair::generate().unwrap();
    let self_signed = params.self_signed(&signing).unwrap();
    let der = self_signed.der().clone();
    let hash: [u8; 32] = sha2::Sha256::digest(der.as_ref()).into();
    let resolver = rustls::sign::SingleCertAndKey::from(
        rustls::sign::CertifiedKey::from_der(
            vec![der],
            rustls::pki_types::PrivateKeyDer::Pkcs8(signing.serialize_der().into()),
            &rustls::crypto::ring::default_provider(),
        )
        .unwrap(),
    );
    let socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let listener = Listener::bind(socket, Arc::new(resolver)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let page = member.device();
    tokio::spawn(listener.serve(
        |_path: &str, _offered: &[String]| Answer::Accept,
        move |_path, session| {
            let serving = serving.clone();
            async move {
                // The page proved itself on the session before it opened the
                // link; the host serves it as that device.
                let link = Link::web_as(session, page);
                let _ = serving.serve_link(&link).await;
            }
        },
    ));

    let host_device = host_key.verifying_key().to_bytes();
    let url = format!("https://127.0.0.1:{port}/peer/v1/link/records");
    let session = tokio::time::timeout(
        Duration::from_secs(30),
        connect(&url, Options::pinned(vec![hash])),
    )
    .await
    .expect("the page opens a session to its host in time")
    .expect("the page opens a session to its host");
    let link = Link::web_as(session, host_device);
    let mut caught_up = member.caught_up();
    let dialed = tokio::time::timeout(
        Duration::from_secs(30),
        member.dial_over(Arc::new(link), host_device),
    )
    .await
    .expect("the host answers the link in time")
    .expect("the host answers the link");
    let running = tokio::spawn({
        let member = member.clone();
        async move { dialed.run(&member).await }
    });
    tokio::time::timeout(Duration::from_secs(30), caught_up.wait_for(|up| *up))
        .await
        .expect("the page catches up")
        .expect("the link is still up");
    until(&member, "the page holds the record", |peer| {
        opened_heads(peer, &KEYS, (APP, EAGER)).is_some()
    })
    .await;

    // Its own write reaches the host over the session.
    let from_the_page = edit("from the page", "1", own_invoker(&member));
    write_sealed(&member, &KEYS, &member_key, (APP, EAGER), from_the_page);
    let written = heads(&member, EAGER);
    until(&host, "the host holds the page's write", |peer| {
        opened_heads(peer, &KEYS, (APP, EAGER)) == written
    })
    .await;
    running.abort();
}
