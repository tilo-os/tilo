//! Whether one origin's broadcast reaches a client over a link at all: the plumbing the
//! daemon's hand-back rides.

use std::sync::Arc;
use std::time::Duration;

use moq_net::Timestamp;

use crate::carrier::{OriginTask, origin_config, start};
use crate::link::{End, Session};
use crate::relay::{Admission, Direction, Peer, Relay, RelayAuthority, SessionAuthorization};

struct Admit;

struct Allow(tokio::sync::watch::Sender<u64>);

impl SessionAuthorization for Allow {
    fn publication(&self) -> Option<&str> {
        None
    }
    fn subscriptions(&self) -> &[String] {
        &[]
    }
    fn subscription_prefix(&self) -> Option<&str> {
        Some("")
    }
    fn current(&self, _direction: Direction, _namespace: &str) -> bool {
        true
    }
    fn changed(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        Some(self.0.subscribe())
    }
    fn call(&self, _direction: Direction, _call: &str) -> bool {
        true
    }
}

impl RelayAuthority for Admit {
    fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
        Box::pin(async move {
            Ok(Arc::new(Allow(tokio::sync::watch::channel(0).0)) as Arc<dyn SessionAuthorization>)
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_origins_broadcast_reaches_the_client_over_the_link() {
    let (daemon, client) = tokio::net::UnixStream::pair().unwrap();
    let relay = Relay::new(Arc::new(Admit));
    let shared = relay.shared();
    let (into, _into_task) = OriginTask::produce(origin_config(1 << 20, Duration::from_secs(5)));
    let broadcast = shared.create_broadcast("held").unwrap();
    let track = broadcast
        .create_track("heard", moq_net::track::Info::default())
        .unwrap();
    let mut group = track.append_group().unwrap();
    group
        .write_frame(Timestamp::ZERO, b"hello".as_slice())
        .unwrap();
    group.finish().unwrap();
    let (reader, writer) = daemon.into_split();
    let transport = Session::new(reader, writer, End::Listener);
    let serving = relay.clone();
    tokio::spawn(async move {
        serving.serve_session(transport, Peer([1; 32])).await;
    });
    let (reader, writer) = client.into_split();
    let transport = Session::new(reader, writer, End::Dialler);
    let _connected = start(transport, &[0], None, Some(into.clone()))
        .await
        .unwrap();
    let consumer = into.consume();
    let got = tokio::time::timeout(Duration::from_secs(5), consumer.routed_broadcast("held"))
        .await
        .expect("the client hears of the broadcast")
        .expect("the broadcast is announced");
    let track = got.track("heard").unwrap();
    let mut subscriber = tokio::time::timeout(
        Duration::from_secs(5),
        track.subscribe(moq_net::track::Subscription::default()),
    )
    .await
    .expect("the client subscribes")
    .unwrap();
    let mut group = tokio::time::timeout(
        Duration::from_secs(5),
        moq_net::kio::wait(|waiter| subscriber.poll_recv_group(waiter)),
    )
    .await
    .expect("a group reaches the client")
    .unwrap()
    .unwrap();
    let frame = group.read_frame().await.unwrap().unwrap();
    assert_eq!(frame.payload.as_ref(), b"hello");
}
