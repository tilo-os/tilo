//! Semantic, capture-free adapter between Chat calls and Tilo realtime
//! continuity. Capture handles, peer tickets, device ids, credentials, and
//! voice membership never enter this projection.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use crate::app::ChatApp;
use crate::model::ChannelKind;

pub const CHAT_CONTINUATION_SCHEMA: u16 = ui::collaboration::semantic_schema::REALTIME_SESSION_V1;
const CHAT_REALTIME_PROVIDER: &str = "tilo.chat";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatCallContinuation {
    pub workspace_id: String,
    pub channel_id: String,
    pub media: ui::realtime_continuity::RealtimeMediaFacets,
    pub controls: ui::realtime_continuity::RealtimeControlIntent,
}

impl ChatCallContinuation {
    #[must_use]
    pub fn valid(&self) -> bool {
        portable_id(&self.workspace_id)
            && portable_id(&self.channel_id)
            && self.media.valid()
            && self.media.microphone
            && !self.media.share_audio
    }

    #[must_use]
    pub fn resume_intent(&self) -> ui::realtime_continuity::RealtimeResumeIntent {
        ui::realtime_continuity::RealtimeResumeIntent {
            provider: CHAT_REALTIME_PROVIDER.into(),
            room_id: self.channel_id.clone(),
            semantic_position: Some(self.workspace_id.clone()),
            media: self.media,
            controls: self.controls,
        }
    }
}

#[derive(Default)]
pub(crate) struct ChatRealtimeContinuity {
    pub(crate) session: Option<String>,
    pub(crate) registration_pending: bool,
    pub(crate) pending_restore: Option<ChatCallContinuation>,
    pub(crate) offer: Option<(String, ChatCallContinuation)>,
    pub(crate) stop_source: Option<String>,
    /// Set by the accepted command result and consumed from `sync`, so the
    /// target call is locally established before `MediaReady` is emitted.
    pub(crate) accepted_target: bool,
    pub(crate) target_media_ready: bool,
    pub(crate) requests: Vec<ui::collaboration::CollabRequest>,
    next_request: u64,
}

impl ChatRealtimeContinuity {
    fn request(
        &mut self,
        command: ui::realtime_continuity::RealtimeCommand,
    ) -> ui::collaboration::CollabRequest {
        self.next_request = self.next_request.saturating_add(1);
        ui::collaboration::CollabRequest::RealtimeCommand {
            request_id: self.next_request,
            app_id: ui::collaboration::local_app_id(),
            command,
        }
    }
}

impl ChatApp {
    pub(crate) fn call_continuation(&self) -> Option<ChatCallContinuation> {
        if !self.call.active
            || self.native_voice.snapshot.state != ui::native_media::State::Live
            || self.native_voice.channel.as_deref() != Some(&self.call.channel)
        {
            return None;
        }
        let channel = self.channels.iter().find(|channel| {
            channel.id == self.call.channel && channel.kind == ChannelKind::Voice
        })?;
        let continuation = ChatCallContinuation {
            workspace_id: crate::app::ROOM_SCOPE.to_owned(),
            channel_id: channel.id.clone(),
            media: ui::realtime_continuity::RealtimeMediaFacets {
                microphone: true,
                camera: false,
                screen_share: false,
                // Chat does not currently publish system/share audio as a
                // distinct track. Never infer it from screen capture.
                share_audio: false,
            },
            controls: ui::realtime_continuity::RealtimeControlIntent {
                microphone_muted: self.native_voice.snapshot.muted,
                camera_muted: true,
                screen_share_paused: false,
                share_audio_muted: true,
            },
        };
        continuation.valid().then_some(continuation)
    }

    pub(crate) fn restore_call_continuation(
        &mut self,
        metadata: &ui::collaboration::ContinuationMetadata,
        doc: &AutoCommit,
    ) -> bool {
        if metadata.schema_version != CHAT_CONTINUATION_SCHEMA {
            return false;
        }
        let Ok(continuation) = serde_json::from_slice::<ChatCallContinuation>(&metadata.bytes)
        else {
            return false;
        };
        let Some(chat) = crate::projection::read(doc) else {
            return false;
        };
        if !continuation.valid()
            || !chat.channel_rows().iter().any(|channel| {
                channel.id == continuation.channel_id && channel.kind == ChannelKind::Voice
            })
        {
            return false;
        }
        // Restore is deliberately inert. It records where a later explicit
        // broker Offer may resume, but never joins or opens capture itself.
        self.realtime.pending_restore = Some(continuation);
        true
    }

    pub(crate) fn queue_realtime_registration(&mut self) {
        if self.realtime.session.is_some() || self.realtime.registration_pending {
            return;
        }
        let Some(continuation) = self.call_continuation() else {
            return;
        };
        self.realtime.registration_pending = true;
        let request = self
            .realtime
            .request(ui::realtime_continuity::RealtimeCommand::Register {
                intent: continuation.resume_intent(),
            });
        self.realtime.requests.push(request);
    }

    /// Called only with fresh opaque capabilities issued by the target-local
    /// capture broker. A restored checkpoint alone never reaches this edge.
    pub(crate) fn accept_realtime_offer(&mut self, capture_authorizations: Vec<String>) -> bool {
        if capture_authorizations.is_empty() {
            return false;
        }
        let Some((session, offered)) = self.realtime.offer.clone() else {
            return false;
        };
        if self.realtime.pending_restore.as_ref() != Some(&offered) {
            return false;
        }
        let request = self
            .realtime
            .request(ui::realtime_continuity::RealtimeCommand::Accept {
                session,
                capture_authorizations,
            });
        self.realtime.requests.push(request);
        true
    }

    /// Notify the broker only after Chat's target media endpoint is actually
    /// established. This is separate from Accept so failures leave the source
    /// call live.
    pub(crate) fn realtime_target_media_ready(&mut self) -> bool {
        let Some((session, offered)) = self.realtime.offer.clone() else {
            return false;
        };
        if !self.call.active
            || self.call.channel != offered.channel_id
            || self.native_voice.snapshot.state != ui::native_media::State::Live
            || self.native_voice.channel.as_deref() != Some(&offered.channel_id)
        {
            return false;
        }
        let request = self
            .realtime
            .request(ui::realtime_continuity::RealtimeCommand::MediaReady { session });
        self.realtime.requests.push(request);
        true
    }

    pub(crate) fn establish_realtime_target(&mut self) {
        if !self.realtime.accepted_target {
            return;
        }
        self.realtime.accepted_target = false;
        let Some((session, offered)) = self.realtime.offer.clone() else {
            return;
        };
        if self.realtime.pending_restore.as_ref() != Some(&offered) {
            return;
        }
        // Continuity intent never carries microphone consent or device state.
        // A fresh native Join starts listening muted; unmute remains a separate
        // local prompt even if the source previously had its microphone open.
        if self
            .native_voice_command(
                &offered.channel_id,
                ui::native_media::Command::Join {
                    channel_id: offered.channel_id.clone(),
                },
            )
            .is_err()
        {
            let request = self
                .realtime
                .request(ui::realtime_continuity::RealtimeCommand::TargetFailed { session });
            self.realtime.requests.push(request);
            self.realtime.offer = None;
            return;
        }
        self.realtime.session = Some(session);
        self.realtime.target_media_ready = true;
    }

    pub(crate) fn apply_realtime_stop(&mut self) {
        if self.realtime.stop_source.take().is_none() || !self.call.active {
            return;
        }
        let channel = self.call.channel.clone();
        let _ = self.native_voice_command(&channel, ui::native_media::Command::Leave);
    }
}

fn portable_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
}

#[cfg(test)]
mod tests {
    use automerge::ROOT;
    use ui::widget::{Collaborative, Continuable, Widget};

    use super::*;

    fn app_and_doc() -> (ChatApp, AutoCommit) {
        let mut app = ChatApp::new_ephemeral();
        let mut doc = AutoCommit::new();
        crate::demo::seed(&mut doc).unwrap();
        Widget::sync(&mut app, &doc, &ROOT);
        (app, doc)
    }

    fn service_live(app: &mut ChatApp) {
        app.native_voice_command(
            "c_huddle",
            ui::native_media::Command::Join {
                channel_id: "c_huddle".into(),
            },
        )
        .unwrap();
        let requests = app.native_voice.take_requests(ui::time::Instant::now());
        let id = requests
            .iter()
            .rev()
            .find_map(|request| match request {
                ui::collaboration::CollabRequest::NativeMediaCommand {
                    request_id,
                    command,
                } if matches!(command.command, ui::native_media::Command::Join { .. }) => {
                    Some(*request_id)
                }
                _ => None,
            })
            .unwrap();
        let mut snapshot = ui::native_media::Snapshot::unavailable();
        snapshot.state = ui::native_media::State::Live;
        snapshot.issue = None;
        Collaborative::handle_collab_event(
            app,
            &ui::collaboration::CollabEvent::NativeMediaCommandResult {
                request_id: id,
                result: Ok(snapshot),
            },
        );
    }

    #[test]
    fn native_continuity_cannot_claim_ready_or_resume_capture_from_view_state() {
        let (mut app, _) = app_and_doc();
        let offered = ChatCallContinuation {
            workspace_id: "ws_acme".into(),
            channel_id: "c_huddle".into(),
            media: ui::realtime_continuity::RealtimeMediaFacets {
                microphone: true,
                camera: true,
                ..Default::default()
            },
            controls: ui::realtime_continuity::RealtimeControlIntent {
                microphone_muted: false,
                camera_muted: false,
                ..Default::default()
            },
        };
        app.realtime.pending_restore = Some(offered.clone());
        app.realtime.offer = Some(("target".into(), offered));
        app.call.join("c_huddle");
        assert!(!app.realtime_target_media_ready());
        assert!(app.call_continuation().is_none());
        app.realtime.accepted_target = true;
        app.establish_realtime_target();
        assert!(app.call.muted && app.call.audio_only && !app.call.sharing);
        assert_eq!(
            app.native_voice.snapshot.state,
            ui::native_media::State::Preparing
        );
        assert!(!app.realtime_target_media_ready());
    }

    #[test]
    fn realtime_request_uses_the_launched_package_id() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_PACKAGE_ID", "so.tilo.chat");
        let mut realtime = ChatRealtimeContinuity::default();
        let request = realtime.request(ui::realtime_continuity::RealtimeCommand::TargetFailed {
            session: "session-1".into(),
        });
        drop(env);
        assert!(matches!(
            request,
            ui::collaboration::CollabRequest::RealtimeCommand { app_id, .. }
                if app_id == "so.tilo.chat"
        ));
    }

    #[test]
    fn call_projection_is_semantic_and_restoring_it_is_inert() {
        // Phase: active_call_projection_is_semantic_and_contains_no_transport_or_capture_state.
        {
            let (mut app, doc) = app_and_doc();
            app.call.join("c_huddle");
            service_live(&mut app);
            app.call.muted = true;
            app.call.sharing = true;
            let metadata = Continuable::continuation_metadata(&app, &doc, &ROOT).unwrap();
            let json = String::from_utf8(metadata.bytes.clone()).unwrap();
            assert!(json.contains("c_huddle") && json.contains(crate::app::ROOM_SCOPE));
            for forbidden in [
                "sim://",
                "pipewire",
                "device",
                "ticket",
                "credential",
                "member",
            ] {
                assert!(!json.to_ascii_lowercase().contains(forbidden));
            }
            let projection: ChatCallContinuation = serde_json::from_slice(&metadata.bytes).unwrap();
            assert!(
                projection.media.microphone
                    && !projection.media.screen_share
                    && !projection.media.camera
            );
            assert!(projection.controls.microphone_muted);
            assert!(!projection.media.share_audio);
        }

        // Phase: restore_is_inert_and_requires_a_real_synced_voice_channel.
        {
            let (mut source, source_doc) = app_and_doc();
            service_live(&mut source);
            let metadata = Continuable::continuation_metadata(&source, &source_doc, &ROOT).unwrap();
            let (mut target, mut target_doc) = app_and_doc();
            assert!(Continuable::restore_continuation_metadata(
                &mut target,
                &metadata,
                &mut ui::widget::Edits::lend(&mut target_doc),
                &ROOT,
            ));
            assert!(!target.call.active, "restore must not join or start media");
            assert!(target.realtime.pending_restore.is_some());

            let mut unknown: ChatCallContinuation =
                serde_json::from_slice(&metadata.bytes).unwrap();
            unknown.channel_id = "c_not_in_document".into();
            let unknown = ui::collaboration::ContinuationMetadata {
                schema_version: CHAT_CONTINUATION_SCHEMA,
                bytes: serde_json::to_vec(&unknown).unwrap(),
            };
            assert!(!Continuable::restore_continuation_metadata(
                &mut target,
                &unknown,
                &mut ui::widget::Edits::lend(&mut target_doc),
                &ROOT,
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn target_starts_media_only_after_fresh_capture_consent() {
        // Phase: accept_needs_fresh_capture_and_media_ready_is_a_separate_edge.
        {
            let (mut app, doc) = app_and_doc();
            let projection = ChatCallContinuation {
                workspace_id: "ws_acme".into(),
                channel_id: "c_huddle".into(),
                media: ui::realtime_continuity::RealtimeMediaFacets {
                    microphone: true,
                    ..Default::default()
                },
                controls: ui::realtime_continuity::RealtimeControlIntent::default(),
            };
            let metadata = ui::collaboration::ContinuationMetadata {
                schema_version: CHAT_CONTINUATION_SCHEMA,
                bytes: serde_json::to_vec(&projection).unwrap(),
            };
            assert!(app.restore_call_continuation(&metadata, &doc));
            app.realtime.offer = Some(("session-1".into(), projection));
            assert!(!app.accept_realtime_offer(Vec::new()));
            assert!(app.realtime.requests.is_empty());
            assert!(app.accept_realtime_offer(vec![
                "ab".repeat(ui::realtime_continuity::OPAQUE_CAPTURE_AUTHORIZATION_BYTES,)
            ]));
            assert!(matches!(
                app.realtime.requests.pop(),
                Some(ui::collaboration::CollabRequest::RealtimeCommand {
                    command: ui::realtime_continuity::RealtimeCommand::Accept { .. },
                    ..
                })
            ));
            assert!(!app.realtime_target_media_ready());
            app.call.join("c_huddle");
            assert!(!app.realtime_target_media_ready());
            service_live(&mut app);
            assert!(app.realtime_target_media_ready());
            assert!(matches!(
                app.realtime.requests.pop(),
                Some(ui::collaboration::CollabRequest::RealtimeCommand {
                    command: ui::realtime_continuity::RealtimeCommand::MediaReady { .. },
                    ..
                })
            ));
            Widget::sync(&mut app, &doc, &ROOT);
        }

        // Phase: shell_consent_drives_target_accept_then_media_ready.
        {
            let (mut app, doc) = app_and_doc();
            let offered = ChatCallContinuation {
                workspace_id: "ws_acme".into(),
                channel_id: "c_huddle".into(),
                media: ui::realtime_continuity::RealtimeMediaFacets {
                    microphone: true,
                    camera: true,
                    ..Default::default()
                },
                controls: ui::realtime_continuity::RealtimeControlIntent {
                    microphone_muted: true,
                    ..Default::default()
                },
            };
            let metadata = ui::collaboration::ContinuationMetadata {
                schema_version: CHAT_CONTINUATION_SCHEMA,
                bytes: serde_json::to_vec(&offered).unwrap(),
            };
            assert!(app.restore_call_continuation(&metadata, &doc));
            app.realtime.offer = Some(("target-session".into(), offered));
            Collaborative::handle_collab_event(
                &mut app,
                &ui::collaboration::CollabEvent::Realtime(
                    ui::realtime_continuity::RealtimeEvent::CaptureAuthorized {
                        session: "target-session".into(),
                        capture_authorizations: vec![
                            "ab".repeat(
                                ui::realtime_continuity::OPAQUE_CAPTURE_AUTHORIZATION_BYTES,
                            ),
                        ],
                    },
                ),
            );
            assert!(matches!(
                Collaborative::take_collab_requests(&mut app)
                    .iter()
                    .find(|request| matches!(
                        request,
                        ui::collaboration::CollabRequest::RealtimeCommand { .. }
                    )),
                Some(ui::collaboration::CollabRequest::RealtimeCommand {
                    command: ui::realtime_continuity::RealtimeCommand::Accept { .. },
                    ..
                })
            ));
            Collaborative::handle_collab_event(
                &mut app,
                &ui::collaboration::CollabEvent::RealtimeCommandResult {
                    request_id: 1,
                    result: Ok(ui::realtime_continuity::RealtimeReply::Accepted),
                },
            );
            assert!(!app.call.active);
            Widget::sync(&mut app, &doc, &ROOT);
            assert!(app.call.active && app.call.muted && app.call.audio_only);
            assert_eq!(app.realtime.session.as_deref(), Some("target-session"));
            let pending = Collaborative::take_collab_requests(&mut app);
            assert!(!pending.iter().any(|request| matches!(
                request,
                ui::collaboration::CollabRequest::RealtimeCommand {
                    command: ui::realtime_continuity::RealtimeCommand::MediaReady { .. },
                    ..
                }
            )));
            let id = pending
                .iter()
                .find_map(|request| match request {
                    ui::collaboration::CollabRequest::NativeMediaCommand {
                        request_id,
                        command,
                    } if matches!(command.command, ui::native_media::Command::Join { .. }) => {
                        Some(*request_id)
                    }
                    _ => None,
                })
                .expect("target must join through native media service");
            let mut live = ui::native_media::Snapshot::unavailable();
            live.state = ui::native_media::State::Live;
            live.issue = None;
            Collaborative::handle_collab_event(
                &mut app,
                &ui::collaboration::CollabEvent::NativeMediaCommandResult {
                    request_id: id,
                    result: Ok(live),
                },
            );
            assert!(
                Collaborative::take_collab_requests(&mut app)
                    .iter()
                    .any(|request| matches!(
                        request,
                        ui::collaboration::CollabRequest::RealtimeCommand {
                            command: ui::realtime_continuity::RealtimeCommand::MediaReady { .. },
                            ..
                        }
                    ))
            );
        }
    }

    #[test]
    fn source_stays_live_until_exact_stop_source_event() {
        let (mut app, doc) = app_and_doc();
        app.call.join("c_huddle");
        app.realtime.session = Some("session-1".into());
        Collaborative::handle_collab_event(
            &mut app,
            &ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::RolledBack {
                    session: "session-1".into(),
                },
            ),
        );
        Widget::sync(&mut app, &doc, &ROOT);
        assert!(app.call.active, "rollback must leave source media live");
        Collaborative::handle_collab_event(
            &mut app,
            &ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::StopSource {
                    session: "foreign".into(),
                },
            ),
        );
        Widget::sync(&mut app, &doc, &ROOT);
        assert!(app.call.active, "foreign cutover cannot stop this call");
        Collaborative::handle_collab_event(
            &mut app,
            &ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::StopSource {
                    session: "session-1".into(),
                },
            ),
        );
        assert!(
            app.call.active,
            "callback queues the safe document mutation"
        );
        Widget::sync(&mut app, &doc, &ROOT);
        assert!(!app.call.active);
    }

    /// Chat continuity is voice-only through the native media service: an
    /// offer that claims a screen share never asks the host for a capture
    /// source and never resumes sharing.
    #[test]
    fn an_offered_screen_share_never_requests_host_capture() {
        let (mut app, doc) = app_and_doc();
        let offered = ChatCallContinuation {
            workspace_id: "ws_acme".into(),
            channel_id: "c_huddle".into(),
            media: ui::realtime_continuity::RealtimeMediaFacets {
                microphone: true,
                screen_share: true,
                ..Default::default()
            },
            controls: ui::realtime_continuity::RealtimeControlIntent::default(),
        };
        let metadata = ui::collaboration::ContinuationMetadata {
            schema_version: CHAT_CONTINUATION_SCHEMA,
            bytes: serde_json::to_vec(&offered).unwrap(),
        };
        assert!(app.restore_call_continuation(&metadata, &doc));
        app.realtime.offer = Some(("target-session".into(), offered));
        app.realtime.accepted_target = true;
        let (services, recorder) = ui::host::HostServices::recording();
        ui::host::scoped(&services, || Widget::sync(&mut app, &doc, &ROOT));
        assert!(recorder.screen_capture_requests().is_empty());
        assert!(!app.call.sharing);
        assert!(!app.realtime_target_media_ready());
    }
}
