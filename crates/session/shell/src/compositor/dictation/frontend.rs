//! Assemble field preparation, local one-session consent and the private actor.
use super::{DictationStatus, SESSION_WINDOW};
use crate::compositor::state::State;
use crate::shell::overlay_host::dictation_layer::{DictationSurface, Request};
use std::time::Instant;

pub(crate) fn service(
    state: &mut State,
    surface: &mut DictationSurface,
    now: Instant,
    locked: bool,
) {
    if locked {
        state.cancel_active_dictation();
        surface.status(DictationStatus::Idle);
        let _ = surface.take_request();
        // Revoke the target before consuming a queued final result, while
        // still servicing termination of the canceled actor.
        state.pump_dictation_worker(now);
        return;
    }
    if let Some(request) = surface.take_request() {
        let result = match request {
            Request::Toggle => {
                if let Some(id) = surface.session() {
                    if state.dictation_status() == DictationStatus::Listening {
                        state.finish_recording(id)
                    } else {
                        state.cancel_active_dictation();
                        surface.status(DictationStatus::Idle);
                        Ok(())
                    }
                } else {
                    state.prepare_dictation(now).map(|id| surface.prepare(id))
                }
            }
            Request::Consent(id) if surface.session() == Some(id) && surface.is_consent() => {
                state.start_authorized_dictation(id, "auto", now).map(|()| {
                    if let Some(session) = state.text_commit.dictation.session.as_mut() {
                        session.deadline = now.checked_add(SESSION_WINDOW).unwrap_or(now);
                    }
                    surface.status(DictationStatus::Loading);
                })
            }
            Request::Finish(id) if surface.session() == Some(id) => state.finish_recording(id),
            Request::Cancel(id) if surface.session() == id => {
                state.cancel_active_dictation();
                state.text_commit.dictation.last_failure = None;
                surface.status(DictationStatus::Idle);
                Ok(())
            }
            _ => return, // A retired prompt cannot cancel or grant its successor.
        };
        if let Err(error) = result {
            // A failed request never leaves capture or a prepared target alive.
            state.cancel_active_dictation();
            surface.failure(error);
        }
    }
    // Explicit cancellation wins over a result arriving in the same turn.
    state.pump_dictation_worker(now);
    if let Some(id) = surface.session() {
        if surface.preparing() {
            match state.advance_dictation_preparation(id, now) {
                Ok(true) => surface.consent(),
                Ok(false) => {}
                Err(error) => {
                    state.cancel_active_dictation();
                    surface.failure(error);
                }
            }
        } else if surface.is_consent() {
            if let Err(error) = state.check_dictation(id, now) {
                state.cancel_active_dictation();
                surface.failure(error);
            }
        } else {
            surface.status(state.dictation_status());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::shell_text_input::{
        FieldRequest, NativeEditStamp, NativeFieldId, ShellTextInput,
    };
    use automerge::transaction::Transactable as _;
    use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

    struct Fixture {
        state: State,
        display: Display<State>,
        proxy: ShellTextInput,
        surface: DictationSurface,
        field: FieldRequest,
        _event_loop: EventLoop<'static, State>,
    }
    impl Fixture {
        fn new() -> Self {
            let event_loop = EventLoop::try_new().unwrap();
            let display = Display::new().unwrap();
            let mut state = State::new(event_loop.handle(), display.handle());
            let proxy = ShellTextInput::connect(&mut state).unwrap();
            let mut fixture = Self {
                state,
                display,
                proxy,
                surface: DictationSurface::default(),
                field: FieldRequest {
                    purpose: winit::window::ImePurpose::Normal,
                    x: 420,
                    y: 100,
                    width: 1,
                    height: 24,
                },
                _event_loop: event_loop,
            };
            let deadline = tilo_test_env::hang_deadline();
            while !fixture.proxy.holds_focus(&fixture.state) {
                assert!(
                    Instant::now() < deadline,
                    "native editable field must activate"
                );
                fixture.pump();
            }
            fixture
        }
        fn pump(&mut self) {
            self.display.dispatch_clients(&mut self.state).unwrap();
            self.display.flush_clients().unwrap();
            self.proxy.sync_for_field(
                &mut self.state,
                Some(self.field),
                Some(NativeFieldId::Widget(42)),
            );
            service(&mut self.state, &mut self.surface, Instant::now(), false);
        }
        fn consent(&mut self) -> super::super::SessionId {
            self.surface.request_toggle();
            let deadline = tilo_test_env::hang_deadline();
            while !self.surface.is_consent() {
                assert!(
                    Instant::now() < deadline,
                    "consent must appear after field activation"
                );
                self.pump();
            }
            assert!(
                self.proxy.holds_focus(&self.state),
                "consent preserves the original keyboard owner"
            );
            assert!(
                self.state.text_commit.dictation.recognition.is_none(),
                "requesting consent cannot start a worker or microphone"
            );
            self.surface.session().unwrap()
        }
    }

    #[test]
    fn dictation_frontend_denial_and_retired_consent_cannot_start_a_successor() {
        let mut fixture = Fixture::new();
        let old = fixture.consent();
        assert!(fixture.surface.owns_key(&winit::keyboard::Key::Named(
            winit::keyboard::NamedKey::Enter
        )));
        assert!(fixture.surface.owns_key(&winit::keyboard::Key::Named(
            winit::keyboard::NamedKey::Escape
        )));
        assert!(
            !fixture
                .surface
                .owns_key(&winit::keyboard::Key::Character("x".into())),
            "consent controls must not capture ordinary field typing"
        );
        fixture.surface.request_cancel();
        fixture.pump();
        assert!(fixture.state.dictation_target_id().is_none());
        assert!(
            !fixture.surface.owns_key(&winit::keyboard::Key::Named(
                winit::keyboard::NamedKey::Enter
            )),
            "dismissed consent must release control keys"
        );
        let next = fixture.consent();
        assert_ne!(old, next);
        fixture.surface.enqueue(Request::Consent(old));
        fixture.pump();
        assert_eq!(fixture.state.dictation_target_id(), Some(next));
        assert!(fixture.surface.is_consent());
        assert!(fixture.state.text_commit.dictation.recognition.is_none());
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            true,
        );
        assert!(
            fixture.state.dictation_target_id().is_none(),
            "lock cancels a prepared request"
        );
        assert!(fixture.surface.session().is_none());
    }

    #[test]
    fn dictation_frontend_same_field_stamp_is_bound_to_each_session_and_edits_cancel() {
        let mut fixture = Fixture::new();
        let mut document = automerge::AutoCommit::new();
        let stamp = |document: &automerge::AutoCommit| NativeEditStamp {
            document: ui::bound_dispatch::DocumentEditStamp::for_document(document),
            fingerprint: [5; 32],
        };
        let old = fixture.consent();
        fixture
            .proxy
            .observe_edit_stamp(&mut fixture.state, Some(stamp(&document)));
        fixture.surface.request_cancel();
        fixture.pump();
        document
            .put(automerge::ROOT, "text", "edited between sessions")
            .unwrap();
        let next = fixture.consent();
        assert_ne!(old, next);
        fixture
            .proxy
            .observe_edit_stamp(&mut fixture.state, Some(stamp(&document)));
        assert!(
            fixture.state.check_dictation(next, Instant::now()).is_ok(),
            "an edit between sessions must not cancel a newly pinned target"
        );
        document
            .put(automerge::ROOT, "text", "during consent")
            .unwrap();
        fixture
            .proxy
            .observe_edit_stamp(&mut fixture.state, Some(stamp(&document)));
        assert!(fixture.state.check_dictation(next, Instant::now()).is_err());
        fixture.pump();
        assert!(!fixture.surface.is_consent());
        assert!(fixture.state.text_commit.dictation.recognition.is_none());
    }

    #[test]
    fn dictation_oversized_final_inserts_nothing_and_keeps_the_bridge_alive() {
        let mut fixture = Fixture::new();
        let id = fixture.consent();
        fixture.state.shell_field_active = true;
        fixture
            .state
            .update_dictation(
                id,
                "x".repeat(super::super::MAX_DRAFT_BYTES),
                Instant::now(),
            )
            .unwrap();
        assert_eq!(
            fixture.state.finish_dictation(id, Instant::now()),
            Err(super::super::DictationError::DraftTooLarge)
        );
        assert!(!fixture.state.take_native_dictation_commit());
        fixture.display.flush_clients().unwrap();
        assert!(fixture.proxy.drain_dictation_commit().is_none());
        fixture.pump();
        assert!(fixture.proxy.holds_focus(&fixture.state));
    }

    #[test]
    fn dictation_native_final_drains_the_largest_frame_before_another_input_turn() {
        let mut fixture = Fixture::new();
        let id = fixture.consent();
        fixture.state.shell_field_active = true;
        let text = "東".repeat(super::super::MAX_COMMIT_BYTES / 3) + "x";
        fixture
            .state
            .update_dictation(id, text.clone(), Instant::now())
            .unwrap();
        fixture.state.finish_dictation(id, Instant::now()).unwrap();
        assert_eq!(
            fixture.state.prepare_dictation(Instant::now()),
            Err(super::super::DictationError::Busy),
            "a successor cannot start before the native final is consumed"
        );
        assert!(fixture.state.take_native_dictation_commit());
        assert!(!fixture.state.take_native_dictation_commit());
        fixture.display.flush_clients().unwrap();
        let events = fixture.proxy.drain_dictation_commit().unwrap();
        let commits = events
            .iter()
            .filter_map(|event| match event {
                winit::event::WindowEvent::Ime(winit::event::Ime::Commit(text)) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(commits, [&text]);
        assert!(
            fixture.proxy.drain_dictation_commit().is_none(),
            "the final must not be delivered twice"
        );
    }
}
