//! Ephemeral client of the shell's governed voice service. Nothing here is
//! serialized into the Chat document or treated as microphone consent.
use std::collections::BTreeMap;
use ui::{
    collaboration::CollabRequest,
    native_media::{Command, Request, Snapshot, State},
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
    last_poll: Option<Instant>,
    pending_since: Option<Instant>,
    unmute_requested: bool,
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
            last_poll: None,
            pending_since: None,
            unmute_requested: false,
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
                self.snapshot = Snapshot::unavailable();
                self.snapshot.state = State::Preparing;
                self.snapshot.issue = None;
                self.unmute_requested = false;
            }
            Command::Leave => {
                self.channel = None;
                self.snapshot = Snapshot::unavailable();
                self.snapshot.state = State::Ended;
                self.snapshot.issue = None;
                self.unmute_requested = false;
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
            Command::SelectInput { .. } | Command::Status => {}
        }
        self.pending.insert(id, command);
        self.queued.push(CollabRequest::NativeMediaCommand {
            request_id: id,
            command: request,
        });
        Ok(())
    }
    pub(crate) fn take_requests(&mut self, now: Instant) -> Vec<CollabRequest> {
        if self
            .pending_since
            .is_some_and(|sent| now.saturating_duration_since(sent) >= Duration::from_secs(2))
        {
            self.invalidate("reply_timeout");
        }
        if self.pending.is_empty()
            && self.queued.is_empty()
            && self
                .last_poll
                .is_none_or(|last| now.saturating_duration_since(last) >= self.status_interval())
        {
            let _ = self.submit(Command::Status);
        }
        if !self.queued.is_empty() {
            self.last_poll = Some(now);
            if self.pending_since.is_none() {
                self.pending_since = Some(now);
            }
        }
        std::mem::take(&mut self.queued)
    }
    fn status_interval(&self) -> Duration {
        if self.channel.is_some() {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(5)
        }
    }

    pub(crate) fn complete(&mut self, request_id: u64, result: &Result<Snapshot, String>) -> bool {
        if self.pending.remove(&request_id).is_none()
            || request_id < self.intent_floor
            || request_id < self.applied
        {
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
                // Transport errors may carry private details. Export only
                // these fixed protocol categories, never the error itself.
                let issue = match error.as_str() {
                    "native media command timed out" => "reply_timeout",
                    "native media command superseded" => "reply_superseded",
                    "native media command refused" => "command_refused",
                    _ => "unavailable",
                };
                self.invalidate(issue);
                return before != self.snapshot || before_channel != self.channel;
            }
        };
        if !snapshot.valid() {
            self.invalidate("invalid_status");
            return before != self.snapshot || before_channel != self.channel;
        }
        self.snapshot = snapshot.clone();
        if !self.unmute_requested {
            self.snapshot.muted = true;
            self.snapshot.capture_active = false;
        }
        if matches!(
            snapshot.state,
            State::Unavailable | State::Idle | State::Ended
        ) {
            if snapshot.state != State::Unavailable {
                self.channel = None;
            }
            self.unmute_requested = false;
            self.snapshot.muted = true;
            self.snapshot.capture_active = false;
        }
        before != self.snapshot || before_channel != self.channel
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
            Command::SetMuted { muted: false } => CallAction::Speak,
            Command::SetMuted { muted: true } | Command::Leave => CallAction::Leave,
            Command::SelectInput { .. } | Command::Status => CallAction::Join,
        };
        if !self.call_allows(action) {
            return Err("Call action is not permitted".into());
        }
        if !matches!(&command, Command::Join { .. } | Command::Status)
            && self
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

    pub(crate) fn project_native_voice(&mut self) {
        let snapshot = &self.native_voice.snapshot;
        // The panel represents explicit local intent, including an unavailable
        // attempt. Only Snapshot::Live represents current service readiness.
        let joined = self.native_voice.channel.is_some()
            && !matches!(snapshot.state, State::Idle | State::Ended);
        if joined {
            if let Some(channel) = &self.native_voice.channel {
                if !self.call.active || self.call.channel != *channel {
                    self.call.join(channel);
                }
                self.call.muted = snapshot.muted;
                self.call.audio_only = true;
                self.call.sharing = false;
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
            })
        });
        self.refresh_native_streams();
        self.request_relayout();
    }

    pub(crate) fn refresh_native_streams(&mut self) {
        self.voice_members.clear();
        let snapshot = &self.native_voice.snapshot;
        if snapshot.state != State::Live {
            return;
        }
        let Some(channel) = &self.native_voice.channel else {
            return;
        };
        let streams = snapshot
            .publishers
            .iter()
            .map(|namespace| crate::model::VoiceMember {
                user: namespace.clone(),
                talking: false,
                muted: false,
                deafened: false,
                audio_only: true,
                sharing: false,
                ticket: None,
                video: None,
            })
            .collect();
        self.voice_members.insert(channel.clone(), streams);
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
                    command: Command::Status,
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
        assert_eq!(status.len(), 1);
        let stopping = voice.take_requests(now + Duration::from_millis(2500));
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

    #[test]
    fn idle_status_poll_backs_off_and_unchanged_replies_are_silent() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        let first = voice.take_requests(now);
        assert_eq!(first.len(), 1);
        assert!(!voice.complete(request_id(&first[0]), &Ok(Snapshot::unavailable())));
        assert!(
            voice
                .take_requests(now + Duration::from_millis(500))
                .is_empty(),
            "idle Chat does not poll voice status every 500 ms"
        );
        let later = voice.take_requests(now + Duration::from_secs(5));
        assert_eq!(
            later.len(),
            1,
            "an idle session still notices a late voice service"
        );
        assert!(!voice.complete(request_id(&later[0]), &Ok(Snapshot::unavailable())));

        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(now + Duration::from_secs(5));
        assert!(voice.complete(request_id(&join[0]), &Ok(live())));
        assert_eq!(
            voice
                .take_requests(now + Duration::from_secs(5) + Duration::from_millis(500))
                .len(),
            1,
            "an active call still polls twice a second"
        );

        let same = live();
        let mut voice = LocalVoice::default();
        voice
            .submit(Command::Join {
                channel_id: "voice".into(),
            })
            .unwrap();
        let join = voice.take_requests(now);
        assert!(voice.complete(request_id(&join[0]), &Ok(same.clone())));
        let status = voice.take_requests(now + Duration::from_millis(500));
        assert!(!voice.complete(request_id(&status[0]), &Ok(same)));
    }

    #[test]
    fn semantic_status_reads_share_the_in_flight_automatic_refresh() {
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
        let refresh = app
            .native_voice
            .take_requests(now + Duration::from_millis(500));
        assert_eq!(refresh.len(), 1);
        for _ in 0..12 {
            app.answer_effect(crate::effects::ChatEffect::CallStatus {
                channel: "c_standup".into(),
            })
            .unwrap();
        }
        assert!(
            app.native_voice
                .take_requests(now + Duration::from_secs(1))
                .is_empty()
        );
        assert_eq!(app.native_voice.snapshot.state, State::Live);
        assert_eq!(app.native_voice.channel.as_deref(), Some("c_standup"));
        assert!(
            !app.native_voice
                .complete(request_id(&refresh[0]), &Ok(live()))
        );
        let next = app
            .native_voice
            .take_requests(now + Duration::from_millis(1500));
        assert_eq!(next.len(), 1, "a later refresh still crosses the transport");
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
        voice.submit(Command::Status).unwrap();
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
            &Err("native media command superseded".into()),
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
        assert_eq!(output["clientIssue"], "reply_superseded");
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

    #[test]
    fn status_polling_and_pending_commands_are_bounded_and_mute_always_fits() {
        let now = ui::time::now();
        let mut voice = LocalVoice::default();
        assert_eq!(voice.take_requests(now).len(), 1);
        assert!(voice.take_requests(now).is_empty());
        for _ in 0..7 {
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
    fn reader_pointer_and_resource_join_use_the_same_service_without_document_mutation() {
        use ui::permissions::{Actor, PermissionContext, Role};
        use ui::widget::Collaborative;
        let mut doc = automerge::AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        let before = doc.save();
        let mut pointer = crate::app::ChatApp::new_ephemeral();
        pointer.reload(&doc);
        pointer.perms = crate::permissions::ChatPermissions::from_context(
            PermissionContext::from_capabilities(
                Actor::person(Some(Role::Viewer), "reader"),
                vec![],
            ),
        );
        pointer.on_hit(
            crate::app::Hit::VoiceChannel("c_standup".into()),
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
}
