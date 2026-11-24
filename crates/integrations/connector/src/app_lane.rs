//! The call lane of one app's connection: the frames an app speaks on its held connection
//! ([`tilo_actions::remote::call`]), carried by [`ConnectorCall`] into the workspace's call.
//!
//! The host builds this once per app connection, with what its workspace's live session needs
//! ([`CallSetup`]) and whether the app was granted a call at all. The app asks for a pair, says
//! who is in its outside call, and hands over each frame it heard; the lane publishes the outside
//! people as bridged participants, answers with the workspace's frames and the line to show in
//! the service. A host whose workspace granted the app no call, or has no live session or media
//! keys to carry one with, refuses every carry with that reason.

use std::collections::BTreeMap;
use std::sync::Arc;

use tilo_actions::remote::call::{
    BridgedCall, CallReport as SaidReport, CallStatus, MAX_CALL_AUDIO_BYTES,
};
use tilo_actions::remote::{ActionCallError, CallLanes, RemoteActionMessage, Take, frame_line};
use tilo_live::calls::CallRule;
use tilo_live::frame::LiveKeys;
use tilo_live::relay::Relay;
use tilo_live::roster::Roster;
use tokio::sync::{broadcast, mpsc, watch};

use crate::call_bridge::{ConnectorCall, Lane};
use crate::process::{CallsReport, OutsideAudio};
use crate::protocol::{CallLink, OutsideCall, OutsidePerson};

/// What a bridge needs to join the workspace's live session and carry one call: the host's own
/// live session's place, and everything its call is held to.
#[derive(Clone)]
pub struct CallSetup {
    /// The workspace's live session's relay, as this device joins it.
    pub relay: Relay,
    /// The workspace carried into.
    pub workspace: String,
    /// This device: the bridge's place in the call.
    pub device: [u8; 32],
    /// What the bridge's device calls itself.
    pub device_name: String,
    /// The person the bridge's device belongs to, if it belongs to one.
    pub person: Option<String>,
    /// The service the app reaches, in its own words.
    pub service: String,
    /// Who may send in the workspace's live session, the bridge among them.
    pub roster: Roster,
    /// The workspace's live session keys.
    pub keys: LiveKeys,
    /// What the workspace's calls allow, by channel.
    pub rules: BTreeMap<String, CallRule>,
    /// The person each device is.
    pub persons: BTreeMap<[u8; 32], String>,
    /// The media keys of the call the app is admitted to: the channel, its rule's generation,
    /// and the keys. A pair that names another channel is refused.
    pub call_keys: Option<(String, u64, LiveKeys)>,
}

/// How lines reach the app that spoke them.
pub type Out = Arc<dyn Fn(String) + Send + Sync>;

/// One app's call lane on its held connection.
pub struct AppLane {
    /// Whether the workspace granted this app a call; `Err` is the refusal it gets instead.
    granted: Result<CallSetup, String>,
    /// The app's calls and the bridge's place, as the app says.
    calls: watch::Sender<CallsReport>,
    /// What the app heard of the outside call.
    audio: broadcast::Sender<OutsideAudio>,
    /// Where the bridge's word to the app goes.
    out: Out,
    /// The carrier running now, if one is.
    carried: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl AppLane {
    /// A lane for the app connection whose lines go to `out`: `granted` is what the workspace
    /// gave this app, or why it gave none.
    #[must_use]
    pub fn new(granted: Result<CallSetup, String>, out: Out) -> Arc<Self> {
        let (calls, _) = watch::channel(CallsReport::default());
        let (audio, _) = broadcast::channel(256);
        Arc::new(Self {
            granted,
            calls,
            audio,
            out,
            carried: std::sync::Mutex::new(None),
        })
    }

    /// Whether the app was granted a call here, and why not when it was not.
    pub fn granted(&self) -> Result<(), String> {
        self.granted.clone().map(|_| ())
    }

    /// Carry `pair`, or let go of the one carried.
    fn carry(&self, pair: Option<BridgedCall>) -> Result<(), ActionCallError> {
        let setup = self
            .granted
            .clone()
            .map_err(|reason| ActionCallError::Refused { reason })?;
        let mut running = lock(&self.carried);
        let Some(pair) = pair else {
            if let Some(carried) = running.take() {
                carried.abort();
            }
            // The app left: nothing of its old call is said any more.
            let _ = self.calls.send(CallsReport::default());
            return Ok(());
        };
        if !pair.valid() {
            return Err(ActionCallError::InvalidRequest {
                reason: "a carried call names its channel here and the service's own".to_owned(),
            });
        }
        let Some((channel, _, _)) = &setup.call_keys else {
            return Err(ActionCallError::Refused {
                reason: "this computer holds no media keys for the call".to_owned(),
            });
        };
        if *channel != pair.tilo {
            return Err(ActionCallError::Refused {
                reason: format!(
                    "this app was admitted to the call in {}, not {}",
                    channel, pair.tilo
                ),
            });
        }
        let (say, says) = mpsc::unbounded_channel::<(Option<String>, Option<String>)>();
        let (speak, speaks) = mpsc::unbounded_channel::<(String, Vec<u8>)>();
        let lane = Lane {
            calls: self.calls.subscribe(),
            audio: self.audio.subscribe(),
            say,
            speak,
        };
        let mut bridge = ConnectorCall::new(
            setup.relay.clone(),
            setup.workspace.clone(),
            setup.device,
            setup.device_name.clone(),
            setup.person.clone(),
            setup.service.clone(),
            lane,
        );
        bridge.set_roster(setup.roster.clone());
        bridge.set_keys(setup.keys.clone());
        bridge.set_calls(setup.rules.clone(), setup.persons.clone());
        if let Some((channel, generation, keys)) = &setup.call_keys {
            bridge.set_call_keys(channel, *generation, keys.clone());
        }
        bridge.carry(Some(crate::call_bridge::BridgedCall {
            outside: pair.outside,
            tilo: pair.tilo,
        }));
        if let Some(carried) = running.take() {
            carried.abort();
        }
        *running = Some(tokio::spawn(bridge.run()));
        drop(running);
        // What the bridge says of itself and what it heard from the workspace reach the app on
        // the connection it holds.
        drop(tokio::spawn(says_status(self.out.clone(), says)));
        drop(tokio::spawn(said_audio(self.out.clone(), speaks)));
        Ok(())
    }

    /// The app's word on its calls, as the outside call's people and the bridge's place.
    fn report(&self, report: SaidReport) -> Result<(), ActionCallError> {
        if !report.valid() {
            return Err(ActionCallError::InvalidRequest {
                reason: "a call report names its calls, its people and its place".to_owned(),
            });
        }
        let calls = report
            .calls
            .into_iter()
            .map(|call| OutsideCall {
                call: call.call,
                people: call
                    .people
                    .into_iter()
                    .map(|person| OutsidePerson {
                        account: person.account,
                        name: person.name,
                        avatar_url: person.avatar_url,
                        avatar: person.avatar,
                        muted: person.muted,
                        deafened: person.deafened,
                    })
                    .collect(),
            })
            .collect();
        let link = match report.link {
            tilo_actions::remote::call::CallLink::Idle => CallLink::Idle,
            tilo_actions::remote::call::CallLink::Joining { call } => CallLink::Joining { call },
            tilo_actions::remote::call::CallLink::Joined { call } => CallLink::Joined { call },
            tilo_actions::remote::call::CallLink::Refused { call, reason } => {
                CallLink::Refused { call, reason }
            }
        };
        let _ = self.calls.send(CallsReport { calls, link });
        Ok(())
    }

    /// One frame an outside person said, for the workspace's call.
    fn heard(&self, frame: &tilo_actions::remote::call::CallAudio) {
        if !frame.valid() || frame.opus.len() > MAX_CALL_AUDIO_BYTES {
            return;
        }
        let _ = self.audio.send(OutsideAudio {
            speaker: frame.speaker.clone(),
            opus: frame.opus.clone(),
        });
    }
}

impl CallLanes for AppLane {
    fn take(&self, message: &RemoteActionMessage) -> Take {
        match message {
            RemoteActionMessage::CallCarryRequest { correlation, pair } => {
                let result = self.carry(pair.clone());
                Take::Answer(frame_line(RemoteActionMessage::CallCarryResponse {
                    correlation: *correlation,
                    result,
                }))
            }
            RemoteActionMessage::CallReport {
                correlation,
                report,
            } => {
                let result = self.report(report.clone());
                Take::Answer(frame_line(RemoteActionMessage::CallReportResponse {
                    correlation: *correlation,
                    result,
                }))
            }
            RemoteActionMessage::CallHeard(frame) => {
                self.heard(frame);
                Take::Silent
            }
            // What the device itself says: an app that sends one back is saying nothing.
            RemoteActionMessage::CallSaid(_)
            | RemoteActionMessage::CallStatus(_)
            | RemoteActionMessage::CallCarryResponse { .. }
            | RemoteActionMessage::CallReportResponse { .. } => Take::Silent,
            _ => Take::NotCall,
        }
    }
}

/// What the bridge says of itself, written to the app as it comes.
async fn says_status(
    out: Out,
    mut says: mpsc::UnboundedReceiver<(Option<String>, Option<String>)>,
) {
    while let Some((call, status)) = says.recv().await {
        out(frame_line(RemoteActionMessage::CallStatus(CallStatus {
            call,
            status,
        })));
    }
}

/// What the workspace's call said, written to the app frame by frame.
async fn said_audio(out: Out, mut speaks: mpsc::UnboundedReceiver<(String, Vec<u8>)>) {
    while let Some((speaker, opus)) = speaks.recv().await {
        out(frame_line(RemoteActionMessage::CallSaid(
            tilo_actions::remote::call::CallAudio { speaker, opus },
        )));
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
