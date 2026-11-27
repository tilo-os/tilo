//! An app's sound on this device's output, on the connection its launcher handed it.
//!
//! A sandboxed app receives neither an audio socket nor the device's mixer: it hands this
//! device the PCM it decodes, 20 ms of stereo 48 kHz at a time, and this device plays it on the
//! desktop session's output, one stream per open output, named after the app so a volume mixer
//! shows it there. The app asks over the one connection its launcher placed for it
//! ([`super::local::Frames::of_an_app`]); the session holds the output it opened, its queue and
//! the person's lockdown, which silences it where it silences every other sound.
//!
//! The lane holds to the bounds the app side does ([`tilo_actions::remote::audio_output`]): one
//! write carries at most [`MAX_SAMPLES`], a full queue answers with the last accepted sequence
//! instead of holding the app's decoder, and every command waits at most [`DEADLINE`]. A
//! connection that ends fences what it opened, so a crashed decoder stops sounding.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_action_line::frame_line;
use tilo_action_line_serve::{CallLanes, Take};
use tilo_actions::remote::audio_output::{self, Request, Snapshot};
use tilo_actions::remote::{ActionCallError, RemoteActionMessage};
use tilo_app_host::collab_app_broker::AudioOutputConnection;
use tilo_lock::MutexRecover as _;

use super::services::Daemon;
use super::workspaces::block_on_owned;
use crate::seats::AppAt;

/// How long one command waits for the device's output service before the app is told it timed
/// out.
const DEADLINE: Duration = audio_output::DEADLINE;

/// The lane an app's connection carries: its sound output, and the evidence each command left.
pub(crate) struct AudioLane {
    connection: AudioOutputConnection,
    /// Per-command transport evidence, containing no PCM, route or identity data.
    evidence: Mutex<Evidence>,
}

#[derive(Default)]
struct Evidence {
    requests: u32,
    replies: u32,
    backpressure: u32,
    last: Option<Snapshot>,
}

impl Evidence {
    fn record(&mut self, request: &Request, result: &Result<Snapshot, String>) {
        self.requests = self.requests.saturating_add(1);
        match result {
            Ok(snapshot) => {
                self.replies = self.replies.saturating_add(1);
                if let Request::Write { sequence, .. } = request
                    && snapshot.accepted_sequence < *sequence
                {
                    self.backpressure = self.backpressure.saturating_add(1);
                }
                self.last = Some(snapshot.clone());
            }
            Err(why) => tracing::warn!(
                target: "tilo::audio_output",
                operation = operation(request),
                requests = self.requests,
                replies = self.replies,
                backpressure = self.backpressure,
                queued_frames = self.last.as_ref().map(|last| last.queued_frames),
                rendered_frames = self.last.as_ref().map(|last| last.rendered_frames),
                paused = self.last.as_ref().map(|last| last.paused),
                ready = self.last.as_ref().map(|last| last.ready),
                %why,
                "governed audio command failed"
            ),
        }
    }
}

impl AudioLane {
    pub(crate) fn new(connection: AudioOutputConnection) -> Self {
        Self {
            connection,
            evidence: Mutex::new(Evidence::default()),
        }
    }

    /// The app's process ended, or its connection did: nothing it opened sounds any more.
    pub(crate) fn disconnect(&self) {
        self.connection.disconnect();
    }

    /// One command of the app's output, answered with the output's state or why it was refused.
    fn answer(&self, correlation: u64, request: Request, body: &[u8]) -> Take {
        let result = self.run(request, body);
        Take::Answer(frame_line(RemoteActionMessage::AudioOutputResponse {
            correlation,
            result,
        }))
    }

    fn run(&self, request: Request, body: &[u8]) -> Result<Snapshot, ActionCallError> {
        let samples = request
            .samples_of(body)
            .map_err(|reason| ActionCallError::Refused {
                reason: reason.to_owned(),
            })?;
        let kept = request.clone();
        let future = self.connection.output(request, samples);
        let result = block_on_owned(async move {
            tokio::time::timeout(DEADLINE, future)
                .await
                .map_err(|_| "Governed audio output timed out".to_string())?
        });
        self.evidence.lock_recover().record(&kept, &result);
        result.map_err(|reason| ActionCallError::Unavailable { reason })
    }
}

impl Drop for AudioLane {
    fn drop(&mut self) {
        self.disconnect();
    }
}

impl CallLanes for AudioLane {
    fn take(&self, message: &RemoteActionMessage) -> Take {
        match message {
            RemoteActionMessage::AudioOutputRequest {
                correlation,
                command,
            } => self.answer(*correlation, command.clone(), &[]),
            _ => Take::NotCall,
        }
    }

    /// A write names how many bytes of PCM follow it; the rest of the lane's commands are one
    /// line and nothing after it.
    fn body_len(&self, message: &RemoteActionMessage) -> Option<usize> {
        match message {
            RemoteActionMessage::AudioOutputRequest { command, .. }
                if matches!(command, Request::Write { .. }) =>
            {
                Some(command.body_len())
            }
            _ => None,
        }
    }

    fn take_body(&self, message: &RemoteActionMessage, body: &[u8]) -> Take {
        match message {
            RemoteActionMessage::AudioOutputRequest {
                correlation,
                command,
            } => self.answer(*correlation, command.clone(), body),
            _ => Take::NotCall,
        }
    }
}

/// The command's name, as the device's evidence names it.
fn operation(request: &Request) -> &'static str {
    match request {
        Request::Open {} => "open",
        Request::Write { .. } => "write",
        Request::Pause { .. } => "pause",
        Request::Volume { .. } => "volume",
        Request::Flush { .. } => "flush",
        Request::Status { .. } => "status",
        Request::Close { .. } => "close",
    }
}

/// The sound output lane of the app connection `daemon` serves for `at`, while the session
/// plays app sound: `None` for a daemon that serves none, which answers the app's commands with
/// the refusal that says so.
pub(crate) fn lane_for(daemon: &Arc<Daemon>, at: &AppAt) -> Option<AudioLane> {
    let broker = daemon.app_action_broker()?;
    let held = Arc::downgrade(daemon);
    let (app, workspace, pid) = (at.app.clone(), at.workspace.clone(), at.pid);
    let current: Arc<dyn Fn() -> Result<(), String> + Send + Sync> = Arc::new(move || {
        let daemon = held
            .upgrade()
            .ok_or_else(|| "this computer's daemon is stopping".to_string())?;
        daemon
            .app_of(pid)
            .filter(|(named, in_workspace)| *named == app && *in_workspace == workspace)
            .map(|_| ())
            .ok_or_else(|| "the app's process ended".to_string())
    });
    let connection =
        broker
            .state()
            .audio_output(u64::from(pid), &at.app, &at.workspace, current)?;
    Some(AudioLane::new(connection))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use tilo_action_line::LineClient;
    use tilo_action_line_serve::serve_lines_with;
    use tilo_actions::remote::audio_output::MAX_SAMPLES;
    use tilo_actions::remote::{Caller, HostedContentPuts};
    use tilo_app_host::collab_app_broker::{
        AudioOutputFuture, AudioOutputServices, MediaRequestContext,
    };

    use super::*;

    /// The device's output as this test plays it: a bounded queue and a lockdown switch.
    #[derive(Default)]
    struct Fake {
        state: Mutex<Device>,
        capacity: usize,
        lockdown: AtomicBool,
        disconnects: AtomicU32,
    }

    #[derive(Default)]
    struct Device {
        generation: u64,
        accepted: u64,
        queued: Vec<i16>,
        received: Vec<Vec<i16>>,
        paused: bool,
        closed: bool,
    }

    impl Fake {
        fn snapshot(device: &Device) -> Snapshot {
            Snapshot {
                generation: device.generation,
                accepted_sequence: device.accepted,
                rendered_frames: 0,
                queued_frames: u32::try_from(device.queued.len() / 2).unwrap_or(u32::MAX),
                paused: device.paused,
                ready: !device.closed,
                closed: device.closed,
            }
        }

        fn apply(
            device: &mut Device,
            capacity: usize,
            lockdown: bool,
            request: &Request,
            samples: Vec<i16>,
        ) -> Result<Snapshot, String> {
            if !request.valid()
                || samples.len() > MAX_SAMPLES
                || (!samples.is_empty()
                    && !samples
                        .len()
                        .is_multiple_of(usize::from(audio_output::CHANNELS)))
            {
                return Err("invalid audio output command".into());
            }
            if !matches!(request, Request::Open {}) && !request_matches(request, device.generation)
            {
                return Err("stale audio output generation".into());
            }
            match request {
                Request::Open {} => {
                    device.generation = device.generation.saturating_add(1);
                    device.accepted = 0;
                    device.queued.clear();
                    device.closed = false;
                }
                Request::Write { sequence, .. } => {
                    if device.closed {
                        return Err("stale audio output generation".into());
                    }
                    if lockdown {
                        device.closed = true;
                        return Err(
                            "sound output is disabled by the person's lockdown policy".into()
                        );
                    }
                    // A full queue takes nothing and says what it last accepted.
                    if device.queued.len().saturating_add(samples.len()) > capacity {
                        return Ok(Self::snapshot(device));
                    }
                    device.queued.extend(samples.iter().copied());
                    device.accepted = *sequence;
                    device.received.push(samples);
                }
                Request::Pause { paused, .. } => device.paused = *paused,
                Request::Flush { .. } => device.queued.clear(),
                Request::Close { .. } => {
                    device.closed = true;
                    device.queued.clear();
                }
                Request::Volume { .. } | Request::Status { .. } => {}
            }
            Ok(Self::snapshot(device))
        }
    }

    /// Whether `request` names the open output `generation`.
    fn request_matches(request: &Request, generation: u64) -> bool {
        match request {
            Request::Open {} => true,
            Request::Write {
                generation: named, ..
            }
            | Request::Pause {
                generation: named, ..
            }
            | Request::Volume {
                generation: named, ..
            }
            | Request::Flush { generation: named }
            | Request::Status { generation: named }
            | Request::Close { generation: named } => *named == generation,
        }
    }

    impl AudioOutputServices for Fake {
        fn output(
            &self,
            context: MediaRequestContext,
            request: Request,
            samples: Vec<i16>,
        ) -> AudioOutputFuture {
            let result = context.revalidate().and_then(|()| {
                let mut device = self.state.lock().expect("the fake output");
                Self::apply(
                    &mut device,
                    self.capacity,
                    self.lockdown.load(Ordering::Acquire),
                    &request,
                    samples,
                )
            });
            Box::pin(std::future::ready(result))
        }

        fn disconnect(&self, _peer_id: u64) {
            {
                let mut device = self.state.lock().expect("the fake output");
                device.closed = true;
                device.queued.clear();
            }
            self.disconnects.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// The app end of a line served by the lane, and the thread that serves it. Every frame is
    /// the lane's, so the provider beside it is a connection to nothing.
    fn lane_line(lane: AudioLane) -> (LineClient, std::thread::JoinHandle<()>) {
        let idle = LineClient::over(std::io::empty(), std::io::sink()).expect("an idle provider");
        let (near, far) = tilo_socket::pair().expect("a line between the app and the device");
        let reader = std::io::BufReader::new(far.try_clone().expect("the serving end"));
        let serving = std::thread::spawn(move || {
            serve_lines_with(
                |_| Ok((idle.clone(), Caller::local_system())),
                &HostedContentPuts::default(),
                None,
                reader,
                far,
                None,
                Some(&lane),
            );
        });
        (LineClient::of_stream(near).expect("the app end"), serving)
    }

    fn write(client: &LineClient, sequence: u64, block: &[i16]) -> Result<Snapshot, String> {
        client
            .audio_output_write(1, sequence, block)
            .map_err(|error| error.to_string())
    }

    fn ask(client: &LineClient, request: Request) -> Result<Snapshot, String> {
        client
            .audio_output(request)
            .map_err(|error| error.to_string())
    }

    /// One app's sound over a line the device serves: its writes reach the output in order, a
    /// full queue answers with backpressure instead of holding, the bounds hold, the person's
    /// lockdown silences it, and a dropped connection closes what it opened.
    #[test]
    fn app_sound_reaches_the_device_output_in_order_bounded_and_closed() {
        let fake = Arc::new(Fake {
            capacity: MAX_SAMPLES * 2,
            ..Fake::default()
        });
        let live = Arc::new(AtomicBool::new(true));
        let checked = Arc::clone(&live);
        let context = MediaRequestContext::daemon_line(
            7,
            "test.app".to_owned(),
            "workspace".to_owned(),
            Arc::new(move || {
                if checked.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err("the app's process ended".to_owned())
                }
            }),
        );
        let service: Arc<dyn AudioOutputServices> = fake.clone();
        let lane = AudioLane::new(AudioOutputConnection::new(service, context));
        let (client, serving) = lane_line(lane);

        assert_eq!(ask(&client, Request::Open {}).unwrap().generation, 1);

        let first = vec![1000; MAX_SAMPLES];
        let second = vec![-1000; MAX_SAMPLES];
        assert_eq!(write(&client, 1, &first).unwrap().accepted_sequence, 1);
        assert_eq!(write(&client, 2, &second).unwrap().accepted_sequence, 2);
        assert_eq!(
            fake.state.lock().expect("the fake output").received,
            vec![first, second],
            "the device plays the packets in the order the app said them"
        );

        // A full queue is answered with the sequence it last took, not a hold.
        let third = vec![3; MAX_SAMPLES];
        assert_eq!(write(&client, 3, &third).unwrap().accepted_sequence, 2);
        assert_eq!(
            fake.state.lock().expect("the fake output").received.len(),
            2,
            "a full queue keeps nothing of the packet it refused"
        );
        assert!(write(&client, 3, &vec![0; MAX_SAMPLES + 2]).is_err());

        // A flush frees the queue; a pause reaches the output; the bounds hold.
        ask(&client, Request::Flush { generation: 1 }).unwrap();
        assert_eq!(write(&client, 3, &third).unwrap().accepted_sequence, 3);
        ask(
            &client,
            Request::Pause {
                generation: 1,
                paused: true,
            },
        )
        .unwrap();
        assert!(fake.state.lock().expect("the fake output").paused);

        // The person's lockdown silences the app's sound at its next write.
        fake.lockdown.store(true, Ordering::Release);
        let refused = write(&client, 4, &vec![4; MAX_SAMPLES]).expect_err("lockdown silences it");
        assert!(refused.contains("lockdown"), "{refused}");
        fake.lockdown.store(false, Ordering::Release);

        // The admission the output was opened under ended: the process is no longer the app.
        live.store(false, Ordering::Release);
        let ended = ask(&client, Request::Status { generation: 1 })
            .expect_err("a process that ended is refused");
        assert!(ended.contains("process ended"), "{ended}");

        // A dropped connection closes what it opened, and stops sounding.
        drop(client);
        serving.join().expect("the lane's serving ends");
        assert!(
            fake.state.lock().expect("the fake output").closed,
            "a dropped connection closes the output"
        );
        assert_eq!(fake.disconnects.load(Ordering::Acquire), 1);
    }
}
