//! Ephemeral client of the shell's governed voice service. Nothing here is
//! serialized into the Chat document or treated as microphone consent.
use std::collections::{BTreeMap, BTreeSet};
use ui::native_media::{PictureLayer, PictureSource};
use ui::{
    collaboration::CollabRequest,
    native_media::{Command, PICTURES_HOLD, Picture, Request, Shown, Snapshot, State},
    time::{Duration, Instant},
};

pub(crate) struct LocalVoice {
    pub(crate) snapshot: Snapshot,
    pub(crate) channel: Option<String>,
    pub(crate) client_issue: Option<&'static str>,
    queued: Vec<CollabRequest>,
    pending: BTreeMap<u64, Command>,
    next_request: u64,
    intent_floor: u64,
    applied: u64,
    /// The one watch outstanding, and when it was sent: the service answers
    /// it the moment the status changes (who joins, leaves, talks).
    watch: Option<(u64, Instant)>,
    /// No watch before this: the service just refused something.
    watch_after: Option<Instant>,
    pending_since: Option<Instant>,
    unmute_requested: bool,
    /// The call's pictures.
    pub(crate) video: CallVideo,
    /// How loud each participant plays on this device.
    pub(crate) volumes: CallVolumes,
}

/// How loud this person hears each participant, by person or guest id:
/// kept for as long as the window, and told to the service whenever a call
/// is live, so a new call plays everyone as they were last set.
#[derive(Default)]
pub(crate) struct CallVolumes {
    /// What the person set: each participant's level, and whether they
    /// silenced them, which keeps the level for when they hear them again.
    pub(crate) set: BTreeMap<String, Listening>,
    /// What the service was last told, in percent.
    told: BTreeMap<String, u16>,
    /// The one change in flight: its request, participant and percent.
    telling: Option<(u64, String, u16)>,
}

/// How loud one participant plays here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Listening {
    /// In percent of as they arrive.
    pub(crate) percent: u16,
    /// Not heard at all, whatever the level.
    pub(crate) silenced: bool,
}

impl Default for Listening {
    fn default() -> Self {
        Self {
            percent: 100,
            silenced: false,
        }
    }
}

impl Listening {
    /// What the service plays them at.
    pub(crate) const fn played(self) -> u16 {
        if self.silenced { 0 } else { self.percent }
    }
}

/// Which pictures of the call this window shows, and their collection: the
/// service is told what the tiles show whenever that changes, and one
/// collection is outstanding while the call is live and anything shows.
#[derive(Default)]
pub(crate) struct CallVideo {
    /// What the tiles show now: by person and source, sorted.
    pub(crate) wanted: Vec<Shown>,
    /// What the service was last told.
    told: Vec<Shown>,
    showing: Option<u64>,
    /// The collection in flight, and when it was sent.
    collecting: Option<(u64, Instant)>,
    /// Where the next collection continues.
    cursor: u64,
    /// No collection before this: the service just refused one.
    retry_after: Option<Instant>,
    /// Pictures collected and not yet handed to the decoder.
    pub(crate) arrived: Vec<Picture>,
}

/// A collection unanswered this long was lost: the service holds one at
/// most [`PICTURES_HOLD`].
const PICTURES_LOST: Duration = PICTURES_HOLD.saturating_add(Duration::from_secs(5));

/// A command unanswered this long was lost.
const REPLY_LOST: Duration = Duration::from_secs(2);

/// How long a service that refused is left alone before it is watched again.
const WATCH_RETRY: Duration = Duration::from_secs(2);
/// A watch unanswered this long was lost: the service holds one at most
/// [`ui::native_media::WATCH_HOLD`].
const WATCH_LOST: Duration = ui::native_media::WATCH_HOLD.saturating_add(Duration::from_secs(5));

/// A deadline found passed by more than this was not woken for in time:
/// the app itself was not running (a stalled page, a suspended device).
const WOKEN_LATE: Duration = Duration::from_secs(1);
/// How long a reply still has once the app runs again after such a stall.
const AFTER_STALL: Duration = Duration::from_secs(5);

/// `sent`, moved so that it is lost `after_stall` from `now`, when its
/// deadline (`sent + lost`) passed long before `now`: a reply cannot come
/// in while the app that would take it is not running, so the time the
/// app stood still says nothing about the service.
fn rearmed_after_stall(sent: Instant, lost: Duration, now: Instant) -> Instant {
    let overdue = now.saturating_duration_since(sent + lost);
    if overdue > WOKEN_LATE {
        (now + AFTER_STALL).checked_sub(lost).unwrap_or(sent)
    } else {
        sent
    }
}

impl Default for LocalVoice {
    fn default() -> Self {
        Self {
            snapshot: Snapshot::unavailable(),
            channel: None,
            client_issue: None,
            queued: Vec::new(),
            pending: BTreeMap::new(),
            next_request: 0,
            intent_floor: 0,
            applied: 0,
            watch: None,
            watch_after: None,
            pending_since: None,
            unmute_requested: false,
            video: CallVideo::default(),
            volumes: CallVolumes::default(),
        }
    }
}

impl LocalVoice {
    pub(crate) fn submit(&mut self, command: Command) -> Result<(), String> {
        let request = Request::new(command.clone());
        if !request.valid() {
            return Err("Invalid call command".into());
        }
        // Semantic status reads and the automatic poll share one refresh.
        // A second read would cancel the transport's first status task; its
        // cancellation reply is a real failure to this fail-closed client.
        if matches!(command, Command::Status)
            && self
                .pending
                .values()
                .any(|pending| matches!(pending, Command::Status))
        {
            return Ok(());
        }
        let stopping = matches!(command, Command::Leave | Command::SetMuted { muted: true });
        if stopping {
            self.queued.clear();
            self.pending.clear();
            self.pending_since = None;
        }
        if self.pending.len() >= 8 {
            return Err("Call controls are busy".into());
        }
        let id = self
            .next_request
            .checked_add(1)
            .ok_or("Call control sequence exhausted")?;
        self.next_request = id;
        if !matches!(command, Command::Status) {
            self.intent_floor = id;
            self.client_issue = None;
        }
        match &command {
            Command::Join { channel_id } => {
                self.channel = Some(channel_id.clone());
                let rosters = std::mem::take(&mut self.snapshot.rosters);
                self.snapshot = Snapshot::unavailable();
                self.snapshot.state = State::Preparing;
                self.snapshot.issue = None;
                self.snapshot.rosters = rosters;
                // Joining is listening, muted: the microphone is asked for
                // only when the person unmutes.
                self.unmute_requested = false;
            }
            Command::Leave => {
                self.channel = None;
                // Who else is in the room's calls stays as heard.
                let rosters = std::mem::take(&mut self.snapshot.rosters);
                self.snapshot = Snapshot::unavailable();
                self.snapshot.state = State::Ended;
                self.snapshot.issue = None;
                self.snapshot.rosters = rosters;
                self.unmute_requested = false;
            }
            Command::SetDeafened { deafened } => {
                // Deafening silences and mutes at once; undeafening waits
                // for the service to say what it put back.
                if *deafened {
                    self.snapshot.deafened = true;
                    self.snapshot.muted = true;
                    self.snapshot.capture_active = false;
                }
            }
            Command::SetMuted { muted } => {
                self.unmute_requested = !muted;
                // Turning capture off is immediate. Turning it on waits for
                // the shell's fresh local prompt and actual capture result.
                if *muted {
                    self.snapshot.muted = true;
                    self.snapshot.capture_active = false;
                }
            }
            Command::SelectInput { .. }
            | Command::Status
            | Command::Watch { .. }
            | Command::SetCamera { .. }
            | Command::SetScreen { .. }
            | Command::Show { .. }
            | Command::SetVolume { .. }
            | Command::Pictures { .. } => {}
        }
        self.pending.insert(id, command);
        self.queued.push(CollabRequest::NativeMediaCommand {
            request_id: id,
            command: request,
        });
        Ok(())
    }
    /// When this client next has to act with nothing arriving: a command
    /// or watch unanswered too long is given up, so the call shows why
    /// instead of "Joining" for ever; a service that refused is watched
    /// again; a lost picture collection is sent again. The window wakes for
    /// it, so it comes with no input. Only times still ahead of `now`: the
    /// request round of the frame a deadline woke acts on it, and a passed
    /// one must not keep waking the window.
    pub(crate) fn next_deadline(&self, now: Instant) -> Option<Instant> {
        [
            self.pending_since.map(|sent| sent + REPLY_LOST),
            self.watch.map(|(_, sent)| sent + WATCH_LOST),
            self.watch_after,
            self.video.collecting.map(|(_, sent)| sent + PICTURES_LOST),
            self.video.retry_after,
        ]
        .into_iter()
        .flatten()
        .filter(|at| *at > now)
        .min()
    }

    pub(crate) fn take_requests(&mut self, now: Instant) -> Vec<CollabRequest> {
        // Woken long after a deadline, the app stood still meanwhile: its
        // replies may be waiting to be taken. Giving the call up for that
        // left calls whenever a page stalled (a busy app beside it), though
        // the service had answered.
        self.pending_since = self
            .pending_since
            .map(|sent| rearmed_after_stall(sent, REPLY_LOST, now));
        self.watch = self
            .watch
            .map(|(id, sent)| (id, rearmed_after_stall(sent, WATCH_LOST, now)));
        if self
            .pending_since
            .is_some_and(|sent| now.saturating_duration_since(sent) >= REPLY_LOST)
        {
            self.invalidate("reply_timeout");
        }
        if self
            .watch
            .is_some_and(|(_, sent)| now.saturating_duration_since(sent) >= WATCH_LOST)
        {
            self.watch = None;
            self.invalidate("reply_timeout");
        }
        if !self.queued.is_empty() && self.pending_since.is_none() {
            self.pending_since = Some(now);
        }
        // Who is in each voice channel and who talks, joined or not: one
        // watch the service answers when that changes, never a poll.
        if self.watch.is_none()
            && self.pending.is_empty()
            && self.queued.is_empty()
            && self.watch_after.is_none_or(|after| now >= after)
            && let Some(id) = self.next_request.checked_add(1)
        {
            self.next_request = id;
            self.watch = Some((id, now));
            self.queued.push(CollabRequest::NativeMediaCommand {
                request_id: id,
                command: Request::new(Command::Watch {
                    revision: self.snapshot.revision,
                }),
            });
        }
        self.video_requests(now);
        self.volume_requests();
        std::mem::take(&mut self.queued)
    }

    /// Tell the service, one at a time, how loud each participant whose
    /// level differs from what it was told plays, while the call is live.
    fn volume_requests(&mut self) {
        let volumes = &mut self.volumes;
        if self.channel.is_none() || self.snapshot.state != State::Live {
            // A new call plays everyone as they arrive until told.
            volumes.told.clear();
            volumes.telling = None;
            return;
        }
        if volumes.telling.is_some() {
            return;
        }
        let Some((participant, percent)) = volumes
            .set
            .iter()
            .map(|(participant, listening)| (participant, listening.played()))
            .find(|(participant, percent)| {
                volumes.told.get(*participant).copied().unwrap_or(100) != *percent
            })
            .map(|(participant, percent)| (participant.clone(), percent))
        else {
            return;
        };
        let Some(id) = self.next_request.checked_add(1) else {
            return;
        };
        self.next_request = id;
        volumes.telling = Some((id, participant.clone(), percent));
        self.queued.push(CollabRequest::NativeMediaCommand {
            request_id: id,
            command: Request::new(Command::SetVolume {
                participant,
                percent,
            }),
        });
    }

    /// The answer to a volume change, if `request_id` was one. A refused
    /// one is not asked again: the call plays them as it can.
    fn complete_volume(&mut self, request_id: u64) -> bool {
        let volumes = &mut self.volumes;
        let Some((_, participant, percent)) = volumes
            .telling
            .take_if(|(telling, ..)| *telling == request_id)
        else {
            return false;
        };
        volumes.told.insert(participant, percent);
        true
    }

    /// Tell the service what the tiles show when that changed, and keep one
    /// collection of their pictures outstanding while the call is live.
    fn video_requests(&mut self, now: Instant) {
        let live = self.channel.is_some() && self.snapshot.state == State::Live;
        if !live {
            // A call starts showing nothing; every stream starts over.
            self.video.told.clear();
            self.video.showing = None;
            self.video.collecting = None;
            self.video.cursor = 0;
            return;
        }
        if self
            .video
            .collecting
            .is_some_and(|(_, sent)| now.saturating_duration_since(sent) >= PICTURES_LOST)
        {
            self.video.collecting = None;
        }
        if self.video.showing.is_none()
            && self.video.wanted != self.video.told
            && let Some(id) = self.next_request.checked_add(1)
        {
            self.next_request = id;
            self.video.showing = Some(id);
            self.video.told.clone_from(&self.video.wanted);
            self.queued.push(CollabRequest::NativeMediaCommand {
                request_id: id,
                command: Request::new(Command::Show {
                    pictures: self.video.wanted.clone(),
                }),
            });
        }
        if self.video.collecting.is_none()
            && !self.video.told.is_empty()
            && self.video.retry_after.is_none_or(|after| now >= after)
            && let Some(id) = self.next_request.checked_add(1)
        {
            self.next_request = id;
            self.video.collecting = Some((id, now));
            self.queued.push(CollabRequest::NativeMediaCommand {
                request_id: id,
                command: Request::new(Command::Pictures {
                    after: self.video.cursor,
                }),
            });
        }
    }

    /// The answer to a `Show` or a collection, if `request_id` was one.
    fn complete_video(&mut self, request_id: u64, result: &Result<Snapshot, String>) -> bool {
        if self.video.showing == Some(request_id) {
            self.video.showing = None;
            if result.is_err() {
                // Told again with the next request round.
                self.video.told.clear();
            }
            return true;
        }
        if self
            .video
            .collecting
            .is_some_and(|(collecting, _)| collecting == request_id)
        {
            self.video.collecting = None;
            match result {
                Ok(snapshot) if snapshot.valid() => {
                    self.video.cursor = self.video.cursor.max(snapshot.pictures_cursor);
                    self.video.arrived.extend(snapshot.pictures.iter().cloned());
                }
                Ok(_) | Err(_) => {
                    self.video.retry_after = Some(ui::time::now() + WATCH_RETRY);
                }
            }
            return true;
        }
        false
    }

    /// The service's answer to the watch `request_id`: everything when no
    /// newer intent is in flight, else only what the room says (who is in
    /// each call, who talks), which no local intent changes.
    fn complete_watch(&mut self, request_id: u64, result: &Result<Snapshot, String>) -> bool {
        let before = self.snapshot.clone();
        let before_channel = self.channel.clone();
        match result {
            Err(error) if error == "native media command superseded" => {}
            Err(error) => {
                self.watch_after = Some(ui::time::now() + WATCH_RETRY);
                let issue = match error.as_str() {
                    "native media command timed out" => "reply_timeout",
                    "native media command refused" => "command_refused",
                    _ => "unavailable",
                };
                self.invalidate(issue);
            }
            Ok(snapshot) if !snapshot.valid() => {
                self.watch_after = Some(ui::time::now() + WATCH_RETRY);
                self.invalidate("invalid_status");
            }
            Ok(snapshot)
                if self.pending.is_empty()
                    && request_id >= self.intent_floor
                    && request_id > self.applied =>
            {
                self.applied = request_id;
                self.apply(snapshot);
            }
            Ok(snapshot) => {
                self.snapshot.rosters.clone_from(&snapshot.rosters);
                self.snapshot.speaking.clone_from(&snapshot.speaking);
            }
        }
        before != self.snapshot || before_channel != self.channel
    }

    pub(crate) fn complete(&mut self, request_id: u64, result: &Result<Snapshot, String>) -> bool {
        if self.complete_video(request_id, result) || self.complete_volume(request_id) {
            return false;
        }
        if self.watch.is_some_and(|(watch, _)| watch == request_id) {
            self.watch = None;
            return self.complete_watch(request_id, result);
        }
        let Some(command) = self.pending.remove(&request_id) else {
            return false;
        };
        if request_id < self.intent_floor || request_id < self.applied {
            return false;
        }
        let before = self.snapshot.clone();
        let before_channel = self.channel.clone();
        if self.pending.is_empty() {
            self.pending_since = None;
        }
        self.applied = request_id;
        // A newer service result supersedes older in-flight local intents.
        // Their eventual replies are ignored, and they cannot keep a stale
        // watchdog deadline alive after the current intent was acknowledged.
        self.pending.retain(|id, _| *id > request_id);
        if self.pending.is_empty() {
            self.pending_since = None;
        }
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // A refusing service is left alone for a moment.
                self.watch_after = Some(ui::time::now() + WATCH_RETRY);
                // Transport errors may carry private details. Export only
                // these fixed protocol categories, never the error itself.
                let issue = match error.as_str() {
                    "native media command timed out" => "reply_timeout",
                    "native media command superseded" => "reply_superseded",
                    "native media command refused" => "command_refused",
                    _ => "unavailable",
                };
                if matches!(command, Command::Join { .. }) && self.channel.is_some() {
                    // The call could not be joined: the card stays and says
                    // so until the person closes it or joins again.
                    self.refused(ui::native_media::Issue::Unavailable);
                    self.client_issue = Some(issue);
                } else {
                    self.invalidate(issue);
                }
                return before != self.snapshot || before_channel != self.channel;
            }
        };
        if !snapshot.valid() {
            self.invalidate("invalid_status");
            return before != self.snapshot || before_channel != self.channel;
        }
        self.apply(snapshot);
        before != self.snapshot || before_channel != self.channel
    }
    /// Take the service's `snapshot` as the call's state, fail-closed: the
    /// microphone shows on only while this person asked for it.
    fn apply(&mut self, snapshot: &Snapshot) {
        self.snapshot = snapshot.clone();
        if !self.unmute_requested {
            self.snapshot.muted = true;
            self.snapshot.capture_active = false;
        }
        if matches!(
            snapshot.state,
            State::Unavailable | State::Idle | State::Ended
        ) {
            // A call that ended for a reason (refused, never granted, ended
            // by the room) keeps its card, which says why.
            let explained = snapshot.state == State::Ended && snapshot.issue.is_some();
            if snapshot.state != State::Unavailable && !explained {
                self.channel = None;
            }
            self.unmute_requested = false;
            self.snapshot.muted = true;
            self.snapshot.capture_active = false;
        }
    }
    /// The join of the current channel failed for `issue`: the call shows
    /// as ended, its card stays to say why, and nothing is captured.
    fn refused(&mut self, issue: ui::native_media::Issue) {
        let rosters = std::mem::take(&mut self.snapshot.rosters);
        self.snapshot = Snapshot::unavailable();
        self.snapshot.state = State::Ended;
        self.snapshot.issue = Some(issue);
        self.snapshot.rosters = rosters;
        self.unmute_requested = false;
    }

    /// Whether this device is already in, or on its way into, the call of
    /// `channel`: joining it again would only start over.
    pub(crate) fn in_call_of(&self, channel: &str) -> bool {
        self.channel.as_deref() == Some(channel)
            && matches!(
                self.snapshot.state,
                State::Preparing | State::Joining | State::Live | State::Reconnecting
            )
    }

    /// Whether the call of the current channel ended for a reason the card
    /// shows, rather than because the person left.
    pub(crate) fn ended_for_a_reason(&self) -> bool {
        self.channel.is_some()
            && self.snapshot.state == State::Ended
            && self.snapshot.issue.is_some()
    }

    /// Forget the service's state and leave the call, if there is one. With
    /// no channel there is nothing to leave: a Leave here would itself be
    /// refused by an absent service and invalidate again, once per reply.
    fn invalidate(&mut self, issue: &'static str) {
        if self.channel.is_some() {
            let _ = self.submit(Command::Leave);
        }
        self.snapshot = Snapshot::unavailable();
        // Cleanup's successful Ended reply does not erase why the call
        // failed. An explicit new command clears this ephemeral diagnosis.
        self.client_issue = Some(issue);
    }
}

/// See [`crate::app::ChatApp::call_layout_inputs`].
type CallLayoutInputs = (
    bool,
    String,
    bool,
    Option<String>,
    Vec<(String, Vec<String>)>,
);

impl crate::app::ChatApp {
    pub(crate) fn native_voice_command(
        &mut self,
        channel: &str,
        command: Command,
    ) -> Result<(), String> {
        use crate::call::CallAction;
        let action = match &command {
            Command::Join { channel_id } => {
                if channel_id != channel
                    || !self
                        .channels
                        .iter()
                        .any(|c| c.id == channel && c.kind == crate::model::ChannelKind::Voice)
                {
                    return Err("Voice channel no longer exists".into());
                }
                CallAction::Join
            }
            Command::SetMuted { muted: false }
            | Command::SetCamera { on: true }
            | Command::SetScreen { on: true } => CallAction::Speak,
            Command::SetMuted { muted: true }
            | Command::SetDeafened { deafened: true }
            | Command::SetCamera { on: false }
            | Command::SetScreen { on: false }
            | Command::Leave => CallAction::Leave,
            // Hearing the call again is being in it; the microphone it puts
            // back answers to the grant as any unmute does.
            Command::SetDeafened { deafened: false }
            | Command::SelectInput { .. }
            | Command::Status
            | Command::Watch { .. }
            | Command::Show { .. }
            | Command::SetVolume { .. }
            | Command::Pictures { .. } => CallAction::Join,
        };
        if !self.call_allows(action) {
            return Err("Call action is not permitted".into());
        }
        // Asking for the call this device is already in shows it; it does
        // not leave and join again.
        if matches!(command, Command::Join { .. }) && self.native_voice.in_call_of(channel) {
            self.project_native_voice();
            return Ok(());
        }
        if !matches!(
            &command,
            Command::Join { .. } | Command::Status | Command::Watch { .. }
        ) && self
            .native_voice
            .channel
            .as_deref()
            .is_some_and(|active| active != channel)
        {
            return Err("This is not the active call".into());
        }
        self.native_voice.submit(command)?;
        self.project_native_voice();
        Ok(())
    }

    /// The call's microphone is muted because this device has none (or it
    /// went away): the mute control says so instead of offering to unmute.
    pub(crate) fn microphone_missing(&self) -> bool {
        self.call.active
            && self.call.muted
            && self.native_voice.snapshot.issue == Some(ui::native_media::Issue::SourceLost)
    }

    pub(crate) fn project_native_voice(&mut self) {
        let laid_out = self.call_layout_inputs();
        let snapshot = &self.native_voice.snapshot;
        // The panel represents explicit local intent, including an unavailable
        // attempt. Only Snapshot::Live represents current service readiness.
        let joined = self.native_voice.channel.is_some()
            && (!matches!(snapshot.state, State::Idle | State::Ended)
                || self.native_voice.ended_for_a_reason());
        if joined {
            if let Some(channel) = &self.native_voice.channel {
                if !self.call.active || self.call.channel != *channel {
                    self.call.join(channel);
                }
                self.call.muted = snapshot.muted;
                self.call.deafened = snapshot.deafened;
                self.call.audio_only = !snapshot.camera;
                self.call.sharing = snapshot.screen;
            }
        } else {
            self.call.leave();
        }
        self.media_error = snapshot.issue.map(|issue| {
            crate::i18n::t(match issue {
                ui::native_media::Issue::Unavailable => "chat-call-issue-unavailable",
                ui::native_media::Issue::Denied => "chat-call-issue-denied",
                ui::native_media::Issue::PermissionDenied => "chat-call-issue-permission-denied",
                ui::native_media::Issue::SourceLost => "chat-call-issue-source-lost",
                ui::native_media::Issue::CaptureFailed => "chat-call-issue-capture-failed",
                ui::native_media::Issue::TransportFailed => "chat-call-issue-transport-failed",
                ui::native_media::Issue::SessionEnded => "chat-call-issue-session-ended",
                ui::native_media::Issue::Stale => "chat-call-issue-stale",
                ui::native_media::Issue::CameraDenied => "chat-call-issue-camera-denied",
                ui::native_media::Issue::CameraLost => "chat-call-issue-camera-lost",
                ui::native_media::Issue::ScreenDenied => "chat-call-issue-screen-denied",
                ui::native_media::Issue::ScreenLost => "chat-call-issue-screen-lost",
                ui::native_media::Issue::Unsupported => "chat-call-issue-unsupported",
            })
        });
        self.refresh_native_streams();
        // A video call turns the camera on once it is live.
        if self.camera_on_live
            && self.native_voice.snapshot.state == State::Live
            && let Some(channel) = self.native_voice.channel.clone()
        {
            self.camera_on_live = false;
            if let Err(error) = self.native_voice_command(&channel, Command::SetCamera { on: true })
            {
                self.media_error = Some(error);
            }
        }
        // Who talks changes several times a second in a call: it repaints
        // the rings, and lays nothing out again.
        if self.call_layout_inputs() == laid_out {
            self.canonical_dirty = true;
        } else {
            self.request_relayout();
        }
    }

    /// What the call's layout depends on: whether and where this person is
    /// in a call, its microphone and issue, and who is in each voice
    /// channel; not who is talking.
    fn call_layout_inputs(&self) -> CallLayoutInputs {
        let mut people: Vec<(String, Vec<String>)> = self
            .voice_members
            .iter()
            .map(|(channel, members)| {
                (
                    channel.clone(),
                    members.iter().map(|member| member.user.clone()).collect(),
                )
            })
            .collect();
        people.sort();
        (
            self.call.active,
            self.call.channel.clone(),
            self.call.muted,
            self.media_error.clone(),
            people,
        )
    }

    /// Whose camera and screen the call this person is in shows now, as the
    /// room says, this person's own camera among them while it is on.
    fn showing_pictures(&self) -> BTreeSet<(String, PictureSource)> {
        let mut showing = BTreeSet::new();
        if !self.call.active {
            return showing;
        }
        for roster in &self.native_voice.snapshot.rosters {
            if roster.channel_id != self.call.channel {
                continue;
            }
            showing.extend(
                roster
                    .cameras
                    .iter()
                    .map(|person| (person.clone(), PictureSource::Camera)),
            );
            showing.extend(
                roster
                    .screens
                    .iter()
                    .map(|person| (person.clone(), PictureSource::Screen)),
            );
        }
        if !self.call.audio_only {
            showing.insert((self.me.id.clone(), PictureSource::Camera));
        }
        showing
    }

    /// The pictures the tiles of the last frame show, each at the layer
    /// that fits its tile: only of people who show them, and none while this
    /// window is not on screen.
    pub(crate) fn wanted_pictures(&self) -> Vec<Shown> {
        if !self.presented {
            return Vec::new();
        }
        let showing = self.showing_pictures();
        let mut wanted: BTreeMap<(String, PictureSource), PictureLayer> = BTreeMap::new();
        for (person, source, (width, height)) in self.picture_tiles.borrow().iter() {
            if !showing.contains(&(person.clone(), *source)) {
                continue;
            }
            let layer = layer_for(*width, *height, *source);
            let entry = wanted.entry((person.clone(), *source)).or_insert(layer);
            *entry = (*entry).max(layer);
        }
        wanted
            .into_iter()
            .map(|((person, source), layer)| Shown {
                person,
                source,
                layer,
            })
            .collect()
    }

    /// Show the pictures decoded since the last frame, and stop showing
    /// those of people who turned their camera off, stopped sharing or left.
    pub(crate) fn sync_call_pictures(&mut self) {
        if !self.call.active {
            if !self.remote_frames.is_empty() || !self.remote_screens.is_empty() {
                self.remote_frames.clear();
                self.remote_screens.clear();
                self.canonical_dirty = true;
            }
            self.call_pictures.clear();
            return;
        }
        self.call_pictures.poll();
        let showing = self.showing_pictures();
        let me = self.me.id.clone();
        let own = |person: &str, me: &str, former: &[String]| {
            person == me || former.iter().any(|id| id == person)
        };
        let former = self.former_ids.clone();
        // This person's own picture comes under the id the call knows them
        // by; their tile knows them by this window's.
        let shown = |person: &str, source: PictureSource| {
            let person = if own(person, &me, &former) {
                me.as_str()
            } else {
                person
            };
            showing.contains(&(person.to_owned(), source))
        };
        self.call_pictures.retain(shown);
        let before = (self.remote_frames.len(), self.remote_screens.len());
        self.remote_frames
            .retain(|person, _| showing.contains(&(person.clone(), PictureSource::Camera)));
        self.remote_screens
            .retain(|person, _| showing.contains(&(person.clone(), PictureSource::Screen)));
        let mut changed = before != (self.remote_frames.len(), self.remote_screens.len());
        if self.call_pictures.take_changed() {
            for (person, source, image) in self.call_pictures.pictures() {
                let person = if own(&person, &me, &former) {
                    me.clone()
                } else {
                    person
                };
                match source {
                    PictureSource::Camera => self.remote_frames.insert(person, image),
                    PictureSource::Screen => self.remote_screens.insert(person, image),
                };
            }
            changed = true;
        }
        if changed {
            self.canonical_dirty = true;
        }
    }

    /// Turn this person's camera or screen on or off in the call they are in.
    pub(crate) fn toggle_call_picture(&mut self, source: PictureSource) {
        let channel = self
            .native_voice
            .channel
            .clone()
            .unwrap_or_else(|| self.call.channel.clone());
        let command = match source {
            PictureSource::Camera => Command::SetCamera {
                on: self.call.audio_only,
            },
            PictureSource::Screen => Command::SetScreen {
                on: !self.call.sharing,
            },
        };
        if let Err(error) = self.native_voice_command(&channel, command) {
            self.media_error = Some(error);
        }
    }

    /// Who is in each voice channel, joined or not: the rosters the service
    /// last heard from the room, by person (this person among them once the
    /// room counts them in).
    pub(crate) fn refresh_native_streams(&mut self) {
        self.voice_members.clear();
        let snapshot = &self.native_voice.snapshot;
        // This person's own mute is known here first; the room's word on it
        // follows a moment later.
        let own = self.call.active.then_some((
            self.call.channel.as_str(),
            self.call.muted,
            self.call.deafened,
        ));
        let me: Vec<&str> = std::iter::once(self.me.id.as_str())
            .chain(self.former_ids.iter().map(String::as_str))
            .collect();
        for roster in &snapshot.rosters {
            // What the room says each person shows and whether they are
            // muted: their tile reserves a picture from that moment, before
            // the first one arrives.
            let hearing = |user: &str| match own {
                Some((channel, muted, deafened))
                    if channel == roster.channel_id && me.contains(&user) =>
                {
                    (muted, deafened)
                }
                _ => (
                    roster.muted.iter().any(|person| person == user),
                    roster.deafened.iter().any(|person| person == user),
                ),
            };
            let member = |user: &str, (muted, deafened): (bool, bool)| crate::model::VoiceMember {
                user: user.to_owned(),
                talking: snapshot.speaking.iter().any(|person| person == user),
                muted,
                deafened,
                audio_only: !roster.cameras.iter().any(|person| person == user),
                sharing: roster.screens.iter().any(|person| person == user),
                ticket: None,
                video: None,
            };
            let voice = self.channels.iter().any(|channel| {
                channel.id == roster.channel_id && channel.kind == crate::model::ChannelKind::Voice
            });
            if voice {
                let mut members: Vec<_> = roster
                    .people
                    .iter()
                    .map(|person| member(person, hearing(person)))
                    .collect();
                // A person's devices are one person; the service sorted them.
                members.dedup_by(|a, b| a.user == b.user);
                // The people a bridge carries in, each as themselves.
                members.extend(
                    roster
                        .guests
                        .iter()
                        .map(|guest| member(&guest.id, (guest.muted, false))),
                );
                self.voice_members
                    .insert(roster.channel_id.clone(), members);
            }
        }
    }

    /// The person a bridge carries into a call from another service, if
    /// `id` names one.
    pub(crate) fn call_guest(&self, id: &str) -> Option<&ui::native_media::Guest> {
        self.native_voice
            .snapshot
            .rosters
            .iter()
            .flat_map(|roster| &roster.guests)
            .find(|guest| guest.id == id)
    }

    /// The services bridging `channel`'s call, as words (`Discord`): empty
    /// for a call only Tilo people hear.
    pub(crate) fn call_bridges(&self, channel: &str) -> Vec<String> {
        self.native_voice
            .snapshot
            .rosters
            .iter()
            .filter(|roster| roster.channel_id == channel)
            .flat_map(|roster| &roster.bridges)
            .map(|service| ui::author::service_label(service))
            .filter(|label| !label.is_empty())
            .collect()
    }

    /// "This call is bridged to Discord": where everyone's voice in
    /// `channel`'s call also goes, or `None` when it stays in Tilo.
    pub(crate) fn call_bridged_note(&self, channel: &str) -> Option<String> {
        let services = self.call_bridges(channel);
        (!services.is_empty()).then(|| {
            crate::i18n::t_args(
                "chat-call-bridged",
                &[("services", services.join(", ").into())],
            )
        })
    }

    /// Who `id` is in a call: the name their face shows, and the service a
    /// bridge reaches them through. A guest linked to a room member is
    /// that member; otherwise they are who the bridge says.
    fn call_person(&self, id: &str) -> (String, Option<String>) {
        match self.call_guest(id) {
            Some(guest) => {
                let name = guest
                    .linked
                    .as_deref()
                    .filter(|linked| self.users.iter().any(|user| user.id == *linked))
                    .map_or_else(|| guest.name.clone(), |linked| self.user_name(linked));
                let service = ui::author::service_label(&guest.service);
                (name, (!service.is_empty()).then_some(service))
            }
            None => (self.user_name(id), None),
        }
    }

    /// The name `id`'s face in a call takes its initials from.
    pub(crate) fn call_face_name(&self, id: &str) -> String {
        self.call_person(id).0
    }

    /// What a call names `id` by: their name, and for someone a bridge
    /// carries in where they are, as their bridged messages read ("Dana
    /// (via Discord)").
    pub(crate) fn call_name(&self, id: &str) -> String {
        match self.call_person(id) {
            (name, Some(service)) => crate::i18n::t_args(
                "chat-via-attribution",
                &[("name", name.into()), ("service", service.into())],
            ),
            (name, None) => name,
        }
    }

    /// The colour `id` wears in a call: a linked guest their member's.
    pub(crate) fn call_colour(&self, id: &str) -> vello::peniko::Color {
        let linked = self
            .call_guest(id)
            .and_then(|guest| guest.linked.as_deref());
        self.identity_colour(linked.unwrap_or(id))
    }

    /// The picture a guest's bridge shows for them, requested from the
    /// room's content store until it is there.
    pub(crate) fn call_picture(&self, id: &str) -> Option<vello::peniko::ImageData> {
        let avatar = self.call_guest(id)?.avatar.as_deref()?;
        self.images.get(&crate::images::blob_key(avatar))
    }

    /// How loud `id` plays on this device.
    pub(crate) fn call_listening(&self, id: &str) -> Listening {
        self.native_voice
            .volumes
            .set
            .get(id)
            .copied()
            .unwrap_or_default()
    }

    /// Set how loud `id` plays on this device; the service hears it with
    /// the next request round.
    pub(crate) fn set_call_listening(&mut self, id: &str, listening: Listening) {
        self.native_voice
            .volumes
            .set
            .insert(id.to_owned(), listening);
        self.canonical_dirty = true;
    }
}

/// The smallest layer whose pictures cover a tile `width` × `height`
/// device pixels (`tilo_moq::video::Layer::for_tile`): a screen has no
/// middle layer.
pub(crate) fn layer_for(width: f64, height: f64, source: PictureSource) -> PictureLayer {
    if width <= 320.0 && height <= 180.0 {
        PictureLayer::Low
    } else if source == PictureSource::Camera && width <= 640.0 && height <= 360.0 {
        PictureLayer::Mid
    } else {
        PictureLayer::High
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::native_media::State;

    fn live() -> Snapshot {
        let mut snapshot = Snapshot::unavailable();
        snapshot.state = State::Live;
        snapshot.issue = None;
        snapshot
    }
    fn request_id(request: &CollabRequest) -> u64 {
        match request {
            CollabRequest::NativeMediaCommand { request_id, .. } => *request_id,
            _ => panic!("only the native media service may receive live call intent"),
        }
    }

    #[test]
    fn shared_space_call_status_exposes_muted_join_and_leave_without_starting_capture() {
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let before = doc.save();
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.reload(&doc);
        app.answer_effect(crate::effects::ChatEffect::JoinCall {
            channel: "c_standup".into(),
        })
        .unwrap();
        app.answer_effect(crate::effects::ChatEffect::CallStatus {
            channel: "c_standup".into(),
        })
        .unwrap();
        assert_eq!(app.native_voice.snapshot.state, State::Preparing);
        assert!(app.native_voice.snapshot.muted);
        assert!(!app.native_voice.snapshot.capture_active);
        app.answer_effect(crate::effects::ChatEffect::LeaveCall {
            channel: "c_standup".into(),
        })
        .unwrap();
        assert_eq!(app.native_voice.snapshot.state, State::Ended);
        assert!(app.native_voice.channel.is_none());
        assert_eq!(
            doc.save(),
            before,
            "call state is transient, never persistent consent"
        );
    }

    #[test]
    fn an_unanswered_join_wakes_the_window_to_give_up() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let _ = voice.take_requests(now);
        assert_eq!(voice.snapshot.state, State::Preparing);
        let due = voice
            .next_deadline(now)
            .expect("an unanswered join asks for a wake");
        assert_eq!(due, now + REPLY_LOST);
        let _ = voice.take_requests(due);
        assert_ne!(voice.snapshot.state, State::Preparing, "the join gave up");
        assert_eq!(voice.client_issue, Some("reply_timeout"));
        assert!(
            voice.next_deadline(due).is_none_or(|next| next > due),
            "a deadline that passed does not keep waking the window"
        );
    }

    #[test]
    fn a_stalled_app_does_not_give_up_a_call_it_could_not_hear_from() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(now);
        voice.complete(request_id(&join[0]), &Ok(live()));
        let watch = voice.take_requests(now);
        assert_eq!(watch.len(), 1, "one watch outstanding");
        let leaves = |requests: &[CollabRequest]| {
            requests.iter().any(|request| {
                matches!(
                    request,
                    CollabRequest::NativeMediaCommand {
                        command: Request {
                            command: Command::Leave,
                            ..
                        },
                        ..
                    }
                )
            })
        };
        let woken = now + WATCH_LOST + Duration::from_secs(30);
        assert!(!leaves(&voice.take_requests(woken)), "the call stays");
        assert_eq!(voice.snapshot.state, State::Live);
        assert_eq!(voice.channel.as_deref(), Some("voice"));
        assert!(!voice.complete(request_id(&watch[0]), &Ok(live())));
        assert_eq!(voice.snapshot.state, State::Live);
        let next = voice.take_requests(woken);
        assert_eq!(next.len(), 1);
        let lost = woken + WATCH_LOST;
        assert!(
            leaves(&voice.take_requests(lost)),
            "a lost reply still ends it"
        );
        assert_eq!(voice.snapshot.state, State::Unavailable);
    }

    #[test]
    fn a_reply_lost_while_running_gives_up_after_the_grace_a_stall_gets() {
        let now = ui::time::now();
        let sent = now;
        // On time: unchanged.
        assert_eq!(
            rearmed_after_stall(sent, REPLY_LOST, now + REPLY_LOST),
            sent
        );
        // Woken late: lost `AFTER_STALL` from then.
        let woken = now + REPLY_LOST + Duration::from_secs(10);
        let rearmed = rearmed_after_stall(sent, REPLY_LOST, woken);
        assert_eq!(rearmed + REPLY_LOST, woken + AFTER_STALL);
    }

    #[test]
    fn newer_join_response_retires_prior_pending_intents() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "first".into(),
            })
            .unwrap();
        let first = voice.take_requests(now);
        voice
            .submit(Command::Join {
                channel_id: "second".into(),
            })
            .unwrap();
        let second = voice.take_requests(now);
        voice.complete(request_id(&second[0]), &Ok(live()));
        voice.complete(request_id(&first[0]), &Ok(live()));
        let requests = voice.take_requests(now + Duration::from_secs(3));
        assert_eq!(voice.snapshot.state, State::Live);
        assert_eq!(voice.channel.as_deref(), Some("second"));
        assert!(requests.iter().all(|request| matches!(
            request,
            CollabRequest::NativeMediaCommand {
                command: Request {
                    command: Command::Watch { .. },
                    ..
                },
                ..
            }
        )));
    }

    #[test]
    fn lost_status_reply_clears_retained_inventory_and_fences_late_response() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(now);
        let mut snapshot = live();
        snapshot.inventory_revision = Some(1);
        snapshot.publishers = vec![format!("{}/{}", "11".repeat(32), "22".repeat(32))];
        voice.complete(request_id(&join[0]), &Ok(snapshot.clone()));
        let status = voice.take_requests(now + Duration::from_millis(500));
        assert_eq!(status.len(), 1, "one watch outstanding");
        assert!(
            voice
                .take_requests(now + Duration::from_secs(20))
                .is_empty(),
            "a held watch is no lost reply"
        );
        let stopping = voice.take_requests(now + Duration::from_millis(500) + WATCH_LOST);
        assert_eq!(voice.snapshot.state, State::Unavailable);
        assert!(voice.snapshot.publishers.is_empty());
        assert!(stopping.iter().any(|request| matches!(
            request,
            CollabRequest::NativeMediaCommand {
                command: Request {
                    command: Command::Leave,
                    ..
                },
                ..
            }
        )));
        voice.complete(request_id(&status[0]), &Ok(snapshot));
        assert_eq!(voice.snapshot.state, State::Unavailable);
    }

    #[test]
    fn join_stays_muted_and_unjoined_until_service_response() {
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        assert_eq!(voice.snapshot.state, State::Preparing);
        assert!(voice.snapshot.muted);
        assert!(!voice.snapshot.capture_active);
        let requests = voice.take_requests(ui::time::now());
        assert_eq!(requests.len(), 1);
        assert!(
            matches!(&requests[0], CollabRequest::NativeMediaCommand { command: Request { version: 1, command: Command::Join { channel_id }, .. }, .. } if channel_id == "voice")
        );
        voice.complete(request_id(&requests[0]), &Ok(live()));
        assert_eq!(voice.snapshot.state, State::Live);
        assert_eq!(voice.channel.as_deref(), Some("voice"));
    }

    #[test]
    fn leave_fences_late_join_and_unmute_results() {
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(ui::time::now());
        voice.submit(Command::Leave).unwrap();
        let leave = voice.take_requests(ui::time::now());
        voice.complete(request_id(&join[0]), &Ok(live()));
        assert_eq!(voice.snapshot.state, State::Ended);
        assert!(voice.channel.is_none());
        assert!(voice.snapshot.muted);
        assert!(matches!(
            &leave[0],
            CollabRequest::NativeMediaCommand {
                command: Request {
                    command: Command::Leave,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn unsolicited_or_invalid_status_cannot_activate_capture() {
        let mut voice = LocalVoice::default();
        voice.complete(999, &Ok(live()));
        assert_eq!(voice.snapshot.state, State::Unavailable);
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let requests = voice.take_requests(ui::time::now());
        let mut invalid = live();
        invalid.capture_active = true;
        voice.complete(request_id(&requests[0]), &Ok(invalid));
        assert_eq!(voice.snapshot.state, State::Unavailable);
        assert!(voice.snapshot.muted);
        assert!(!voice.snapshot.capture_active);
    }

    fn watch_of(request: &CollabRequest) -> Option<u64> {
        match request {
            CollabRequest::NativeMediaCommand {
                command:
                    Request {
                        command: Command::Watch { revision },
                        ..
                    },
                ..
            } => Some(*revision),
            _ => None,
        }
    }

    #[test]
    fn one_watch_is_outstanding_and_the_next_waits_past_what_was_seen() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        let first = voice.take_requests(now);
        assert_eq!(first.len(), 1);
        assert_eq!(watch_of(&first[0]), Some(0), "nothing seen yet");
        assert!(
            voice.take_requests(now + Duration::from_secs(5)).is_empty(),
            "no poll while the watch is held"
        );
        let mut unavailable = Snapshot::unavailable();
        unavailable.revision = 7;
        voice.complete(request_id(&first[0]), &Ok(unavailable.clone()));
        let next = voice.take_requests(now + Duration::from_secs(5));
        assert_eq!(next.len(), 1);
        assert_eq!(watch_of(&next[0]), Some(7), "the next waits past it");
        assert!(
            !voice.complete(request_id(&next[0]), &Ok(unavailable)),
            "an unchanged answer changes nothing"
        );

        // Someone joins the standup: the held watch answers with it.
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(now + Duration::from_secs(5));
        assert_eq!(join.len(), 1, "a join waits for no watch");
        assert!(voice.complete(request_id(&join[0]), &Ok(live())));
        let held = voice.take_requests(now + Duration::from_secs(5));
        let mut joined = live();
        joined.rosters = vec![ui::native_media::Roster {
            channel_id: "voice".into(),
            people: vec!["a".repeat(64)],
            ..Default::default()
        }];
        joined.speaking = vec!["a".repeat(64)];
        joined.revision = 9;
        assert!(voice.complete(request_id(&held[0]), &Ok(joined)));
        assert_eq!(voice.snapshot.speaking, vec!["a".repeat(64)]);
        assert_eq!(voice.snapshot.rosters.len(), 1);
    }

    #[test]
    fn a_watch_answered_under_a_newer_intent_takes_only_who_is_in_the_call() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(now);
        voice.complete(request_id(&join[0]), &Ok(live()));
        let watch = voice.take_requests(now);
        voice.submit(Command::SetMuted { muted: false }).unwrap();
        let unmute = voice.take_requests(now);
        let mut stale = live();
        stale.state = State::Reconnecting;
        stale.speaking = vec!["b".repeat(64)];
        voice.complete(request_id(&watch[0]), &Ok(stale));
        assert_eq!(
            voice.snapshot.state,
            State::Live,
            "the unmute in flight decides the call's own state"
        );
        assert_eq!(voice.snapshot.speaking, vec!["b".repeat(64)]);
        voice.complete(request_id(&unmute[0]), &Ok(live()));
    }

    #[test]
    fn semantic_status_reads_coalesce_beside_the_watch() {
        let now = ui::time::now();
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let before = doc.save();
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.reload(&doc);
        app.answer_effect(crate::effects::ChatEffect::JoinCall {
            channel: "c_standup".into(),
        })
        .unwrap();
        let join = app.native_voice.take_requests(now);
        app.native_voice.complete(request_id(&join[0]), &Ok(live()));
        let watch = app.native_voice.take_requests(now);
        assert_eq!(watch.len(), 1);
        assert!(watch_of(&watch[0]).is_some());
        for _ in 0..12 {
            app.answer_effect(crate::effects::ChatEffect::CallStatus {
                channel: "c_standup".into(),
            })
            .unwrap();
        }
        let reads = app.native_voice.take_requests(now + Duration::from_secs(1));
        assert_eq!(reads.len(), 1, "twelve reads are one");
        assert_eq!(app.native_voice.snapshot.state, State::Live);
        assert_eq!(app.native_voice.channel.as_deref(), Some("c_standup"));
        assert!(
            !app.native_voice
                .complete(request_id(&reads[0]), &Ok(live()))
        );
        assert!(
            app.native_voice
                .take_requests(now + Duration::from_millis(1500))
                .is_empty(),
            "the watch is still held"
        );
        assert_eq!(
            doc.save(),
            before,
            "query coalescing writes no shared state"
        );
    }

    #[test]
    fn coalesced_status_keeps_service_failures_terminal_and_leave_immediate() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(now);
        voice.complete(request_id(&join[0]), &Ok(live()));
        let refresh = voice.take_requests(now + Duration::from_millis(500));
        assert!(watch_of(&refresh[0]).is_some());
        assert!(voice.complete(request_id(&refresh[0]), &Err("service refused".into())));
        let stop = voice.take_requests(now + Duration::from_secs(1));
        assert_eq!(stop.len(), 1);
        assert!(matches!(
            &stop[0],
            CollabRequest::NativeMediaCommand {
                command: Request {
                    command: Command::Leave,
                    ..
                },
                ..
            }
        ));
        assert!(voice.channel.is_none());
        assert!(voice.snapshot.muted);
        assert!(!voice.snapshot.capture_active);
    }

    #[test]
    fn public_status_retains_a_safe_client_failure_after_cleanup_then_clears_on_rejoin() {
        let now = ui::time::now();
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.reload(&doc);
        app.answer_effect(crate::effects::ChatEffect::JoinCall {
            channel: "c_standup".into(),
        })
        .unwrap();
        let join = app.native_voice.take_requests(now);
        app.native_voice.complete(request_id(&join[0]), &Ok(live()));
        let refresh = app
            .native_voice
            .take_requests(now + Duration::from_millis(500));
        app.native_voice.complete(
            request_id(&refresh[0]),
            &Err("native media command timed out".into()),
        );
        let stop = app.native_voice.take_requests(now + Duration::from_secs(1));
        let mut ended = Snapshot::unavailable();
        ended.state = State::Ended;
        ended.issue = None;
        app.native_voice.complete(request_id(&stop[0]), &Ok(ended));
        let ui::framework::Effected::Done(output) = app
            .answer_effect(crate::effects::ChatEffect::CallStatus {
                channel: "c_standup".into(),
            })
            .unwrap()
        else {
            panic!("status must answer immediately");
        };
        let output = output.value.unwrap();
        assert_eq!(output["reported"]["state"], "ended");
        assert_eq!(output["clientIssue"], "reply_timeout");
        assert_eq!(output["channel"], serde_json::Value::Null);
        app.answer_effect(crate::effects::ChatEffect::JoinCall {
            channel: "c_standup".into(),
        })
        .unwrap();
        assert_eq!(app.native_voice.client_issue, None);
        let join = app
            .native_voice
            .take_requests(now + Duration::from_millis(1500));
        let join_id = join
            .iter()
            .find_map(|request| match request {
                CollabRequest::NativeMediaCommand {
                    request_id,
                    command:
                        Request {
                            command: Command::Join { .. },
                            ..
                        },
                } => Some(*request_id),
                _ => None,
            })
            .unwrap();
        app.native_voice.complete(
            join_id,
            &Err("private service detail must not escape".into()),
        );
        let ui::framework::Effected::Done(output) = app
            .answer_effect(crate::effects::ChatEffect::CallStatus {
                channel: "c_standup".into(),
            })
            .unwrap()
        else {
            panic!("status must answer immediately");
        };
        let output = output.value.unwrap();
        assert_eq!(output["clientIssue"], "unavailable");
        assert!(!output.to_string().contains("private service detail"));
        assert_eq!(output["reported"]["capture_active"], false);
    }

    /// A join the room refuses, or a grant that never comes, leaves the
    /// voice card up saying why, never a card stuck joining or one that
    /// vanishes; leaving closes it.
    #[test]
    fn a_refused_join_keeps_the_card_with_its_reason_until_left() {
        let now = ui::time::now();
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.reload(&doc);
        let join = |app: &mut crate::app::ChatApp, at| {
            app.answer_effect(crate::effects::ChatEffect::JoinCall {
                channel: "c_standup".into(),
            })
            .unwrap();
            app.native_voice
                .take_requests(at)
                .iter()
                .find_map(|request| match request {
                    CollabRequest::NativeMediaCommand {
                        request_id,
                        command:
                            Request {
                                command: Command::Join { .. },
                                ..
                            },
                    } => Some(*request_id),
                    _ => None,
                })
                .unwrap()
        };

        let refused = join(&mut app, now);
        assert!(
            app.native_voice
                .complete(refused, &Err("the room said no".into()))
        );
        app.project_native_voice();
        assert!(app.call.active, "the card stays");
        assert_eq!(app.native_voice.snapshot.state, State::Ended);
        assert_eq!(
            app.media_error.as_deref(),
            Some(crate::i18n::t("chat-call-issue-unavailable").as_str())
        );
        assert!(
            app.native_voice
                .take_requests(now)
                .iter()
                .all(|request| !matches!(
                    request,
                    CollabRequest::NativeMediaCommand {
                        command: Request {
                            command: Command::Leave,
                            ..
                        },
                        ..
                    }
                ))
        );

        // The service ends a call it never got a grant for, with a reason.
        let pending = join(&mut app, now + Duration::from_secs(3));
        let mut joining = live();
        joining.state = State::Joining;
        app.native_voice.complete(pending, &Ok(joining));
        let mut ended = Snapshot::unavailable();
        ended.state = State::Ended;
        ended.issue = Some(ui::native_media::Issue::Denied);
        let status = app
            .native_voice
            .take_requests(now + Duration::from_secs(4))
            .iter()
            .map(request_id)
            .next()
            .unwrap();
        app.native_voice.complete(status, &Ok(ended));
        app.project_native_voice();
        assert!(app.call.active, "the card stays");
        assert_eq!(
            app.media_error.as_deref(),
            Some(crate::i18n::t("chat-call-issue-denied").as_str())
        );

        app.answer_effect(crate::effects::ChatEffect::LeaveCall {
            channel: "c_standup".into(),
        })
        .unwrap();
        app.project_native_voice();
        assert!(!app.call.active, "leaving closes the card");
        assert!(app.native_voice.channel.is_none());
    }

    #[test]
    fn status_polling_and_pending_commands_are_bounded_and_mute_always_fits() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        assert_eq!(voice.take_requests(now).len(), 1, "the watch");
        assert!(voice.take_requests(now).is_empty());
        for _ in 0..8 {
            voice
                .submit(Command::SelectInput {
                    input_id: "input".into(),
                })
                .unwrap();
        }
        assert!(
            voice
                .submit(Command::SelectInput {
                    input_id: "input".into()
                })
                .is_err()
        );
        voice.submit(Command::SetMuted { muted: true }).unwrap();
        assert_eq!(voice.take_requests(now).len(), 1);
        assert!(voice.snapshot.muted);
    }

    #[test]
    fn a_voice_channel_says_who_is_in_its_call() {
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let mut chat = crate::app::ChatApp::new_ephemeral();
        chat.reload(&doc);
        // The people in the call are the channel's rows in the sidebar.
        let in_call = |chat: &crate::app::ChatApp| -> Vec<String> {
            chat.sidebar_sections()
                .into_iter()
                .flat_map(|section| section.items)
                .find(|item| item.id == crate::sidebar::channel_place("c_standup"))
                .map(|item| item.children.into_iter().map(|row| row.label).collect())
                .unwrap_or_default()
        };
        assert!(in_call(&chat).is_empty());
        chat.voice_members.insert(
            "c_standup".into(),
            vec![crate::model::VoiceMember {
                user: "someone".into(),
                talking: false,
                muted: false,
                deafened: false,
                audio_only: true,
                sharing: false,
                ticket: None,
                video: None,
            }],
        );
        assert_eq!(in_call(&chat), [chat.user_name("someone")]);
    }

    #[test]
    fn reader_pointer_and_resource_join_use_the_same_service_without_document_mutation() {
        use ui::permissions::{PermissionContext, Role};
        use ui::widget::Collaborative;
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let before = doc.save();
        let mut pointer = crate::app::ChatApp::new_ephemeral();
        pointer.reload(&doc);
        pointer.perms = crate::permissions::ChatPermissions::from_context(
            PermissionContext::member("reader", Role::Viewer),
        );
        pointer.on_hit(
            crate::app::Hit::Channel("c_standup".into()),
            &mut ui::widget::Edits::lend(&mut doc),
        );
        let pointer_requests = pointer.take_collab_requests();
        let mut resource = crate::app::ChatApp::new_ephemeral();
        resource.reload(&doc);
        resource.perms = pointer.perms.clone();
        resource
            .answer_effect(crate::effects::ChatEffect::JoinCall {
                channel: "c_standup".into(),
            })
            .unwrap();
        let resource_requests = resource.take_collab_requests();
        let command = |requests: Vec<CollabRequest>| match requests.into_iter().next().unwrap() {
            CollabRequest::NativeMediaCommand { command, .. } => command,
            _ => panic!("native service expected"),
        };
        assert_eq!(command(pointer_requests), command(resource_requests));
        assert_eq!(doc.save(), before);
        assert!(pointer.call.muted);
        assert!(
            pointer
                .native_voice_command("c_standup", Command::SetMuted { muted: false })
                .is_err()
        );
    }

    /// A join is listening, muted; a refused microphone keeps the person in
    /// the call, muted, and the call says why instead of ending.
    #[test]
    fn a_join_is_muted_and_a_refused_microphone_stays_in_the_call() {
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "c_standup".into(),
            })
            .unwrap();
        let join = request_id(&voice.take_requests(Instant::now())[0]);
        // Even a service that answers unmuted is shown muted: no one asked.
        let mut answered = live();
        answered.muted = false;
        answered.capture_active = true;
        answered.input_id = Some("mic".into());
        voice.complete(join, &Ok(answered));
        assert!(voice.snapshot.muted && !voice.snapshot.capture_active);
        voice.submit(Command::SetMuted { muted: false }).unwrap();
        let unmute = voice
            .take_requests(Instant::now())
            .iter()
            .map(request_id)
            .max()
            .unwrap();
        let mut refused = live();
        refused.issue = Some(ui::native_media::Issue::PermissionDenied);
        voice.complete(unmute, &Ok(refused));
        assert_eq!(voice.channel.as_deref(), Some("c_standup"), "still in it");
        assert_eq!(voice.snapshot.state, State::Live);
        assert!(voice.snapshot.muted);
        assert_eq!(
            voice.snapshot.issue,
            Some(ui::native_media::Issue::PermissionDenied)
        );
    }

    #[test]
    fn joining_panel_is_visible_without_claiming_live_capture_or_streams() {
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.reload(&doc);
        app.native_voice_command(
            "c_standup",
            Command::Join {
                channel_id: "c_standup".into(),
            },
        )
        .unwrap();
        assert!(
            app.call.active,
            "the joining/unavailable status must be visible"
        );
        assert_eq!(app.native_voice.snapshot.state, State::Preparing);
        assert!(app.call.muted);
        assert!(!app.native_voice.snapshot.capture_active);
        assert!(app.native_voice.snapshot.publishers.is_empty());
    }

    fn person(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    #[test]
    fn a_tile_asks_for_the_smallest_layer_that_covers_it() {
        use ui::native_media::{PictureLayer, PictureSource};
        assert_eq!(
            layer_for(200.0, 112.0, PictureSource::Camera),
            PictureLayer::Low
        );
        assert_eq!(
            layer_for(480.0, 270.0, PictureSource::Camera),
            PictureLayer::Mid
        );
        assert_eq!(
            layer_for(1100.0, 620.0, PictureSource::Camera),
            PictureLayer::High
        );
        // A screen has no middle layer: past its preview it is the screen.
        assert_eq!(
            layer_for(480.0, 270.0, PictureSource::Screen),
            PictureLayer::High
        );
    }

    #[test]
    fn the_call_is_asked_for_exactly_the_pictures_its_tiles_show_of_people_who_show_them() {
        use ui::native_media::{PictureLayer, PictureSource, Roster, Shown};
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.reload(&doc);
        app.presented = true;
        app.call.join("c_standup");
        app.native_voice.snapshot.rosters = vec![Roster {
            channel_id: "c_standup".into(),
            people: vec![person(1), person(2), person(3)],
            cameras: vec![person(1), person(3)],
            screens: vec![person(3)],
            ..Default::default()
        }];
        {
            let mut tiles = app.picture_tiles.borrow_mut();
            tiles.push((person(1), PictureSource::Camera, (500.0, 280.0)));
            // Their camera is off: nothing is asked for, whatever the tile.
            tiles.push((person(2), PictureSource::Camera, (500.0, 280.0)));
            tiles.push((person(3), PictureSource::Camera, (200.0, 110.0)));
            tiles.push((person(3), PictureSource::Screen, (1600.0, 900.0)));
        }
        assert_eq!(
            app.wanted_pictures(),
            vec![
                Shown {
                    person: person(1),
                    source: PictureSource::Camera,
                    layer: PictureLayer::Mid,
                },
                Shown {
                    person: person(3),
                    source: PictureSource::Camera,
                    layer: PictureLayer::Low,
                },
                Shown {
                    person: person(3),
                    source: PictureSource::Screen,
                    layer: PictureLayer::High,
                },
            ]
        );
        app.presented = false;
        assert!(app.wanted_pictures().is_empty());
    }

    #[test]
    fn a_live_call_tells_the_service_what_shows_and_keeps_one_collection_outstanding() {
        use ui::native_media::{Picture, PictureLayer, PictureSource, Shown};
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(now);
        voice.complete(request_id(&join[0]), &Ok(live()));
        let shown = vec![Shown {
            person: person(1),
            source: PictureSource::Camera,
            layer: PictureLayer::Low,
        }];
        voice.video.wanted = shown.clone();
        let requests = voice.take_requests(now);
        let command = |request: &CollabRequest| match request {
            CollabRequest::NativeMediaCommand { command, .. } => command.command.clone(),
            _ => panic!("only native media requests"),
        };
        let show = requests
            .iter()
            .find(|request| matches!(command(request), Command::Show { .. }))
            .expect("the service hears what shows");
        assert_eq!(command(show), Command::Show { pictures: shown });
        let collect = requests
            .iter()
            .find(|request| matches!(command(request), Command::Pictures { after: 0 }))
            .expect("and its pictures are collected");
        // Nothing more while both are in flight.
        assert!(voice.take_requests(now).iter().all(|request| !matches!(
            command(request),
            Command::Show { .. } | Command::Pictures { .. }
        )));
        voice.complete(request_id(show), &Ok(live()));
        let mut answer = live();
        answer.pictures = vec![Picture {
            person: person(1),
            stream: "s".into(),
            source: PictureSource::Camera,
            object: vec![1; 40],
        }];
        answer.pictures_cursor = 5;
        assert!(!voice.complete(request_id(collect), &Ok(answer)));
        assert_eq!(voice.video.arrived.len(), 1, "for the decoder");
        let next = voice.take_requests(now);
        assert!(
            next.iter()
                .any(|request| command(request) == Command::Pictures { after: 5 }),
            "the next collection continues where the last ended"
        );
        voice.submit(Command::Leave).unwrap();
        assert!(
            voice
                .take_requests(now)
                .iter()
                .all(|request| !matches!(command(request), Command::Pictures { .. }))
        );
    }

    /// The Standup's call with Ana in it and Dana and Ben, two people on
    /// Discord a bridge carries in, Ben linked to Ana's room member.
    fn bridged_standup() -> crate::app::ChatApp {
        use ui::native_media::{Guest, Roster};
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.reload(&doc);
        app.native_voice.snapshot.rosters = vec![Roster {
            channel_id: "c_standup".into(),
            people: vec!["u_aria".into()],
            guests: vec![
                Guest {
                    id: "discord:1".into(),
                    name: "Dana".into(),
                    avatar: Some("ab".repeat(32)),
                    service: "discord".into(),
                    linked: None,
                    muted: false,
                },
                Guest {
                    id: "discord:2".into(),
                    name: "benji".into(),
                    avatar: None,
                    service: "discord".into(),
                    linked: Some("u_milo".into()),
                    muted: false,
                },
            ],
            bridges: vec!["discord".into()],
            ..Default::default()
        }];
        app.native_voice.snapshot.speaking = vec!["discord:1".into()];
        app.refresh_native_streams();
        app
    }

    #[test]
    fn a_bridged_call_shows_each_guest_as_themselves_via_their_service() {
        let mut app = bridged_standup();
        let members: Vec<(&str, bool)> = app
            .voice_in("c_standup")
            .iter()
            .map(|member| (member.user.as_str(), member.talking))
            .collect();
        assert_eq!(
            members,
            [("u_aria", false), ("discord:1", true), ("discord:2", false)],
            "each guest is their own participant, talking on their own"
        );
        assert_eq!(app.call_name("discord:1"), "Dana (via Discord)");
        assert_eq!(app.call_face_name("discord:1"), "Dana");
        assert_eq!(
            app.call_name("discord:2"),
            "benji (via Discord)",
            "linked to a member this window does not know: as the bridge says"
        );
        app.users.push(crate::model::User {
            id: "u_milo".into(),
            name: "Milo".into(),
            initials: "M".into(),
            color_idx: 0,
            status: crate::model::Presence::Online,
            is_agent: false,
        });
        assert_eq!(
            app.call_name("discord:2"),
            "Milo (via Discord)",
            "a linked guest is their member, reached through Discord"
        );
        assert_eq!(app.call_colour("discord:2"), app.identity_colour("u_milo"));
        assert_eq!(app.call_name("u_aria"), app.user_name("u_aria"));
        assert_eq!(
            app.call_bridged_note("c_standup").as_deref(),
            Some("This call is bridged to Discord")
        );
        assert_eq!(app.call_bridged_note("c_huddle"), None);
    }

    #[test]
    fn the_sidebar_names_guests_in_the_voice_channel_and_says_it_is_bridged() {
        let app = bridged_standup();
        let sections = app.sidebar_sections();
        let standup = sections
            .iter()
            .flat_map(|section| &section.items)
            .find(|item| item.id.ends_with("c_standup"))
            .expect("the standup row");
        assert_eq!(standup.secondary.as_deref(), Some("Bridged to Discord"));
        let dana = standup
            .children
            .iter()
            .find(|child| child.id.ends_with("/discord:1"))
            .expect("Dana sits in the call");
        assert_eq!(dana.label, "Dana (via Discord)");
        let face = dana.person.as_ref().expect("her face");
        assert_eq!(face.name.as_deref(), Some("Dana"));
    }

    #[test]
    fn a_participants_volume_is_told_once_the_call_is_live_and_again_in_the_next() {
        let now = ui::time::now();
        let mut app = bridged_standup();
        app.set_call_listening(
            "discord:1",
            Listening {
                percent: 150,
                silenced: true,
            },
        );
        let volume = |requests: &[CollabRequest]| {
            requests.iter().find_map(|request| match request {
                CollabRequest::NativeMediaCommand {
                    request_id,
                    command:
                        Request {
                            command:
                                Command::SetVolume {
                                    participant,
                                    percent,
                                },
                            intent,
                            ..
                        },
                } => Some((*request_id, participant.clone(), *percent, *intent)),
                _ => None,
            })
        };
        assert_eq!(
            volume(&app.native_voice.take_requests(now)),
            None,
            "not outside a call"
        );
        app.native_voice
            .submit(Command::Join {
                channel_id: "c_standup".into(),
            })
            .unwrap();
        let join = app.native_voice.take_requests(now);
        app.native_voice.complete(request_id(&join[0]), &Ok(live()));
        let (id, participant, percent, intent) =
            volume(&app.native_voice.take_requests(now)).expect("told once live");
        assert_eq!((participant.as_str(), percent, intent), ("discord:1", 0, 0));
        assert_eq!(
            volume(&app.native_voice.take_requests(now)),
            None,
            "one at a time"
        );
        app.native_voice.complete(id, &Ok(live()));
        assert_eq!(volume(&app.native_voice.take_requests(now)), None, "told");
        app.set_call_listening(
            "discord:1",
            Listening {
                percent: 150,
                silenced: false,
            },
        );
        let (_, _, percent, _) = volume(&app.native_voice.take_requests(now)).expect("heard again");
        assert_eq!(percent, 150);
    }
}
