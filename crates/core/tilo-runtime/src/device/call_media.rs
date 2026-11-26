//! An app's own place in a workspace's call, on the connection its launcher handed it.
//!
//! A sandboxed app holds no microphone, camera or live session: its views say what its person
//! did — join the call a voice channel holds, mute, share a camera or a screen, which pictures
//! the view draws — and read the call's status back. The device's call service holds the
//! capture, asks the person for each the first time, and joins the call on the live session the
//! daemon holds; the app reaches it over the one connection its launcher placed for it
//! ([`super::local::Frames::of_an_app`]), and every command is bound to that connection's app,
//! its workspace and its grants by the caller's context, checked again before each.
//!
//! A command is answered on the same connection once the call service has it: a watch and a
//! picture collection are held there (up to [`WATCH_DEADLINE`] and [`PICTURES_DEADLINE`]), and
//! must never hold the app's other frames, so each runs apart from the line's reader. A
//! connection that ends fences what it asked, so a crashed view joins nothing and captures
//! nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tilo_action_services::{CallLanes, Take, frame_line};
use tilo_actions::remote::native_media::{self, Request, Snapshot, WATCH_DEADLINE};
use tilo_actions::remote::{ActionCallError, RemoteActionMessage};
use tilo_app_host::collab_app_broker::CallMediaConnection;

use super::services::Daemon;
use super::workspaces::block_on_owned;
use crate::seats::AppAt;

/// Where the lane writes what it answers the app later: the connection's own writer.
pub(crate) type Out = Arc<dyn Fn(String) + Send + Sync>;

/// How many commands of one app's call run at once. An app holds one watch and one picture
/// collection outstanding; anything past this is refused rather than held.
const MAX_INFLIGHT: usize = 16;

/// The call lane an app's connection carries: the commands of the call its views take part in,
/// and the answers written back once the device's call service has them.
pub(crate) struct CallMediaLane {
    connection: CallMediaConnection,
    out: Out,
    inflight: AtomicUsize,
}

impl CallMediaLane {
    #[must_use]
    pub(crate) fn new(connection: CallMediaConnection, out: Out) -> Self {
        Self {
            connection,
            out,
            inflight: AtomicUsize::new(0),
        }
    }

    /// The app's process ended, or its connection did: nothing it joined stays, and nothing it
    /// captures runs.
    pub(crate) fn disconnect(&self) {
        self.connection.disconnect();
    }

    /// One command of the app's call, answered on the connection once the call service has it.
    fn take_command(&self, correlation: u64, request: Request) -> Take {
        if !request.dispatch_valid() {
            return Take::Answer(frame_line(RemoteActionMessage::CallMediaResult {
                correlation,
                result: Err(ActionCallError::InvalidRequest {
                    reason: "invalid call command".to_owned(),
                }),
            }));
        }
        if self.inflight.fetch_add(1, Ordering::AcqRel) >= MAX_INFLIGHT {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return Take::Answer(frame_line(RemoteActionMessage::CallMediaResult {
                correlation,
                result: Err(ActionCallError::Unavailable {
                    reason: "the call of this connection has too many commands waiting".to_owned(),
                }),
            }));
        }
        let connection = self.connection.clone();
        let out = Arc::clone(&self.out);
        let started = std::thread::Builder::new()
            .name("tilo-call-media".into())
            .spawn(move || {
                let held = held(&request);
                let result = block_on_owned(async move {
                    match tokio::time::timeout(held, connection.command(request)).await {
                        Ok(result) => result,
                        Err(_) => Err("the call service did not answer in time".to_owned()),
                    }
                });
                out(answer_line(correlation, result));
            });
        if started.is_err() {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return Take::Answer(frame_line(RemoteActionMessage::CallMediaResult {
                correlation,
                result: Err(ActionCallError::Unavailable {
                    reason: "the call service could not be reached".to_owned(),
                }),
            }));
        }
        Take::Silent
    }
}

impl Drop for CallMediaLane {
    fn drop(&mut self) {
        self.disconnect();
    }
}

impl CallLanes for CallMediaLane {
    fn take(&self, message: &RemoteActionMessage) -> Take {
        match message {
            RemoteActionMessage::CallMediaRequest {
                correlation,
                request,
            } => self.take_command(*correlation, request.clone()),
            _ => Take::NotCall,
        }
    }
}

/// How long the device keeps one command before it answers regardless: a watch and a picture
/// collection are held for their own holds, every other command for the service's answer.
fn held(request: &Request) -> Duration {
    match request.command {
        native_media::Command::Watch { .. } => WATCH_DEADLINE,
        native_media::Command::Pictures { .. } => native_media::PICTURES_DEADLINE,
        _ => native_media::REPLY_TIMEOUT,
    }
}

/// The line one command's answer is written as. An answer past the frame bound is answered with
/// the refusal that says so rather than a line the app cannot read.
fn answer_line(correlation: u64, result: Result<Snapshot, String>) -> String {
    let message = RemoteActionMessage::CallMediaResult {
        correlation,
        result: result.map_err(|reason| ActionCallError::Unavailable { reason }),
    };
    let frame = tilo_actions::remote::RemoteActionFrame::new(message);
    match frame.encode() {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => frame_line(RemoteActionMessage::CallMediaResult {
            correlation,
            result: Err(ActionCallError::Unavailable {
                reason: "the call's status is too large to carry".to_owned(),
            }),
        }),
    }
}

/// The device's call service as the app connection `daemon` serves for `at` reaches it, bound
/// to that app process: `None` for a daemon that serves no calls, which answers the app's
/// commands with the refusal that says so.
pub(crate) fn connection_for(
    daemon: &Arc<Daemon>,
    at: &AppAt,
) -> Option<tilo_app_host::collab_app_broker::CallMediaConnection> {
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
    broker
        .state()
        .call_media(u64::from(pid), &at.app, &at.workspace, current)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use std::io::Write as _;
    use tilo_actions::remote::native_media::{Command, Input, Roster, Shown};

    use tilo_action_services::{LineClient, serve_lines_with};
    use tilo_actions::remote::{Caller, HostedContentPuts};
    use tilo_app_host::collab_app_broker::{
        CallMediaFuture, CallMediaServices, MediaRequestContext,
    };

    use super::*;

    const ADA: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    /// This device's call as the test holds it: who is joined, what the person sends, and the
    /// watch a command can be held on until the test lets it go.
    #[derive(Default)]
    struct Fake {
        state: Mutex<State>,
        live: AtomicBool,
        disconnects: std::sync::atomic::AtomicU32,
        holds: Mutex<std::collections::VecDeque<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[derive(Default)]
    struct State {
        joined: Option<String>,
        muted: bool,
        camera: bool,
        commands: Vec<Command>,
    }

    fn snapshot(state: &State) -> Snapshot {
        let mut snapshot = Snapshot::unavailable();
        snapshot.input_id = Some("mic-a".to_owned());
        snapshot.inputs = vec![Input {
            id: "mic-a".to_owned(),
            label: "Built-in".to_owned(),
        }];
        snapshot.rosters = state
            .joined
            .as_ref()
            .map(|channel| {
                vec![Roster {
                    channel_id: channel.clone(),
                    people: vec![ADA.to_owned()],
                    cameras: Vec::new(),
                    screens: Vec::new(),
                    muted: Vec::new(),
                    deafened: Vec::new(),
                    bridges: Vec::new(),
                    bridged: Vec::new(),
                }]
            })
            .unwrap_or_default();
        // The other member's frames reach this device's call: the status says they speak.
        snapshot.speaking = state.joined.iter().map(|_| ADA.to_owned()).collect();
        snapshot.state = if state.joined.is_some() {
            native_media::State::Live
        } else {
            native_media::State::Idle
        };
        snapshot.muted = state.muted || state.joined.is_none();
        snapshot.capture_active = state.joined.is_some() && !state.muted;
        snapshot.camera = state.camera;
        snapshot.revision = 1;
        snapshot
    }

    impl Fake {
        #[expect(
            clippy::significant_drop_tightening,
            reason = "the fake's one state is read and written together"
        )]
        fn apply(&self, request: &Request) -> Snapshot {
            let mut state = self.state.lock().expect("the fake call");
            match &request.command {
                Command::Join { channel_id } => {
                    state.joined = Some(channel_id.clone());
                    state.muted = false;
                }
                Command::Leave => {
                    state.joined = None;
                    state.muted = true;
                }
                Command::SetMuted { muted } => state.muted = *muted,
                Command::SetCamera { on } => state.camera = *on,
                Command::Watch { .. }
                | Command::Pictures { .. }
                | Command::Show { .. }
                | Command::Status
                | Command::SetDeafened { .. }
                | Command::SelectInput { .. }
                | Command::SelectCamera { .. }
                | Command::SetScreen { .. }
                | Command::SetVolume { .. } => {}
            }
            state.commands.push(request.command.clone());
            snapshot(&state)
        }
    }

    impl CallMediaServices for Fake {
        fn command(&self, context: MediaRequestContext, request: Request) -> CallMediaFuture {
            let hold = self.holds.lock().expect("the fake holds").pop_front();
            let revalidated = context.revalidate();
            // A watch is held until the test lets it go, as the real service holds one for the
            // change it waits on.
            if let Some(hold) = hold {
                let status = snapshot(&self.state.lock().expect("the fake call"));
                Box::pin(async move {
                    revalidated?;
                    let _ = hold.await;
                    Ok(status)
                })
            } else {
                let result = revalidated.map(|()| self.apply(&request));
                Box::pin(std::future::ready(result))
            }
        }

        fn disconnect(&self, _peer_id: u64) {
            self.disconnects
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let mut state = self.state.lock().expect("the fake call");
            state.joined = None;
        }
    }

    /// The app end of a line served by a lane over `connection`, whose answers the lane writes
    /// on the same connection, as the daemon's does.
    fn lane_line(
        connection: tilo_app_host::collab_app_broker::CallMediaConnection,
    ) -> (LineClient, std::thread::JoinHandle<()>) {
        let idle = LineClient::over(std::io::empty(), std::io::sink()).expect("an idle provider");
        let (near, far) = tilo_socket::pair().expect("a line between the app and the device");
        let writing = std::sync::Arc::new(std::sync::Mutex::new(
            far.try_clone().expect("the lane's writer"),
        ));
        let out: Out = Arc::new(move |line: String| {
            if let Ok(mut writer) = writing.lock() {
                let _ = writeln!(writer, "{line}");
            }
        });
        let lane = CallMediaLane::new(connection, out);
        let reader = std::io::BufReader::new(far.try_clone().expect("the serving end"));
        let serving = std::thread::spawn(move || {
            serve_lines_with(
                |_| Ok((idle.clone(), Caller::local_system())),
                &HostedContentPuts::default(),
                None,
                reader,
                far,
                Duration::ZERO,
                Some(&lane),
            );
        });
        (LineClient::of_stream(near).expect("the app end"), serving)
    }

    fn lane(fake: &Arc<Fake>) -> tilo_app_host::collab_app_broker::CallMediaConnection {
        let live = Arc::clone(fake);
        let context = MediaRequestContext::daemon_line(
            7,
            "test.app".to_owned(),
            "workspace".to_owned(),
            Arc::new(move || {
                if live.live.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err("the app's process ended".to_owned())
                }
            }),
        );
        let service: Arc<dyn CallMediaServices> = fake.clone();
        tilo_app_host::collab_app_broker::CallMediaConnection::new(service, context)
    }

    fn command(calls: &LineClient, intent: u64, command: Command) -> Result<Snapshot, String> {
        calls
            .call_media(Request {
                version: 1,
                intent,
                command,
            })
            .map_err(|error| error.to_string())
    }

    /// One app's call over a line the device serves: it joins the call, hears who speaks there,
    /// mutes, leaves; a held watch does not hold its other commands; once its process is gone
    /// it is refused with why, and a dropped connection leaves the call.
    #[test]
    fn an_app_joins_hears_mutes_and_leaves_on_its_line() {
        let fake = Arc::new(Fake {
            live: AtomicBool::new(true),
            ..Fake::default()
        });
        let (client, serving) = lane_line(lane(&fake));

        // Join the workspace's call: the status says who is in it and who speaks.
        let joined = command(
            &client,
            1,
            Command::Join {
                channel_id: "voice:standup".to_owned(),
            },
        )
        .expect("the call takes the join");
        assert_eq!(joined.state, native_media::State::Live);
        assert_eq!(joined.rosters[0].people, vec![ADA.to_owned()]);
        assert_eq!(
            joined.speaking,
            vec![ADA.to_owned()],
            "the other member's frames are heard by this device's call"
        );
        assert!(!joined.muted && joined.capture_active);

        // What the view draws is said, and a microphone chosen, while the call is live.
        let shown = command(
            &client,
            0,
            Command::Show {
                pictures: vec![Shown {
                    person: ADA.to_owned(),
                    source: native_media::PictureSource::Camera,
                    layer: native_media::PictureLayer::Mid,
                }],
            },
        )
        .expect("the call is told which pictures are drawn");
        assert_eq!(shown.state, native_media::State::Live);

        // Mute: the call stops hearing this person.
        command(&client, 3, Command::SetMuted { muted: true }).expect("the call mutes");
        let muted = command(&client, 0, Command::Status).expect("the status is read");
        assert!(muted.muted && !muted.capture_active);
        assert!(
            fake.state
                .lock()
                .expect("the fake call")
                .commands
                .contains(&Command::SetMuted { muted: true }),
            "the device's call heard the mute"
        );

        // Leave: the call is over here.
        let left = command(&client, 5, Command::Leave).expect("the call is left");
        assert_eq!(left.state, native_media::State::Idle);
        assert!(fake.state.lock().expect("the fake call").joined.is_none());

        // The app's process ended: its commands are refused with why.
        fake.live.store(false, Ordering::Release);
        let refused = command(&client, 0, Command::Status).expect_err("a dead process is refused");
        assert!(refused.contains("process ended"), "{refused}");

        // A dropped connection leaves the call and fences capture.
        drop(client);
        serving.join().expect("the lane's serving ends");
        assert_eq!(
            fake.disconnects.load(Ordering::Acquire),
            1,
            "the connection that ended fenced the process"
        );
    }

    /// A held watch answers on the same connection without holding the app's next command: a
    /// view that waits for who joins next still reads the call's status now.
    #[test]
    fn a_held_watch_does_not_hold_the_next_command() {
        let fake = Arc::new(Fake {
            live: AtomicBool::new(true),
            ..Fake::default()
        });
        let (client, serving) = lane_line(lane(&fake));
        command(
            &client,
            1,
            Command::Join {
                channel_id: "voice:standup".to_owned(),
            },
        )
        .expect("the call takes the join");

        // A watch whose answer waits on a second command having been asked and answered.
        let (release, held) = tokio::sync::oneshot::channel();
        fake.holds.lock().expect("the fake holds").push_back(held);
        let watch = client.call_media_ask(Request {
            version: 1,
            intent: 0,
            command: Command::Watch { revision: 1 },
        });
        assert!(watch.is_ok(), "the watch is taken");

        let status = command(&client, 0, Command::Status).expect("the status is read meanwhile");
        assert_eq!(status.state, native_media::State::Live);

        let _ = release.send(());
        let mut answered = Vec::new();
        for _ in 0..500 {
            answered.extend(client.call_media_answers());
            if !answered.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (_, result) = answered.pop().expect("the held watch is answered");
        assert_eq!(
            result.expect("the watch's status").state,
            native_media::State::Live
        );
        drop(client);
        serving.join().expect("the lane's serving ends");
    }
}
