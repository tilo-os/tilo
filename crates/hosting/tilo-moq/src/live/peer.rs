//! A native `LIVE_V1` participant: the client side of [`super::LiveRelay`].
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
/// Local retention for presence and ops.
const RETAIN: Duration = Duration::from_secs(2);

/// One connected live participant. Dropping it closes the session.
pub struct Peer {
    pub connected: carrier::Connected,
    /// Broadcasts the relay announces to this participant (the room's egress).
    pub incoming: moq_net::origin::Producer,
    outgoing: moq_net::origin::Producer,
    namespace: Option<String>,
    broadcast: Option<moq_net::broadcast::Producer>,
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

    /// The `MoQ` session.
    pub fn session(&self) -> &moq_net::Session {
        &self.connected.session
    }

    /// The own `presence` track, when publishing.
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

    /// Write one frame as its own group on `record`.
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

    /// Stop publishing: the namespace broadcast and every record end.
    pub fn unpublish(&mut self) {
        for (_, (broadcast, track)) in self.records.drain() {
            let _ = track.finish();
            broadcast.finish();
        }
        if let Some(presence) = self.presence.take() {
            let _ = presence.finish();
        }
        if let Some(broadcast) = self.broadcast.take() {
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
            broadcast: self.broadcast,
            presence: self.presence,
            records: HashMap::new(),
            _origins: self.origins,
        }
    }
}
