//! A playback on a television or a speaker reached by a cast protocol.
//!
//! The computer never speaks `Google Cast`, `AirPlay`, `Miracast` or `FCast` itself: a separate
//! projector service runs the session and reports what it is doing over its control socket
//! ([`tilo_projector::control`]). [`CastHost`] is the [`OutputHost`] a playback applies its cast
//! outputs to. It finds a receiver a person named, asks the service to project to it, reports the
//! stages a person sees while it connects, and starts it again when the receiver drops the
//! session - until the retries run out, at which point the output fails in words a person can
//! read.
//!
//! The service runs one projection at a time, so a playback casts to one receiver: a second cast
//! output is refused rather than silently replacing the first. [`transport::ProjectorTransport`]
//! is the real one, behind the crate's `cast` feature so a build without casting links none of
//! the protocol stacks.

use std::collections::BTreeMap;

use tilo_clock::time::{Duration, Instant};

use crate::playback::{Output, OutputHost, OutputKind, OutputState};
pub use crate::playback_sink::HostEvent;

/// How long a receiver list is used before the host looks again: a television that woke up or
/// went to sleep since is still found, without a search per frame.
pub const RECEIVER_TTL: Duration = Duration::from_secs(30);
/// How many times a dropped session is started again before its output fails.
pub const RECONNECT_ATTEMPTS: u32 = 3;
/// How long the host waits between tries.
pub const RECONNECT_DELAY: Duration = Duration::from_secs(2);
/// The projector service runs one projection at a time, so a playback casts to one receiver.
pub const MAX_CASTS: usize = 1;

/// A television or speaker a discovery found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receiver {
    /// What a person picks it by, and what an output names in `OutputKind::Cast::target`.
    pub id: String,
    /// What a person reads it as.
    pub name: String,
    /// The protocol as a person reads it: `Google Cast`, `AirPlay`, `Miracast`, `FCast`.
    pub protocol: String,
    /// Where it stands or what it is, as the service found it.
    pub detail: String,
}

/// How far a cast output has got in connecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastStage {
    Starting,
    /// Talking to the receiver: the cast handshake, `AirPlay` pairing or Miracast negotiation.
    Handshake,
    WaitingForFrames,
    Loading,
}

impl CastStage {
    /// What the person is told while this stage runs.
    #[must_use]
    pub fn detail(self) -> &'static str {
        match self {
            Self::Starting => "Starting the connection.",
            Self::Handshake => "Connecting to the receiver.",
            Self::WaitingForFrames => "Waiting for the receiver to show it.",
            Self::Loading => "Loading it on the receiver.",
        }
    }
}

/// What a cast session reports about itself, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CastEvent {
    Stage(CastStage),
    /// The receiver shows the stream.
    Connected,
    /// The session is over: the receiver stopped it, the link failed, or the service did.
    Ended {
        reason: Option<String>,
    },
}

/// The projector behind a [`CastHost`]: a real service, or a test's stand-in. It runs at most
/// one cast at a time, named by the output it serves.
pub trait CastTransport: Send {
    /// Look for receivers now.
    ///
    /// # Errors
    /// What a person reads as the reason nothing was found.
    fn discover(&mut self) -> Result<Vec<Receiver>, String>;

    /// Start casting to `receiver` for the output `cast`, replacing any session running. Returns
    /// once the session is starting; [`CastTransport::events`] carries its progress.
    ///
    /// # Errors
    /// What a person reads as the reason it could not start.
    fn begin(&mut self, cast: &str, receiver: &Receiver) -> Result<(), String>;

    /// Stop the session serving `cast`, if it is still running.
    fn end(&mut self, cast: &str);

    /// What happened to the casts since the last call, by output.
    fn events(&mut self) -> Vec<(String, CastEvent)>;
}

/// A playback's cast outputs, as the projector service runs them.
pub struct CastHost<T: CastTransport> {
    transport: T,
    casts: BTreeMap<String, Cast>,
    events: Vec<HostEvent>,
    receivers: Vec<Receiver>,
    found_at: Option<Instant>,
}

struct Cast {
    receiver: Receiver,
    state: OutputState,
    /// How many times this output has been started again after a drop.
    attempts: u32,
    retry_at: Option<Instant>,
}

impl<T: CastTransport> CastHost<T> {
    #[must_use]
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            casts: BTreeMap::new(),
            events: Vec::new(),
            receivers: Vec::new(),
            found_at: None,
        }
    }

    /// Where an output's cast has got to, for a view that shows it.
    #[must_use]
    pub fn state(&self, id: &str) -> Option<OutputState> {
        self.casts.get(id).map(|cast| cast.state)
    }

    /// The receivers the last discovery found, for the picker a person casts from.
    #[must_use]
    pub fn receivers(&self) -> &[Receiver] {
        &self.receivers
    }

    /// Look for receivers again and hand back what is there now.
    ///
    /// # Errors
    /// What a person reads as the reason nothing was found.
    pub fn refresh(&mut self) -> Result<Vec<Receiver>, String> {
        let found = self.transport.discover()?;
        self.receivers.clone_from(&found);
        self.found_at = Some(tilo_clock::now());
        Ok(found)
    }

    /// The receivers, searching only when none were found yet or the list has aged.
    fn warmed(&mut self) -> Result<(), String> {
        let fresh = self
            .found_at
            .is_some_and(|at| tilo_clock::now().saturating_duration_since(at) < RECEIVER_TTL);
        if fresh && !self.receivers.is_empty() {
            return Ok(());
        }
        self.refresh().map(|_| ())
    }

    /// The outputs that changed without being asked since the last call: the stages of a
    /// connection, a session that ended, and the tries to start it again.
    pub fn events(&mut self) -> Vec<HostEvent> {
        std::mem::take(&mut self.events)
    }

    /// Apply what the service reported and start again the casts whose retry is due at `now`.
    /// Returns whether anything a person looks at changed.
    pub fn poll_at(&mut self, now: Instant) -> bool {
        let mut changed = false;
        let reported = self.transport.events();
        for (id, event) in reported {
            let Some(cast) = self.casts.get_mut(&id) else {
                continue;
            };
            match event {
                CastEvent::Stage(stage) => {
                    cast.state = OutputState::Connecting;
                    self.events.push(HostEvent {
                        id,
                        state: OutputState::Connecting,
                        detail: Some(stage.detail().to_owned()),
                    });
                }
                CastEvent::Connected => {
                    cast.state = OutputState::Playing;
                    cast.attempts = 0;
                    cast.retry_at = None;
                    self.events.push(HostEvent {
                        id,
                        state: OutputState::Playing,
                        detail: None,
                    });
                }
                CastEvent::Ended { reason } => {
                    if let Some(reason) = reason {
                        tracing::warn!(%reason, "cast session ended");
                    }
                    if cast.attempts >= RECONNECT_ATTEMPTS {
                        cast.state = OutputState::Failed;
                        cast.retry_at = None;
                        self.events.push(HostEvent {
                            id,
                            state: OutputState::Failed,
                            detail: Some(format!("{} kept disconnecting.", cast.receiver.name)),
                        });
                    } else {
                        cast.attempts = cast.attempts.saturating_add(1);
                        cast.retry_at = Some(now.checked_add(RECONNECT_DELAY).unwrap_or(now));
                        self.events.push(HostEvent {
                            id,
                            state: OutputState::Connecting,
                            detail: Some(format!(
                                "{} disconnected; reconnecting.",
                                cast.receiver.name
                            )),
                        });
                    }
                }
            }
            changed = true;
        }
        for (id, cast) in &mut self.casts {
            if cast.retry_at.is_none_or(|at| at > now) {
                continue;
            }
            cast.retry_at = None;
            let started = self.transport.begin(id, &cast.receiver);
            match started {
                Ok(()) => {
                    cast.state = OutputState::Connecting;
                }
                Err(why) => {
                    cast.attempts = RECONNECT_ATTEMPTS;
                    cast.state = OutputState::Failed;
                    self.events.push(HostEvent {
                        id: id.clone(),
                        state: OutputState::Failed,
                        detail: Some(why),
                    });
                }
            }
            changed = true;
        }
        changed
    }

    /// [`CastHost::poll_at`] at the current instant.
    pub fn poll(&mut self) -> bool {
        self.poll_at(tilo_clock::now())
    }

    /// The receiver an output names, by the id or the name a person picked.
    fn receiver_named(&self, target: &str) -> Option<Receiver> {
        self.receivers
            .iter()
            .find(|receiver| receiver.id == target)
            .or_else(|| {
                self.receivers
                    .iter()
                    .find(|receiver| receiver.name == target)
            })
            .cloned()
    }
}

impl<T: CastTransport> OutputHost for CastHost<T> {
    fn start(&mut self, output: &Output) -> Result<(), String> {
        let OutputKind::Cast { target, .. } = &output.kind else {
            return Err("That output is not a cast receiver.".to_owned());
        };
        if self.casts.contains_key(&output.id) {
            return Err("That output is already casting.".to_owned());
        }
        if self.casts.len() >= MAX_CASTS {
            return Err("One playback casts to one receiver at a time.".to_owned());
        }
        self.warmed()?;
        let receiver = self
            .receiver_named(target)
            .ok_or_else(|| format!("Couldn't find {} on the network.", output.label))?;
        // The cast is known before the session starts, so the stages a fast connection reports
        // are not dropped.
        self.casts.insert(
            output.id.clone(),
            Cast {
                receiver: receiver.clone(),
                state: OutputState::Connecting,
                attempts: 0,
                retry_at: None,
            },
        );
        if let Err(why) = self.transport.begin(&output.id, &receiver) {
            self.casts.remove(&output.id);
            return Err(why);
        }
        Ok(())
    }

    fn update(&mut self, output: &Output) -> Result<(), String> {
        // A receiver's loudness and timing are its own; only the session's progress changes what
        // is shown, and that arrives through `poll` and `events`. The same output naming another
        // receiver is that receiver now: a person cast to a different room.
        let OutputKind::Cast { target, .. } = &output.kind else {
            return Err("That output is not a cast receiver.".to_owned());
        };
        let current = self
            .casts
            .get(&output.id)
            .ok_or("That output is not casting.")?;
        if current.receiver.id == *target || current.receiver.name == *target {
            return Ok(());
        }
        self.warmed()?;
        let receiver = self
            .receiver_named(target)
            .ok_or_else(|| format!("Couldn't find {} on the network.", output.label))?;
        self.transport.end(&output.id);
        let started = self.transport.begin(&output.id, &receiver);
        if let Some(cast) = self.casts.get_mut(&output.id) {
            cast.receiver = receiver;
            cast.attempts = 0;
            cast.retry_at = None;
            cast.state = if started.is_ok() {
                OutputState::Connecting
            } else {
                OutputState::Failed
            };
        }
        started
    }

    fn stop(&mut self, id: &str) {
        if self.casts.remove(id).is_some() {
            self.transport.end(id);
        }
    }
}

impl<T: CastTransport> Drop for CastHost<T> {
    /// A playback that ends stops casting: the projector service stops when its client does.
    fn drop(&mut self) {
        for id in std::mem::take(&mut self.casts).into_keys() {
            self.transport.end(&id);
        }
    }
}

#[cfg(feature = "cast")]
pub mod transport;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion, and their counting cannot overflow"
)]
mod tests {
    use super::*;
    use crate::playback::{PlaybackOutputs, Playout};

    /// A transport that records what it was asked and reports what the test decides.
    #[derive(Default)]
    struct Log {
        receivers: Vec<Receiver>,
        refuse: Option<String>,
        begun: Vec<String>,
        ended: Vec<String>,
        queue: Vec<(String, CastEvent)>,
    }

    struct Fake(std::sync::Arc<std::sync::Mutex<Log>>);

    impl Fake {
        fn new(log: &std::sync::Arc<std::sync::Mutex<Log>>) -> Self {
            Self(std::sync::Arc::clone(log))
        }
        fn log(&self) -> std::sync::MutexGuard<'_, Log> {
            self.0.lock().unwrap()
        }
    }

    impl CastTransport for Fake {
        fn discover(&mut self) -> Result<Vec<Receiver>, String> {
            Ok(self.log().receivers.clone())
        }
        fn begin(&mut self, cast: &str, receiver: &Receiver) -> Result<(), String> {
            let mut log = self.log();
            if log.refuse.as_deref() == Some(receiver.id.as_str()) {
                return Err("The television refused the connection.".to_owned());
            }
            log.begun.push(cast.to_owned());
            drop(log);
            Ok(())
        }
        fn end(&mut self, cast: &str) {
            self.log().ended.push(cast.to_owned());
        }
        fn events(&mut self) -> Vec<(String, CastEvent)> {
            std::mem::take(&mut self.log().queue)
        }
    }

    fn receiver() -> Receiver {
        Receiver {
            id: "cast:living-room".to_owned(),
            name: "Living room".to_owned(),
            protocol: "Google Cast".to_owned(),
            detail: "192.0.2.20".to_owned(),
        }
    }

    fn kitchen() -> Receiver {
        Receiver {
            id: "cast:kitchen".to_owned(),
            name: "Kitchen".to_owned(),
            protocol: "AirPlay".to_owned(),
            detail: "192.0.2.21".to_owned(),
        }
    }

    fn cast(id: &str, target: &str) -> Output {
        Output {
            id: id.to_owned(),
            kind: OutputKind::Cast {
                protocol: "Google Cast".to_owned(),
                target: target.to_owned(),
            },
            label: "Living room".to_owned(),
            volume: 1.0,
            sync_ms: 0,
            state: OutputState::Connecting,
        }
    }

    fn started(log: &std::sync::Arc<std::sync::Mutex<Log>>) -> Playout<CastHost<Fake>> {
        let mut outputs = PlaybackOutputs::default();
        outputs.add(cast("tv", "cast:living-room"));
        let mut playout = Playout::new(CastHost::new(Fake::new(log)));
        playout.apply(&outputs).expect("the television is found");
        playout
    }

    #[test]
    fn a_cast_reports_its_stages_and_starts_again_until_the_retries_run_out() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Log {
            receivers: vec![receiver()],
            ..Log::default()
        }));
        let mut playout = started(&log);

        log.lock()
            .unwrap()
            .queue
            .push(("tv".to_owned(), CastEvent::Stage(CastStage::Handshake)));
        assert!(playout.host_mut().poll());
        assert_eq!(
            playout.host_mut().events(),
            [HostEvent {
                id: "tv".to_owned(),
                state: OutputState::Connecting,
                detail: Some("Connecting to the receiver.".to_owned()),
            }]
        );

        log.lock()
            .unwrap()
            .queue
            .push(("tv".to_owned(), CastEvent::Connected));
        playout.host_mut().poll();
        assert_eq!(
            playout.host_mut().events(),
            [HostEvent {
                id: "tv".to_owned(),
                state: OutputState::Playing,
                detail: None,
            }]
        );

        let start = tilo_clock::now();
        for attempt in 1..=RECONNECT_ATTEMPTS {
            log.lock()
                .unwrap()
                .queue
                .push(("tv".to_owned(), CastEvent::Ended { reason: None }));
            playout.host_mut().poll_at(start);
            assert_eq!(
                playout.host_mut().events(),
                [HostEvent {
                    id: "tv".to_owned(),
                    state: OutputState::Connecting,
                    detail: Some("Living room disconnected; reconnecting.".to_owned()),
                }]
            );
            let due = start.checked_add(RECONNECT_DELAY * (attempt + 1)).unwrap();
            playout.host_mut().poll_at(due);
            assert_eq!(
                log.lock().unwrap().begun.len(),
                attempt as usize + 1,
                "the session starts again once its delay has passed"
            );
        }

        log.lock()
            .unwrap()
            .queue
            .push(("tv".to_owned(), CastEvent::Ended { reason: None }));
        playout.host_mut().poll_at(start);
        assert_eq!(
            playout.host_mut().events(),
            [HostEvent {
                id: "tv".to_owned(),
                state: OutputState::Failed,
                detail: Some("Living room kept disconnecting.".to_owned()),
            }]
        );
    }

    #[test]
    fn the_same_cast_naming_another_receiver_moves_the_session() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Log {
            receivers: vec![receiver(), kitchen()],
            ..Log::default()
        }));
        let mut playout = started(&log);
        let mut next = PlaybackOutputs::default();
        next.add(cast("tv", "cast:kitchen"));
        playout
            .apply(&next)
            .expect("the kitchen receiver takes over");
        assert_eq!(log.lock().unwrap().ended, ["tv"]);
        assert_eq!(log.lock().unwrap().begun, ["tv", "tv"]);
        assert_eq!(playout.host().state("tv"), Some(OutputState::Connecting));
    }

    #[test]
    fn a_receiver_nobody_can_find_is_said_and_nothing_starts() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Log {
            receivers: vec![receiver()],
            ..Log::default()
        }));
        let mut outputs = PlaybackOutputs::default();
        outputs.add(cast("tv", "cast:bedroom"));
        let mut playout = Playout::new(CastHost::new(Fake::new(&log)));
        let failed = playout.apply(&outputs).expect_err("no such receiver");
        assert_eq!(failed, ["tv: Couldn't find Living room on the network."]);
        assert!(log.lock().unwrap().begun.is_empty());
    }

    #[test]
    fn a_refused_receiver_is_said_and_nothing_starts() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Log {
            receivers: vec![receiver()],
            refuse: Some("cast:living-room".to_owned()),
            ..Log::default()
        }));
        let mut outputs = PlaybackOutputs::default();
        outputs.add(cast("tv", "cast:living-room"));
        let mut playout = Playout::new(CastHost::new(Fake::new(&log)));
        let failed = playout.apply(&outputs).expect_err("the receiver refuses");
        assert_eq!(failed, ["tv: The television refused the connection."]);
    }

    #[test]
    fn one_playback_casts_to_one_receiver() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Log {
            receivers: vec![receiver()],
            ..Log::default()
        }));
        let mut outputs = PlaybackOutputs::default();
        outputs.add(cast("tv", "cast:living-room"));
        outputs.add(cast("bedroom", "cast:living-room"));
        let mut playout = Playout::new(CastHost::new(Fake::new(&log)));
        let failed = playout
            .apply(&outputs)
            .expect_err("the service runs one projection");
        assert_eq!(
            failed,
            ["bedroom: One playback casts to one receiver at a time."]
        );
    }

    #[test]
    fn ending_a_playback_stops_casting() {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Log {
            receivers: vec![receiver()],
            ..Log::default()
        }));
        let playout = started(&log);
        drop(playout);
        assert_eq!(log.lock().unwrap().ended, ["tv"]);
    }
}
