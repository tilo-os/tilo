//! The system's calls an instance's views take part in: who is in the call held on each resource,
//! as the shell's call service says on the instance's own connection, and the steps the person
//! takes in one (join, leave, mute, deafen, camera, screen), carried out through that service.
//! The pictures of the call's cameras and shared screens are collected here too, for the window
//! that draws them: the views' tiles ask for them, and the service sends the layers they show and
//! nothing else. A page whose tiles stand in its own HTML decodes the same stream where they
//! stand (`pictures_local`): nothing of it is collected here.
//!
//! The service is the shell's own voice service (`CollabRequest::NativeMediaCommand`): it
//! binds every command to the connection's own app, workspace and record, asks for the
//! microphone, camera and screen through the system's prompts, and holds the call on the
//! workspace's live session. Nothing of a call is stored in the app's record. One watch is
//! outstanding while the instance runs, so the people of every call of the record show the moment
//! someone joins, leaves, talks or mutes, joined or not.

use std::collections::{BTreeMap, HashMap};

use collaboration::{CollabEvent, CollabRequest};
use tilo_render_protocol::patch::{CallPicture, CallPictureSource, CallShown, ToUi};
use tilo_ui_base::permissions::MemberId;
use tilolang_tilo::instance::Instance;
use tilolang_tilo::runtime::platform::call_place;
use tilolang_tilo::runtime::value::ResourceRef;
use tilolang_tilo::runtime::{Actor, Bridged, CallInput, CallMember, CallStep};
use ui_core::native_media::{
    Command, Issue, PICTURES_LOST, PictureLayer, PictureSource, REPLY_LOST, Request, Shown,
    Snapshot, State, WATCH_LOST,
};
use ui_core::time::{Duration, Instant};

/// What an instance knows of the system's calls, and what it asked of them.
pub struct Calls {
    /// The person, by the id the service names people by.
    me: String,
    /// The next request id on the connection.
    next_request: u64,
    /// The last intent sent: the service takes a step only with a later one than any before.
    intent: u64,
    /// The steps in flight, by request id, and when each was sent.
    pending: BTreeMap<u64, Instant>,
    /// The one watch outstanding, and when it was sent.
    watch: Option<(u64, Instant)>,
    /// No watch before this: the service just refused one.
    watch_after: Option<Instant>,
    /// What the service said last.
    snapshot: Option<Snapshot>,
    /// The call's place this device joined, while it is in one.
    joined: Option<String>,
    /// What waits to be sent.
    queued: Vec<CollabRequest>,
    /// Why the service refused the last step, to say once.
    refused: Option<String>,
    /// Whether the instance is to be told who is where again.
    changed: bool,
    /// The pictures this device's view shows, as the call was last told: the people on camera and
    /// the screens shared in the call this device joined, at the layer their tiles take.
    shown: Vec<Shown>,
    /// The pictures the call delivered since the window was last told, oldest first.
    for_window: Vec<CallPicture>,
    /// The collection outstanding: its request id and when it was sent. One is outstanding while
    /// anything is shown, and the next is asked as each answer lands.
    pictures: Option<(u64, Instant)>,
    /// Where the next collection starts: past every picture this instance received.
    cursor: u64,
    /// What the window was last told the view shows, so it is told only when that changes.
    window_shown: Option<Vec<CallShown>>,
    /// The microphones the call service last offered, and the one it sends from: what the view's
    /// controls offer, told to the instance only when they change.
    inputs: Vec<CallInput>,
}

impl Calls {
    /// The calls of the person `me`.
    #[must_use]
    pub fn new(me: &str) -> Self {
        Self {
            me: me.to_owned(),
            next_request: 1,
            intent: 0,
            pending: BTreeMap::new(),
            watch: None,
            watch_after: None,
            snapshot: None,
            joined: None,
            queued: Vec::new(),
            refused: None,
            changed: false,
            shown: Vec::new(),
            for_window: Vec::new(),
            pictures: None,
            cursor: 0,
            window_shown: Some(Vec::new()),
            inputs: Vec::new(),
        }
    }

    /// Carry out the person's `step` in the call held on `target`.
    pub fn step(&mut self, target: &ResourceRef, step: CallStep) {
        let place = call_place(target);
        let command = match step {
            CallStep::Join => {
                self.joined = Some(place.clone());
                Command::Join { channel_id: place }
            }
            CallStep::Leave => {
                if self.joined.as_deref() != Some(place.as_str()) {
                    return;
                }
                self.joined = None;
                Command::Leave
            }
            // A switch is the call's this device is in: one pressed on another call's view says
            // nothing to it.
            CallStep::Mute(on) if self.in_call(&place) => Command::SetMuted { muted: on },
            CallStep::Deafen(on) if self.in_call(&place) => Command::SetDeafened { deafened: on },
            CallStep::Camera(on) if self.in_call(&place) => Command::SetCamera { on },
            CallStep::Screen(on) if self.in_call(&place) => Command::SetScreen { on },
            // One step chooses a microphone or a camera: the call said which its id is.
            CallStep::SelectInput(input_id) if self.in_call(&place) => {
                let camera = self.snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.cameras.iter().any(|camera| camera.id == input_id)
                });
                if camera {
                    Command::SelectCamera {
                        camera_id: input_id,
                    }
                } else {
                    Command::SelectInput { input_id }
                }
            }
            // How loud a member plays here changes nothing of the call, so it carries no intent:
            // a newer one replaces the one before at the service.
            CallStep::Volume { member, percent } if self.in_call(&place) => {
                self.changed = true;
                let request_id = self.request_id();
                self.pending.insert(request_id, ui_core::time::now());
                self.queued.push(CollabRequest::NativeMediaCommand {
                    request_id,
                    command: Request::new(Command::SetVolume { member, percent }),
                });
                return;
            }
            CallStep::Mute(_)
            | CallStep::Deafen(_)
            | CallStep::Camera(_)
            | CallStep::Screen(_)
            | CallStep::SelectInput(_)
            | CallStep::Volume { .. } => {
                return;
            }
        };
        self.intent = self.intent.saturating_add(1);
        let request_id = self.request_id();
        self.pending.insert(request_id, ui_core::time::now());
        self.queued.push(CollabRequest::NativeMediaCommand {
            request_id,
            command: Request {
                version: 1,
                intent: self.intent,
                command,
            },
        });
        self.changed = true;
    }

    /// Whether this device joined a call, or asked to.
    #[must_use]
    pub const fn joined(&self) -> bool {
        self.joined.is_some()
    }

    /// Whether this device is in the call held on `place`.
    fn in_call(&self, place: &str) -> bool {
        self.joined.as_deref() == Some(place)
    }

    fn request_id(&mut self) -> u64 {
        let id = self.next_request;
        self.next_request = self.next_request.saturating_add(1);
        id
    }

    /// Take `event` when it is the service's answer to one of this instance's requests. Whether it
    /// was.
    pub fn take(&mut self, event: &CollabEvent) -> bool {
        let CollabEvent::NativeMediaCommandResult { request_id, result } = event else {
            return false;
        };
        if self.pictures.is_some_and(|(id, _)| id == *request_id) {
            self.pictures = None;
            if let Ok(snapshot) = result {
                self.cursor = self.cursor.max(snapshot.pictures_cursor);
                for picture in &snapshot.pictures {
                    self.for_window.push(received(picture));
                }
                if !snapshot.pictures.is_empty() {
                    self.changed = true;
                }
            }
            return true;
        }
        let watched = self.watch.is_some_and(|(id, _)| id == *request_id);
        if watched {
            self.watch = None;
        } else if self.pending.remove(request_id).is_none() {
            return false;
        }
        match result {
            Ok(snapshot) => {
                if let Some(issue) = snapshot.issue
                    && !watched
                {
                    self.refused = Some(said(issue).to_owned());
                }
                if matches!(
                    snapshot.state,
                    State::Ended | State::Unavailable | State::Idle
                ) && self.pending.is_empty()
                {
                    // The call this device was in is over: its controls show it so.
                    self.joined = None;
                }
                self.snapshot = Some(snapshot.clone());
            }
            Err(why) => {
                tracing::debug!(%why, "the call service refused a request");
                if watched {
                    // Not before a while: a service that refuses a watch refuses the next one too.
                    self.watch_after = ui_core::time::now().checked_add(Duration::from_secs(5));
                } else {
                    self.refused = Some("The call could not do that just now.".to_owned());
                    if self.pending.is_empty() {
                        self.joined = None;
                    }
                }
            }
        }
        self.changed = true;
        true
    }

    /// When the host is to wake for the calls with nothing arriving: the outstanding watch or a
    /// step given up on, or the refused watch asked again.
    #[must_use]
    pub fn wakes_in(&self) -> Option<std::time::Duration> {
        let now = ui_core::time::now();
        let lost = self
            .pictures
            .and_then(|(_, sent)| sent.checked_add(PICTURES_LOST))
            .into_iter()
            .chain(
                self.watch
                    .and_then(|(_, sent)| sent.checked_add(WATCH_LOST)),
            )
            .chain(
                self.pending
                    .values()
                    .filter_map(|sent| sent.checked_add(REPLY_LOST)),
            )
            .chain(self.watch_after)
            .min()?;
        Some(lost.saturating_duration_since(now))
    }

    /// Send what waits through `send`, and keep a watch outstanding: who is in each call of the
    /// record shows as it moves.
    pub fn say(&mut self, mut send: impl FnMut(CollabRequest) -> bool) {
        let now = ui_core::time::now();
        // A step whose answer never came is given up on, so the controls show what is.
        let before = self.pending.len();
        self.pending
            .retain(|_, sent| now.saturating_duration_since(*sent) < REPLY_LOST);
        if self.pending.len() != before {
            self.changed = true;
        }
        if self
            .watch
            .is_some_and(|(_, sent)| now.saturating_duration_since(sent) >= WATCH_LOST)
        {
            self.watch = None;
        }
        // A collection that never came back is given up on, and asked again: pictures are
        // drawn from what already arrived, never waited for.
        if self
            .pictures
            .is_some_and(|(_, sent)| now.saturating_duration_since(sent) >= PICTURES_LOST)
        {
            self.pictures = None;
        }
        self.follow_pictures(now);
        if self.watch.is_none() && self.watch_after.is_none_or(|after| now >= after) {
            self.watch_after = None;
            let request_id = self.request_id();
            let revision = self.snapshot.as_ref().map_or(0, |s| s.revision);
            self.queued.push(CollabRequest::NativeMediaCommand {
                request_id,
                command: Request::new(Command::Watch { revision }),
            });
            self.watch = Some((request_id, now));
        }
        for request in self.queued.drain(..) {
            let watch = matches!(
                &request,
                CollabRequest::NativeMediaCommand { request_id, .. }
                    if self.watch.is_some_and(|(id, _)| id == *request_id)
            );
            if !send(request) && watch {
                // Asked again on the next frame.
                self.watch = None;
            }
        }
    }

    /// Tell `instance` who is in each call of the record, and what the service refused.
    pub fn show(&mut self, instance: &mut Instance) {
        if let Some(why) = self.refused.take() {
            instance.failed(why);
        }
        let changed = std::mem::take(&mut self.changed);
        if changed {
            instance.set_calls(self.members());
        }
        if let Some(message) = self.take_window_message() {
            instance.say(message);
            instance.flush();
        }
        self.show_inputs(instance);
    }

    /// Tell `instance` the microphones the call offered, and the one it sends from, when that
    /// changed: what the view's controls offer.
    fn show_inputs(&mut self, instance: &mut Instance) {
        let now: Vec<CallInput> = self
            .snapshot
            .as_ref()
            .filter(|_| self.joined.is_some())
            .map(|snapshot| {
                let microphones = snapshot.inputs.iter().map(|input| CallInput {
                    id: input.id.clone(),
                    label: input.label.clone(),
                    current: snapshot.input_id.as_deref() == Some(input.id.as_str()),
                    camera: false,
                });
                let cameras = snapshot.cameras.iter().map(|camera| CallInput {
                    id: camera.id.clone(),
                    label: camera.label.clone(),
                    current: snapshot.camera_id.as_deref() == Some(camera.id.as_str()),
                    camera: true,
                });
                microphones.chain(cameras).collect()
            })
            .unwrap_or_default();
        if now == self.inputs {
            return;
        }
        self.inputs = now;
        let place = self.joined.clone().unwrap_or_default();
        let inputs: std::collections::HashMap<String, Vec<CallInput>> = if place.is_empty() {
            std::collections::HashMap::new()
        } else {
            [(place, self.inputs.clone())].into_iter().collect()
        };
        instance.set_call_inputs(inputs);
    }

    /// What the window that draws the call's pictures is to be told, when there is anything: the
    /// tiles the view shows now, and the pictures the call delivered for them since it was last
    /// told. `None` when it already knows both.
    pub fn take_window_message(&mut self) -> Option<ToUi> {
        let shown: Vec<CallShown> = self.shown.iter().map(shown_of).collect();
        if self.window_shown.as_ref() == Some(&shown) && self.for_window.is_empty() {
            return None;
        }
        let pictures = std::mem::take(&mut self.for_window);
        self.window_shown = Some(shown.clone());
        Some(ToUi::CallPictures { shown, pictures })
    }

    /// Tell the call which pictures the view shows when that changed, and keep one collection of
    /// the ones it delivered outstanding while anything is shown. A call service that draws the
    /// pictures itself (a browser page decodes them where the view shows them) is told the tiles
    /// and nothing more: there is nothing for it to collect.
    fn follow_pictures(&mut self, now: Instant) {
        let wanted = self.wanted();
        if wanted != self.shown {
            self.shown = wanted;
            let request_id = self.request_id();
            self.queued.push(CollabRequest::NativeMediaCommand {
                request_id,
                command: Request::new(Command::Show {
                    pictures: self.shown.clone(),
                }),
            });
            if self.shown.is_empty() {
                // Nothing is drawn: what was still on its way is nobody's.
                self.pictures = None;
            }
        }
        let local = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.pictures_local);
        if !local && !self.shown.is_empty() && self.pictures.is_none() {
            let request_id = self.request_id();
            self.queued.push(CollabRequest::NativeMediaCommand {
                request_id,
                command: Request::new(Command::Pictures { after: self.cursor }),
            });
            self.pictures = Some((request_id, now));
        }
    }

    /// The pictures the view shows of the call this device is in: everyone on camera and every
    /// screen shared in it, each at the layer a tile takes.
    fn wanted(&self) -> Vec<Shown> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        if !matches!(
            snapshot.state,
            State::Preparing | State::Joining | State::Live | State::Reconnecting
        ) || self.joined.is_none()
        {
            return Vec::new();
        }
        let Some(joined) = &self.joined else {
            return Vec::new();
        };
        let mut shown = Vec::new();
        if let Some(roster) = snapshot
            .rosters
            .iter()
            .find(|roster| roster.channel_id == **joined)
        {
            let shown_of = |person: &String, source: PictureSource, layer: PictureLayer| Shown {
                person: person.clone(),
                source,
                layer,
            };
            for person in &roster.people {
                if roster.cameras.contains(person) {
                    shown.push(shown_of(person, PictureSource::Camera, PictureLayer::Mid));
                }
                if roster.screens.contains(person) {
                    shown.push(shown_of(person, PictureSource::Screen, PictureLayer::High));
                }
            }
        }
        if self.in_call(joined) {
            if snapshot.camera {
                shown.push(Shown {
                    person: self.me.clone(),
                    source: PictureSource::Camera,
                    layer: PictureLayer::Mid,
                });
            }
            if snapshot.screen {
                shown.push(Shown {
                    person: self.me.clone(),
                    source: PictureSource::Screen,
                    layer: PictureLayer::High,
                });
            }
        }
        shown.sort();
        shown.dedup();
        shown
    }

    /// Who is in each call the service told of, by its place, this device's person in the one it
    /// joined from the moment it asked to.
    fn members(&self) -> HashMap<String, Vec<CallMember>> {
        let mut out: HashMap<String, Vec<CallMember>> = HashMap::new();
        let Some(snapshot) = &self.snapshot else {
            if let Some(joined) = &self.joined {
                out.insert(joined.clone(), vec![self.own(None)]);
            }
            return out;
        };
        let live = matches!(
            snapshot.state,
            State::Preparing | State::Joining | State::Live | State::Reconnecting
        );
        let volumes = |person: &str| {
            snapshot
                .volumes
                .iter()
                .find(|volume| volume.member == person)
                .map_or(100, |volume| volume.percent)
        };
        for roster in &snapshot.rosters {
            let mine = live && self.in_call(&roster.channel_id);
            let has = |list: &[String], person: &str| list.iter().any(|p| p == person);
            let people = roster
                .people
                .iter()
                .filter(|person| !(mine && **person == self.me))
                .map(|person| CallMember {
                    who: Actor::Member(MemberId::person(person.as_str())),
                    mine: false,
                    speaking: mine && has(&snapshot.speaking, person),
                    muted: has(&roster.muted, person),
                    deafened: has(&roster.deafened, person),
                    camera: has(&roster.cameras, person),
                    screen: has(&roster.screens, person),
                    volume: volumes(person),
                    // Someone a bridge carries in from another service is shown by the name and
                    // picture they have there, and marked as reached through it.
                    bridged: roster
                        .bridged
                        .iter()
                        .find(|bridged| bridged.person == **person)
                        .map(|bridged| Bridged {
                            service: bridged.service.clone(),
                            name: bridged.name.clone(),
                            avatar: bridged
                                .avatar
                                .as_deref()
                                .map(|hash| format!("tilo-blob://{hash}"))
                                .unwrap_or_default(),
                        }),
                });
            out.entry(roster.channel_id.clone())
                .or_default()
                .extend(people);
        }
        if let Some(joined) = &self.joined
            && (live || !self.pending.is_empty())
        {
            out.entry(joined.clone())
                .or_default()
                .insert(0, self.own(Some(snapshot)));
        }
        out
    }

    /// The person themselves in the call this device joined, as the service last said of it.
    fn own(&self, snapshot: Option<&Snapshot>) -> CallMember {
        CallMember {
            who: Actor::Member(MemberId::person(self.me.as_str())),
            mine: true,
            speaking: snapshot.is_some_and(|s| s.speaking.contains(&self.me)),
            muted: snapshot.is_none_or(|s| s.muted),
            deafened: snapshot.is_some_and(|s| s.deafened),
            camera: snapshot.is_some_and(|s| s.camera),
            screen: snapshot.is_some_and(|s| s.screen),
            volume: snapshot
                .and_then(|snapshot| {
                    snapshot
                        .volumes
                        .iter()
                        .find(|volume| volume.member == self.me)
                        .map(|volume| volume.percent)
                })
                .unwrap_or(100),
            bridged: None,
        }
    }
}

/// A picture the call delivered, as the window that draws it takes it.
fn received(picture: &ui_core::native_media::Picture) -> CallPicture {
    CallPicture {
        person: picture.person.clone(),
        stream: picture.stream.clone(),
        source: source_of(picture.source),
        object: picture.object.clone(),
    }
}

/// One picture a view's call tiles show, as the window that draws them takes it.
fn shown_of(shown: &Shown) -> CallShown {
    CallShown {
        person: shown.person.clone(),
        source: source_of(shown.source),
    }
}

/// A picture's source, as the window's protocol names it.
const fn source_of(source: PictureSource) -> CallPictureSource {
    match source {
        PictureSource::Camera => CallPictureSource::Camera,
        PictureSource::Screen => CallPictureSource::Screen,
    }
}

/// What the service's `issue` means to the person.
const fn said(issue: Issue) -> &'static str {
    match issue {
        Issue::PermissionDenied => "The call can't use your microphone: allow it in the prompt.",
        Issue::Denied => "You can't speak or listen in this call.",
        Issue::SourceLost => "Your microphone went away.",
        Issue::CaptureFailed => "Your microphone could not be started.",
        Issue::TransportFailed | Issue::SessionEnded => "The call lost its connection.",
        Issue::CameraDenied => "The call can't use your camera: allow it in the prompt.",
        Issue::CameraLost => "Your camera went away.",
        Issue::ScreenDenied => "The call can't share your screen: allow it in the prompt.",
        Issue::ScreenLost => "Sharing your screen stopped.",
        Issue::Unsupported => "This device can't do that in a call.",
        Issue::Unavailable | Issue::Stale => "Calls aren't available on this device right now.",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;
    use ui_core::native_media::{Picture, Roster};

    const ME: &str = "abababababababababababababababababababababababababababababababab";
    const ADA: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
    const PLACE: &str = "voice:standup";

    fn snapshot(state: State, roster: Roster) -> Snapshot {
        Snapshot {
            version: 1,
            state,
            muted: false,
            capture_active: state == State::Live,
            input_id: None,
            inputs: Vec::new(),
            issue: None,
            inventory_revision: None,
            publication: None,
            publishers: Vec::new(),
            rosters: vec![roster],
            speaking: Vec::new(),
            camera: false,
            screen: false,
            deafened: false,
            revision: 1,
            pictures: Vec::new(),
            pictures_cursor: 0,
            volumes: Vec::new(),
            cameras: Vec::new(),
            camera_id: None,
            pictures_local: false,
        }
    }

    fn channel_call(cameras: bool, screens: bool) -> Roster {
        Roster {
            channel_id: PLACE.to_owned(),
            people: vec![ADA.to_owned()],
            cameras: cameras.then(|| ADA.to_owned()).into_iter().collect(),
            screens: screens.then(|| ADA.to_owned()).into_iter().collect(),
            muted: Vec::new(),
            deafened: Vec::new(),
            bridges: Vec::new(),
            bridged: Vec::new(),
        }
    }

    fn joined(calls: &mut Calls) -> u64 {
        let target = ResourceRef::root("voice:standup", "call");
        calls.step(&target, CallStep::Join);
        // The join's own request id is the first this instance asked.
        1
    }

    fn sent(calls: &mut Calls) -> Vec<Request> {
        let mut requests = Vec::new();
        calls.say(|request| {
            if let CollabRequest::NativeMediaCommand { command, .. } = request {
                requests.push(command);
            }
            true
        });
        requests
    }

    fn answer(calls: &mut Calls, request_id: u64, snapshot: Snapshot) {
        calls.take(&CollabEvent::NativeMediaCommandResult {
            request_id,
            result: Ok(snapshot),
        });
    }

    fn picture(person: &str, source: PictureSource, byte: u8) -> Picture {
        use ui_core::native_media::{PictureCodec, PictureLayer, PictureUnit};
        let object = PictureUnit {
            keyframe: true,
            codec: PictureCodec::H264,
            width: 16,
            height: 16,
            payload: &[byte; 8],
        }
        .to_object(PictureLayer::Mid, 1, 0);
        Picture {
            person: person.to_owned(),
            stream: format!("{person}/stream"),
            source,
            object,
        }
    }

    tilo_test_env::scenario! {
        fn a_call_view_shows_the_pictures_of_whoever_is_on_camera;

        phase a_view_in_a_live_call_shows_what_the_call_says_is_shown;
        {
            let mut calls = Calls::new(ME);
            let id = joined(&mut calls);
            answer(&mut calls, id, snapshot(State::Live, channel_call(true, false)));
            let requests = sent(&mut calls);
            assert!(
                requests.iter().any(|request| matches!(
                    &request.command,
                    Command::Show { pictures }
                        if pictures == &vec![Shown {
                            person: ADA.to_owned(),
                            source: PictureSource::Camera,
                            layer: PictureLayer::Mid,
                        }]
                )),
                "the call is told which pictures the view draws"
            );
            assert!(
                requests.iter().any(|request| matches!(&request.command, Command::Pictures { after: 0 })),
                "and collects them"
            );

            let window = calls.take_window_message().expect("the window is told the tiles");
            let ToUi::CallPictures { shown, pictures } = window else {
                panic!("the window takes the call's pictures");
            };
            assert_eq!(shown, vec![CallShown {
                person: ADA.to_owned(),
                source: CallPictureSource::Camera,
            }]);
            assert!(pictures.is_empty(), "nothing arrived yet");
            assert!(calls.take_window_message().is_none(), "nothing new to say");
        }

        phase a_service_that_draws_the_pictures_itself_is_told_the_tiles_and_collects_nothing;
        {
            let mut calls = Calls::new(ME);
            let id = joined(&mut calls);
            let mut local = snapshot(State::Live, channel_call(true, false));
            local.pictures_local = true;
            answer(&mut calls, id, local);
            let requests = sent(&mut calls);
            assert!(
                requests.iter().any(|request| matches!(
                    &request.command,
                    Command::Show { pictures } if pictures.len() == 1
                )),
                "the page is told which tiles the view draws"
            );
            assert!(
                !requests
                    .iter()
                    .any(|request| matches!(&request.command, Command::Pictures { .. })),
                "a page decodes the pictures where its tiles stand: nothing is collected"
            );
            let window = calls.take_window_message().expect("the window is told the tiles");
            let ToUi::CallPictures { shown, pictures } = window else {
                panic!("the window takes the call's pictures");
            };
            assert_eq!(shown.len(), 1);
            assert!(pictures.is_empty(), "the window draws no frame the page decoded");
        }

        phase a_picture_the_call_delivered_reaches_the_window_and_is_collected_again;
        {
            let mut calls = Calls::new(ME);
            let id = joined(&mut calls);
            answer(&mut calls, id, snapshot(State::Live, channel_call(true, true)));
            let requests = sent(&mut calls);
            let collect = requests
                .iter()
                .find(|request| matches!(&request.command, Command::Pictures { .. }))
                .expect("a collection is outstanding");
            let (request_id, _) = calls.pictures.expect("the collection is held");
            assert_eq!(collect.command, Command::Pictures { after: 0 });

            let mut with_pictures = snapshot(State::Live, channel_call(true, true));
            with_pictures.pictures = vec![picture(ADA, PictureSource::Camera, 7)];
            with_pictures.pictures_cursor = 1;
            answer(&mut calls, request_id, with_pictures);

            let window = calls.take_window_message().expect("the picture reaches the window");
            let ToUi::CallPictures { pictures, .. } = window else {
                panic!("the window takes the call's pictures");
            };
            assert_eq!(pictures.len(), 1);
            assert_eq!(pictures[0].person, ADA);
            assert_eq!(pictures[0].source, CallPictureSource::Camera);

            // The next collection starts past what was taken.
            let requests = sent(&mut calls);
            assert!(
                requests
                    .iter()
                    .any(|request| matches!(&request.command, Command::Pictures { after: 1 })),
                "the next collection starts where the last one ended"
            );
        }

        phase the_microphone_and_a_member_volume_are_steps_of_their_own;
        {
            let mut calls = Calls::new(ME);
            let id = joined(&mut calls);
            let mut live = snapshot(State::Live, channel_call(false, false));
            live.inputs = vec![
                ui_core::native_media::Input {
                    id: "mic-a".to_owned(),
                    label: "Built-in".to_owned(),
                },
                ui_core::native_media::Input {
                    id: "mic-b".to_owned(),
                    label: "Headset".to_owned(),
                },
            ];
            live.input_id = Some("mic-b".to_owned());
            live.cameras = vec![
                ui_core::native_media::Input {
                    id: "video0".to_owned(),
                    label: "Integrated Camera".to_owned(),
                },
                ui_core::native_media::Input {
                    id: "video2".to_owned(),
                    label: "Desk camera".to_owned(),
                },
            ];
            live.camera_id = Some("video0".to_owned());
            answer(&mut calls, id, live);
            sent(&mut calls);

            let target = ResourceRef::root("voice:standup", "call");
            calls.step(&target, CallStep::SelectInput("mic-a".to_owned()));
            let requests = sent(&mut calls);
            let chosen = requests
                .iter()
                .find(|request| {
                    matches!(
                        &request.command,
                        Command::SelectInput { input_id } if input_id == "mic-a"
                    )
                })
                .expect("the call is told which microphone to send");
            assert!(chosen.intent > 0, "choosing a microphone changes the call");

            // A camera is chosen the same way, and the call hears it as the camera's.
            calls.step(&target, CallStep::SelectInput("video2".to_owned()));
            let requests = sent(&mut calls);
            assert!(
                requests.iter().any(|request| matches!(
                    &request.command,
                    Command::SelectCamera { camera_id } if camera_id == "video2"
                )),
                "the call is told which camera to send: {requests:?}"
            );

            calls.step(
                &target,
                CallStep::Volume {
                    member: ADA.to_owned(),
                    percent: 40,
                },
            );
            let requests = sent(&mut calls);
            let volume = requests
                .iter()
                .find(|request| matches!(&request.command, Command::SetVolume { .. }))
                .expect("the call is told how loud to play the member");
            assert_eq!(volume.intent, 0, "how loud it plays here changes nothing of the call");
            assert_eq!(
                volume.command,
                Command::SetVolume {
                    member: ADA.to_owned(),
                    percent: 40,
                }
            );
        }

        phase leaving_the_call_stops_showing_and_collecting;
        {
            let mut calls = Calls::new(ME);
            let id = joined(&mut calls);
            answer(&mut calls, id, snapshot(State::Live, channel_call(true, false)));
            sent(&mut calls);
            let showing = calls.take_window_message().expect("the window shows the tile");
            let ToUi::CallPictures { shown, .. } = showing else {
                panic!("the window takes the call's pictures");
            };
            assert_eq!(shown.len(), 1);
            let target = ResourceRef::root("voice:standup", "call");
            calls.step(&target, CallStep::Leave);
            let requests = sent(&mut calls);
            assert!(
                requests.iter().any(|request| matches!(
                    &request.command,
                    Command::Show { pictures } if pictures.is_empty()
                )),
                "the call is told nothing is drawn any more"
            );
            assert!(calls.pictures.is_none(), "and no collection is held");
            let window = calls.take_window_message().expect("the window forgets the tiles");
            let ToUi::CallPictures { shown, pictures } = window else {
                panic!("the window takes the call's pictures");
            };
            assert!(shown.is_empty() && pictures.is_empty());
        }
    }
}
