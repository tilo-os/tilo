//! A connector's outside call carried into a workspace call: the worker is in
//! a voice channel there, and this device joins the workspace's call for it as
//! its bridge, never as a member.
//!
//! The bridge is one device of the workspace on its live session. It says a
//! call that is bridged to `service` (`discord`), so every call view shows the
//! call as bridged there and the bridge as nobody's face, and it carries each
//! person on the other side as a bridged participant of their own: a member of
//! the call, shown by their name there, marked as reached through its service
//! ([`crate::presence::BridgedMember`]). What the workspace's participants
//! say is handed to the worker frame by frame, and what the outside call says
//! is published on the bridge's own audio track, one voice at a time: a speaker
//! holds the track while they talk, and the next takes it when they pause
//! ([`FLOOR`]). Nothing the outside call said goes back to it.
//!
//! Which call is carried, and what it allows, are the host's: it retargets the
//! bridge with [`ConnectorCall::carry`], gives it the workspace's roster and
//! keys ([`ConnectorCall::set_roster`], [`ConnectorCall::set_keys`]) and the
//! rules of its calls ([`ConnectorCall::set_calls`], with the media keys of the
//! rule's generation in [`ConnectorCall::set_call_keys`]). The live session
//! holds the media to those rules itself, so a person a call denies is neither
//! heard nor listed, and a bridge a call denies speaks nothing.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capture::Session as CaptureSession;
use crate::live::Track;
use crate::presence::{Board, BridgedMember, Call, LiveCall, Own};
use crate::voice::{FRAME_SAMPLES, SAMPLE_RATE, VoiceObject};
use collab_types::presence::{Bridged, CallPresence};
use tilo_actions::remote::call::{
    BridgedCall as WireBridgedCall, CallReport as SaidReport, CallStatus, MAX_CALL_AUDIO_BYTES,
};
pub use tilo_actions::remote::call::{CallLink, OutsideCall, OutsidePerson};
use tilo_actions::remote::{ActionCallError, CallLanes, RemoteActionMessage, Take, frame_line};
use tokio::sync::{broadcast, mpsc, watch};

/// The service's calls and the worker's place in them, as it last said.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CallsReport {
    pub calls: Vec<OutsideCall>,
    pub link: CallLink,
}

/// One Opus frame a person said in the outside call the worker is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutsideAudio {
    /// Their account on the service.
    pub speaker: String,
    /// The 20 ms Opus frame, as the service said it.
    pub opus: Vec<u8>,
}

/// How often the live session is read and its presence said: half a voice
/// frame, so a frame waits at most that long either way.
const HEARING: Duration = Duration::from_millis(10);
/// How often occupancy and the worker's place are looked at again.
const TICK: Duration = Duration::from_secs(1);
/// How long an outside speaker keeps the bridge's voice after their last
/// frame: a pause this long hands it to whoever speaks next.
pub const FLOOR: Duration = Duration::from_millis(200);
/// The name the bridge's device shows in the workspace.
const BRIDGE_NAME: &str = "Bridge";

/// One outside call and the workspace call it is carried into: one call,
/// heard on both sides.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BridgedCall {
    /// The service's id of its call (its voice channel).
    pub outside: String,
    /// The workspace's voice channel id.
    pub tilo: String,
}

/// The connector worker a bridge carries a call to, and the frames each way.
///
/// The host wires these to the worker it runs: the reports and audio the
/// worker said, and the asks and frames it is to hear. A test wires the same
/// shape to a fake worker and a fake service.
pub struct Lane {
    /// The service's calls and the worker's place, as they change.
    pub calls: watch::Receiver<CallsReport>,
    /// The audio the outside call says, frame by frame.
    pub audio: broadcast::Receiver<OutsideAudio>,
    /// Ask the worker into an outside call (or out of it), showing `status`.
    pub say: mpsc::UnboundedSender<(Option<String>, Option<String>)>,
    /// Hand the worker one Opus frame a workspace participant said.
    pub speak: mpsc::UnboundedSender<(String, Vec<u8>)>,
}

/// Say `frames` through `session`: the daemon seals them as the device's presence.
fn say_presence(session: &dyn CaptureSession, frames: &[(u32, collab_types::presence::Presence)]) {
    for (sequence, presence) in frames {
        let Ok(body) = presence.encode() else {
            continue;
        };
        let _ = session.publish(Track::Presence, *sequence, &body);
    }
}

/// The outside speaker who holds the bridge's voice, and the bridge's audio
/// track as it stands.
struct Voice {
    holder: Option<(String, Instant)>,
    sequence: u32,
    timestamp: u64,
    started: Instant,
}

impl Voice {
    fn new() -> Self {
        Self {
            holder: None,
            sequence: 0,
            timestamp: 0,
            started: Instant::now(),
        }
    }

    /// The next object of the bridge's track for `opus` from `speaker`, or
    /// `None` while someone else holds it. Its timestamp follows the clock,
    /// so a receiver hears a pause as one. The object names `who`, the
    /// bridged participant the voice is theirs.
    fn take(
        &mut self,
        speaker: &str,
        who: [u8; 32],
        opus: Vec<u8>,
        now: Instant,
    ) -> Option<VoiceObject> {
        if let Some((holder, at)) = &self.holder
            && holder != speaker
            && now.saturating_duration_since(*at) < FLOOR
        {
            return None;
        }
        self.holder = Some((speaker.to_owned(), now));
        let elapsed = now.saturating_duration_since(self.started);
        let by_clock = u64::try_from(
            elapsed
                .as_micros()
                .saturating_mul(u128::from(SAMPLE_RATE))
                .checked_div(1_000_000)
                .unwrap_or_default(),
        )
        .unwrap_or(u64::MAX);
        let timestamp = if self.sequence == 0 {
            by_clock
        } else {
            by_clock.max(self.timestamp.saturating_add(u64::from(FRAME_SAMPLES)))
        };
        self.timestamp = timestamp;
        let object = VoiceObject {
            sequence: u64::from(self.sequence),
            timestamp,
            duration: FRAME_SAMPLES,
            speaker: Some(who),
            payload: opus,
        };
        self.sequence = self.sequence.wrapping_add(1);
        Some(object)
    }
}

/// A connector's outside call carried into one workspace's call.
pub struct ConnectorCall {
    /// The live session the device's daemon holds: the bridge takes part in the call through
    /// it, and never seals anything itself.
    session: Arc<dyn CaptureSession>,
    workspace: String,
    /// This device: the bridge's place in the call.
    device: [u8; 32],
    /// What the bridge's device calls itself.
    device_name: String,
    /// The person the bridge's device belongs to, if it belongs to one.
    person: Option<String>,
    /// The service the call is bridged to (`discord`).
    service: String,
    /// The pair carried now, whether the bridge is in the Tilo call, and the people on the
    /// other side whose voice is carried.
    carried: Option<BridgedCall>,
    in_tilo: bool,
    heard_from: BTreeSet<String>,
    /// What the worker was last asked: the outside call and its status.
    asked: Option<(Option<String>, Option<String>)>,
    /// The worker's latest word: who is in which outside call.
    report: CallsReport,
    /// What the bridge last said of itself, to say it again only when it moved.
    said: String,
    /// The bridge's own presence, and everyone's the session heard.
    board: Option<Board>,
    voice: Voice,
    /// The worker's asks and the frames it hears.
    say: mpsc::UnboundedSender<(Option<String>, Option<String>)>,
    speak: mpsc::UnboundedSender<(String, Vec<u8>)>,
    calls: Option<watch::Receiver<CallsReport>>,
    audio: Option<broadcast::Receiver<OutsideAudio>>,
}

impl ConnectorCall {
    /// The bridge of `workspace`, taking part in its calls through `session`, the live session
    /// the device's daemon holds, and reaching the outside service through `lane`.
    #[must_use]
    pub fn new(
        session: Arc<dyn CaptureSession>,
        workspace: impl Into<String>,
        device: [u8; 32],
        device_name: impl Into<String>,
        person: Option<String>,
        service: impl Into<String>,
        lane: Lane,
    ) -> Self {
        Self {
            session,
            workspace: workspace.into(),
            device,
            device_name: device_name.into(),
            person,
            service: service.into(),
            carried: None,
            in_tilo: false,
            heard_from: BTreeSet::new(),
            asked: None,
            report: CallsReport::default(),
            said: String::new(),
            board: None,
            voice: Voice::new(),
            say: lane.say,
            speak: lane.speak,
            calls: Some(lane.calls),
            audio: Some(lane.audio),
        }
    }

    /// Carry `pair` from now on, or none: the host sets this as the app says
    /// which channels are bridged.
    pub fn carry(&mut self, pair: Option<BridgedCall>) {
        self.carried = pair;
    }

    /// Carry the outside call for as long as this bridge runs; leaves both
    /// sides on the way out.
    pub async fn run(mut self) {
        let (Some(mut calls), Some(mut audio)) = (self.calls.take(), self.audio.take()) else {
            return;
        };
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut hearing = tokio::time::interval(HEARING);
        hearing.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let mut reconcile = false;
            tokio::select! {
                changed = calls.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    reconcile = true;
                }
                frame = audio.recv() => match frame {
                    Ok(frame) => self.carry_in(frame),
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = hearing.tick() => {
                    self.hear();
                }
                _ = tick.tick() => {
                    self.keep_taking();
                    reconcile = true;
                }
            }
            if reconcile {
                let report = calls.borrow_and_update().clone();
                self.reconcile(&report);
            }
        }
        self.let_go();
    }

    /// Say what the bridge owes: its own presence when it moved, and the frames that are due.
    fn keep_taking(&mut self) {
        if self.board.is_none() {
            let mut board = Board::new(self.workspace.clone(), self.device);
            board.say(&self.own());
            self.board = Some(board);
        }
        let now = now_ms();
        if let Some(frames) = self.board.as_mut().and_then(|board| board.due(now)) {
            say_presence(self.session.as_ref(), &frames);
        }
    }

    /// Read what the live session heard: presence into the board, and the
    /// carried call's voices to the worker. Say what the bridge owes.
    fn hear(&mut self) {
        let now = now_ms();
        let own_device = self.device;
        let mut voices = Vec::new();
        while let Some(heard) = self.session.try_recv() {
            if heard.sender == own_device {
                continue;
            }
            match heard.track {
                Track::Presence => {
                    if let Some(board) = &mut self.board {
                        board.heard(heard.sender, &heard.body, now);
                    }
                }
                Track::Audio => voices.push(heard),
                _ => {}
            }
        }
        if let Some(frames) = self.board.as_mut().and_then(|board| board.due(now)) {
            say_presence(self.session.as_ref(), &frames);
        }
        let live = self.board.as_ref().map(Board::calls).unwrap_or_default();
        // Only the carried call's people are handed to the worker, and only
        // while the bridge is in it.
        let speakers: BTreeSet<String> = match (&self.carried, self.in_tilo) {
            (Some(pair), true) => tilo_callers(&live, &pair.tilo)
                .into_iter()
                .map(|caller| caller.id.clone())
                .collect(),
            _ => BTreeSet::new(),
        };
        for heard in voices {
            let speaker = hex::encode(heard.sender);
            if !speakers.contains(&speaker) {
                continue;
            }
            let Ok(object) = VoiceObject::decode(&heard.body) else {
                continue;
            };
            if self.speak.send((speaker, object.payload)).is_err() {
                // The worker is not running: what it missed is gone.
                break;
            }
        }
    }

    /// One frame an outside person said: the bridge says it in the Tilo call,
    /// naming the bridged participant it is theirs, while they hold its voice
    /// and the call lets them speak.
    fn carry_in(&mut self, frame: OutsideAudio) {
        if self.carried.is_none() {
            return;
        }
        if !self.in_tilo || !self.heard_from.contains(&frame.speaker) {
            return;
        }
        let Some(member) = self
            .bridged_members()
            .into_iter()
            .find(|member| member.bridged.account == frame.speaker)
        else {
            return;
        };
        let who = member.publisher(self.device);
        let person = member.person(self.device);
        // The session holds the call's rule: what it lets through is what is heard.
        let _ = person;
        let Some(object) = self
            .voice
            .take(&frame.speaker, who, frame.opus, Instant::now())
        else {
            return;
        };
        let Ok(body) = object.encode() else {
            return;
        };
        let sequence = u32::try_from(object.sequence).unwrap_or(u32::MAX);
        let _ = self.session.publish(Track::Audio, sequence, &body);
    }

    /// Carry the pair who is in calls asks for, and let go of the rest.
    fn reconcile(&mut self, report: &CallsReport) {
        self.report.clone_from(report);
        let Some(pair) = self.carried.clone() else {
            return self.let_go();
        };
        let live = self.board.as_ref().map(Board::calls).unwrap_or_default();
        let outside = outside_people(report, &pair.outside);
        let tilo = tilo_callers(&live, &pair.tilo);
        if outside.is_empty() && tilo.is_empty() {
            return self.let_go();
        }
        self.heard_from = outside
            .iter()
            .filter(|person| !person.muted)
            .map(|person| person.account.clone())
            .collect();
        let status = status(&tilo);
        self.ask(Some(pair.outside.clone()), Some(status));
        // Nothing crosses until the worker is in the outside call.
        let joined = matches!(&report.link, CallLink::Joined { call } if *call == pair.outside);
        self.set_in_tilo(joined);
        self.say_own();
    }

    /// Be in the carried pair's Tilo call, or not: what the bridge's presence
    /// says, and so what every call view shows.
    fn set_in_tilo(&mut self, in_tilo: bool) {
        if self.in_tilo == in_tilo {
            return;
        }
        self.in_tilo = in_tilo;
        self.say_own();
    }

    /// Say the bridge's presence again if what it says moved: the call it is
    /// in, or the people it carries there.
    fn say_own(&mut self) {
        let own = self.own();
        let text = format!("{own:?}");
        if text == self.said {
            return;
        }
        self.said = text;
        if let Some(board) = &mut self.board {
            board.say(&own);
        }
    }

    /// What the bridge says of itself: its name, the call it bridges, and the
    /// people it carries there.
    fn own(&self) -> Own {
        let bridged = if self.in_tilo {
            self.bridged_members()
        } else {
            Vec::new()
        };
        Own {
            label: BRIDGE_NAME.to_owned(),
            device: self.device_name.clone(),
            user: self.person.clone(),
            call: self
                .carried
                .as_ref()
                .filter(|_| self.in_tilo)
                .map(|pair| Call {
                    call: CallPresence {
                        channel: pair.tilo.clone(),
                        name: String::new(),
                        bridge: Some(self.service.clone()),
                    },
                    camera: false,
                    screen: false,
                    muted: false,
                    deafened: false,
                }),
            bridged,
            ..Own::default()
        }
    }

    /// The people on the other side, as the bridge carries them into the Tilo
    /// call: each a member of it of their own, by the name the service shows.
    fn bridged_members(&self) -> Vec<BridgedMember> {
        let Some(pair) = &self.carried else {
            return Vec::new();
        };
        outside_people(&self.report, &pair.outside)
            .into_iter()
            .map(|person| BridgedMember {
                session: session_of(&self.service, &person.account),
                name: person.name.clone(),
                bridged: Bridged {
                    service: self.service.clone(),
                    account: person.account.clone(),
                    name: person.name.clone(),
                    // The picture's hash, when the bridge kept it here: a call view draws what
                    // the workspace holds, never a URL only the service reaches.
                    avatar: person.avatar.clone(),
                    linked: None,
                },
                call: Call {
                    call: CallPresence {
                        channel: pair.tilo.clone(),
                        name: String::new(),
                        bridge: Some(self.service.clone()),
                    },
                    camera: false,
                    screen: false,
                    // What the service says of them: a muted or deafened outside person is shown
                    // as such in the call, as anyone here would be.
                    muted: person.muted || person.deafened,
                    deafened: person.deafened,
                },
            })
            .collect()
    }

    /// Ask the worker into `call` showing `status`, unless that is what it was
    /// last asked.
    fn ask(&mut self, call: Option<String>, status: Option<String>) {
        let wanted = (call, status);
        if self.asked.as_ref() == Some(&wanted) {
            return;
        }
        let _ = self.say.send(wanted.clone());
        self.asked = Some(wanted);
    }

    /// Carry no call: leave both sides.
    fn let_go(&mut self) {
        self.set_in_tilo(false);
        self.heard_from.clear();
        self.say_own();
        self.ask(None, None);
    }
}

/// The name a bridged participant's stand-in is keyed by, stable for as long as
/// they are on the service.
fn session_of(service: &str, account: &str) -> String {
    format!("{service}:{account}")
}

/// Who is in outside call `call`, unmuted or not, by account.
fn outside_people<'a>(report: &'a CallsReport, call: &str) -> Vec<&'a OutsidePerson> {
    report
        .calls
        .iter()
        .filter(|outside| outside.call == call)
        .flat_map(|outside| &outside.people)
        .collect()
}

/// The devices in the Tilo call of voice channel `channel`, bridges and the
/// people they carry left out.
fn tilo_callers<'a>(live: &'a [LiveCall], channel: &str) -> Vec<&'a crate::presence::LiveCaller> {
    live.iter()
        .filter(|call| call.channel == channel)
        .flat_map(|call| &call.people)
        .filter(|caller| caller.bridge.is_none() && caller.bridged.is_none())
        .collect()
}

/// Who is on the Tilo side, as a line for the service.
fn status(callers: &[&crate::presence::LiveCaller]) -> String {
    let mut names: Vec<String> = callers
        .iter()
        .map(|caller| caller.label.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    names.dedup();
    if names.is_empty() {
        "Nobody on Tilo yet".to_owned()
    } else {
        format!("On Tilo: {}", names.join(", "))
    }
}

/// The wall clock, in milliseconds, as presence is stamped.
#[expect(
    clippy::cast_precision_loss,
    reason = "milliseconds since the epoch are exact in an f64"
)]
fn now_ms() -> f64 {
    tilo_clock::unix_now_ms() as f64
}

/// What a bridge needs to carry one call through the session the device's daemon holds: which
/// call it was admitted to, and the session itself. No key material is here, and none reaches
/// the app: the daemon seals and opens everything through `session`.
#[derive(Clone)]
pub struct CallSetup {
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
    /// The call of the workspace this app was admitted to carry, and its rule's generation. A
    /// pair that names another channel is refused.
    pub channel: String,
    pub generation: u64,
    /// The live session the device's daemon holds, which the bridge publishes and hears
    /// through.
    pub session: Arc<dyn CaptureSession>,
}

/// How lines reach the app that spoke them.
pub type Out = Arc<dyn Fn(String) + Send + Sync>;

/// One app's call lane on its held connection.
pub struct AppLane {
    /// Whether the workspace admitted this app to a call; `Err` is the refusal it gets instead.
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
    fn carry(&self, pair: Option<WireBridgedCall>) -> Result<(), ActionCallError> {
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
        if setup.channel != pair.tilo {
            return Err(ActionCallError::Refused {
                reason: format!(
                    "this app was admitted to the call in {}, not {}",
                    setup.channel, pair.tilo
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
            Arc::clone(&setup.session),
            setup.workspace.clone(),
            setup.device,
            setup.device_name.clone(),
            setup.person.clone(),
            setup.service.clone(),
            lane,
        );
        bridge.carry(Some(BridgedCall {
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
        let _ = self.calls.send(CallsReport {
            calls,
            link: report.link,
        });
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
