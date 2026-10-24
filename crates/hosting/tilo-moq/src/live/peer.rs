//! A native `LIVE_V1` participant: the client side of [`LiveRelay`].
//!
//! It publishes its own namespace broadcast (with the `presence` track) and
//! one record broadcast per record it writes, and receives the room's egress
//! broadcasts in a local origin.
use super::{PRESENCE_TRACK, RECORD_TRACK, record_path};
use crate::carrier;
use std::collections::HashMap;
use std::time::Duration;

/// Bytes either local origin may cache.
const ORIGIN_BYTES: u64 = 1024 * 1024;
/// Local retention of a group not read yet: a reader that fell behind still
/// gets a track's last change, as the relay keeps it for such a reader.
const RETAIN: Duration = super::EGRESS_KEEP;

/// One connected live participant. Dropping it closes the session.
pub struct Peer {
    pub connected: carrier::Connected,
    /// Broadcasts the relay announces to this participant (the room's egress).
    pub incoming: moq_net::origin::Producer,
    outgoing: moq_net::origin::Producer,
    namespace: Option<String>,
    /// Kept so the namespace broadcast stays announced while the peer lives.
    _broadcast: Option<moq_net::broadcast::Producer>,
    presence: Option<moq_net::track::Producer>,
    records: HashMap<String, (moq_net::broadcast::Producer, moq_net::track::Producer)>,
    _origins: [carrier::OriginTask; 2],
}

impl Peer {
    /// Dial the live route, present `grant` and start the session. With
    /// `namespace`, this participant publishes `presence` there.
    ///
    /// # Errors
    ///
    /// Returns [`carrier::AdmissionError`] when the relay is unreachable or
    /// refuses the grant.
    pub async fn connect(
        url: &url::Url,
        trust: &carrier::Trust,
        grant: &[u8],
        namespace: Option<&str>,
    ) -> Result<Self, carrier::AdmissionError> {
        let (pending, publish, subscribe) = Self::prepare(namespace)?;
        let connected = carrier::connect(
            url,
            trust,
            grant,
            publish,
            subscribe,
            Duration::from_secs(10),
        )
        .await?;
        Ok(pending.ready(connected))
    }

    /// Like [`connect`](Self::connect) over an already open WebTransport
    /// session (answered with [`carrier::PROTOCOL`]).
    ///
    /// # Errors
    ///
    /// Returns [`carrier::AdmissionError`] when the relay refuses the grant or
    /// the session fails.
    pub async fn start(
        transport: web_transport_quinn::Session,
        grant: &[u8],
        namespace: Option<&str>,
    ) -> Result<Self, carrier::AdmissionError> {
        let (pending, publish, subscribe) = Self::prepare(namespace)?;
        let connected = carrier::start(transport, grant, publish, subscribe).await?;
        Ok(pending.ready(connected))
    }

    fn prepare(
        namespace: Option<&str>,
    ) -> Result<
        (
            Pending,
            Option<moq_net::origin::Consumer>,
            Option<moq_net::origin::Producer>,
        ),
        carrier::AdmissionError,
    > {
        let (outgoing, outgoing_task) =
            carrier::OriginTask::produce(carrier::origin_config(ORIGIN_BYTES, RETAIN));
        let (incoming, incoming_task) =
            carrier::OriginTask::produce(carrier::origin_config(ORIGIN_BYTES, RETAIN));
        let (broadcast, presence) = match namespace {
            Some(namespace) => {
                let broadcast = outgoing
                    .create_broadcast(namespace)
                    .map_err(|_| carrier::AdmissionError::Invalid)?;
                let presence = broadcast
                    .create_track(
                        PRESENCE_TRACK,
                        moq_net::track::Info::default().with_max_age(Duration::from_millis(100)),
                    )
                    .map_err(|_| carrier::AdmissionError::Invalid)?;
                broadcast
                    .announce(moq_net::origin::Route::default())
                    .map_err(|_| carrier::AdmissionError::Invalid)?;
                (Some(broadcast), Some(presence))
            }
            None => (None, None),
        };
        let publish = namespace.map(|_| outgoing.consume());
        let subscribe = Some(incoming.clone());
        Ok((
            Pending {
                incoming,
                outgoing,
                namespace: namespace.map(str::to_owned),
                broadcast,
                presence,
                origins: [outgoing_task, incoming_task],
            },
            publish,
            subscribe,
        ))
    }

    pub fn session(&self) -> &moq_net::Session {
        &self.connected.session
    }

    pub fn presence(&mut self) -> Option<&mut moq_net::track::Producer> {
        self.presence.as_mut()
    }

    /// Send one presence frame as a datagram. `false` when not publishing.
    pub fn publish_presence(&mut self, frame: &[u8]) -> bool {
        self.presence.as_mut().is_some_and(|track| {
            track
                .append_datagram(
                    moq_net::Timestamp::ZERO,
                    bytes::Bytes::copy_from_slice(frame),
                )
                .is_ok()
        })
    }

    /// The ordered track for `record` (64 lowercase hex), opening its record
    /// broadcast on first use. At most [`super::MAX_OPEN_RECORDS`] are open; the
    /// relay refuses more.
    pub fn record(&mut self, record: &str) -> Option<&mut moq_net::track::Producer> {
        if !self.records.contains_key(record) {
            let path = record_path(self.namespace.as_deref()?, record)?;
            let broadcast = self.outgoing.create_broadcast(path.as_str()).ok()?;
            let track = broadcast
                .create_track(
                    RECORD_TRACK,
                    moq_net::track::Info::default().with_max_age(Duration::from_secs(2)),
                )
                .ok()?;
            broadcast.announce(moq_net::origin::Route::default()).ok()?;
            self.records.insert(record.to_owned(), (broadcast, track));
        }
        self.records.get_mut(record).map(|(_, track)| track)
    }

    pub fn write_record(&mut self, record: &str, frame: &[u8]) -> bool {
        let Some(track) = self.record(record) else {
            return false;
        };
        let Ok(mut group) = track.append_group() else {
            return false;
        };
        group
            .write_frame(
                moq_net::Timestamp::ZERO,
                bytes::Bytes::copy_from_slice(frame),
            )
            .is_ok()
            && group.finish().is_ok()
    }

    /// Close a record this participant was writing.
    pub fn close_record(&mut self, record: &str) {
        if let Some((broadcast, track)) = self.records.remove(record) {
            let _ = track.finish();
            broadcast.finish();
        }
    }

    /// The egress broadcast of a room member, once the relay announces it.
    pub async fn member(&self, namespace: &str) -> Option<moq_net::broadcast::Consumer> {
        self.incoming
            .consume()
            .routed_broadcast(namespace)
            .await
            .ok()
    }

    /// Room member announcements as the relay offers them.
    pub fn announced(&self) -> moq_net::announce::Consumer {
        self.incoming.consume().announced()
    }
}

/// One room member's presence datagrams, followed the way a page follows
/// them (`watchMembers` in the browser runtime's live transport): the relay
/// withdraws a member's namespace (a revoked client, a host-fed namespace
/// idle past [`super::HOST_SLOT_IDLE`]) and announces it afresh when the
/// member speaks again, and the follower reads on from the new broadcast.
/// Following runs on its own task, so a caller that stops waiting loses
/// nothing: a subscription under way completes, and frames queue (the
/// newest give way once [`MEMBER_PRESENCE_QUEUE`] are waiting).
pub struct MemberPresence {
    frames: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    _follow: crate::Task,
}

/// Presence frames a [`MemberPresence`] keeps for its reader.
pub const MEMBER_PRESENCE_QUEUE: usize = 64;

/// How soon a [`MemberPresence`] asks a still-announced broadcast again
/// after it refused the subscription or ended the track.
const MEMBER_RESUBSCRIBE: std::time::Duration = std::time::Duration::from_millis(200);

impl MemberPresence {
    /// Follow the member publishing under `namespace` on `peer`'s session.
    #[must_use]
    pub fn follow(peer: &Peer, namespace: &str) -> Self {
        let (tx, frames) = tokio::sync::mpsc::channel(MEMBER_PRESENCE_QUEUE);
        let follow = follow_member(peer, namespace, PRESENCE_TRACK, None, move |mut track| {
            let tx = tx.clone();
            async move {
                while let Ok(Some(datagram)) = track.recv_datagram().await {
                    if tx.is_closed() {
                        return false;
                    }
                    let _ = tx.try_send(datagram.payload);
                }
                !tx.is_closed()
            }
        });
        Self {
            frames,
            _follow: follow,
        }
    }

    /// The member's next presence datagram, or `None` when none arrives
    /// within `wait`.
    pub async fn next(&mut self, wait: Duration) -> Option<bytes::Bytes> {
        tokio::time::timeout(wait, self.frames.recv())
            .await
            .ok()
            .flatten()
    }
}

/// Read `track` of the member under `namespace` on `peer`'s session, as a
/// page's `subscribeTrack` under `watchMembers` does (the browser runtime's
/// live transport): `read` is handed each subscription until it ends. The
/// relay withdraws a member's namespace (a revoked client, a host-fed
/// namespace idle past [`super::HOST_SLOT_IDLE`]) and announces it afresh,
/// and a subscription can end while the broadcast stays announced (a
/// re-announcement racing it on a slow node); either way the follower
/// subscribes again, to the new broadcast or, shortly, to the same one.
/// `read` returns whether anyone still listens; following stops when not.
fn follow_member<F, R>(
    peer: &Peer,
    namespace: &str,
    track: &str,
    subscription: Option<moq_net::track::Subscription>,
    mut read: F,
) -> crate::Task
where
    F: FnMut(moq_net::track::Subscriber) -> R + Send + 'static,
    R: std::future::Future<Output = bool> + Send,
{
    let origin = peer.incoming.consume();
    let namespace = namespace.to_owned();
    let track = track.to_owned();
    crate::Task::spawn(async move {
        // Ends when the session's origin closes; the reader then hears
        // nothing more.
        while let Ok(broadcast) = origin.routed_broadcast(namespace.as_str()).await {
            if let Ok(consumer) = broadcast.track(&track)
                && let Ok(subscriber) = consumer.subscribe(subscription.clone()).await
                && !read(subscriber).await
            {
                return;
            }
            tokio::select! {
                _ = broadcast.closed() => {}
                () = tokio::time::sleep(MEMBER_RESUBSCRIBE) => {}
            }
        }
        std::future::pending::<()>().await;
    })
}

/// How far an ops reader may fall behind newer groups before it skips older
/// ones: the page's ops subscription (`OPS_SUBSCRIPTION` in the browser
/// runtime's live transport).
const OPS_AGE: Duration = Duration::from_millis(2500);

/// One room member's ops track, followed the way a page follows it
/// (`subscribeOps` in the browser runtime's live transport): frames in
/// order, the relay's empty seal frame skipped, a group that fails (one the
/// publisher superseded before it was read) given up for the next, and the
/// subscription renewed through withdrawal and re-announcement as
/// [`MemberPresence`] renews its own. A renewed subscription starts at the
/// track's latest group, so a reader can see that group's frames again.
pub struct MemberOps {
    frames: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    _follow: crate::Task,
}

impl MemberOps {
    /// Follow ops track `track` (`ops/<hex record digest>`) of the member
    /// publishing under `namespace` on `peer`'s session.
    #[must_use]
    pub fn follow(peer: &Peer, namespace: &str, track: &str) -> Self {
        let (tx, frames) = tokio::sync::mpsc::channel(MEMBER_PRESENCE_QUEUE);
        let subscription = moq_net::track::Subscription::default()
            .with_priority(1)
            .with_max_age(OPS_AGE);
        let follow = follow_member(
            peer,
            namespace,
            track,
            Some(subscription),
            move |mut track| {
                let tx = tx.clone();
                async move {
                    while let Ok(Some(mut group)) = track.recv_group().await {
                        while let Ok(Some(frame)) = group.read_frame().await {
                            if frame.payload.is_empty() {
                                continue;
                            }
                            if tx.send(frame.payload).await.is_err() {
                                return false;
                            }
                        }
                    }
                    !tx.is_closed()
                }
            },
        );
        Self {
            frames,
            _follow: follow,
        }
    }

    /// The track's next frame, or `None` when none arrives within `wait`.
    pub async fn next(&mut self, wait: Duration) -> Option<bytes::Bytes> {
        tokio::time::timeout(wait, self.frames.recv())
            .await
            .ok()
            .flatten()
    }
}

/// A peer being assembled before its session exists.
struct Pending {
    incoming: moq_net::origin::Producer,
    outgoing: moq_net::origin::Producer,
    namespace: Option<String>,
    broadcast: Option<moq_net::broadcast::Producer>,
    presence: Option<moq_net::track::Producer>,
    origins: [carrier::OriginTask; 2],
}

impl Pending {
    fn ready(self, connected: carrier::Connected) -> Peer {
        Peer {
            connected,
            incoming: self.incoming,
            outgoing: self.outgoing,
            namespace: self.namespace,
            _broadcast: self.broadcast,
            presence: self.presence,
            records: HashMap::new(),
            _origins: self.origins,
        }
    }
}
