//! Semantic, capture-free adapter between Chat calls and Tilo realtime
//! continuity. Capture handles, peer tickets, device ids, credentials, and
//! voice membership never enter this projection.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use crate::app::ChatApp;
use crate::doc::ChatDoc;
use crate::model::ChannelKind;

pub const CHAT_CONTINUATION_SCHEMA: u16 = 1;
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
            app_id: "so.tilo.chat".into(),
            command,
        }
    }
}

impl ChatApp {
    pub(crate) fn call_continuation(&self) -> Option<ChatCallContinuation> {
        if !self.call.active {
            return None;
        }
        let channel = self.channels.iter().find(|channel| {
            channel.id == self.call.channel && channel.kind == ChannelKind::Voice
        })?;
        let continuation = ChatCallContinuation {
            workspace_id: channel.workspace.clone(),
            channel_id: channel.id.clone(),
            media: ui::realtime_continuity::RealtimeMediaFacets {
                microphone: true,
                camera: !self.call.audio_only,
                screen_share: self.call.sharing,
                // Chat does not currently publish system/share audio as a
                // distinct track. Never infer it from screen capture.
                share_audio: false,
            },
            controls: ui::realtime_continuity::RealtimeControlIntent {
                microphone_muted: self.call.muted,
                camera_muted: self.call.audio_only,
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
        if !continuation.valid()
            || !ChatDoc::list_channels(doc).iter().any(|channel| {
                channel.id == continuation.channel_id
                    && channel.workspace == continuation.workspace_id
                    && channel.kind == ChannelKind::Voice
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
        if !self.call.active || self.call.channel != offered.channel_id {
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
        self.call.join(&offered.channel_id);
        self.call.muted = offered.controls.microphone_muted;
        self.call.audio_only = !offered.media.camera || offered.controls.camera_muted;
        let resume_screen = offered.media.screen_share && !offered.controls.screen_share_paused;
        if resume_screen && !self.acquire_screen() {
            self.call.leave();
            let request = self
                .realtime
                .request(ui::realtime_continuity::RealtimeCommand::TargetFailed { session });
            self.realtime.requests.push(request);
            self.realtime.offer = None;
            return;
        }
        self.call.sharing = resume_screen;
        self.realtime.session = Some(session);
        self.realtime.target_media_ready = true;
    }

    pub(crate) fn apply_realtime_stop(&mut self, doc: &mut AutoCommit) {
        if self.realtime.stop_source.take().is_none() || !self.call.active {
            return;
        }
        let channel = self.call.channel.clone();
        self.perform_action(
            doc,
            &crate::actions::ChatAction::LeaveCall {
                channel,
                user: self.me.id.clone(),
            },
        );
        self.media
            .transport_mut()
            .unpublish(crate::media::TrackKind::Audio);
        self.media
            .transport_mut()
            .unpublish(crate::media::TrackKind::Video);
        self.media
            .transport_mut()
            .unpublish(crate::media::TrackKind::Screen);
        self.call.leave();
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
        Widget::sync(&mut app, &mut doc, &ROOT);
        (app, doc)
    }

    #[test]
    fn active_call_projection_is_semantic_and_contains_no_transport_or_capture_state() {
        let (mut app, doc) = app_and_doc();
        app.call.join("c_huddle");
        app.call.muted = true;
        app.call.sharing = true;
        let metadata = Continuable::continuation_metadata(&app, &doc, &ROOT).unwrap();
        let json = String::from_utf8(metadata.bytes.clone()).unwrap();
        assert!(json.contains("c_huddle") && json.contains("ws_acme"));
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
        assert!(projection.media.microphone && projection.media.screen_share);
        assert!(projection.controls.microphone_muted);
        assert!(!projection.media.share_audio);
    }

    #[test]
    fn restore_is_inert_and_requires_a_real_synced_voice_channel() {
        let (mut source, source_doc) = app_and_doc();
        source.call.join("c_huddle");
        let metadata = Continuable::continuation_metadata(&source, &source_doc, &ROOT).unwrap();
        let (mut target, mut target_doc) = app_and_doc();
        assert!(Continuable::restore_continuation_metadata(
            &mut target,
            &metadata,
            &mut target_doc,
            &ROOT,
        ));
        assert!(!target.call.active, "restore must not join or start media");
        assert!(target.realtime.pending_restore.is_some());

        let mut unknown: ChatCallContinuation = serde_json::from_slice(&metadata.bytes).unwrap();
        unknown.channel_id = "c_not_in_document".into();
        let unknown = ui::collaboration::ContinuationMetadata {
            schema_version: CHAT_CONTINUATION_SCHEMA,
            bytes: serde_json::to_vec(&unknown).unwrap(),
        };
        assert!(!Continuable::restore_continuation_metadata(
            &mut target,
            &unknown,
            &mut target_doc,
            &ROOT,
        ));
    }

    #[test]
    fn accept_needs_fresh_capture_and_media_ready_is_a_separate_edge() {
        let (mut app, mut doc) = app_and_doc();
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
        assert!(app.realtime_target_media_ready());
        assert!(matches!(
            app.realtime.requests.pop(),
            Some(ui::collaboration::CollabRequest::RealtimeCommand {
                command: ui::realtime_continuity::RealtimeCommand::MediaReady { .. },
                ..
            })
        ));
        Widget::sync(&mut app, &mut doc, &ROOT);
    }

    #[test]
    fn source_stays_live_until_exact_stop_source_event() {
        let (mut app, mut doc) = app_and_doc();
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
        Widget::sync(&mut app, &mut doc, &ROOT);
        assert!(app.call.active, "rollback must leave source media live");
        Collaborative::handle_collab_event(
            &mut app,
            &ui::collaboration::CollabEvent::Realtime(
                ui::realtime_continuity::RealtimeEvent::StopSource {
                    session: "foreign".into(),
                },
            ),
        );
        Widget::sync(&mut app, &mut doc, &ROOT);
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
        Widget::sync(&mut app, &mut doc, &ROOT);
        assert!(!app.call.active);
    }

    #[test]
    fn shell_consent_drives_target_accept_then_media_ready() {
        let (mut app, mut doc) = app_and_doc();
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
                        "ab".repeat(ui::realtime_continuity::OPAQUE_CAPTURE_AUTHORIZATION_BYTES),
                    ],
                },
            ),
        );
        assert!(matches!(
            Collaborative::take_collab_requests(&mut app).as_slice(),
            [ui::collaboration::CollabRequest::RealtimeCommand {
                command: ui::realtime_continuity::RealtimeCommand::Accept { .. },
                ..
            }]
        ));
        Collaborative::handle_collab_event(
            &mut app,
            &ui::collaboration::CollabEvent::RealtimeCommandResult {
                request_id: 1,
                result: Ok(ui::realtime_continuity::RealtimeReply::Accepted),
            },
        );
        assert!(!app.call.active);
        Widget::sync(&mut app, &mut doc, &ROOT);
        assert!(app.call.active && app.call.muted && !app.call.audio_only);
        assert_eq!(app.realtime.session.as_deref(), Some("target-session"));
        assert!(matches!(
            Collaborative::take_collab_requests(&mut app).as_slice(),
            [ui::collaboration::CollabRequest::RealtimeCommand {
                command: ui::realtime_continuity::RealtimeCommand::MediaReady { .. },
                ..
            }]
        ));
    }
}
