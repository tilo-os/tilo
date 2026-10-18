//! Opt-in Chat navigation through the SDK's existing viewport presence.
//! The shell owns who follows whom; no pixels, capture lease or room grant
//! are created here. Only bounded navigation ids and scroll leave the app.
use serde::{Deserialize, Serialize};
use ui::view::{ViewContribution, capability::ViewState};

const PREFIX: &str = "chat-context-v1:";
const MAX_CONTEXT_BYTES: usize = 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    channel: String,
    thread: Option<String>,
}

impl ViewState for crate::ChatApp {
    fn view_state(&self) -> Option<ViewContribution> {
        if !self.sharing_context || self.active_channel.is_empty() {
            return None;
        }
        let channel = self.channel(&self.active_channel)?;
        if channel.kind != crate::model::ChannelKind::Text || channel.archived {
            return None;
        }
        let context = Context {
            channel: self.active_channel.clone(),
            thread: self.thread_open.clone(),
        };
        let mode = serde_json::to_string(&context).ok()?;
        if mode.len() > MAX_CONTEXT_BYTES {
            return None;
        }
        Some(ViewContribution {
            scroll: vec![self.scroll, f64::from(self.thread_scroll.offset_y())],
            view_mode: Some(format!("{PREFIX}{mode}")),
        })
    }

    fn restore_view_state(&mut self, view: &ViewContribution) {
        if !self.context_follow_requested {
            return;
        }
        self.following_context = false;
        let Some(mode) = view
            .view_mode
            .as_deref()
            .and_then(|mode| mode.strip_prefix(PREFIX))
        else {
            self.following_context = false;
            self.request_relayout();
            return;
        };
        if mode.len() > MAX_CONTEXT_BYTES {
            return;
        }
        let [scroll, thread_scroll] = view.scroll.as_slice() else {
            return;
        };
        if view
            .scroll
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1_000_000.0).contains(value))
        {
            return;
        }
        let Ok(context) = serde_json::from_str::<Context>(mode) else {
            return;
        };
        // Presence never resolves an arbitrary address or grants access. The
        // target must already be in this permissioned room's loaded resource.
        let Some(channel) = self.channel(&context.channel) else {
            return;
        };
        if channel.kind != crate::model::ChannelKind::Text || channel.archived {
            return;
        }
        if context.thread.as_ref().is_some_and(|thread| {
            !self.search_messages.iter().any(|message| {
                message.id == *thread
                    && message.channel == context.channel
                    && message.parent.is_none()
            })
        }) {
            return;
        }
        self.active_channel = context.channel;
        self.thread_open = context.thread;
        self.scroll = *scroll;
        self.timeline_at_end = self.scroll <= 0.5;
        self.thread_scroll
            .set_offset_y(ui::num::f32_of_f64(*thread_scroll));
        self.following_context = true;
        self.reselect();
        self.request_relayout();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Collaborative;

    fn room() -> (crate::testing::Room, crate::ChatApp) {
        let doc = crate::testing::demo_doc();
        let mut app = crate::app::ChatApp::new_ephemeral();
        app.attach_room(&doc);
        app.reload(&doc);
        app.active_channel = "c_crit".into();
        (doc, app)
    }

    #[test]
    fn shared_space_context_follows_through_sdk_presence_and_releases_on_shell_stop() {
        use ui::collaboration::{CollabEvent, PresencePayload};
        use ui::view::FreezeThaw;
        let (doc, mut presenter) = room();
        presenter
            .answer_effect(crate::effects::ChatEffect::ShareLiveContext)
            .unwrap();
        presenter.scroll = 180.0;
        let contribution = presenter.view_state().unwrap();
        let mut follower = crate::app::ChatApp::new_ephemeral();
        follower.attach_room(&doc);
        follower.reload(&doc);
        follower.active_channel = "c_general".into();
        // Publishing our own view lets the real SDK capture observe the
        // restored navigation. It is independent of choosing to follow.
        follower
            .answer_effect(crate::effects::ChatEffect::ShareLiveContext)
            .unwrap();
        let mut app = ui::App::new(ui::State::from_doc(doc.chat), Box::new(follower));
        let frame = |scroll| CollabEvent::RemotePresenceUpdate {
            user_id: "presenter".into(),
            key: ui::presence::PresenceKey::view(&ui::view::ViewSubset {
                scroll: vec![scroll, 0.0],
                view_mode: contribution.view_mode.clone(),
                ..Default::default()
            }),
            payload: PresencePayload::Arrived,
        };
        app.apply_collab_event_for_test(frame(180.0));
        let before = <ui::App as FreezeThaw>::capture_view(&app);
        assert_eq!(
            before.scroll,
            vec![0.0, 0.0],
            "presence alone never navigates"
        );
        app.apply_collab_event_for_test(CollabEvent::FollowChanged {
            peer_id: Some("presenter".into()),
        });
        let restored = <ui::App as FreezeThaw>::capture_view(&app);
        assert_eq!(restored.scroll, vec![180.0, 0.0]);
        assert_eq!(restored.view_mode, contribution.view_mode);
        app.apply_collab_event_for_test(frame(240.0));
        assert_eq!(
            <ui::App as FreezeThaw>::capture_view(&app).scroll,
            vec![240.0, 0.0]
        );
        app.apply_collab_event_for_test(CollabEvent::FollowChanged { peer_id: None });
        app.apply_collab_event_for_test(frame(600.0));
        assert_eq!(
            <ui::App as FreezeThaw>::capture_view(&app).scroll,
            vec![240.0, 0.0]
        );
        assert!(app.followed_peer().is_none());
    }

    #[test]
    fn shared_space_context_requires_local_consent_and_stop_withdraws_navigation() {
        let (mut doc, mut presenter) = room();
        let before = doc.save();
        let (_, mut follower) = room();
        assert!(ui::view::capability::view_state(&presenter).is_none());
        presenter
            .answer_effect(crate::effects::ChatEffect::ShareLiveContext)
            .unwrap();
        presenter.scroll = 120.0;
        let view = ui::view::capability::view_state(&presenter).unwrap();
        follower.handle_collab_event(&ui::collaboration::CollabEvent::FollowChanged {
            peer_id: Some("presenter".into()),
        });
        ui::view::capability::restore_view_state(&mut follower, &view);
        assert_eq!(follower.active_channel, "c_crit");
        assert_eq!(follower.scroll, 120.0);
        assert!(follower.following_context);
        assert!(
            !follower.sharing_context,
            "following never grants publication consent"
        );
        presenter
            .answer_effect(crate::effects::ChatEffect::StopSharingLiveContext)
            .unwrap();
        let withdrawn = ui::view::capability::view_state(&presenter).unwrap_or_default();
        assert!(withdrawn.view_mode.is_none());
        ui::view::capability::restore_view_state(&mut follower, &withdrawn);
        assert!(!follower.following_context);
        assert_eq!(follower.scroll, 120.0, "withdrawal does not navigate");
        assert_eq!(doc.save(), before);
        assert!(!presenter.native_voice.snapshot.capture_active);
        follower
            .handle_collab_event(&ui::collaboration::CollabEvent::FollowChanged { peer_id: None });
        assert!(!follower.following_context);
    }

    #[test]
    fn shared_space_context_cannot_navigate_to_unknown_rooms_or_import_capture_state() {
        let (_, mut app) = room();
        let initial = app.active_channel.clone();
        app.handle_collab_event(&ui::collaboration::CollabEvent::FollowChanged {
            peer_id: Some("presenter".into()),
        });
        for mode in [
            r#"{"workspace":"other","channel":"c_general","thread":null}"#,
            r#"{"channel":"c_missing","thread":null}"#,
            r#"{"channel":"c_crit","thread":"missing"}"#,
            r#"{"channel":"c_crit","thread":null,"capture":true}"#,
        ] {
            ui::view::capability::restore_view_state(
                &mut app,
                &ViewContribution {
                    view_mode: Some(format!("{PREFIX}{mode}")),
                    scroll: vec![50.0, 0.0],
                },
            );
            assert_eq!(app.active_channel, initial);
            assert!(!app.following_context);
        }
        for scroll in [f64::NAN, -1.0, f64::INFINITY] {
            ui::view::capability::restore_view_state(
                &mut app,
                &ViewContribution {
                    view_mode: Some(format!(
                        "{PREFIX}{{\"channel\":\"c_general\",\"thread\":null}}"
                    )),
                    scroll: vec![scroll, 0.0],
                },
            );
            assert_eq!(app.active_channel, initial);
        }
        assert!(!app.sharing_context);
        assert!(!app.native_voice.snapshot.capture_active);
    }
}
