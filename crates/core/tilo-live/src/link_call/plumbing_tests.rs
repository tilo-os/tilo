//! Whether an origin's broadcast reaches a client over a link at all: the plumbing the
//! daemon's hand-back rides.

use std::time::Duration;

use moq_net::Timestamp;

use crate::carrier::{Accepted, OriginTask, accept, origin_config, start};
use crate::link::{End, Session};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_origins_broadcast_reaches_the_client_over_the_link() {
    let (daemon, client) = tokio::net::UnixStream::pair().unwrap();
    let (out, _out_task) = OriginTask::produce(origin_config(1 << 20, Duration::from_secs(5)));
    let (into, _into_task) = OriginTask::produce(origin_config(1 << 20, Duration::from_secs(5)));
    let broadcast = out.create_broadcast("held").unwrap();
    let track = broadcast
        .create_track("heard", moq_net::track::Info::default())
        .unwrap();
    let mut group = track.append_group().unwrap();
    group
        .write_frame(Timestamp::ZERO, b"hello".as_slice())
        .unwrap();
    group.finish().unwrap();
    broadcast
        .announce(moq_net::origin::Route::default())
        .unwrap();
    let server = moq_net::Server::new().with_publisher(out.consume());
    let (reader, writer) = daemon.into_split();
    let transport = Session::new(reader, writer, End::Listener);
    tokio::spawn({
        let server = server;
        async move {
            let Ok((accepted, ())) =
                accept(transport, move |_| async move { Some((server, ())) }, || {}).await
            else {
                return;
            };
            let Accepted {
                session,
                driver,
                pace,
                ..
            } = accepted;
            let mut driver = std::pin::pin!(pace.drive(driver));
            tokio::select! {
                _ = session.closed() => {}
                _ = &mut driver => {}
            }
        }
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

/// The shape the daemon and its desktop use: each end publishes one broadcast and reads the
/// other's, over one link.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn both_ends_publish_and_read_over_one_link() {
    let (daemon, client) = tokio::net::UnixStream::pair().unwrap();
    let (server_in, _a) = OriginTask::produce(origin_config(1 << 20, Duration::from_secs(5)));
    let (server_out, _b) = OriginTask::produce(origin_config(1 << 20, Duration::from_secs(5)));
    let (client_out, _c) = OriginTask::produce(origin_config(1 << 20, Duration::from_secs(5)));
    let (client_in, _d) = OriginTask::produce(origin_config(1 << 20, Duration::from_secs(5)));
    let held = server_out.create_broadcast("held").unwrap();
    let heard = held
        .create_track("heard", moq_net::track::Info::default())
        .unwrap();
    held.announce(moq_net::origin::Route::default()).unwrap();
    let capture = client_out.create_broadcast("capture").unwrap();
    let audio = capture
        .create_track("audio", moq_net::track::Info::default())
        .unwrap();
    capture.announce(moq_net::origin::Route::default()).unwrap();
    let server = moq_net::Server::new()
        .with_subscriber(server_in.clone())
        .with_publisher(server_out.clone());
    let (mut reader, writer) = daemon.into_split();
    tokio::spawn(async move {
        let Ok(_toward) = crate::link::read_route(&mut reader).await else {
            return;
        };
        let transport = Session::new(reader, writer, End::Listener);
        let Ok((accepted, ())) =
            accept(transport, move |_| async move { Some((server, ())) }, || {}).await
        else {
            return;
        };
        let Accepted {
            session,
            driver,
            pace,
            ..
        } = accepted;
        let mut driver = std::pin::pin!(pace.drive(driver));
        tokio::select! {
            _ = session.closed() => {}
            _ = &mut driver => {}
        }
    });
    let (reader, mut writer) = client.into_split();
    crate::link::write_route(&mut writer, &crate::link::Toward::Here)
        .await
        .unwrap();
    let transport = Session::new(reader, writer, End::Dialler);
    let _connected = start(
        transport,
        &[0],
        Some(client_out.consume()),
        Some(client_in.clone()),
    )
    .await
    .unwrap();
    // The client reads the daemon's broadcast.
    let broadcast = tokio::time::timeout(
        Duration::from_secs(5),
        client_in.consume().routed_broadcast("held"),
    )
    .await
    .expect("the client hears of held")
    .expect("held is announced");
    let mut subscriber = tokio::time::timeout(
        Duration::from_secs(5),
        broadcast
            .track("heard")
            .unwrap()
            .subscribe(moq_net::track::Subscription::default()),
    )
    .await
    .expect("the client subscribes")
    .unwrap();
    let mut group = heard.append_group().unwrap();
    group
        .write_frame(Timestamp::ZERO, b"hello".as_slice())
        .unwrap();
    group.finish().unwrap();
    let mut group = tokio::time::timeout(
        Duration::from_secs(5),
        moq_net::kio::wait(|waiter| subscriber.poll_recv_group(waiter)),
    )
    .await
    .expect("a group reaches the client")
    .unwrap()
    .unwrap();
    assert_eq!(
        group.read_frame().await.unwrap().unwrap().payload.as_ref(),
        b"hello"
    );
    // The daemon reads the client's broadcast.
    let broadcast = tokio::time::timeout(
        Duration::from_secs(5),
        server_in.consume().routed_broadcast("capture"),
    )
    .await
    .expect("the daemon hears of capture")
    .expect("capture is announced");
    assert!(broadcast.track("audio").is_ok());
}
