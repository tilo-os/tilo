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

#[path = "support.rs"]
mod support;
use std::sync::{Arc, Mutex};
use support::{WORKSPACE, addr, endpoint, keys, scope};

use key_group::ContentKeys;
use tilo_live::grant;
use tilo_live::link::Toward;
use tilo_live::link_call::Call;
use tilo_live::live::{Namespace, Track};
use tilo_live::live_session::{Join, LiveError, LiveSession};
use tilo_live::relay::{
    Admission, AuthorizationStatus, Denied, Direction, Peer, Relay, RelayAuthority,
    SessionAuthorization,
};
use tilo_live::roster::Roster;

const DAEMON: [u8; 32] = [1; 32];
const PEER: [u8; 32] = [2; 32];

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

            // The desktop's presence is the daemon's to say: the link carries none of it.
            assert!(matches!(
                call.publish(Track::Presence, 2, &said("drawing")),
                Err(tilo_live::live_session::LiveError::Invalid)
            ));

            // What the desktop is doing it says itself: the daemon seals it as the device, and
            // the peer hears the device say it.
            call.publish(Track::Activity, 2, &said("drawing")).unwrap();
            let heard = until(|| {
                let mut heard = None;
                while let Some(object) = peer.try_recv() {
                    if object.track == Track::Activity && object.sender == DAEMON {
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

/// The person's devices as their own scopes admit them: each certificate, by its key. A device of
/// another person is in no such list.
type Persons = Arc<std::collections::BTreeMap<[u8; 32], Vec<u8>>>;

/// Where each device answers.
type Addrs = Arc<Mutex<std::collections::BTreeMap<[u8; 32], iroh::EndpointAddr>>>;

/// The judgement a daemon's relay passes on its own scope: a grant for this device's scope, from a
/// device whose certificate is the one the person's record keeps for its key. It publishes its
/// own input there and reads nothing; only the device itself reads it, joined in-process.
struct OwnScope {
    own: [u8; 32],
    person: Persons,
}

impl RelayAuthority for OwnScope {
    fn admit<'a>(&'a self, peer: Peer, proof: &'a [u8]) -> Admission<'a> {
        let Peer(device) = peer;
        let admitted = grant::Native::parse(proof).filter(|grant| {
            grant.scope == own_scope(self.own).id()
                && self.person.get(&device) == Some(&grant.certificate)
        });
        let own = self.own;
        Box::pin(std::future::ready(match admitted {
            Some(_) => Ok(
                Arc::new(Publishes(Namespace::new(&own_scope(own), device).path()))
                    as Arc<dyn SessionAuthorization>,
            ),
            None => Err(Denied),
        }))
    }
}

/// A session that publishes its own input in a device's scope, and reads nothing there.
struct Publishes(String);

impl SessionAuthorization for Publishes {
    fn publication(&self) -> Option<&str> {
        Some(&self.0)
    }
    fn subscriptions(&self) -> &[String] {
        &[]
    }
    fn current(&self, direction: Direction, namespace: &str) -> bool {
        direction == Direction::Publish && namespace == self.0
    }
    fn status(&self, direction: Direction, namespace: &str) -> AuthorizationStatus {
        if self.current(direction, namespace) {
            AuthorizationStatus::Current
        } else {
            AuthorizationStatus::Denied
        }
    }
}

fn own_scope(device: [u8; 32]) -> tilo_live::scope::Scope {
    tilo_live::scope::Scope::device(WORKSPACE, device)
}

fn own_grant(device: [u8; 32], certificate: &[u8], epoch: u64) -> Vec<u8> {
    grant::Native::new(
        tilo_live::live::workspace_bytes(WORKSPACE).unwrap(),
        &own_scope(device).id(),
        epoch,
        certificate.to_vec(),
        Vec::new(),
    )
    .unwrap()
    .encode()
}

/// A device's daemon as it carries its desktop into the scopes of its person's devices: it joins
/// the scope as the device, sealed under the person's keys, and serves the desktop's link into it.
struct OwnDaemon {
    relay: Relay,
    endpoint: iroh::Endpoint,
    certificate: Vec<u8>,
    /// The person's own workspace's keys, which every device of theirs holds.
    keys: ContentKeys,
    person: Persons,
    addrs: Addrs,
    _router: iroh::protocol::Router,
}

impl OwnDaemon {
    fn device(&self) -> [u8; 32] {
        *self.endpoint.id().as_bytes()
    }

    async fn join(&self, device: [u8; 32]) -> Result<LiveSession, LiveError> {
        let own = self.device();
        let here = device == own;
        let join = Join {
            scope: own_scope(device),
            device: own,
            keys: self.keys.clone().into(),
            roster: if here {
                Roster::new(self.person.keys().copied().filter(|key| *key != own))
            } else {
                Roster::new([])
            },
            publish: !here,
            presence: false,
        };
        if here {
            return LiveSession::local(&self.relay, &join);
        }
        let addr = self.addrs.lock().unwrap().get(&device).cloned().unwrap();
        let dial = Arc::new(tilo_live::native::Iroh::new(self.endpoint.clone(), addr));
        LiveSession::join_within(
            dial,
            &own_grant(
                device,
                &self.certificate,
                tilo_live::frame::LiveKeys::from(self.keys.clone()).epoch(),
            ),
            join,
            std::time::Duration::from_secs(4),
        )
        .await
    }

    /// What opens one link of the desktop on this daemon's socket.
    fn links(self: &Arc<Self>) -> impl Fn() -> tilo_live::link::Opening + Send + Sync + 'static {
        let daemon = Arc::clone(self);
        move || {
            let daemon = Arc::clone(&daemon);
            let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
            tokio::spawn(async move {
                let (reader, writer) = theirs.into_split();
                let own = daemon.device();
                let _ = tilo_live::link::carry(
                    reader,
                    writer,
                    &daemon.relay,
                    &daemon.endpoint,
                    own,
                    Some(&*daemon),
                )
                .await;
            });
            Box::pin(async move {
                let (reader, writer) = ours.into_split();
                Ok((
                    Box::new(reader) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                    Box::new(writer) as Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
                ))
            }) as tilo_live::link::Opening
        }
    }
}

impl tilo_live::link::Calls for OwnDaemon {
    fn carry<'a>(
        &'a self,
        _workspace: String,
        session: tilo_live::link::Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move { session.close(0, "no workspace here") })
    }

    fn carry_device<'a>(
        &'a self,
        device: [u8; 32],
        session: tilo_live::link::Session,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            match self.join(device).await {
                Ok(joined) => {
                    let _ = tilo_live::link_call::serve(session, Arc::new(joined)).await;
                }
                Err(_) => session.close(0, "the scope was not joined"),
            }
        })
    }
}

/// A daemon on `endpoint` whose own scope admits `person`'s devices.
fn own_daemon(
    endpoint: iroh::Endpoint,
    person: &Persons,
    addrs: &Addrs,
    (certificate, keys): (&[u8], &ContentKeys),
) -> Arc<OwnDaemon> {
    let own = *endpoint.id().as_bytes();
    let relay = Relay::new(Arc::new(OwnScope {
        own,
        person: Arc::clone(person),
    }));
    let served = relay.clone();
    let router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(
            tilo_live::native::ALPN,
            tilo_live::native::handler(move |session: tilo_live::native::Session| {
                let relay = served.clone();
                async move { tilo_live::native::serve_on(&relay, session).await }
            }),
        )
        .spawn();
    addrs.lock().unwrap().insert(own, addr(&endpoint));
    Arc::new(OwnDaemon {
        relay,
        endpoint,
        certificate: certificate.to_vec(),
        keys: keys.clone(),
        person: Arc::clone(person),
        addrs: Arc::clone(addrs),
        _router: router,
    })
}

tilo_test_env::scenario! {
    fn a_persons_devices_reach_each_other_through_their_daemons_and_nobody_else_does;

    phase the_input_one_desktop_sends_another_of_the_persons_devices_is_heard_there_from_it;
    // Two devices of one person: the driving desktop opens a link toward the other device's
    // scope, its daemon joins it at the other device's relay and seals what the desktop sends; the
    // driven desktop reads its own scope through its daemon and hears it, opened, as from the
    // driving device. A device of another person is turned away at the relay.
    {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (laptop_end, desk_end, stranger_end) =
                (endpoint().await, endpoint().await, endpoint().await);
            let laptop_key = *laptop_end.id().as_bytes();
            let desk_key = *desk_end.id().as_bytes();
            let stranger_key = *stranger_end.id().as_bytes();
            let person: Persons = Arc::new(
                [
                    (laptop_key, b"laptop certificate".to_vec()),
                    (desk_key, b"desk certificate".to_vec()),
                ]
                .into(),
            );
            let addrs = Addrs::default();
            let (ours, someone_elses) = (keys(), keys());
            let laptop = own_daemon(laptop_end, &person, &addrs, (b"laptop certificate", &ours));
            let desk = own_daemon(desk_end, &person, &addrs, (b"desk certificate", &ours));
            let theirs: Persons = Arc::new([(stranger_key, b"stranger certificate".to_vec())].into());
            let stranger = own_daemon(
                stranger_end,
                &theirs,
                &addrs,
                (b"stranger certificate", &someone_elses),
            );

            let driven =
                Call::open_toward(desk.links(), Toward::Device { device: desk_key }, desk_key)
                    .await
                    .expect("the desk's desktop reads its own scope through its daemon");
            let driving =
                Call::open_toward(laptop.links(), Toward::Device { device: desk_key }, laptop_key)
                    .await
                    .expect("the laptop's daemon joins the desk's scope at the desk's relay");
            let heard = until(|| {
                driving.publish(Track::Input, 0, b"key a").unwrap();
                driven.try_recv().filter(|heard| heard.track == Track::Input)
            })
            .await
            .expect("the desk hears the laptop's input");
            assert_eq!((heard.sender, heard.body.as_slice()), (laptop_key, &b"key a"[..]));

            // A device of another person presents its own certificate and is turned away: its
            // daemon cannot join the desk's scope, and its desktop's link closes unopened.
            assert!(matches!(stranger.join(desk_key).await, Err(LiveError::Denied)));
            assert!(
                Call::open_toward(stranger.links(), Toward::Device { device: desk_key }, stranger_key)
                    .await
                    .is_err(),
                "another person's device opens no link into the desk's scope"
            );
            assert!(
                std::iter::from_fn(|| driven.try_recv()).all(|heard| heard.sender == laptop_key),
                "nothing of the other person's device reached the desk"
            );
        });
    }
}
