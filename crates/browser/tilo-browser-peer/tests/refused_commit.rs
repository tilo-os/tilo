//! A change the room refuses is not kept as if it were shared: the host
//! answers the author's commit with a refusal, the record that made it
//! withdraws it, and the app that made it is told which action the room
//! refused and why, in words for a person, exactly as for an action refused
//! on the device.

#![allow(
    clippy::significant_drop_tightening,
    reason = "a test holds its fixtures for the length of a step"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

mod support;

use collab_types::{DocumentMutation, document_channel_id};
use tilo_app::prelude::*;
use tilo_browser_peer::{ContentCipher, PeerEvent, PeerHandle};
use tilo_relay::member_limits::MemberLimitsConfig;
use tilo_relay_protocol::refusal::{PLAIN_TOO_FAST, plain_reason};
use tilo_user_identity::{DeviceCapabilities, UserIdentity};

use support::{ROOM, door, enrol, enter, route, start_host_limiting, wait_for, workspace};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub notes: Collection<Note>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Note {
    pub text: String,
}

#[resource(kind = "test.board", label = "Board")]
impl Board {
    /// Pin a note.
    #[action]
    pub fn pin(&mut self, cx: &mut ActionCx, text: String) {
        self.notes.push(cx.id(), Note { text });
    }
}

struct BoardApp;

impl ResourceApp for BoardApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-board";
    const TITLE: &'static str = "Board";

    fn view(cx: &mut ViewCx<Board>) -> Element {
        Box::new(cx.text(|board| board.notes.len().to_string()))
    }
}

/// The channel's content keys, as its steward holds them.
struct Keys(channel_crypto::ChannelContentKeys);

impl ContentCipher for Keys {
    fn seal(
        &mut self,
        artifact: collab_types::ArtifactId,
        retention: collab_types::ContentRetention,
        cleartext: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        self.0
            .seal_content(artifact, retention, cleartext)
            .map_err(|error| error.to_string())
    }

    fn open(
        &mut self,
        artifact: collab_types::ArtifactId,
        metadata: &collab_types::EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.0
            .open_content(artifact, metadata, payload)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }
}

/// The host's answer to the one commit `peer` has outstanding.
enum Answer {
    Committed,
    Refused {
        changes: Vec<[u8; 32]>,
        reason: String,
    },
}

async fn answer(peer: &PeerHandle) -> Answer {
    wait_for("the host's answer to the commit", || {
        let answer = peer.drain().into_iter().find_map(|event| match event {
            PeerEvent::Committed { .. } => Some(Answer::Committed),
            PeerEvent::CommitRefused {
                changes, reason, ..
            } => Some(Answer::Refused { changes, reason }),
            _ => None,
        });
        async move { answer }
    })
    .await
}

/// `app` withdrew note `pinned`, the one whose `changes` the room refused
/// for `reason`, and was told the room refused its verb.
fn withdrawn_as_refused(
    app: &mut TestApp<BoardApp>,
    pinned: usize,
    changes: &[[u8; 32]],
    reason: &str,
) {
    assert_eq!(reason, tilo_relay_protocol::refusal::TOO_MANY_CHANGES);
    assert!(!changes.is_empty(), "the refusal names the change it was");
    assert_eq!(
        app.state().notes.len(),
        pinned + 1,
        "applied before it was sent"
    );
    let withdrawn_text = format!("note {pinned}");

    // The page withdraws what the room refused, and the app hears of it as
    // a refusal of the verb it invoked, for the note it would have pinned.
    assert!(app.refused_by_room(changes, plain_reason(reason)).unwrap());
    assert_eq!(app.state().notes.len(), pinned, "the refused note is gone");
    assert!(
        app.state()
            .notes
            .iter()
            .all(|(_, note)| note.text != withdrawn_text),
        "only the refused note was withdrawn"
    );
    let told = app.refused().expect("the app is told the room refused it");
    assert_eq!(told.action, "pin");
    assert_eq!(told.reason, PLAIN_TOO_FAST);
    assert!(told.by_room);
    // Pinning acts on the board, not on an item of it.
    assert_eq!(told.item, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_change_over_the_change_budget_is_withdrawn_and_its_app_told_why() {
    // Alice's host governs the room and holds guests to a change budget;
    // Bob was invited through the room and came in on his browser.
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let (bob, _) = UserIdentity::generate("Bob", 1);
    let browser = enrol(&bob, "browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host_limiting(
        &alice,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        None,
        &[],
        Some(MemberLimitsConfig {
            changes_per_window: 1,
            window_secs: 3600,
            burst: 4,
            ..MemberLimitsConfig::default()
        }),
    )
    .await;
    let room = host
        .relay
        .rooms()
        .unwrap()
        .create_room("Board", Some(ROOM))
        .await
        .unwrap();
    assert_eq!(room.workspace_id, workspace());
    let route = route();
    let steward_dir = tempfile::tempdir().unwrap();
    let steward = channel_crypto::ChannelSteward::open_or_create(
        steward_dir.path(),
        document_channel_id(&route.app_id, &route.cluster_id, &route.record),
        bob.clone(),
        &channel_crypto::VaultKey::new([7; 32]),
    )
    .unwrap();
    let peer = enter(
        &host,
        door(
            &host,
            &alice,
            &host_device,
            &room.room,
            bob.verifying_key().to_bytes(),
        )
        .await,
        &browser.cert,
        &browser.key,
        &host_device.cert,
        workspace(),
    )
    .await;
    peer.install_cipher(
        route.clone(),
        Box::new(Keys(steward.content_keys().unwrap())),
    )
    .unwrap();

    // Bob pins notes on his board, each sent as the page sends a record's
    // changes, until the host stops taking them.
    let mut app = TestApp::<BoardApp>::new().unwrap();
    let mut refused = None;
    for pinned in 0..16 {
        let heads = app.read_document(|doc| doc.clone().get_heads());
        app.invoke(Board::PIN.with(format!("note {pinned}")))
            .unwrap();
        let delta = app.read_document(|doc| doc.clone().save_after(&heads));
        peer.publish(route.clone(), &DocumentMutation::Incremental(delta))
            .unwrap();
        if let Answer::Refused { changes, reason } = answer(&peer).await {
            refused = Some((pinned, changes, reason));
            break;
        }
    }
    let (pinned, changes, reason) = refused.expect("the budget refuses a change in time");
    withdrawn_as_refused(&mut app, pinned, &changes, &reason);
    let status = peer.status();
    let route_status = status.routes.get(&route).unwrap();
    assert_eq!(route_status.refused_operations, 1);
    assert_eq!(
        route_status.pending_operations, 0,
        "nothing is left outstanding"
    );
    peer.close();
    host.relay.shutdown().await;
}
