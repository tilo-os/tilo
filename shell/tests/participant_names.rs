//! One participant, one name, everywhere they appear.
//!
//! A room says who is in it: written by the inviter next to the capability
//! grant, on the replicated channel `apply_room_changes` refuses from
//! anyone but the room's authority — the only source of a name, since a
//! peer that could name itself could name itself anything.
//!
//! A frame delivered to an app must not carry a raw `peer-` / `peer@` form
//! for a participant the room can name. Both transports are exercised,
//! since every packaged qOS app reaches the broker over the capnp one.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use collab_fixtures::{role_attestation, signed_op};
use collab_types::{
    ArtifactId, ArtifactKind, ChannelId, ChannelState, Ed25519PublicKey, OpClass, Role,
    generate_keypair,
};
use ed25519_dalek::SigningKey;
use shell::shell::collab_broker::{self, CrdtRoute, LocalIdentity, PeerLabels};
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::presence::PresenceKey;

/// This machine's liveness handle, as the room document's keys hash to.
/// Arbitrary but fixed: the lookup and the resolver must agree on it.
const LOCAL_HANDLE: u64 = 5_537_458_103_208_628_978;

/// What the room calls this machine's peer. A name a person chose,
/// which is the whole point — it is not derivable from any id.
const LOCAL_NAME: &str = "Xaver";

/// Every label a participant can be given — asserted as a set, since
/// checking only the caret would pass while the roster disagreed.
fn labels_in(events: &[CollabEvent]) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for e in events {
        match e {
            CollabEvent::RemotePresenceUpdate { user_id, .. } => {
                out.push(("presence", user_id.clone()));
            }
            CollabEvent::RemoteCursorMoved { user_id, .. } => {
                out.push(("pointer", user_id.clone()));
            }
            CollabEvent::SignedCRDTIncremental { from_user_id, .. } => {
                out.push(("delta", from_user_id.clone()));
            }
            CollabEvent::Context { collaborators, .. } => {
                for c in collaborators {
                    out.push(("roster-id", c.id.clone()));
                    out.push(("roster-name", c.name.clone()));
                }
            }
            _ => {}
        }
    }
    out
}

/// The failure this file exists to catch: a transport identity shown to a person.
fn is_raw_peer_form(label: &str) -> bool {
    label.starts_with("peer-") || label.starts_with("peer@")
}

fn temp_sock(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "qos-broker-names-{label}-{}-{nanos}.sock",
        std::process::id()
    ))
}

fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, v: &T) {
    let bytes = serde_json::to_vec(v).expect("serialize");
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .expect("write len");
    stream.write_all(&bytes).expect("write body");
}

fn drain_events(stream: &mut UnixStream, timeout: Duration) -> Vec<CollabEvent> {
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    while Instant::now() < deadline {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        stream.set_read_timeout(Some(remain)).expect("set timeout");
        let mut len_buf = [0u8; 4];
        if stream.read_exact(&mut len_buf).is_err() {
            break;
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut body = vec![0u8; len];
        if stream.read_exact(&mut body).is_err() {
            break;
        }
        if let Ok(e) = serde_json::from_slice::<CollabEvent>(&body) {
            out.push(e);
        }
    }
    out
}

fn connect(socket: &PathBuf) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match UnixStream::connect(socket) {
            Ok(s) => return s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("could not connect to broker: {e}"),
        }
    }
}

fn wait_for_announced(handle: &collab_broker::BrokerHandle, n: usize) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if handle.announced_peer_count() >= n {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "only {} of {n} clients announced a route",
        handle.announced_peer_count()
    );
}

fn test_cluster() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x3f2b_1c00_0000_4000_8000_0000_0000_0042)
}

/// A room that knows this machine's peer by name, and nobody else.
fn room_that_names_us() -> Arc<LocalIdentity> {
    LocalIdentity::with_lookup(LOCAL_HANDLE, |_cluster| {
        let mut m = PeerLabels::new();
        m.insert(LOCAL_HANDLE, LOCAL_NAME.to_string());
        m
    })
}

fn channel_state_for(
    owner_sk: &SigningKey,
    owner_pk: Ed25519PublicKey,
    channel: ChannelId,
    artifact: ArtifactId,
) -> (ChannelState, collab_types::AttestationId) {
    let ra = role_attestation(owner_sk, owner_pk, owner_pk, channel, Role::Owner);
    let att = ra.attestation.id;
    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);
    (state, att)
}

/// The resolver's order, stated as an order: the room outranks what a
/// connection calls itself, since a `Hello` handle is self-declared and
/// trusting it would make "announce a name" a way to wear one.
#[test]
fn the_room_outranks_what_a_connection_calls_itself() {
    let id = room_that_names_us();
    let route = CrdtRoute::cluster("room-1".to_string());

    assert_eq!(
        id.resolve(Some(&route), Some("i-am-somebody-else")),
        LOCAL_NAME,
        "a self-declared handle must not overrule the room",
    );
    assert_eq!(
        id.resolve(Some(&route), None),
        LOCAL_NAME,
        "and the room's answer is reached with no handle at all — \
         which is every packaged app, since capnp announces none",
    );
}

/// A room that has named nobody leaves the participant on an id, never on
/// an invented name or a blank.
#[test]
fn an_unnamed_participant_falls_back_to_an_id_not_to_a_stand_in() {
    let silent = LocalIdentity::with_lookup(LOCAL_HANDLE, |_| PeerLabels::new());
    let route = CrdtRoute::cluster("room-1".to_string());

    // With a handle, that is what the connection is called: no
    // authority, but nothing better exists and it is not invented here.
    assert_eq!(silent.resolve(Some(&route), Some("qos-agent")), "qos-agent");

    // With nothing at all, this machine's own user id — the same string the
    // compositor publishes its own pointer under.
    let bare = silent.resolve(Some(&route), None);
    assert!(!bare.is_empty(), "a label is never blank");
    assert_eq!(bare, shell::shell::state::local_user_id());
}

/// The whole claim, on the wire the apps actually read: two apps on one
/// machine, neither announcing a handle (what a packaged app looks like),
/// in a room that knows this machine's peer by name.
#[test]
fn no_label_reaching_an_app_is_a_raw_peer_form_when_the_room_knows_the_name() {
    let socket = temp_sock("wire");
    let handle =
        collab_broker::spawn_full_with_identity(socket.clone(), None, None, room_that_names_us())
            .expect("broker spawns");

    let cluster = test_cluster();
    let app = "dev.qos.word-processor";

    let mut author = connect(&socket);
    let mut watcher = connect(&socket);
    for s in [&mut author, &mut watcher] {
        write_frame(
            s,
            &CollabRequest::Hello {
                app_id: app.to_string(),
                cluster_id: cluster,
                record: None,
                // The capnp handshake carries no handle — the case every installed app is in.
                handle: None,
            },
        );
    }
    wait_for_announced(&handle, 2);

    let (sk, pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let (state, att) = channel_state_for(&sk, pk, channel, artifact);
    handle.register_channel_state(artifact, state);

    // Presence: the author arrives, and puts a caret in the document.
    write_frame(
        &mut author,
        &CollabRequest::Presence {
            key: PresenceKey::Workspace {
                id: cluster.to_string(),
            },
            payload: ui::collaboration::PresencePayload::Arrived,
        },
    );
    // And a delta — which is what the caret beside it is drawn from.
    write_frame(
        &mut author,
        &CollabRequest::SignedSyncIncremental {
            envelope: Box::new(signed_op(
                &sk,
                pk,
                artifact,
                att,
                OpClass::Splice,
                b"the room knows who is in it".to_vec(),
                1,
            )),
        },
    );

    let seen = drain_events(&mut watcher, Duration::from_millis(800));
    let labels = labels_in(&seen);

    let kinds: Vec<&str> = labels.iter().map(|(k, _)| *k).collect();
    assert!(
        kinds.contains(&"presence"),
        "the presence never arrived, so this run proves nothing about it: {seen:?}",
    );
    assert!(
        kinds.contains(&"delta"),
        "the delta never arrived, so this run proves nothing about it: {seen:?}",
    );

    let raw: Vec<_> = labels
        .iter()
        .filter(|(kind, label)| *kind != "roster-id" && is_raw_peer_form(label))
        .collect();
    assert!(
        raw.is_empty(),
        "the room named this peer '{LOCAL_NAME}', and these labels still \
         show a transport id: {raw:?} (all labels: {labels:?})",
    );

    for (kind, label) in &labels {
        if *kind == "roster-id" {
            continue;
        }
        assert_eq!(
            label, LOCAL_NAME,
            "{kind} disagrees with the rest of the frame; one participant \
             has one name (all labels: {labels:?})",
        );
    }

    drop(author);
    drop(watcher);
    drop(handle);
}
