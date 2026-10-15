//! Browser sessions: each one is an ordinary relay peer.
//!
//! After the same [`auth`](crate::auth) handshake an iroh peer runs, the
//! host starts [`run_peer_until`](crate::peer::run_peer_until) over an in-process duplex
//! stream, exactly as for a `tilo-relay/1` stream. `/send` writes the
//! browser's envelopes into that stream; a pump reads the host's envelopes
//! out into an event log that `/events` serves from a host-MAC'd cursor and
//! trims on acknowledgement. Subscribe, commit ordering, store-and-forward,
//! the live plane and the revocation audit are therefore the relay's own
//! code paths, and browser and iroh peers share one host order.
//!
//! Nothing here blocks a runtime thread: handlers await the log's `Notify`,
//! and the only periodic work is the idle janitor.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tilo_clock::unix_now_ns;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{Notify, watch};

use crate::auth::{AuthenticatedPeer, Hello, HelloAck2, PendingServer};
use crate::daemon::WebContext;
use crate::handshake::EndpointTrust;
use crate::transport::Envelope;
use crate::web::protocol::{
    Cursor, EventsPage, MAX_PAGE_BYTES, MAX_PAGE_ENVELOPES, SessionChallenge, SessionGrant,
    envelope_bytes,
};
use tilo_lock::MutexRecover as _;

/// A handshake must be finished within this.
const HANDSHAKE_TTL: Duration = Duration::from_secs(30);
/// Sessions end after this, even when active; the device reauthenticates.
const SESSION_TTL: Duration = Duration::from_secs(15 * 60);
/// A session nobody polled for this long ends; its peer unsubscribes and the
/// host queues for the device instead.
const IDLE_TTL: Duration = Duration::from_secs(45);
/// Event bytes the host holds for a session until the device acknowledges
/// them. Past it the session stops taking envelopes from its peer loop, whose
/// live deliveries then stay on disk until the device reads and its room
/// resumes (`crate::peer::Lag`): a slow reader costs latency, not its session.
const MAX_LOG_BYTES: usize = 16 * 1024 * 1024;
/// Duplex buffer between the HTTPS side and the peer loop.
const DUPLEX_BYTES: usize = 4 * 1024 * 1024;

/// Why a session route refused.
#[derive(Debug)]
pub(crate) enum SessionError {
    /// Unknown, expired or ended session (401).
    Gone,
    /// Malformed or failed handshake, or an invalid cursor (400/403).
    Refused(String),
    /// Capacity (503).
    Busy,
}

struct Handshake {
    pending: PendingServer,
    started: Instant,
}

struct Log {
    /// Sequence of `items[0]`.
    first_seq: u64,
    items: VecDeque<Envelope>,
    /// `envelope_bytes` of `items`.
    bytes: usize,
    closed: bool,
}

enum Admit {
    Logged,
    Closed,
    /// The log is at its byte budget; the envelope comes back.
    Full(Envelope),
}

pub(crate) struct Session {
    pub(crate) peer: AuthenticatedPeer,
    token: [u8; 32],
    expires: Instant,
    last_poll: Mutex<Instant>,
    to_host: tokio::sync::Mutex<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
    log: Mutex<Log>,
    changed: Notify,
    /// Signalled when acknowledged events leave the log.
    drained: Notify,
    end: watch::Sender<bool>,
    /// [`Sessions::ended`], shared by every session of the host.
    ended: Arc<Notify>,
}

impl Session {
    fn log(&self) -> MutexGuard<'_, Log> {
        self.log.lock_recover()
    }

    fn close(&self) {
        self.end.send_replace(true);
        self.log().closed = true;
        self.changed.notify_waiters();
        self.drained.notify_one();
        self.ended.notify_one();
    }

    /// Log `envelope` for the device unless that would exceed
    /// [`MAX_LOG_BYTES`]; an envelope larger than the budget is logged alone.
    fn admit(&self, envelope: Envelope) -> Admit {
        let size = envelope_bytes(&envelope);
        let mut log = self.log();
        if log.closed {
            return Admit::Closed;
        }
        if log.bytes != 0 && log.bytes.saturating_add(size) > MAX_LOG_BYTES {
            return Admit::Full(envelope);
        }
        log.bytes = log.bytes.saturating_add(size);
        log.items.push_back(envelope);
        Admit::Logged
    }

    pub(crate) fn token(&self) -> [u8; 32] {
        self.token
    }

    pub(crate) fn is_open(&self) -> bool {
        !*self.end.borrow() && Instant::now() < self.expires
    }
}

/// Every browser session of one host.
pub(crate) struct Sessions {
    web: WebContext,
    mac_key: [u8; 32],
    handshakes: Mutex<HashMap<[u8; 16], Handshake>>,
    live: Mutex<HashMap<[u8; 32], Arc<Session>>>,
    /// Signalled when any session ends, for the live authority's observer.
    ended: Arc<Notify>,
    /// Concurrent sessions and handshakes.
    limits: super::WebLimits,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock_recover()
}

impl Sessions {
    #[cfg(test)]
    pub(crate) fn new(web: WebContext) -> Arc<Self> {
        Self::with_limits(web, super::WebLimits::default())
    }

    pub(crate) fn with_limits(web: WebContext, limits: super::WebLimits) -> Arc<Self> {
        Arc::new(Self {
            web,
            mac_key: rand::random(),
            handshakes: Mutex::default(),
            live: Mutex::default(),
            ended: Arc::default(),
            limits,
        })
    }

    fn mac(&self, token: &[u8; 32], seq: u64) -> [u8; 32] {
        let mut input = [0u8; 40];
        input[..32].copy_from_slice(token);
        input[32..].copy_from_slice(&seq.to_le_bytes());
        *blake3::keyed_hash(&self.mac_key, &input).as_bytes()
    }

    fn cursor(&self, token: &[u8; 32], seq: u64) -> Cursor {
        Cursor {
            seq,
            mac: self.mac(token, seq),
        }
    }

    /// First handshake step: the relay's checked `Hello` and its `HelloAck`,
    /// under the live policy as on the relay ALPN.
    pub(crate) fn hello(&self, hello: &Hello) -> Result<SessionChallenge, SessionError> {
        let (pending, ack) = self
            .web
            .relay
            .identity
            .begin(EndpointTrust::Live(&self.web.relay.trust_policy), hello)
            .map_err(|error| {
                self.web.relay.metrics.inc_auth_rejected();
                SessionError::Refused(error.to_string())
            })?;
        let handshake: [u8; 16] = rand::random();
        let mut handshakes = lock(&self.handshakes);
        handshakes.retain(|_, pending| pending.started.elapsed() < HANDSHAKE_TTL);
        if handshakes.len() >= self.limits.max_handshakes {
            return Err(SessionError::Busy);
        }
        handshakes.insert(
            handshake,
            Handshake {
                pending,
                started: Instant::now(),
            },
        );
        drop(handshakes);
        Ok(SessionChallenge { ack, handshake })
    }

    /// Second handshake step: verify the device's signature and start its
    /// peer session.
    pub(crate) fn finish(
        self: &Arc<Self>,
        handshake: &[u8; 16],
        ack2: &HelloAck2,
    ) -> Result<SessionGrant, SessionError> {
        let pending = lock(&self.handshakes)
            .remove(handshake)
            .filter(|pending| pending.started.elapsed() < HANDSHAKE_TTL)
            .ok_or_else(|| SessionError::Refused("unknown handshake".into()))?;
        let peer = pending.pending.finish(ack2).map_err(|error| {
            self.web.relay.metrics.inc_auth_rejected();
            SessionError::Refused(error.to_string())
        })?;
        self.web.relay.metrics.inc_conns_authed();
        if lock(&self.live).len() >= self.limits.max_sessions {
            return Err(SessionError::Busy);
        }
        let token: [u8; 32] = rand::random();
        let (host_side, browser_side) = tokio::io::duplex(DUPLEX_BYTES);
        let (host_read, host_write) = tokio::io::split(host_side);
        let (browser_read, browser_write) = tokio::io::split(browser_side);
        let (end, mut ended) = watch::channel(false);
        let session = Arc::new(Session {
            peer: peer.clone(),
            token,
            expires: Instant::now()
                .checked_add(SESSION_TTL)
                .unwrap_or_else(Instant::now),
            last_poll: Mutex::new(Instant::now()),
            to_host: tokio::sync::Mutex::new(browser_write),
            log: Mutex::new(Log {
                first_seq: 1,
                items: VecDeque::new(),
                bytes: 0,
                closed: false,
            }),
            changed: Notify::new(),
            drained: Notify::new(),
            end,
            ended: Arc::clone(&self.ended),
        });
        let context = self.web.relay.peer_context();
        let pumped = session.clone();
        tokio::spawn(async move {
            let closed = async move {
                let _ = ended.wait_for(|ended| *ended).await;
            };
            if let Err(error) = Box::pin(crate::peer::run_peer_until(
                peer, closed, host_read, host_write, context,
            ))
            .await
            {
                tracing::debug!("browser session ended: {error}");
            }
        });
        tokio::spawn(async move {
            let mut reader = crate::frame::FrameReader::new(browser_read);
            while let Ok(Some(envelope)) = reader.read_next().await {
                let mut envelope = envelope;
                loop {
                    let drained = pumped.drained.notified();
                    match pumped.admit(envelope) {
                        Admit::Logged => break,
                        Admit::Closed => return,
                        Admit::Full(back) => envelope = back,
                    }
                    let waiting = crate::timing::start();
                    drained.await;
                    // A reader this far behind holds its peer loop's writes.
                    crate::timing::window("web.log_full_wait", waiting);
                }
                pumped.changed.notify_waiters();
            }
            pumped.close();
        });
        let expires_at_ns =
            unix_now_ns().saturating_add(u64::try_from(SESSION_TTL.as_nanos()).unwrap_or(u64::MAX));
        lock(&self.live).insert(token, session);
        Ok(SessionGrant {
            token,
            expires_at_ns,
        })
    }

    /// The open session for `token`.
    pub(crate) fn get(&self, token: &[u8; 32]) -> Result<Arc<Session>, SessionError> {
        lock(&self.live)
            .get(token)
            .filter(|session| session.is_open())
            .cloned()
            .ok_or(SessionError::Gone)
    }

    /// Hand the browser's envelopes to its peer session, in order.
    pub(crate) async fn send(
        &self,
        session: &Session,
        envelopes: Vec<Envelope>,
    ) -> Result<(), SessionError> {
        let mut bytes = Vec::new();
        for envelope in &envelopes {
            bytes.extend(
                crate::frame::encode(envelope)
                    .map_err(|error| SessionError::Refused(error.to_string()))?,
            );
        }
        let mut to_host = session.to_host.lock().await;
        to_host
            .write_all(&bytes)
            .await
            .map_err(|_| SessionError::Gone)
    }

    /// Acknowledge up to `ack`, then wait up to `wait` for events after it.
    pub(crate) async fn events(
        &self,
        session: &Session,
        ack: Option<Cursor>,
        wait: Duration,
    ) -> Result<EventsPage, SessionError> {
        *lock(&session.last_poll) = Instant::now();
        let after = match ack {
            Some(cursor) if cursor.mac == self.mac(&session.token, cursor.seq) => cursor.seq,
            Some(_) => return Err(SessionError::Refused("invalid cursor".into())),
            None => 0,
        };
        {
            let mut log = session.log();
            let mut freed = false;
            while log.first_seq <= after
                && let Some(envelope) = log.items.pop_front()
            {
                log.bytes = log.bytes.saturating_sub(envelope_bytes(&envelope));
                log.first_seq = log.first_seq.saturating_add(1);
                freed = true;
            }
            drop(log);
            if freed {
                session.drained.notify_one();
            }
        }
        let deadline = tokio::time::Instant::now()
            .checked_add(wait)
            .unwrap_or_else(tokio::time::Instant::now);
        loop {
            let notified = session.changed.notified();
            {
                let log = session.log();
                let start = after.saturating_add(1).max(log.first_seq);
                let skip =
                    usize::try_from(start.saturating_sub(log.first_seq)).unwrap_or(usize::MAX);
                let mut page_bytes = 0usize;
                let available: Vec<Envelope> = log
                    .items
                    .iter()
                    .skip(skip)
                    .take(MAX_PAGE_ENVELOPES)
                    .take_while(|envelope| {
                        let size = envelope_bytes(envelope);
                        let fits =
                            page_bytes == 0 || page_bytes.saturating_add(size) <= MAX_PAGE_BYTES;
                        if fits {
                            page_bytes = page_bytes.saturating_add(size);
                        }
                        fits
                    })
                    .cloned()
                    .collect();
                if !available.is_empty() {
                    let last = start
                        .saturating_add(available.len() as u64)
                        .saturating_sub(1);
                    return Ok(EventsPage {
                        cursor: Some(self.cursor(&session.token, last)),
                        envelopes: available,
                    });
                }
                if log.closed {
                    return Err(SessionError::Gone);
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Ok(EventsPage {
                    cursor: ack,
                    envelopes: Vec::new(),
                });
            }
        }
    }

    /// End idle and expired sessions and forget stale handshakes.
    pub(crate) fn sweep(&self) {
        let ended: Vec<Arc<Session>> = {
            let mut live = lock(&self.live);
            let ended = live
                .values()
                .filter(|session| {
                    !session.is_open() || lock(&session.last_poll).elapsed() > IDLE_TTL
                })
                .cloned()
                .collect::<Vec<_>>();
            live.retain(|_, session| !ended.iter().any(|gone| Arc::ptr_eq(gone, session)));
            ended
        };
        for session in ended {
            session.close();
        }
        lock(&self.handshakes).retain(|_, pending| pending.started.elapsed() < HANDSHAKE_TTL);
    }

    /// Open sessions per person (user root). A public host counts people
    /// from it, and tells its one spectator person's sessions apart.
    pub(crate) fn online_users(&self) -> std::collections::BTreeMap<[u8; 32], usize> {
        let mut online = std::collections::BTreeMap::new();
        for session in lock(&self.live)
            .values()
            .filter(|session| session.is_open())
        {
            let count = online
                .entry(session.peer.user_public_key)
                .or_insert(0_usize);
            *count = count.saturating_add(1);
        }
        online
    }

    /// Device and user of every open session, for the live authority.
    pub(crate) fn open_devices(&self) -> Vec<([u8; 32], AuthenticatedPeer)> {
        lock(&self.live)
            .iter()
            .filter(|(_, session)| session.is_open())
            .map(|(token, session)| (*token, session.peer.clone()))
            .collect()
    }

    /// Resolves after a session ended (closed or swept) since the last call
    /// returned; an end before the first wait is not lost.
    pub(crate) async fn ended(&self) {
        self.ended.notified().await;
    }

    /// When the soonest open session's lifetime runs out: it stops being
    /// open then without anything announcing it.
    pub(crate) fn next_expiry(&self) -> Option<Instant> {
        lock(&self.live)
            .values()
            .filter(|session| session.is_open())
            .map(|session| session.expires)
            .min()
    }

    pub(crate) fn context(&self) -> &WebContext {
        &self.web
    }
}
