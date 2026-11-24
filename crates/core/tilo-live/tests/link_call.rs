//! A desktop's capture and playout through the live session its device's daemon holds: the
//! desktop opens one link, the daemon carries it into the workspace's live session as the
//! device, seals what the desktop says and hands back what the session heard, opened.
//!
//! The link is an in-memory byte stream here; everything above it is the code a desktop and a
//! daemon run: the link's frames, its route, the admission preface, `moq-lite`, and the live
//! sessions' sealing. The relay is in-process and admits nobody by itself, as a device's own
//! relay takes the device that serves it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed unwrap in a test is its assertion"
)]

use std::sync::Arc;

use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_live::link_call::Call;
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;

const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const DAEMON: [u8; 32] = [1; 32];
const PEER: [u8; 32] = [2; 32];

fn scope() -> Scope {
    Scope::workspace(WORKSPACE)
}

fn keys() -> ContentKeys {
    let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    KeyGroup::create(workspace, MemberKey::generate("owner").unwrap())
        .unwrap()
        .content_keys()
        .unwrap()
}

/// The relay the sessions meet on: in-process, admitting nobody, since each joins the relay
/// this device serves.
fn local_relay() -> Relay {
    struct Nobody;
    impl RelayAuthority for Nobody {
        fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
            Box::pin(std::future::ready(Err(Denied)))
        }
    }
    Relay::new(Arc::new(Nobody))
}

fn joined(relay: &Relay, device: [u8; 32], keys: &ContentKeys) -> LiveSession {
    LiveSession::local(
        relay,
        &Join {
            scope: scope(),
            device,
            keys: keys.clone().into(),
            roster: Roster::new([DAEMON, PEER]),
            publish: true,
            presence: true,
        },
    )
    .unwrap()
}

/// A presence frame a device says, encoded as its track carries it.
fn said(text: &str) -> Vec<u8> {
    collab_types::presence::Presence::desktop(
        WORKSPACE,
        collab_types::presence::Payload::Status {
            text: text.to_owned(),
            automatic: false,
        },
    )
    .encode()
    .unwrap()
}

/// `probe`'s answer as soon as it has one, for a moment at most.
async fn until<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    for _ in 0..500 {
        if let Some(value) = probe() {
            return Some(value);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    None
}

tilo_test_env::scenario! {
    fn a_desktops_capture_and_hearing_go_through_the_live_session_its_daemon_holds;

    phase what_the_desktop_says_is_heard_by_a_peer_as_the_device_and_the_other_way;
    {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let relay = local_relay();
            let keys = keys();
            let daemon = Arc::new(joined(&relay, DAEMON, &keys));
            let peer = Arc::new(joined(&relay, PEER, &keys));
            // The daemon side of each link: the desktop opens one, and the daemon serves it
            // into the live session it holds.
            let serving = Arc::clone(&daemon);
            let open = move || {
                let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
                let serving = Arc::clone(&serving);
                tokio::spawn(async move {
                    // The link opens with where it goes; the daemon takes it into the live
                    // session it holds for that workspace.
                    let (mut reader, writer) = theirs.into_split();
                    let Ok(_toward) = tilo_live::link::read_route(&mut reader).await else {
                        return;
                    };
                    let session =
                        tilo_live::link::Session::new(reader, writer, tilo_live::link::End::Listener);
                    let _ = tilo_live::link_call::serve(session, serving).await;
                });
                Box::pin(async move {
                    let (reader, writer) = ours.into_split();
                    Ok((
                        Box::new(reader) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                        Box::new(writer) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                    ))
                }) as tilo_live::link::Opening
            };
            let call = Call::open(open, WORKSPACE.to_owned(), DAEMON).await.unwrap();

            // The peer says itself: the daemon opens it and hands it over the link.
            peer.publish_burst(Track::Presence, &[(1, &said("here"))])
                .unwrap();
            // The desktop hears it through the link.
            let heard = until(|| {
                call.try_recv()
                    .filter(|heard| heard.track == Track::Presence && heard.sender == PEER)
            })
            .await
            .expect("the desktop hears the peer through its daemon");
            assert!(
                collab_types::presence::Presence::decode(&heard.body).is_some(),
                "what the daemon handed back opens as the presence it was"
            );

            // The desktop says itself: the daemon seals it as the device, and the peer hears
            // the device say it.
            call.publish(Track::Presence, 2, &said("drawing")).unwrap();
            let heard = until(|| {
                let mut heard = None;
                while let Some(object) = peer.try_recv() {
                    if object.track == Track::Presence && object.sender == DAEMON {
                        heard = Some(object);
                    }
                }
                heard
            })
            .await
            .expect("the peer hears the desktop as the device");
            assert!(collab_types::presence::Presence::decode(&heard.body).is_some());
        });
    }
}
