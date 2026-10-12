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
    field_start: Option<ui::webview::text_input::Receipt>,
) {
    state.expire_dictation_recovery(now);
    if locked {
        state.discard_dictation_recovery();
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
                    if surface.is_recovery()
                        || matches!(
                            state.dictation_status(),
                            DictationStatus::StoppingRecording | DictationStatus::Finishing
                        )
                    {
                        Ok(())
                    } else if state.dictation_status() == DictationStatus::Listening {
                        state.finish_recording(id)
                    } else {
                        state.discard_dictation_recovery();
                        state.text_commit.dictation.last_failure = None;
                        surface.status(state.dictation_status());
                        Ok(())
                    }
                } else {
                    state.prepare_dictation(now).map(|id| surface.prepare(id))
                }
            }
            Request::Consent(id) if surface.session() == Some(id) && surface.is_consent() => {
                let language =
                    tilo_accessibility_settings::AccessibilitySettings::load().dictation_language;
                state
                    .start_dictation_after_field_ack(id, language.code(), now, field_start)
                    .map(|()| {
                        if let Some(session) = state.text_commit.dictation.session.as_mut() {
                            session.deadline = now.checked_add(SESSION_WINDOW).unwrap_or(now);
                        }
                        surface.status(DictationStatus::Loading);
                    })
            }
            Request::Finish(id) if surface.session() == Some(id) && !surface.is_recovery() => {
                state.finish_recording(id)
            }
            Request::Copy(id) if surface.session() == Some(id) && surface.is_recovery() => {
                state.copy_dictation_recovery(id, now)
            }
            Request::Cancel(id) if surface.session() == id => {
                state.discard_dictation_recovery();
                state.text_commit.dictation.last_failure = None;
                surface.status(state.dictation_status());
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
    if let Err(error) = state.advance_authorized_dictation(now) {
        state.cancel_active_dictation();
        surface.failure(error);
    }
    // Explicit cancellation wins over a result arriving in the same turn.
    state.pump_dictation_worker(now);
    if state
        .text_commit
        .dictation
        .recovery
        .as_ref()
        .is_some_and(|recovery| Some(recovery.id) == surface.session())
    {
        surface.status(state.dictation_status());
        return;
    }
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
    let preview = surface.session().and_then(|id| state.dictation_preview(id));
    surface.preview(preview);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::shell_text_input::{
        FieldRequest, NativeEditStamp, NativeFieldId, ShellTextInput,
    };
    use automerge::transaction::Transactable as _;
    use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
    use smithay::wayland::text_input::TextInputSeat as _;

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
                    multiline: false,
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
        fn pump(&mut self) -> Vec<winit::event::WindowEvent> {
            self.display.dispatch_clients(&mut self.state).unwrap();
            self.display.flush_clients().unwrap();
            let events = self.proxy.sync_for_field(
                &mut self.state,
                Some(self.field),
                Some(NativeFieldId::Widget(42)),
            );
            service(
                &mut self.state,
                &mut self.surface,
                Instant::now(),
                false,
                None,
            );
            events
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

        fn dictionary_actor(
            &mut self,
            reader: settings_store::personal_dictionary::Reader,
            pid: &std::path::Path,
            completes: bool,
        ) -> super::super::SessionId {
            let id = self.consent();
            let actor = super::super::runtime::dictionary_test_peer(
                std::sync::Arc::new(reader),
                pid,
                completes,
            );
            self.state.text_commit.dictation.recognition = Some(super::super::Recognition {
                id,
                actor,
                language: "en".into(),
                literal: true,
                phase: DictationStatus::Listening,
            });
            self.surface.status(DictationStatus::Listening);
            id
        }
    }

    #[test]
    fn dictionary_edit_retains_visible_draft_but_revocation_erases_it() {
        use super::super::{RecoveryReason, host::VocabularyBinding};
        use settings_store::personal_dictionary::{Edit, Service, Word};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        for mode in ["edit", "switch", "relogin", "drop"] {
            let mut fixture = Fixture::new();
            let directory = tempfile::tempdir().unwrap();
            let dictionary = Arc::new(Service::new(directory.path().join("dictionary")).unwrap());
            let reader = dictionary.bind(Some([7; 32])).unwrap();
            let word = Word::new("TiLo", None).unwrap();
            let snapshot = dictionary
                .edit(&reader, 0, Edit::Add(word.clone()))
                .unwrap();
            let wakes = Arc::new(AtomicUsize::new(0));
            let notified = Arc::clone(&wakes);
            let mut binding = VocabularyBinding::from_service(
                Some(&dictionary),
                Arc::new(move || {
                    notified.fetch_add(1, Ordering::Relaxed);
                }),
            );
            binding.refresh(&mut fixture.state);
            let id =
                fixture.dictionary_actor(reader.clone(), &directory.path().join("peer"), false);
            fixture
                .state
                .update_dictation(id, "Visible draft 日本語".into(), Instant::now())
                .unwrap();
            fixture.surface.request_finish();
            fixture.pump();
            match mode {
                "edit" => {
                    dictionary
                        .edit(&reader, snapshot.revision, Edit::Remove(word))
                        .unwrap();
                }
                "switch" => {
                    dictionary.bind(Some([8; 32]));
                }
                "relogin" => {
                    dictionary.bind(None);
                    dictionary.bind(Some([7; 32]));
                }
                _ => drop(dictionary),
            }
            assert!(
                wakes.load(Ordering::Relaxed) > 0,
                "actual service changes must wake an idle native loop"
            );
            binding.refresh(&mut fixture.state);
            if mode == "edit" {
                assert_eq!(
                    fixture.state.dictation_target_id(),
                    Some(id),
                    "ordinary edits preserve the draft until actor retirement"
                );
            } else {
                assert!(
                    fixture.state.dictation_target_id().is_none(),
                    "identity replacement cancels before consuming queued words"
                );
            }
            let deadline = tilo_test_env::hang_deadline();
            while fixture.state.text_commit.dictation.recognition.is_some() {
                assert!(
                    Instant::now() < deadline,
                    "changed dictionary must reap the actor"
                );
                fixture.pump();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(fixture.state.dictation_target_id().is_none());
            assert!(
                !fixture.state.take_native_dictation_commit(),
                "a draft is never automatically inserted"
            );
            if mode == "edit" {
                let recovery = fixture
                    .state
                    .text_commit
                    .dictation
                    .recovery
                    .as_ref()
                    .unwrap();
                assert_eq!(recovery.reason, RecoveryReason::VocabularyChanged);
                assert_eq!(recovery.text.as_str(), "Visible draft 日本語");
                assert!(!recovery.copied, "no automatic clipboard export");
                assert!(fixture.surface.is_recovery());
                fixture.state.set_dictation_vocabulary(None);
                assert!(
                    fixture.state.text_commit.dictation.recovery.is_none(),
                    "rebinding erases retained text"
                );
            } else {
                assert!(fixture.state.text_commit.dictation.recovery.is_none());
            }
        }
    }

    #[test]
    fn live_account_switch_rejects_already_queued_final_without_discarding_normal_delivery() {
        use super::super::host::VocabularyBinding;
        use settings_store::personal_dictionary::{Edit, Service, Word};
        use std::sync::Arc;
        for switch in [false, true] {
            let mut fixture = Fixture::new();
            fixture.state.shell_field_active = true;
            let directory = tempfile::tempdir().unwrap();
            let dictionary = Arc::new(Service::new(directory.path().join("dictionary")).unwrap());
            let reader = dictionary.bind(Some([7; 32])).unwrap();
            dictionary
                .edit(&reader, 0, Edit::Add(Word::new("TiLo", None).unwrap()))
                .unwrap();
            let mut binding = VocabularyBinding::from_service(Some(&dictionary), Arc::new(|| {}));
            binding.refresh(&mut fixture.state);
            let pid = directory.path().join("peer");
            fixture.dictionary_actor(reader, &pid, true);
            fixture.surface.request_finish();
            fixture.pump();
            std::fs::write(pid.with_extension("release"), b"release prerecorded peer").unwrap();
            let deadline = tilo_test_env::hang_deadline();
            // Reap the real producer without consuming its queued Done reply.
            while !fixture
                .state
                .text_commit
                .dictation
                .recognition
                .as_mut()
                .unwrap()
                .actor
                .is_stopped()
            {
                assert!(
                    Instant::now() < deadline,
                    "the peer must finish before the account switch"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            if switch {
                dictionary.bind(Some([8; 32]));
                binding.refresh(&mut fixture.state);
            }
            fixture.pump();
            assert!(fixture.state.text_commit.dictation.recognition.is_none());
            assert_eq!(fixture.state.take_native_dictation_commit(), !switch);
            assert!(fixture.state.text_commit.dictation.recovery.is_none());
            if switch {
                assert!(
                    fixture
                        .state
                        .text_commit
                        .dictation
                        .native_delivery
                        .is_none()
                );
            } else {
                assert_eq!(
                    fixture
                        .state
                        .text_commit
                        .dictation
                        .native_delivery
                        .as_ref()
                        .unwrap()
                        .recovery
                        .text
                        .as_str(),
                    "Corrected 👋"
                );
            }
            fixture.pump();
            assert!(
                !fixture.state.take_native_dictation_commit(),
                "a final is delivered at most once"
            );
        }
    }

    #[test]
    fn deferred_consent_requires_original_start_ack_before_any_worker() {
        for failure in [
            None,
            Some(ui::webview::text_input::Failure::Rejected),
            Some(ui::webview::text_input::Failure::Unknown),
        ] {
            let mut fixture = Fixture::new();
            fixture.state.shell_field_active = true;
            fixture.state.set_native_dictation_deferred(true);
            let id = fixture.consent();
            assert!(
                fixture
                    .state
                    .native_dictation_start_required(id, Instant::now())
            );
            // The old synchronous entry cannot bypass deferred acknowledgement.
            assert_eq!(
                fixture
                    .state
                    .start_authorized_dictation(id, "en", Instant::now()),
                Err(super::super::DictationError::TargetChanged)
            );
            fixture.surface.enqueue(Request::Consent(id));
            service(
                &mut fixture.state,
                &mut fixture.surface,
                Instant::now(),
                false,
                failure.map(ui::webview::text_input::Receipt::failed),
            );
            assert!(fixture.state.text_commit.dictation.recognition.is_none());
            assert!(fixture.state.text_commit.dictation.field_start.is_none());
            assert!(fixture.state.dictation_target_id().is_none());
            assert!(!fixture.surface.is_consent());
        }
    }

    #[test]
    fn endpoint_correction_commits_once_without_appending_a_stable_hallucination() {
        let mut fixture = Fixture::new();
        let id = fixture.consent();
        fixture.state.shell_field_active = true;
        fixture
            .state
            .update_dictation(id, "Vielen Dank.".into(), Instant::now())
            .unwrap();
        assert!(!fixture.state.take_native_dictation_commit());
        fixture.display.flush_clients().unwrap();
        assert!(fixture.proxy.drain_dictation_commit().is_none());

        let corrected = "Für Hongkong — nein, für Kowloon. 東京 café";
        fixture
            .state
            .update_dictation(id, corrected.into(), Instant::now())
            .unwrap();
        assert!(!fixture.state.take_native_dictation_commit());
        fixture.state.finish_dictation(id, Instant::now()).unwrap();
        assert!(fixture.state.take_native_dictation_commit());
        fixture.display.flush_clients().unwrap();
        let events = fixture.proxy.drain_dictation_commit().unwrap();
        let committed: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                winit::event::WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(committed, [corrected]);
        assert!(fixture.proxy.drain_dictation_commit().is_none());
    }

    #[test]
    fn dictation_repeated_finish_and_toggle_keep_one_final_on_the_original_field() {
        let mut fixture = Fixture::new();
        let id = fixture.consent();
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("peer");
        let actor = super::super::runtime::finishing_test_peer(&pid);
        fixture.state.shell_field_active = true;
        fixture.state.text_commit.dictation.recognition = Some(super::super::Recognition {
            id,
            actor,
            language: "en".into(),
            literal: true,
            phase: DictationStatus::Listening,
        });
        fixture.surface.status(DictationStatus::Listening);
        fixture.surface.request_finish();
        fixture.pump();
        assert!(matches!(
            fixture.state.dictation_status(),
            DictationStatus::StoppingRecording | DictationStatus::Finishing
        ));
        for request in [Request::Finish(id), Request::Toggle, Request::Finish(id)] {
            fixture.surface.enqueue(request);
            fixture.pump();
            assert_eq!(fixture.state.dictation_target_id(), Some(id));
        }
        std::fs::write(
            directory.path().join("peer.release"),
            b"public fixture release",
        )
        .unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let mut events = Vec::new();
        while fixture.state.dictation_target_id().is_some()
            || fixture.state.text_commit.dictation.recognition.is_some()
        {
            assert!(
                Instant::now() < deadline,
                "the final and actor teardown must complete"
            );
            events.extend(fixture.pump());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // A final can be consumed by the native field before its actor retires.
        events.extend(fixture.pump());
        assert!(fixture.state.take_native_dictation_commit());
        fixture.display.flush_clients().unwrap();
        events.extend(fixture.proxy.drain_dictation_commit().unwrap_or_default());
        let commits: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                winit::event::WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(commits, ["Corrected 👋"]);
        assert!(fixture.proxy.drain_dictation_commit().is_none());
        let pid = std::fs::read_to_string(pid).unwrap();
        assert!(!std::path::Path::new("/proc").join(pid).exists());
    }

    #[test]
    fn final_numeric_repair_and_literal_mode_use_one_original_field_commit() {
        for (language, literal, expected) in [
            ("en", false, "Let me grab three of these"),
            ("en", true, "Let me grab two, uh, no, three of these"),
            ("de", false, "TiLo braucht drei Stück."),
            ("de", true, "TiLo braucht zwei, äh, nein, drei Stück."),
            ("fr", false, "Prendre trois exemplaires."),
            ("fr", true, "Prendre deux, euh, non, trois exemplaires."),
            ("es", false, "TiLo necesita tres unidades."),
            ("es", true, "TiLo necesita dos, eh, no, tres unidades."),
        ] {
            let mut fixture = Fixture::new();
            let id = fixture.consent();
            let directory = tempfile::tempdir().unwrap();
            let pid = directory.path().join("peer");
            let actor = super::super::runtime::repair_test_peer(&pid, language);
            fixture.state.shell_field_active = true;
            fixture.state.text_commit.dictation.recognition = Some(super::super::Recognition {
                id,
                actor,
                language: language.into(),
                literal,
                phase: DictationStatus::Listening,
            });
            fixture.surface.status(DictationStatus::Listening);
            fixture.surface.request_finish();
            fixture.pump();
            std::fs::write(
                directory.path().join("peer.release"),
                b"public fixture release",
            )
            .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            let mut events = Vec::new();
            while fixture.state.dictation_target_id().is_some()
                || fixture.state.text_commit.dictation.recognition.is_some()
            {
                assert!(
                    Instant::now() < deadline,
                    "final edit and worker teardown must complete"
                );
                events.extend(fixture.pump());
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            // Preserve the final across an additional native input turn.
            events.extend(fixture.pump());
            assert!(fixture.state.take_native_dictation_commit());
            fixture.display.flush_clients().unwrap();
            events.extend(fixture.proxy.drain_dictation_commit().unwrap_or_default());
            let commits: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    winit::event::WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(commits, [expected]);
            assert!(fixture.proxy.drain_dictation_commit().is_none());
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
            None,
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
        assert_eq!(fixture.state.finish_dictation(id, Instant::now()), Ok(()));
        assert!(!fixture.state.take_native_dictation_commit());
        fixture.display.flush_clients().unwrap();
        assert!(fixture.proxy.drain_dictation_commit().is_none());
        fixture.pump();
        assert!(fixture.proxy.holds_focus(&fixture.state));
    }

    fn retain_multilingual(fixture: &mut Fixture) -> (super::super::SessionId, String) {
        let id = fixture.consent();
        let text = "東京 café مرحبا 👩🏽‍💻 ".repeat(200);
        assert!(text.len() > super::super::MAX_COMMIT_BYTES);
        assert!(text.len() <= super::super::MAX_DRAFT_BYTES);
        fixture
            .state
            .update_dictation(id, text.clone(), Instant::now())
            .unwrap();
        fixture.state.finish_dictation(id, Instant::now()).unwrap();
        fixture.pump();
        assert!(fixture.surface.is_recovery());
        assert_eq!(
            fixture
                .state
                .text_commit
                .dictation
                .recovery
                .as_ref()
                .unwrap()
                .text
                .as_str(),
            text
        );
        assert!(fixture.state.text_commit.dictation.session.is_none());
        assert_eq!(
            fixture.state.dictation_service_deadline(Instant::now()),
            Some(
                fixture
                    .state
                    .text_commit
                    .dictation
                    .recovery
                    .as_ref()
                    .unwrap()
                    .deadline
            ),
            "idle recovery must schedule only expiry, without polling"
        );
        (id, text)
    }

    #[test]
    fn dictation_long_multilingual_recovery_requires_explicit_export_after_focus_change() {
        use crate::shell::overlay_host::layers::{LayerIntent, ShellLayer};
        let mut fixture = Fixture::new();
        let selection = fixture.state.selection_store();
        let services = crate::compositor::selection::host_services(selection.clone());
        let _host = ui::host::enter(&services);
        let (id, text) = retain_multilingual(&mut fixture);
        assert!(
            crate::compositor::selection::mirror_text(
                &selection,
                crate::compositor::selection::Target::Clipboard
            )
            .is_none(),
            "no automatic clipboard fallback"
        );
        fixture.surface.request_toggle();
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            false,
            None,
        );
        fixture.surface.enqueue(Request::Finish(id));
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            false,
            None,
        );
        assert!(
            fixture.surface.is_recovery(),
            "start/finish cannot silently discard recovery"
        );
        assert_eq!(
            fixture.state.prepare_dictation(Instant::now()),
            Err(super::super::DictationError::Busy)
        );
        fixture.state.seat.text_input().leave();
        fixture.state.seat.text_input().set_focus(None);
        fixture.state.cancel_active_dictation();
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            false,
            None,
        );
        let node = fixture
            .surface
            .accessibility(crate::shell::overlay_host::layers::LayerGeometry {
                viewport: vello::kurbo::Rect::new(0.0, 0.0, 1000.0, 800.0),
                outputs: &[],
                focused_output: None,
            })
            .unwrap();
        let target = node.children[0].action_target.as_deref().unwrap();
        let Some(LayerIntent::Dictation(request)) = fixture.surface.semantic_intent(target) else {
            panic!("recovery copy is an accessible action")
        };
        assert_eq!(request, Request::Copy(id));
        fixture.surface.enqueue(request);
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            false,
            None,
        );
        assert_eq!(
            crate::compositor::selection::mirror_text(
                &selection,
                crate::compositor::selection::Target::Clipboard
            )
            .as_deref(),
            Some(text.as_str()),
            "the real compositor clipboard must accept the complete UTF-8 transcript"
        );
        assert!(matches!(
            fixture.state.dictation_status(),
            DictationStatus::Recovery {
                reason: super::super::RecoveryReason::TooLong,
                copied: true,
                copy_failed: false
            }
        ));
        fixture.display.flush_clients().unwrap();
        assert!(
            fixture.proxy.drain_dictation_commit().is_none(),
            "export never inserts into another field"
        );
    }

    #[test]
    fn dictation_recovery_copy_failure_cancel_and_retired_actions_preserve_ownership() {
        let services = ui::host::HostServices::null();
        let _host = ui::host::enter(&services);
        let mut fixture = Fixture::new();
        let (id, text) = retain_multilingual(&mut fixture);
        fixture.surface.enqueue(Request::Copy(id));
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            false,
            None,
        );
        assert!(matches!(
            fixture.state.dictation_status(),
            DictationStatus::Recovery {
                copy_failed: true,
                ..
            }
        ));
        assert_eq!(
            fixture
                .state
                .text_commit
                .dictation
                .recovery
                .as_ref()
                .unwrap()
                .text
                .as_str(),
            text
        );
        fixture.surface.enqueue(Request::Cancel(Some(id)));
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            false,
            None,
        );
        assert!(fixture.state.text_commit.dictation.recovery.is_none());
        assert!(
            fixture
                .state
                .copy_dictation_recovery(id, Instant::now())
                .is_err()
        );
        let next = fixture.consent();
        assert_ne!(id, next);
        fixture.surface.enqueue(Request::Copy(id));
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            false,
            None,
        );
        assert!(fixture.surface.is_consent());
        assert!(fixture.state.check_dictation(next, Instant::now()).is_ok());
    }

    #[test]
    fn dictation_recovery_lock_and_expiry_discard_private_text() {
        let mut fixture = Fixture::new();
        let (id, _) = retain_multilingual(&mut fixture);
        service(
            &mut fixture.state,
            &mut fixture.surface,
            Instant::now(),
            true,
            None,
        );
        assert!(fixture.state.text_commit.dictation.recovery.is_none());
        assert!(
            fixture
                .state
                .copy_dictation_recovery(id, Instant::now())
                .is_err()
        );
        let (next, _) = retain_multilingual(&mut fixture);
        let expired = Instant::now() + super::super::RECOVERY_WINDOW;
        service(
            &mut fixture.state,
            &mut fixture.surface,
            expired,
            false,
            None,
        );
        assert!(fixture.state.text_commit.dictation.recovery.is_none());
        assert!(
            fixture
                .state
                .copy_dictation_recovery(next, expired)
                .is_err()
        );
        assert!(matches!(
            fixture.state.dictation_status(),
            DictationStatus::Unavailable(super::super::runtime::RuntimeError::TimedOut)
        ));
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
    #[test]
    fn native_dictation_rejection_keeps_the_complete_private_transcript() {
        let mut fixture = Fixture::new();
        let id = fixture.consent();
        fixture.state.shell_field_active = true;
        fixture
            .state
            .update_dictation(id, "日本語 café".into(), Instant::now())
            .unwrap();
        fixture.state.finish_dictation(id, Instant::now()).unwrap();
        assert_eq!(fixture.state.dictation_status(), DictationStatus::Finishing);
        assert_eq!(
            fixture.state.prepare_dictation(Instant::now()),
            Err(super::super::DictationError::Busy)
        );
        fixture
            .state
            .acknowledge_native_dictation(Some(ui::webview::text_input::Receipt::failed(
                ui::webview::text_input::Failure::Rejected,
            )));
        fixture.state.poll_native_dictation_delivery(Instant::now());
        let recovery = fixture
            .state
            .text_commit
            .dictation
            .recovery
            .as_ref()
            .unwrap();
        assert_eq!(&*recovery.text, "日本語 café");
        assert_eq!(recovery.reason, super::super::RecoveryReason::Rejected);
        assert_eq!(recovery.id, id);
        assert!(!recovery.copied);
        fixture.state.discard_dictation_recovery();
        assert!(fixture.state.text_commit.dictation.recovery.is_none());
    }

    #[test]
    fn disappearing_deferred_owner_cannot_be_acknowledged_as_a_synchronous_edit() {
        let mut fixture = Fixture::new();
        fixture.state.shell_field_active = true;
        fixture.state.set_native_dictation_deferred(true);
        let id = fixture.consent();
        fixture
            .state
            .update_dictation(id, "Original browser transcript".into(), Instant::now())
            .unwrap();
        fixture.state.finish_dictation(id, Instant::now()).unwrap();
        fixture.state.set_native_dictation_deferred(false);
        assert!(
            fixture.state.native_dictation_requires_ack(),
            "the original session retains its delivery contract"
        );
        fixture.state.acknowledge_native_dictation(None);
        fixture.state.poll_native_dictation_delivery(Instant::now());
        let recovery = fixture
            .state
            .text_commit
            .dictation
            .recovery
            .as_ref()
            .unwrap();
        assert_eq!(&*recovery.text, "Original browser transcript");
        assert_eq!(recovery.reason, super::super::RecoveryReason::Rejected);
    }

    #[test]
    fn dictionary_binding_replacement_revokes_original_native_final_and_recovery() {
        use settings_store::personal_dictionary::Service;
        let mut fixture = Fixture::new();
        let directory = tempfile::tempdir().unwrap();
        let dictionary = Service::new(directory.path().join("dictionary")).unwrap();
        let original = dictionary.bind(Some([1; 32])).unwrap();
        fixture
            .state
            .set_dictation_vocabulary(Some(std::sync::Arc::new(original)));
        let id = fixture.consent();
        fixture.state.shell_field_active = true;
        fixture
            .state
            .update_dictation(id, "Private original".into(), Instant::now())
            .unwrap();
        fixture.state.finish_dictation(id, Instant::now()).unwrap();
        assert!(
            fixture
                .state
                .text_commit
                .dictation
                .native_delivery
                .is_some()
        );
        let successor = dictionary.bind(Some([2; 32])).unwrap();
        fixture
            .state
            .set_dictation_vocabulary(Some(std::sync::Arc::new(successor)));
        assert!(
            fixture
                .state
                .text_commit
                .dictation
                .native_delivery
                .is_none()
        );
        assert!(fixture.state.dictation_target_id().is_none());
        assert!(
            fixture
                .state
                .update_dictation(id, "Late original".into(), Instant::now())
                .is_err()
        );
        fixture
            .state
            .poll_native_dictation_delivery(Instant::now() + std::time::Duration::from_secs(11));
        assert!(fixture.state.text_commit.dictation.recovery.is_none());
    }

    #[test]
    fn native_final_cancel_requires_its_original_session_and_drops_private_recovery() {
        let mut fixture = Fixture::new();
        let id = fixture.consent();
        fixture.state.shell_field_active = true;
        fixture
            .state
            .update_dictation(id, "Private original".into(), Instant::now())
            .unwrap();
        fixture.state.finish_dictation(id, Instant::now()).unwrap();
        assert!(!fixture.state.cancel_dictation(super::super::SessionId(0)));
        assert!(
            fixture
                .state
                .text_commit
                .dictation
                .native_delivery
                .is_some()
        );
        assert!(fixture.state.cancel_dictation(id));
        assert!(
            fixture
                .state
                .text_commit
                .dictation
                .native_delivery
                .is_none()
        );
        fixture
            .state
            .poll_native_dictation_delivery(Instant::now() + std::time::Duration::from_secs(11));
        assert!(fixture.state.text_commit.dictation.recovery.is_none());
    }

    #[test]
    fn unacknowledged_native_final_recovers_once_without_retargeting_or_retry() {
        let mut fixture = Fixture::new();
        let id = fixture.consent();
        fixture.state.shell_field_active = true;
        fixture
            .state
            .update_dictation(id, "Original private transcript".into(), Instant::now())
            .unwrap();
        fixture.state.finish_dictation(id, Instant::now()).unwrap();
        fixture.state.poll_native_dictation_delivery(Instant::now());
        assert!(fixture.state.text_commit.dictation.recovery.is_none());
        let expired = Instant::now() + std::time::Duration::from_secs(11);
        fixture.state.poll_native_dictation_delivery(expired);
        let recovery = fixture
            .state
            .text_commit
            .dictation
            .recovery
            .as_ref()
            .unwrap();
        assert_eq!(&*recovery.text, "Original private transcript");
        assert_eq!(
            recovery.reason,
            super::super::RecoveryReason::UnknownDelivery
        );
        fixture.state.acknowledge_native_dictation(None);
        fixture.state.poll_native_dictation_delivery(expired);
        assert_eq!(
            &*fixture
                .state
                .text_commit
                .dictation
                .recovery
                .as_ref()
                .unwrap()
                .text,
            "Original private transcript"
        );
        assert!(
            fixture
                .state
                .text_commit
                .dictation
                .native_delivery
                .is_none()
        );
    }
}
