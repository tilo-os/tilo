//! A connector's outside call carried into a workspace call: the worker is in
//! a voice channel there, and this device joins the workspace's call for it as
//! its bridge, never as a member.
//!
//! The bridge is one device of the workspace on its live session. It says a
//! call that is bridged to `service` (`discord`), so every call view shows the
//! call as bridged there and the bridge as nobody's face, and it carries each
//! person on the other side as a bridged participant of their own: a member of
//! the call, shown by their name there, marked as reached through its service
//! ([`tilo_live::presence::BridgedMember`]). What the workspace's participants
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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use collab_types::presence::{Bridged, CallPresence};
use tilo_live::calls::{CallRule, Directions};
use tilo_live::frame::LiveKeys;
use tilo_live::live::Track;
use tilo_live::live_session::{Join, LiveSession};
use tilo_live::presence::{self, Board, BridgedMember, Call, LiveCall, Own};
use tilo_live::relay::Relay;
use tilo_live::roster::Roster;
use tilo_live::scope::Scope;
use tilo_live::voice::{FRAME_SAMPLES, SAMPLE_RATE, VoiceObject};
use tokio::sync::{broadcast, mpsc, watch};

pub use crate::process::{CallsReport, ConnectorProcess, OutsideAudio};
use crate::protocol::CallLink;

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

impl Lane {
    /// The lane of a worker this device runs, whose asks and frames are
    /// handed over as they come.
    #[must_use]
    pub fn of(process: &Arc<ConnectorProcess>) -> Self {
        let (say, mut says) = mpsc::unbounded_channel();
        let (speak, mut speaks) = mpsc::unbounded_channel();
        let asks = Arc::clone(process);
        tokio::spawn(async move {
            while let Some((call, status)) = says.recv().await {
                let _ = asks.set_call(call, status).await;
            }
        });
        let speaks_for = Arc::clone(process);
        tokio::spawn(async move {
            while let Some((speaker, opus)) = speaks.recv().await {
                let _ = speaks_for.send_audio(speaker, opus).await;
            }
        });
        Self {
            calls: process.calls(),
            audio: process.audio(),
            say,
            speak,
        }
    }
}

/// This device's live session and the board it reads everyone's presence on.
struct Taking {
    live: LiveSession,
    board: Board,
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
    relay: Relay,
    workspace: String,
    /// This device: the bridge's place in the call.
    device: [u8; 32],
    /// What the bridge's device calls itself.
    device_name: String,
    /// The person the bridge's device belongs to, if it belongs to one.
    person: Option<String>,
    /// The service the call is bridged to (`discord`).
    service: String,
    roster: Roster,
    keys: Option<LiveKeys>,
    rules: BTreeMap<String, CallRule>,
    persons: BTreeMap<[u8; 32], String>,
    /// The call media keys of the generation the host handed over.
    call_keys: Option<(String, u64, LiveKeys)>,
    /// The pair carried now, whether the bridge is in the Tilo call, and the
    /// people on the other side whose voice is carried.
    carried: Option<BridgedCall>,
    in_tilo: bool,
    heard_from: BTreeSet<String>,
    /// What the worker was last asked: the outside call and its status.
    asked: Option<(Option<String>, Option<String>)>,
    /// The worker's latest word: who is in which outside call.
    report: CallsReport,
    /// What the bridge last said of itself, to say it again only when it moved.
    said: String,
    taking: Option<Taking>,
    voice: Voice,
    /// The worker's asks and the frames it hears.
    say: mpsc::UnboundedSender<(Option<String>, Option<String>)>,
    speak: mpsc::UnboundedSender<(String, Vec<u8>)>,
    calls: Option<watch::Receiver<CallsReport>>,
    audio: Option<broadcast::Receiver<OutsideAudio>>,
}

impl ConnectorCall {
    /// The bridge of `workspace`, taking part in its calls on `relay` as
    /// `device` and reaching the outside service through `lane`.
    #[must_use]
    pub fn new(
        relay: Relay,
        workspace: impl Into<String>,
        device: [u8; 32],
        device_name: impl Into<String>,
        person: Option<String>,
        service: impl Into<String>,
        lane: Lane,
    ) -> Self {
        Self {
            relay,
            workspace: workspace.into(),
            device,
            device_name: device_name.into(),
            person,
            service: service.into(),
            roster: Roster::default(),
            keys: None,
            rules: BTreeMap::new(),
            persons: BTreeMap::new(),
            call_keys: None,
            carried: None,
            in_tilo: false,
            heard_from: BTreeSet::new(),
            asked: None,
            report: CallsReport::default(),
            said: String::new(),
            taking: None,
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

    /// Who may send in the workspace's live session now, the bridge among
    /// them.
    pub fn set_roster(&mut self, roster: Roster) {
        self.roster = roster.clone();
        if let Some(taking) = &self.taking {
            taking.live.set_roster(roster);
        }
    }

    /// The workspace's keys, which seal its presence.
    pub fn set_keys(&mut self, keys: impl Into<LiveKeys>) {
        let keys = keys.into();
        if let Some(taking) = &self.taking {
            taking.live.set_keys(keys.clone());
        }
        self.keys = Some(keys);
    }

    /// What the workspace's calls allow, by channel, and the person each
    /// device is.
    pub fn set_calls(
        &mut self,
        rules: BTreeMap<String, CallRule>,
        persons: BTreeMap<[u8; 32], String>,
    ) {
        if let Some(taking) = &self.taking {
            taking.live.set_calls(rules.clone(), persons.clone());
        }
        self.rules = rules;
        self.persons = persons;
    }

    /// Seal and open the media of `channel`'s call under the keys of the
    /// rule's `generation`.
    pub fn set_call_keys(&mut self, channel: &str, generation: u64, keys: impl Into<LiveKeys>) {
        let keys = keys.into();
        if let Some(taking) = &self.taking {
            taking.live.set_call_keys(channel, generation, keys.clone());
        }
        self.call_keys = Some((channel.to_owned(), generation, keys));
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

    /// Join the workspace's live session once its keys are here, and say what
    /// the bridge owes.
    fn keep_taking(&mut self) {
        if let Some(taking) = &self.taking
            && taking.live.ended()
        {
            self.taking = None;
        }
        if self.taking.is_none() {
            let Some(keys) = self.keys.clone() else {
                return;
            };
            let join = Join {
                scope: Scope::workspace(self.workspace.clone()),
                device: self.device,
                keys,
                roster: self.roster.clone(),
                publish: true,
                presence: true,
            };
            if let Ok(live) = LiveSession::local(&self.relay, &join) {
                live.set_calls(self.rules.clone(), self.persons.clone());
                if let Some((channel, generation, keys)) = &self.call_keys {
                    live.set_call_keys(channel, *generation, keys.clone());
                }
                let mut board = Board::new(self.workspace.clone(), self.device);
                board.say(&self.own());
                self.taking = Some(Taking { live, board });
            }
        }
        if let Some(taking) = &mut self.taking {
            let now = now_ms();
            if let Some(frames) = taking.board.due(now) {
                let _ = presence::say(&taking.live, &frames);
            }
        }
    }

    /// Read what the live session heard: presence into the board, and the
    /// carried call's voices to the worker. Say what the bridge owes.
    fn hear(&mut self) {
        let now = now_ms();
        let own_device = self.device;
        let mut voices = Vec::new();
        if let Some(taking) = &mut self.taking {
            while let Some(heard) = taking.live.try_recv() {
                if heard.sender == own_device {
                    continue;
                }
                match heard.track {
                    Track::Presence => {
                        taking.board.heard(heard.sender, &heard.body, now);
                    }
                    Track::Audio => voices.push(heard),
                    _ => {}
                }
            }
            if let Some(frames) = taking.board.due(now) {
                let _ = presence::say(&taking.live, &frames);
            }
        }
        let live = self
            .taking
            .as_ref()
            .map(|taking| taking.board.calls())
            .unwrap_or_default();
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
        let person = member.person(self.device);
        let may = self
            .rules
            .get(&pair.tilo)
            .map_or(Directions::BOTH, |rule| rule.may(Some(&person)));
        if !may.publish {
            return;
        }
        let Some(taking) = &self.taking else {
            return;
        };
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
        let _ = taking.live.publish(Track::Audio, sequence, &body);
    }

    /// Carry the pair who is in calls asks for, and let go of the rest.
    fn reconcile(&mut self, report: &CallsReport) {
        self.report.clone_from(report);
        let Some(pair) = self.carried.clone() else {
            return self.let_go();
        };
        let live = self
            .taking
            .as_ref()
            .map(|taking| taking.board.calls())
            .unwrap_or_default();
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
        if let Some(taking) = &mut self.taking {
            taking.board.say(&own);
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
fn outside_people<'a>(
    report: &'a CallsReport,
    call: &str,
) -> Vec<&'a crate::protocol::OutsidePerson> {
    report
        .calls
        .iter()
        .filter(|outside| outside.call == call)
        .flat_map(|outside| &outside.people)
        .collect()
}

/// The devices in the Tilo call of voice channel `channel`, bridges and the
/// people they carry left out.
fn tilo_callers<'a>(
    live: &'a [LiveCall],
    channel: &str,
) -> Vec<&'a tilo_live::presence::LiveCaller> {
    live.iter()
        .filter(|call| call.channel == channel)
        .flat_map(|call| &call.people)
        .filter(|caller| caller.bridge.is_none() && caller.bridged.is_none())
        .collect()
}

/// Who is on the Tilo side, as a line for the service.
fn status(callers: &[&tilo_live::presence::LiveCaller]) -> String {
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_precision_loss,
        clippy::arithmetic_side_effects,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use key_group::{ContentKeys, KeyGroup, MemberKey};
    use tilo_live::live::Track;
    use tilo_live::live_session::{Join, LiveSession};
    use tilo_live::presence::{Board, Call, Own};
    use tilo_live::relay::{Admission, Denied, Peer, Relay, RelayAuthority};
    use tilo_live::roster::Roster;
    use tilo_live::scope::Scope;
    use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};
    use tokio::sync::{broadcast, mpsc, watch};

    use super::{BridgedCall, ConnectorCall, Lane};
    use crate::process::{CallsReport, OutsideAudio};
    use crate::protocol::{CallLink, OutsideCall, OutsidePerson};

    const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
    const CALL: &str = "voice-tl";
    const BRIDGE: [u8; 32] = [7; 32];
    const GRACE: [u8; 32] = [9; 32];

    fn keys() -> ContentKeys {
        let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
        let owner = MemberKey::generate("owner").unwrap();
        KeyGroup::create(workspace, owner)
            .unwrap()
            .content_keys()
            .unwrap()
    }

    fn relay() -> Relay {
        struct Nobody;
        impl RelayAuthority for Nobody {
            fn admit<'a>(&'a self, _peer: Peer, _proof: &'a [u8]) -> Admission<'a> {
                Box::pin(std::future::ready(Err(Denied)))
            }
        }
        Relay::new(Arc::new(Nobody))
    }

    struct Member {
        live: LiveSession,
        board: Board,
        voices: Vec<(Option<[u8; 32]>, Vec<u8>)>,
        sequence: u32,
    }

    impl Member {
        fn new(relay: &Relay, device: [u8; 32], keys: &ContentKeys, roster: &Roster) -> Self {
            let live = LiveSession::local(
                relay,
                &Join {
                    scope: Scope::workspace(WORKSPACE),
                    device,
                    keys: keys.clone().into(),
                    roster: roster.clone(),
                    publish: true,
                    presence: true,
                },
            )
            .unwrap();
            let mut board = Board::new(WORKSPACE, device);
            board.say(&Own {
                label: "Grace".into(),
                device: "laptop".into(),
                ..Own::default()
            });
            Self {
                live,
                board,
                voices: Vec::new(),
                sequence: 0,
            }
        }

        fn pump(&mut self) {
            let now = tilo_clock::unix_now_ms() as f64;
            while let Some(heard) = self.live.try_recv() {
                match heard.track {
                    Track::Presence => {
                        self.board.heard(heard.sender, &heard.body, now);
                    }
                    Track::Audio => {
                        if let Ok(object) = VoiceObject::decode(&heard.body) {
                            self.voices.push((object.speaker, object.payload));
                        }
                    }
                    _ => {}
                }
            }
            if let Some(frames) = self.board.due(now) {
                let _ = tilo_live::presence::say(&self.live, &frames);
            }
        }

        fn in_call(&mut self) {
            self.board.say(&Own {
                label: "Grace".into(),
                device: "laptop".into(),
                call: Some(Call {
                    call: collab_types::presence::CallPresence {
                        channel: CALL.into(),
                        name: "voice".into(),
                        bridge: None,
                    },
                    camera: false,
                    screen: false,
                    muted: false,
                    deafened: false,
                }),
                ..Own::default()
            });
        }

        fn speak(&mut self, opus: &[u8]) {
            let object = VoiceObject {
                sequence: u64::from(self.sequence),
                timestamp: u64::from(self.sequence) * u64::from(FRAME_SAMPLES),
                duration: FRAME_SAMPLES,
                speaker: None,
                payload: opus.to_vec(),
            };
            self.live
                .publish(Track::Audio, self.sequence, &object.encode().unwrap())
                .unwrap();
            self.sequence = self.sequence.wrapping_add(1);
        }
    }

    /// A worker in an outside call: Dana is there, unmuted, and the worker is
    /// joined. The bridge's lane is the test's.
    fn joined(channel: &str) -> CallsReport {
        CallsReport {
            calls: vec![OutsideCall {
                call: channel.into(),
                people: vec![OutsidePerson {
                    account: "7001".into(),
                    name: "Dana".into(),
                    ..OutsidePerson::default()
                }],
            }],
            link: CallLink::Joined {
                call: channel.into(),
            },
        }
    }

    /// Poll `check`, pumping `grace`, until it answers.
    async fn until<T>(
        grace: &mut Member,
        what: &str,
        mut check: impl FnMut(&Member) -> Option<T>,
    ) -> T {
        let deadline = tokio::time::Instant::from_std(tilo_test_env::hang_deadline());
        loop {
            grace.pump();
            if let Some(found) = check(grace) {
                return found;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{what} did not happen"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// The two directions of a bridged call on the Tilo stack alone: an
    /// outside frame is published as the bridged person's voice, and a
    /// participant's voice is handed to the worker, while the bridge is in no
    /// call and carries nobody.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_bridged_voice_carries_both_ways_on_the_live_session() {
        let relay = relay();
        let keys = keys();
        let roster = Roster::new([GRACE, BRIDGE]);
        let (say, mut says) = mpsc::unbounded_channel();
        let (speak, mut speaks) = mpsc::unbounded_channel();
        let (calls, calls_rx) = watch::channel(CallsReport::default());
        let (audio, audio_rx) = broadcast::channel(256);
        let mut bridge = ConnectorCall::new(
            relay.clone(),
            WORKSPACE,
            BRIDGE,
            "home-server",
            Some("aa".repeat(32)),
            "discord",
            Lane {
                calls: calls_rx,
                audio: audio_rx,
                say,
                speak,
            },
        );
        bridge.set_roster(roster.clone());
        bridge.set_keys(keys.clone());
        bridge.set_calls(BTreeMap::new(), BTreeMap::new());
        bridge.set_call_keys(CALL, 0, keys.clone());
        bridge.carry(Some(BridgedCall {
            outside: "voice".into(),
            tilo: CALL.into(),
        }));
        let carrier = tokio::spawn(bridge.run());

        let mut grace = Member::new(&relay, GRACE, &keys, &roster);
        grace.live.set_calls(BTreeMap::new(), BTreeMap::new());
        grace.live.set_call_keys(CALL, 0, keys.clone());
        grace.in_call();

        // The worker joins the outside call: the bridge joins Tilo's.
        calls.send(joined("voice")).unwrap();
        until(&mut grace, "the bridge carries the call", |grace| {
            grace
                .board
                .calls()
                .iter()
                .find(|call| call.channel == CALL)
                .and_then(|call| call.people.iter().find(|person| person.bridge.is_some()))
                .map(|_| ())
        })
        .await;
        let asked = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some((call, _)) = says.recv().await
                    && call.as_deref() == Some("voice")
                {
                    return;
                }
            }
        })
        .await;
        assert!(asked.is_ok(), "the worker was asked into the outside call");

        // Dana says a frame: it is published as her own voice, not the bridge's.
        let dana = collab_types::presence::stand_in(BRIDGE, "discord:7001");
        audio
            .send(OutsideAudio {
                speaker: "7001".into(),
                opus: vec![7; 40],
            })
            .unwrap();
        let heard = until(&mut grace, "Dana's voice reaches Tilo", |grace| {
            grace
                .voices
                .iter()
                .find(|(who, _)| *who == Some(dana))
                .map(|(_, payload)| payload.clone())
        })
        .await;
        assert_eq!(heard, vec![7; 40]);

        // Grace says a frame: it is handed to the worker, under her device.
        grace.speak(&[9; 40]);
        let (who, opus) = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(frame) = speaks.recv().await {
                    return frame;
                }
            }
        })
        .await
        .expect("Grace's voice reaches the worker");
        assert_eq!(who, hex::encode(GRACE));
        assert_eq!(opus, vec![9; 40]);

        // The worker leaves: the bridge says it is in no call and carries nobody.
        calls
            .send(CallsReport {
                calls: Vec::new(),
                link: CallLink::Idle,
            })
            .unwrap();
        until(&mut grace, "the bridge leaves the call", |grace| {
            grace
                .board
                .calls()
                .iter()
                .all(|call| call.channel != CALL)
                .then_some(())
        })
        .await;
        carrier.abort();
    }
}
