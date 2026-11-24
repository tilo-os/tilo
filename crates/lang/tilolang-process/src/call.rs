//! The system's calls an instance's views take part in: who is in the call held on each resource,
//! as the shell's call service says on the instance's own connection, and the steps the person
//! takes in one (join, leave, mute, deafen, camera, screen), carried out through that service.
//!
//! The service is the shell's governed voice service (`CollabRequest::NativeMediaCommand`): it
//! binds every command to the connection's own app, workspace and record, asks for the
//! microphone, camera and screen through the system's prompts, and holds the call on the
//! workspace's live session. Nothing of a call is stored in the app's record. One watch is
//! outstanding while the instance runs, so the people of every call of the record show the moment
//! someone joins, leaves, talks or mutes, joined or not.

use std::collections::{BTreeMap, HashMap};

use collaboration::{CollabEvent, CollabRequest};
use tilolang_tilo::instance::Instance;
use tilolang_tilo::runtime::platform::call_place;
use tilolang_tilo::runtime::value::ResourceRef;
use tilolang_tilo::runtime::{Actor, Bridged, CallMember, CallStep};
use ui_core::native_media::{Command, Issue, REPLY_LOST, Request, Snapshot, State, WATCH_LOST};
use ui_core::permissions::MemberId;
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
            CallStep::Mute(_) | CallStep::Deafen(_) | CallStep::Camera(_) | CallStep::Screen(_) => {
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
            .watch
            .and_then(|(_, sent)| sent.checked_add(WATCH_LOST))
            .into_iter()
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
        if !std::mem::take(&mut self.changed) {
            return;
        }
        instance.set_calls(self.members());
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
            bridged: None,
        }
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
