//! Bounded output lane on the admitted primary route, independent of CRDT and call commands.
use media::audio_output::{Factory, Observation, Output};
use runtime_collab::audio_output::{Command, MAX_SAMPLES, Snapshot};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use tokio::sync::mpsc as async_mpsc;

const DEADLINE: Duration = Duration::from_secs(3);
type ProgressReply =
    std::pin::Pin<Box<dyn std::future::Future<Output = (u64, Result<Snapshot, String>)>>>;
/// Per-command transport evidence, containing no PCM, route or identity data.
#[derive(Default)]
struct DispatchProgress {
    requests: u32,
    replies: u32,
    backpressure: u32,
    last: Option<Snapshot>,
}
impl DispatchProgress {
    fn timeout(&self, command: &Command) {
        let operation = match command {
            Command::Open {} => "open",
            Command::Write { .. } => "write",
            Command::Pause { .. } => "pause",
            Command::Volume { .. } => "volume",
            Command::Flush { .. } => "flush",
            Command::Status { .. } => "status",
            Command::Close { .. } => "close",
        };
        tracing::warn!(
            target: "tilo::audio_output",
            operation,
            requests = self.requests,
            replies = self.replies,
            backpressure = self.backpressure,
            queued_frames = self.last.as_ref().map(|last| last.queued_frames),
            rendered_frames = self.last.as_ref().map(|last| last.rendered_frames),
            paused = self.last.as_ref().map(|last| last.paused),
            ready = self.last.as_ref().map(|last| last.ready),
            "governed audio command reached its existing deadline"
        );
    }
}
struct Job {
    command: Command,
    reply: mpsc::SyncSender<Result<Snapshot, String>>,
}
struct Connection {
    normal: async_mpsc::Sender<Job>,
    stops: async_mpsc::Sender<Job>,
    alive: AtomicBool,
    observed: Mutex<Option<Snapshot>>,
    failure: Mutex<Option<String>>,
}
struct Live(Arc<Connection>);
impl Drop for Live {
    fn drop(&mut self) {
        self.0.alive.store(false, Ordering::Release);
    }
}

pub(super) fn install(channel: runtime_collab::ChannelHandle) {
    media::audio_output::install(Arc::new(HostFactory(serve(channel))));
}

/// Start the lane's worker on the current `LocalSet`.
fn serve(channel: runtime_collab::ChannelHandle) -> Arc<Connection> {
    let (normal, mut receive) = async_mpsc::channel::<Job>(16);
    let (stops, mut stop_receive) = async_mpsc::channel::<Job>(8);
    let connection = Arc::new(Connection {
        normal,
        stops,
        alive: AtomicBool::new(true),
        observed: Mutex::new(None),
        failure: Mutex::new(None),
    });
    let worker = connection.clone();
    tokio::task::spawn_local(async move {
        let _live = Live(worker.clone());
        let mut generation = None;
        let mut progress_reply: Option<ProgressReply> = None;
        let mut progress_superseded = false;
        let mut progress = tokio::time::interval(Duration::from_millis(50));
        progress.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let job = tokio::select! { biased;
                job = stop_receive.recv() => job,
                job = receive.recv() => job,
                reply = async {
                    match progress_reply.as_mut() {
                        Some(reply) => reply.await,
                        None => std::future::pending().await,
                    }
                }, if progress_reply.is_some() => {
                    progress_reply = None;
                    if generation == Some(reply.0) {
                        match reply.1 {
                            Ok(snapshot) if !progress_superseded => worker.observe(snapshot),
                            Ok(_) => {},
                            Err(error) => {
                                worker.fail(error);
                                generation = None;
                            }
                        }
                    }
                    continue;
                },
                _ = progress.tick(), if generation.is_some() && progress_reply.is_none() => {
                    if let Some(current) = generation {
                        // A slow observation must not consume the PCM caller's reply budget.
                        // Poll one bounded request beside the command lanes, never ahead of them.
                        let channel = channel.clone();
                        progress_superseded = false;
                        progress_reply = Some(Box::pin(async move {
                            let result = tokio::time::timeout(DEADLINE, channel.audio_output_command(&Command::Status { generation: current }))
                                .await
                                .map_err(|_| "Governed audio output progress ended".to_string())
                                .and_then(|result| result.map_err(|error| error.to_string()));
                            (current, result)
                        }));
                    }
                    continue;
                },
            };
            let Some(job) = job else {
                break;
            };
            let mut transport = DispatchProgress::default();
            let result = tokio::time::timeout(
                DEADLINE,
                dispatch(&channel, &job.command, &mut stop_receive, &mut transport),
            )
            .await
            .map_err(|_| {
                transport.timeout(&job.command);
                "Governed audio output timed out".to_string()
            })
            .and_then(|result| result);
            if let Ok(snapshot) = &result {
                // Even a flush/pause acknowledgment can supersede a status
                // without changing the generation or PCM sequence counters.
                progress_superseded |= progress_reply.is_some();
                if matches!(job.command, Command::Open {})
                    && let Ok(mut failure) = worker.failure.lock()
                {
                    *failure = None;
                }
                let next_generation = (!snapshot.closed).then_some(snapshot.generation);
                if generation != next_generation {
                    progress_reply = None;
                }
                generation = next_generation;
                worker.observe(snapshot.clone());
            } else if let Err(error) = &result {
                worker.fail(error.clone());
            }
            let _ = job.reply.send(result);
        }
    });
    connection
}

// Cap'n Proto capabilities stay on the SDK's LocalSet; this future never crosses threads.
fn dispatch<'a>(
    channel: &'a runtime_collab::ChannelHandle,
    command: &'a Command,
    urgent: &'a mut async_mpsc::Receiver<Job>,
    progress: &'a mut DispatchProgress,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Snapshot, String>> + 'a>> {
    Box::pin(async move {
        loop {
            progress.requests = progress.requests.saturating_add(1);
            let snapshot = channel
                .audio_output_command(command)
                .await
                .map_err(|error| error.to_string())?;
            progress.replies = progress.replies.saturating_add(1);
            progress.last = Some(snapshot.clone());
            if let Command::Write { sequence, .. } = command
                && snapshot.accepted_sequence < *sequence
            {
                progress.backpressure = progress.backpressure.saturating_add(1);
                // A full PCM queue must not hold pause, seek or teardown behind its retry.
                tokio::select! { biased;
                    Some(job) = urgent.recv() => {
                        let result = tokio::time::timeout(DEADLINE, channel.audio_output_command(&job.command))
                            .await.map_err(|_| "Governed audio control timed out".to_string())
                            .and_then(|result| result.map_err(|error| error.to_string()));
                        let _ = job.reply.send(result);
                    }
                    () = tokio::time::sleep(Duration::from_millis(5)) => {},
                }
                continue;
            }
            return Ok(snapshot);
        }
    })
}

impl Connection {
    fn fail(&self, error: String) {
        if let Ok(mut failure) = self.failure.lock() {
            *failure = Some(error);
        }
    }
    fn observe(&self, snapshot: Snapshot) {
        if let Ok(mut observed) = self.observed.lock() {
            *observed = Some(snapshot);
        }
    }
    fn enqueue(
        &self,
        command: Command,
    ) -> Result<mpsc::Receiver<Result<Snapshot, String>>, String> {
        if !self.alive.load(Ordering::Acquire) {
            return Err("Governed audio output connection ended".into());
        }
        let stopping = matches!(
            command,
            Command::Close { .. }
                | Command::Pause { .. }
                | Command::Flush { .. }
                | Command::Volume { .. }
        );
        let (reply, receive) = mpsc::sync_channel(1);
        let sender = if stopping { &self.stops } else { &self.normal };
        sender
            .try_send(Job { command, reply })
            .map_err(|_| "Governed audio output lane is full or closed".to_string())?;
        Ok(receive)
    }
    fn call(&self, command: Command) -> Result<Snapshot, String> {
        // Only decoder/device workers use this API; the UI sends ordinary resource actions.
        self.enqueue(command)?
            .recv_timeout(DEADLINE.saturating_add(Duration::from_millis(100)))
            .map_err(|_| "Governed audio output reply was lost".to_string())?
    }
}
struct HostFactory(Arc<Connection>);
impl Factory for HostFactory {
    fn open(&self) -> Result<Box<dyn Output>, String> {
        let snapshot = self.0.call(Command::Open {})?;
        Ok(Box::new(HostOutput {
            connection: self.0.clone(),
            generation: snapshot.generation,
            sequence: 0,
            closed: false,
        }))
    }
}
struct HostOutput {
    connection: Arc<Connection>,
    generation: u64,
    sequence: u64,
    closed: bool,
}
fn observation(snapshot: Snapshot) -> Observation {
    Observation {
        rendered_frames: snapshot.rendered_frames,
        queued_frames: snapshot.queued_frames,
        ready: snapshot.ready && !snapshot.closed,
    }
}
impl Output for HostOutput {
    fn cached_error(&self) -> Option<String> {
        self.connection
            .failure
            .try_lock()
            .ok()
            .and_then(|value| value.clone())
    }
    fn cached_observation(&self) -> Observation {
        self.connection
            .observed
            .try_lock()
            .ok()
            .and_then(|value| {
                value
                    .as_ref()
                    .filter(|value| value.generation == self.generation)
                    .cloned()
            })
            .map_or(Observation::default(), observation)
    }
    fn write(&mut self, samples: &[i16]) -> Result<Observation, String> {
        if self.closed
            || samples.is_empty()
            || samples.len() > MAX_SAMPLES
            || !samples.len().is_multiple_of(2)
        {
            return Err("Invalid or closed governed audio output packet".into());
        }
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Audio output sequence exhausted")?;
        let snapshot = self.connection.call(Command::Write {
            generation: self.generation,
            sequence,
            samples: samples.to_vec(),
        })?;
        if snapshot.accepted_sequence != sequence || snapshot.closed {
            return Err("Governed audio output did not accept the packet".into());
        }
        self.sequence = sequence;
        Ok(observation(snapshot))
    }
    fn observation(&mut self) -> Result<Observation, String> {
        self.connection
            .call(Command::Status {
                generation: self.generation,
            })
            .map(observation)
    }
    fn pause(&mut self, paused: bool) -> Result<(), String> {
        self.connection
            .enqueue(Command::Pause {
                generation: self.generation,
                paused,
            })
            .map(|_| ())
    }
    fn flush(&mut self) -> Result<(), String> {
        self.connection
            .enqueue(Command::Flush {
                generation: self.generation,
            })
            .map(|_| ())
    }
    fn volume(&mut self, percent: u8) -> Result<(), String> {
        self.connection
            .enqueue(Command::Volume {
                generation: self.generation,
                percent,
            })
            .map(|_| ())
    }
    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.connection.enqueue(Command::Close {
            generation: self.generation,
        });
    }
}
impl Drop for HostOutput {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    //! SDK wire scheduling fixtures, not proofs of native launcher custody or device output.
    use super::*;
    use runtime_collab::collab_capnp::{channel_handle, collab_service};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    #[derive(Clone, Default)]
    struct Peer(Rc<RefCell<(bool, bool, Vec<Command>)>>);
    impl channel_handle::Server for Peer {
        fn audio_output_command(
            self: Rc<Self>,
            params: channel_handle::AudioOutputCommandParams,
            mut results: channel_handle::AudioOutputCommandResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let command = Command::decode(params.get().unwrap().get_command().unwrap()).unwrap();
            let mut state = self.0.borrow_mut();
            state.2.push(command.clone());
            if state.1 {
                return std::future::ready(Err(capnp::Error::failed("generation ended".into())));
            }
            match command {
                Command::Flush { .. } => state.0 = true,
                Command::Close { .. } => state.1 = true,
                _ => {}
            }
            let accepted = if let Command::Write { sequence, .. } = command {
                if state.0 { sequence } else { 0 }
            } else {
                0
            };
            let snapshot = Snapshot {
                generation: 7,
                accepted_sequence: accepted,
                rendered_frames: 0,
                queued_frames: 0,
                paused: false,
                ready: !state.1,
                closed: state.1,
            };
            drop(state);
            results
                .get()
                .set_reply(&serde_json::to_vec(&snapshot).unwrap());
            std::future::ready(Ok(()))
        }
    }
    impl collab_service::Server for Peer {
        fn join_channel(
            self: Rc<Self>,
            _: collab_service::JoinChannelParams,
            mut results: collab_service::JoinChannelResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results
                .get()
                .set_handle(capnp_rpc::new_client(self.as_ref().clone()));
            std::future::ready(Ok(()))
        }
    }

    #[derive(Clone)]
    struct DelayedProgressPeer {
        status_started: Rc<tokio::sync::Notify>,
        accepted: Rc<RefCell<Vec<i16>>>,
        generation: Rc<Cell<u64>>,
        fail_progress: bool,
    }
    impl channel_handle::Server for DelayedProgressPeer {
        #[expect(
            clippy::future_not_send,
            reason = "the Cap'n Proto fixture retains Rc capabilities and runs only on the SDK LocalSet"
        )]
        fn audio_output_command(
            self: Rc<Self>,
            params: channel_handle::AudioOutputCommandParams,
            mut results: channel_handle::AudioOutputCommandResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let command = Command::decode(params.get().unwrap().get_command().unwrap()).unwrap();
            async move {
                let (generation, sequence) = match command {
                    Command::Open {} => {
                        self.generation.set(self.generation.get() + 1);
                        (self.generation.get(), 0)
                    }
                    Command::Status { generation } => {
                        self.status_started.notify_one();
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        if self.fail_progress && generation == 7 {
                            return Err(capnp::Error::failed("generation ended".into()));
                        }
                        (generation, 0)
                    }
                    Command::Write {
                        generation,
                        sequence,
                        samples,
                    } => {
                        *self.accepted.borrow_mut() = samples;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        (generation, sequence)
                    }
                    _ => (self.generation.get(), 0),
                };
                let snapshot = Snapshot {
                    generation,
                    accepted_sequence: sequence,
                    rendered_frames: if sequence > 0 { 960 } else { 0 },
                    queued_frames: 0,
                    paused: false,
                    ready: true,
                    closed: false,
                };
                results
                    .get()
                    .set_reply(&serde_json::to_vec(&snapshot).unwrap());
                Ok(())
            }
        }
    }
    impl collab_service::Server for DelayedProgressPeer {
        fn join_channel(
            self: Rc<Self>,
            _: collab_service::JoinChannelParams,
            mut results: collab_service::JoinChannelResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results
                .get()
                .set_handle(capnp_rpc::new_client(self.as_ref().clone()));
            std::future::ready(Ok(()))
        }
    }

    /// The reply to `command` on the lane, awaited on the paused clock the
    /// lane's deadlines and the fixtures' delays run on.
    async fn reply(connection: &Connection, command: Command) -> Result<Snapshot, String> {
        let reply = connection.enqueue(command)?;
        loop {
            match reply.try_recv() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(mpsc::TryRecvError::Disconnected) => panic!("the lane dropped its reply"),
            }
        }
    }

    /// The lane's own [`HostFactory::open`], awaited on the paused clock.
    async fn open(connection: &Arc<Connection>) -> HostOutput {
        let snapshot = reply(connection, Command::Open {}).await.unwrap();
        HostOutput {
            connection: connection.clone(),
            generation: snapshot.generation,
            sequence: 0,
            closed: false,
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn governed_output_delayed_progress_does_not_lose_accepted_pcm_reply() {
        tokio::task::LocalSet::new().run_until(async {
            let peer = DelayedProgressPeer {
                status_started: Rc::new(tokio::sync::Notify::new()),
                accepted: Rc::new(RefCell::new(Vec::new())),
                generation: Rc::new(Cell::new(6)),
                fail_progress: false,
            };
            let channel = runtime_collab::CollabClient::from_raw(capnp_rpc::new_client(peer.clone()))
                .join_channel("fixture", &[]).await.unwrap();
            let connection = serve(channel);
            let output = open(&connection).await;
            tokio::time::timeout(Duration::from_secs(1), peer.status_started.notified())
                .await.unwrap();
            let result = reply(&connection, Command::Write {
                generation: output.generation,
                sequence: 1,
                samples: vec![16384; MAX_SAMPLES],
            }).await;
            assert_eq!(*peer.accepted.borrow(), vec![16384; MAX_SAMPLES]);
            assert!(
                result.as_ref().is_ok_and(|snapshot| snapshot.accepted_sequence == 1),
                "individually subdeadline status and accepted PCM replies must not combine into a lost write reply: {result:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(output.cached_observation().rendered_frames, 960,
                "a progress reply captured before the write must not erase its newer acknowledgment");
        }).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn governed_output_progress_failure_is_scoped_to_its_generation() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for replace in [false, true] {
                    let peer = DelayedProgressPeer {
                        status_started: Rc::new(tokio::sync::Notify::new()),
                        accepted: Rc::new(RefCell::new(Vec::new())),
                        generation: Rc::new(Cell::new(6)),
                        fail_progress: true,
                    };
                    let channel =
                        runtime_collab::CollabClient::from_raw(capnp_rpc::new_client(peer.clone()))
                            .join_channel("fixture", &[])
                            .await
                            .unwrap();
                    let connection = serve(channel);
                    let first = open(&connection).await;
                    tokio::time::timeout(Duration::from_secs(1), peer.status_started.notified())
                        .await
                        .unwrap();
                    if replace {
                        let replacement = open(&connection).await;
                        assert_eq!(peer.generation.get(), 8);
                        tokio::time::sleep(Duration::from_millis(2050)).await;
                        // A timer can wake this test before the ready RPC task.
                        tokio::task::yield_now().await;
                        assert_eq!(
                            replacement.cached_error(),
                            None,
                            "an old progress failure must not poison a newly opened generation"
                        );
                    } else {
                        tokio::time::sleep(Duration::from_millis(2050)).await;
                        // A timer can wake this test before the ready RPC task.
                        tokio::task::yield_now().await;
                        assert!(
                            first.cached_error().is_some(),
                            "current-generation progress failure must remain observable"
                        );
                    }
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn governed_output_timeout_distinguishes_rpc_wait_from_queue_backpressure() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for delayed_rpc in [true, false] {
                    let client = if delayed_rpc {
                        capnp_rpc::new_client(DelayedProgressPeer {
                            status_started: Rc::new(tokio::sync::Notify::new()),
                            accepted: Rc::new(RefCell::new(Vec::new())),
                            generation: Rc::new(Cell::new(7)),
                            fail_progress: false,
                        })
                    } else {
                        capnp_rpc::new_client(Peer::default())
                    };
                    let channel = runtime_collab::CollabClient::from_raw(client)
                        .join_channel("fixture", &[])
                        .await
                        .unwrap();
                    let (_urgent, mut receive) = async_mpsc::channel(8);
                    let write = Command::Write {
                        generation: 7,
                        sequence: 1,
                        samples: vec![16384; 1920],
                    };
                    let mut transport = DispatchProgress::default();
                    let result = tokio::time::timeout(
                        Duration::from_millis(50),
                        dispatch(&channel, &write, &mut receive, &mut transport),
                    )
                    .await;
                    assert!(result.is_err());
                    if delayed_rpc {
                        assert_eq!(transport.requests, 1);
                        assert_eq!(transport.replies, 0);
                        assert_eq!(transport.backpressure, 0);
                        assert!(transport.last.is_none());
                    } else {
                        assert!(transport.replies > 0);
                        assert!(transport.backpressure > 0);
                        assert!(transport.last.is_some());
                    }
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn governed_output_full_pcm_lane_still_processes_flush_and_close() {
        tokio::task::LocalSet::new()
            .run_until(async {
                for stop in [
                    Command::Flush { generation: 7 },
                    Command::Close { generation: 7 },
                ] {
                    let peer = Peer::default();
                    let channel =
                        runtime_collab::CollabClient::from_raw(capnp_rpc::new_client(peer.clone()))
                            .join_channel("fixture", &[])
                            .await
                            .unwrap();
                    let (urgent, mut receive) = async_mpsc::channel(8);
                    let (reply, received) = mpsc::sync_channel(1);
                    urgent
                        .try_send(Job {
                            command: stop.clone(),
                            reply,
                        })
                        .unwrap();
                    let write = Command::Write {
                        generation: 7,
                        sequence: 1,
                        samples: vec![16384; 2],
                    };
                    let mut transport = DispatchProgress::default();
                    let result = tokio::time::timeout(
                        Duration::from_secs(1),
                        dispatch(&channel, &write, &mut receive, &mut transport),
                    )
                    .await
                    .unwrap();
                    assert!(received.try_recv().unwrap().is_ok());
                    assert!(transport.backpressure > 0);
                    assert!(transport.replies > 0);
                    assert!(transport.requests >= transport.replies);
                    assert!(transport.last.is_some());
                    if matches!(stop, Command::Flush { .. }) {
                        assert_eq!(result.unwrap().accepted_sequence, 1);
                    } else {
                        assert!(result.is_err());
                    }
                    let calls = &peer.0.borrow().2;
                    assert_eq!(calls.first(), Some(&write));
                    assert_eq!(calls.get(1), Some(&stop));
                }
            })
            .await;
    }
}
