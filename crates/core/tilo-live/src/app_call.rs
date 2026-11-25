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
//! say is handed to the worker as one stream, a frame every 20 ms, mixed when
//! several speak at once ([`crate::mix`]), and what the outside call says
//! is published on the bridge's own audio track, one voice at a time: a speaker
//! holds the track while they talk, and the next takes it when they pause
//! ([`FLOOR`]). Nothing the outside call said goes back to it.
//!
//! Which call is carried is the app's to say ([`AppLane`]); whether it may is the
//! device's daemon's. The app's first carry joins the call as the app, an action the daemon
//! admits under the app's grants ([`JoinCall`]); the daemon then holds the call's media keys and
//! seals and opens everything through the live session it holds, so a person a call denies is
//! neither heard nor listed, and nothing of a key reaches the app. The bridge's word, the people
//! it carries, is part of the device's one word in the workspace ([`crate::speaker::Speaker`]).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live::Track;
use crate::live_session::Heard;
use crate::mix::Mixer;
use crate::presence::{BridgedMember, Call, LiveCall};
use crate::speaker::Speaker;
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

/// How often occupancy and the worker's place are looked at again.
const TICK: Duration = Duration::from_secs(1);
/// How long an outside speaker keeps the bridge's voice after their last
/// frame: a pause this long hands it to whoever speaks next.
pub const FLOOR: Duration = Duration::from_millis(200);

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
    /// The device's one word in the workspace, said by its daemon on the live session it holds:
    /// the bridge says the people it carries through it, and never seals anything itself.
    speaker: Arc<Speaker>,
    /// This device: the bridge's place in the call.
    device: [u8; 32],
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
    voice: Voice,
    /// What the workspace's call says, as the live session opens it.
    ear: Option<mpsc::UnboundedReceiver<Heard>>,
    /// The worker's asks and the frames it hears.
    say: mpsc::UnboundedSender<(Option<String>, Option<String>)>,
    speak: mpsc::UnboundedSender<(String, Vec<u8>)>,
    calls: Option<watch::Receiver<CallsReport>>,
    audio: Option<broadcast::Receiver<OutsideAudio>>,
}

impl ConnectorCall {
    /// The bridge of the workspace `speaker` says this device's word in, taking part in its
    /// calls through the live session the device's daemon holds, and reaching the outside
    /// service `service` through `lane`.
    #[must_use]
    pub fn new(speaker: Arc<Speaker>, service: impl Into<String>, lane: Lane) -> Self {
        let (heard, ear) = mpsc::unbounded_channel();
        speaker.session().tap(
            Track::Audio,
            Arc::new(move |object: &Heard| heard.send(object.clone()).is_ok()),
        );
        let device = speaker.session().device();
        Self {
            speaker,
            device,
            service: service.into(),
            carried: None,
            in_tilo: false,
            heard_from: BTreeSet::new(),
            asked: None,
            report: CallsReport::default(),
            voice: Voice::new(),
            ear: Some(ear),
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
        let (Some(mut calls), Some(mut audio), Some(mut ear)) =
            (self.calls.take(), self.audio.take(), self.ear.take())
        else {
            return;
        };
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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
                heard = ear.recv() => match heard {
                    Some(heard) => self.hear(&heard),
                    None => break,
                },
                _ = tick.tick() => {
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

    /// One voice frame the workspace's call said: handed to the worker when it is one of the
    /// carried call's people, while the bridge is in it.
    fn hear(&self, heard: &Heard) {
        if heard.sender == self.device || heard.track != Track::Audio {
            return;
        }
        let Some(pair) = self.carried.as_ref().filter(|_| self.in_tilo) else {
            return;
        };
        let speaker = hex::encode(heard.sender);
        let live = self.speaker.calls();
        if !tilo_callers(&live, &pair.tilo)
            .iter()
            .any(|caller| caller.id == speaker)
        {
            return;
        }
        let Ok(object) = VoiceObject::decode(&heard.body) else {
            return;
        };
        // A worker that is not running missed the frame: live audio is never queued.
        let _ = self.speak.send((speaker, object.payload));
    }

    /// One frame an outside person said: the bridge says it in the Tilo call,
    /// naming the bridged participant it is theirs, while they hold its voice
    /// and the call lets them speak.
    fn carry_in(&mut self, frame: OutsideAudio) {
        let Some(pair) = self.carried.clone() else {
            return;
        };
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
        // The session holds the call's rule by device; a carried person is denied as the
        // stand-in they are here, which is no device of this call.
        if !self
            .speaker
            .session()
            .may(&pair.tilo, &member.person(self.device))
            .publish
        {
            return;
        }
        // The session holds the call's rule: what it lets through is what is heard.
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
        let _ = self
            .speaker
            .session()
            .publish(Track::Audio, sequence, &body);
    }

    /// Carry the pair who is in calls asks for, and let go of the rest.
    fn reconcile(&mut self, report: &CallsReport) {
        self.report.clone_from(report);
        let Some(pair) = self.carried.clone() else {
            return self.let_go();
        };
        let live = self.speaker.calls();
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
        self.in_tilo = joined;
        self.say_own();
    }

    /// Say the people the bridge carries into the Tilo call, as members of it of their own,
    /// while it is in it.
    fn say_own(&self) {
        let bridged = if self.in_tilo {
            self.bridged_members()
        } else {
            Vec::new()
        };
        self.speaker.set_bridged(bridged);
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
        self.in_tilo = false;
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

/// Join the call a [`CallPresence`] names, or leave the one joined (`None`), as the app the
/// lane is for: admitted, asked and recorded as any action of the app's is, by the device's
/// daemon. Answers the call's channel and the generation of its rule the media is sealed under,
/// or why the app may not carry it.
pub type JoinCall =
    Arc<dyn Fn(Option<CallPresence>) -> Result<Option<(String, u64)>, String> + Send + Sync>;

/// What a bridge needs to carry one call through the live session the device's daemon holds: the
/// device's one word in the workspace, and the way to join a call as the app. No key material is
/// here, and none reaches the app: the daemon seals and opens everything.
#[derive(Clone)]
pub struct CallSetup {
    /// The service the app reaches, in its own words.
    pub service: String,
    /// The device's word in the app's workspace, said on the live session the daemon holds.
    pub speaker: Arc<Speaker>,
    /// Join or leave a call of the workspace as the app.
    pub join: JoinCall,
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
    /// The carrier running now, if one is, and the call it joined.
    carried: std::sync::Mutex<Option<(tokio::task::JoinHandle<()>, String)>>,
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

    /// Carry `pair`, or let go of the one carried. The first carry joins the pair's call as the
    /// app; a carry naming another call while one is carried is refused.
    fn carry(&self, pair: Option<WireBridgedCall>) -> Result<(), ActionCallError> {
        let setup = self
            .granted
            .clone()
            .map_err(|reason| ActionCallError::Refused { reason })?;
        let mut running = lock(&self.carried);
        let Some(pair) = pair else {
            if let Some((carried, _)) = running.take() {
                carried.abort();
                // The app left: the device is in its call no longer, and neither is anyone it
                // carried.
                setup.speaker.set_bridged(Vec::new());
                (setup.join)(None).map_err(|reason| ActionCallError::Refused { reason })?;
            }
            let _ = self.calls.send(CallsReport::default());
            return Ok(());
        };
        if !pair.valid() {
            return Err(ActionCallError::InvalidRequest {
                reason: "a carried call names its channel here and the service's own".to_owned(),
            });
        }
        if let Some((_, joined)) = running.as_ref()
            && *joined != pair.tilo
        {
            return Err(ActionCallError::Refused {
                reason: format!(
                    "this app was admitted to the call in {joined}, not {}",
                    pair.tilo
                ),
            });
        }
        if running.is_none() {
            let joined = (setup.join)(Some(CallPresence {
                channel: pair.tilo.clone(),
                name: String::new(),
                bridge: Some(setup.service.clone()),
            }))
            .map_err(|reason| ActionCallError::Refused { reason })?;
            if joined.is_none_or(|(channel, _)| channel != pair.tilo) {
                return Err(ActionCallError::Refused {
                    reason: format!("the call in {} took this app in no call", pair.tilo),
                });
            }
        }
        let (say, says) = mpsc::unbounded_channel::<(Option<String>, Option<String>)>();
        let (speak, speaks) = mpsc::unbounded_channel::<(String, Vec<u8>)>();
        let lane = Lane {
            calls: self.calls.subscribe(),
            audio: self.audio.subscribe(),
            say,
            speak,
        };
        let mut bridge =
            ConnectorCall::new(Arc::clone(&setup.speaker), setup.service.clone(), lane);
        let tilo = pair.tilo.clone();
        bridge.carry(Some(BridgedCall {
            outside: pair.outside,
            tilo: pair.tilo,
        }));
        if let Some((carried, _)) = running.take() {
            carried.abort();
        }
        *running = Some((tokio::spawn(bridge.run()), tilo));
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

impl Drop for AppLane {
    /// An app whose connection closed carries nothing: the device leaves the call it joined.
    fn drop(&mut self) {
        let carried = lock(&self.carried).take();
        if let (Some((carried, _)), Ok(setup)) = (carried, &self.granted) {
            carried.abort();
            setup.speaker.set_bridged(Vec::new());
            let _ = (setup.join)(None);
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

/// The speaker a frame of several of the workspace's people speaking at once names: it is
/// theirs together, mixed into one.
pub const MIXED: &str = "mixed";

/// How often the app is handed the workspace's next frame: one Opus frame's length.
const SAID_EVERY: Duration = Duration::from_millis(20);

/// What the workspace's call said, written to the app as one stream, a frame every 20 ms: one
/// speaker's frames untouched and named as theirs, several speaking at once mixed into one frame
/// named [`MIXED`], so the outside call hears each of them instead of their frames interleaved.
/// Nothing ticks while nobody speaks.
async fn said_audio(out: Out, mut speaks: mpsc::UnboundedReceiver<(String, Vec<u8>)>) {
    let mut mixer = Mixer::default();
    loop {
        if !mixer.active() {
            let Some((speaker, opus)) = speaks.recv().await else {
                return;
            };
            mixer.push(&speaker, opus);
        }
        let mut every = tokio::time::interval(SAID_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while mixer.active() {
            tokio::select! {
                spoken = speaks.recv() => {
                    let Some((speaker, opus)) = spoken else { return };
                    mixer.push(&speaker, opus);
                }
                _ = every.tick() => {
                    if let Some((speaker, opus)) = mixer.tick_said() {
                        let speaker = speaker.unwrap_or_else(|| MIXED.to_owned());
                        out(frame_line(RemoteActionMessage::CallSaid(
                            tilo_actions::remote::call::CallAudio { speaker, opus },
                        )));
                    }
                }
            }
        }
    }
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
