use super::*;

#[test]
fn a_scroll_says_its_unit_and_only_a_finger_scroll_may_be_empty() {
    let scroll = |horizontal, vertical, unit| FabricInputEvent::Scroll {
        horizontal,
        vertical,
        unit,
    };
    assert!(scroll(0, -30, ScrollUnit::Lines).validate());
    assert!(
        scroll(0, 0, ScrollUnit::Pixels).validate(),
        "a finger scroll's end"
    );
    assert!(!scroll(0, 0, ScrollUnit::Lines).validate());
    assert!(!scroll(2_000_000, 0, ScrollUnit::Pixels).validate());
    let wire = serde_json::to_string(&scroll(0, -60, ScrollUnit::Lines)).unwrap();
    assert_eq!(
        wire,
        r#"{"scroll":{"horizontal":0,"vertical":-60,"unit":"lines"}}"#
    );
    assert!(
        serde_json::from_str::<FabricInputEvent>(r#"{"scroll":{"horizontal":0,"vertical":-2}}"#)
            .is_err(),
        "a scroll without a unit is not guessed at"
    );
}

fn id(value: &str) -> DeviceId {
    DeviceId::new(value).unwrap()
}
fn stamp(device: &str, counter: u64) -> FabricStamp {
    FabricStamp {
        counter,
        device: id(device),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn media_handoff_and_media_actions_are_capability_and_locator_checked() {
    fn media_action_locators_reject_host_paths_and_url_credentials() {
        let source = id("source");
        let target = id("target");
        for locator in [
            MediaSourceLocator::Workspace {
                path: "../private/song.flac".into(),
            },
            MediaSourceLocator::RemoteStream {
                url: "https://secret@example.com/song".into(),
            },
            MediaSourceLocator::AppResource {
                uri: "1music://track/foreign".into(),
            },
            MediaSourceLocator::PersonalTransfer {
                transfer_id: [0; 16],
                file_name: "../foreign.mp4".into(),
            },
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [4; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::ResumeMedia {
                    session_id: "player:one".into(),
                    app_id: "so.tilo.music".into(),
                    workspace_id: "11111111-1111-1111-1111-111111111111".into(),
                    title: "Track".into(),
                    locator,
                    position_ms: 0,
                    resume_playing: true,
                    has_video: false,
                },
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10_000),
                Err(FabricError::InvalidAction)
            );
        }
    }

    fn media_terminal_actions_require_exact_nonzero_request_and_session() {
        let source = id("source");
        let target = id("target");
        for action in [
            PersonalFabricAction::MediaRejected {
                request_id: [9; 16],
                session_id: "player:one".into(),
                failure: MediaHandoffFailure::OpenFailed,
            },
            PersonalFabricAction::MediaCancelled {
                request_id: [9; 16],
                session_id: "player:one".into(),
            },
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [4; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action,
            };
            assert_eq!(envelope.validate_at(&source, &target, 10_000), Ok(()));
        }

        let invalid = PersonalFabricActionEnvelope {
            request_id: [4; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::MediaRejected {
                request_id: [0; 16],
                session_id: "player:one".into(),
                failure: MediaHandoffFailure::OpenFailed,
            },
        };
        assert_eq!(
            invalid.validate_at(&source, &target, 10_000),
            Err(FabricError::InvalidAction)
        );
    }

    media_action_locators_reject_host_paths_and_url_credentials();
    media_terminal_actions_require_exact_nonzero_request_and_session();
}

#[test]
fn window_views_are_bounded_distinct_and_sized() {
    let source = id("viewer");
    let target = id("host");
    let envelope =
        |views: Vec<WindowView>, decode: Vec<HardwareDecode>| PersonalFabricActionEnvelope {
            request_id: [5; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::WindowViews { views, decode },
        };
    let view = |tab, width, height| WindowView { tab, width, height };
    let ok = |views| {
        envelope(views, Vec::new())
            .validate_at(&source, &target, 10_000)
            .is_ok()
    };
    let decodes = |decode| {
        envelope(vec![view(7, 1280, 800)], decode)
            .validate_at(&source, &target, 10_000)
            .is_ok()
    };
    let hw = |codec: &str, max| HardwareDecode {
        codec: codec.into(),
        max,
    };

    assert!(ok(Vec::new()), "an empty list ends every stream");
    assert!(ok(vec![
        view(7, 1280, 800),
        view(8, 1, MAX_WINDOW_VIEW_SIDE)
    ]));
    assert!(!ok(vec![view(0, 1280, 800)]), "a tab id is never zero");
    assert!(!ok(vec![view(7, 0, 800)]));
    assert!(!ok(vec![view(7, 1280, MAX_WINDOW_VIEW_SIDE + 1)]));
    assert!(
        !ok(vec![view(7, 1280, 800), view(7, 640, 480)]),
        "one size per tab"
    );
    assert!(!ok((1..=u64::try_from(MAX_WINDOW_VIEWS).unwrap() + 1)
        .map(|tab| view(tab, 100, 100))
        .collect()));

    // A board's decoder limit travels with the list.
    assert!(decodes(vec![
        hw("h264", Some((1920, 1088))),
        hw("av1", None)
    ]));
    assert!(!decodes(vec![hw("mjpeg", None)]), "a codec streams use");
    assert!(!decodes(vec![hw("h264", Some((0, 1088)))]));
    assert!(
        !decodes(vec![hw("h264", None), hw("h264", Some((1280, 720)))]),
        "one limit per codec"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn clipboard_and_nearby_payloads_are_bounded_and_tamper_checked() {
    fn clipboard_is_deduplicated_and_bounded() {
        let mut history = ClipboardHistory::default();
        for n in 0..=MAX_CLIPBOARD_ITEMS {
            history
                .push(
                    ClipboardEntry::shareable(
                        ClipboardPayload::Text(format!("item-{n}")),
                        id("laptop"),
                        n as u64,
                        stamp("laptop", n as u64),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        assert_eq!(history.entries().count(), MAX_CLIPBOARD_ITEMS);
    }

    fn clipboard_hash_tampering_is_rejected() {
        let mut entry = ClipboardEntry::shareable(
            ClipboardPayload::Text("safe".into()),
            id("phone"),
            1,
            stamp("phone", 1),
        )
        .unwrap();
        entry.payload = ClipboardPayload::Text("changed".into());
        assert_eq!(
            entry.validate(&id("phone")),
            Err(FabricError::ClipboardHashMismatch)
        );
        assert_eq!(
            entry.validate(&id("laptop")),
            Err(FabricError::ForeignAuthorship),
            "another device's copy is not this one's to validate"
        );
    }

    fn nearby_transfer_actions_bind_nonzero_ids_and_bounded_manifests() {
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        assert_eq!(
            envelope(PersonalFabricAction::NearbyTransferOffer {
                transfer_id: [4; 16],
                name: "Photos".into(),
                kind: tilo_nearby_service::TransferKind::FolderArchive,
                total_bytes: 100,
                content_hash: [5; 32],
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        assert_eq!(
            envelope(PersonalFabricAction::NearbyTransferReady {
                request_id: [0; 16],
                transfer_id: [4; 16],
                committed_offset: 0,
                prefix_hash: *blake3::hash(&[]).as_bytes(),
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::NearbyTransferOffer {
                transfer_id: [4; 16],
                name: "../escape".into(),
                kind: tilo_nearby_service::TransferKind::File,
                total_bytes: 100,
                content_hash: [5; 32],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
    }

    fn nearby_drop_rejects_paths_tampering_and_oversize_payloads() {
        let source = id("source");
        let target = id("target");
        for (name, bytes, hash) in [
            ("../secret", b"ok".to_vec(), *blake3::hash(b"ok").as_bytes()),
            ("note.txt", b"changed".to_vec(), [0; 32]),
            ("note.txt", vec![1; MAX_NEARBY_DROP_BYTES + 1], [0; 32]),
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [1; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10,
                action: PersonalFabricAction::NearbyDrop {
                    file_name: name.into(),
                    content_hash: hash,
                    bytes,
                },
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10),
                Err(FabricError::InvalidAction)
            );
        }
    }

    clipboard_is_deduplicated_and_bounded();
    clipboard_hash_tampering_is_rejected();
    nearby_transfer_actions_bind_nonzero_ids_and_bounded_manifests();
    nearby_drop_rejects_paths_tampering_and_oversize_payloads();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn remote_input_and_gamepad_actions_require_exact_bound_leases() {
    fn remote_input_requires_a_short_lived_explicit_lease() {
        let lease = InputLease {
            controller: id("laptop"),
            target: id("tablet"),
            issued_unix_ms: 100,
            expires_unix_ms: 200,
            nonce: [7; 16],
            allow_keyboard: true,
            allow_pointer: true,
        };
        assert_eq!(lease.validate_at(150), Ok(()));
        assert_eq!(lease.validate_at(201), Err(FabricError::ExpiredInputLease));
    }

    fn input_lease_must_be_issued_by_the_target_to_its_controller() {
        let controller = id("controller");
        let target = id("target");
        let lease = InputLease {
            controller: controller.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            expires_unix_ms: 20_000,
            nonce: [8; 16],
            allow_keyboard: true,
            allow_pointer: true,
        };
        let envelope = PersonalFabricActionEnvelope {
            request_id: [9; 16],
            source: target.clone(),
            target: controller.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::InputLeaseGranted { lease },
        };
        assert_eq!(envelope.validate_at(&target, &controller, 10_100), Ok(()));
        assert_eq!(
            envelope.validate_at(&id("forged-target"), &controller, 10_100),
            Err(FabricError::InvalidAction)
        );
    }

    fn input_actions_reject_unbound_or_malformed_events() {
        let source = id("controller");
        let target = id("target");
        for (nonce, event) in [
            ([0; 16], FabricInputEvent::PointerMotion { x: 10, y: 20 }),
            (
                [1; 16],
                FabricInputEvent::PointerButton {
                    button: 0,
                    pressed: true,
                },
            ),
            (
                [1; 16],
                FabricInputEvent::Key {
                    keycode: 0x300,
                    pressed: true,
                    modifiers: 0,
                },
            ),
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [2; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action: PersonalFabricAction::Input {
                    lease_nonce: nonce,
                    event,
                },
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
        }
    }

    fn gamepad_actions_are_typed_exact_bound_and_nonzero() {
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [8; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        assert!(
            envelope(PersonalFabricAction::GamepadForwardEvent {
                forwarding_id: [1; 16],
                lease_nonce: [2; 16],
                sequence: 1,
                event: os_actions::ForwardedGamepadEvent::Button {
                    button: os_actions::GamepadButton::South,
                    pressed: true,
                },
            })
            .validate_at(&source, &target, 10_001)
            .is_ok()
        );
        for action in [
            PersonalFabricAction::GamepadForwardGrant {
                forwarding_id: [1; 16],
                accepted: true,
                lease_nonce: [0; 16],
            },
            PersonalFabricAction::GamepadForwardGrant {
                forwarding_id: [1; 16],
                accepted: false,
                lease_nonce: [2; 16],
            },
            PersonalFabricAction::GamepadForwardEvent {
                forwarding_id: [1; 16],
                lease_nonce: [2; 16],
                sequence: 2,
                event: os_actions::ForwardedGamepadEvent::Hat { x: 2, y: 0 },
            },
        ] {
            assert_eq!(
                envelope(action).validate_at(&source, &target, 10_001),
                Err(FabricError::InvalidAction)
            );
        }
    }

    remote_input_requires_a_short_lived_explicit_lease();
    input_lease_must_be_issued_by_the_target_to_its_controller();
    input_actions_reject_unbound_or_malformed_events();
    gamepad_actions_are_typed_exact_bound_and_nonzero();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn wire_actions_are_fresh_source_bound_and_never_renumbered() {
    fn ephemeral_actions_are_transport_bound_and_fresh() {
        let source = id("source");
        let target = id("target");
        let envelope = PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::ResumeMedia {
                session_id: "player:one".into(),
                app_id: "so.tilo.music".into(),
                workspace_id: "11111111-1111-1111-1111-111111111111".into(),
                title: "Track".into(),
                locator: MediaSourceLocator::Workspace {
                    path: "Music/track.flac".into(),
                },
                position_ms: 42,
                resume_playing: true,
                has_video: false,
            },
        };
        assert_eq!(envelope.validate_at(&source, &target, 10_100), Ok(()));
        assert_eq!(
            envelope.validate_at(&id("intruder"), &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope.validate_at(&source, &id("other-target"), 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope.validate_at(&source, &target, 10_000 + MAX_FABRIC_ACTION_AGE_MS + 1),
            Err(FabricError::InvalidAction)
        );
    }

    fn new_action_variants_do_not_renumber_existing_wire_actions() {
        let input = postcard::to_allocvec(&PersonalFabricAction::Input {
            lease_nonce: [1; 16],
            event: FabricInputEvent::PointerMotion { x: 0, y: 0 },
        })
        .unwrap();
        assert_eq!(input.first(), Some(&4));
        // The retired actions hold their tags, so those after them keep theirs.
        for (retired, index) in [
            (PersonalFabricAction::RetiredBluetoothPrepare, 5),
            (PersonalFabricAction::RetiredBluetoothBondOffered, 10),
            (PersonalFabricAction::RetiredBluetoothCancel, 34),
            (PersonalFabricAction::RetiredWakeDevice, 51),
            (PersonalFabricAction::RetiredWakeProbeReply, 54),
        ] {
            assert_eq!(
                postcard::to_allocvec(&retired).unwrap().first(),
                Some(&index)
            );
        }
        let certify =
            postcard::to_allocvec(&PersonalFabricAction::CertifyAsk { ask: vec![1] }).unwrap();
        assert_eq!(certify.first(), Some(&55));
        let nearby = postcard::to_allocvec(&PersonalFabricAction::NearbyDrop {
            file_name: "note.txt".into(),
            content_hash: *blake3::hash(b"hello").as_bytes(),
            bytes: b"hello".to_vec(),
        })
        .unwrap();
        assert_eq!(nearby.first(), Some(&12));
        let notification = postcard::to_allocvec(&PersonalFabricAction::NotificationRetire {
            id: crate::fabric::notification::ContinuityNotificationId {
                epoch: [1; 16],
                token: [2; 16],
            },
        })
        .unwrap();
        assert_eq!(notification.first(), Some(&20));
        for (action, index) in [
            (
                PersonalFabricAction::GamepadForwardRequest {
                    forwarding_id: [3; 16],
                    app_id: "so.tilo.game".into(),
                    workspace_id: "workspace".into(),
                    session_id: "session".into(),
                    label: "Controller".into(),
                },
                23,
            ),
            (
                PersonalFabricAction::GamepadForwardGrant {
                    forwarding_id: [3; 16],
                    accepted: true,
                    lease_nonce: [4; 16],
                },
                24,
            ),
            (
                PersonalFabricAction::GamepadForwardEvent {
                    forwarding_id: [3; 16],
                    lease_nonce: [4; 16],
                    sequence: 1,
                    event: os_actions::ForwardedGamepadEvent::Hat { x: 0, y: -1 },
                },
                25,
            ),
            (
                PersonalFabricAction::GamepadForwardStop {
                    forwarding_id: [3; 16],
                    lease_nonce: [4; 16],
                },
                26,
            ),
        ] {
            assert_eq!(
                postcard::to_allocvec(&action).unwrap().first(),
                Some(&index)
            );
        }
    }

    fn notification_actions_are_source_bound_bounded_and_fresh() {
        let source = id("source");
        let target = id("target");
        let notification = crate::fabric::notification::ContinuityNotificationProjection {
            id: crate::fabric::notification::ContinuityNotificationId {
                epoch: [1; 16],
                token: [2; 16],
            },
            app_id: "so.tilo.mail".into(),
            app_name: "Mail".into(),
            title: "New message".into(),
            body: "Preview".into(),
            body_redacted: false,
            actions: vec![("reply".into(), "Reply".into())],
            urgency: 1,
            expires_unix_ms: 20_000,
        };
        let envelope = PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action: PersonalFabricAction::NotificationProject { notification },
        };
        assert!(envelope.validate_at(&source, &target, 10_000).is_ok());
        assert_eq!(
            envelope.validate_at(&id("spoof"), &target, 10_000),
            Err(FabricError::InvalidAction)
        );

        let invalid = PersonalFabricActionEnvelope {
            action: PersonalFabricAction::NotificationInvoke {
                id: crate::fabric::notification::ContinuityNotificationId {
                    epoch: [1; 16],
                    token: [2; 16],
                },
                action_key: "bad\naction".into(),
            },
            ..envelope
        };
        assert_eq!(
            invalid.validate_at(&source, &target, 10_000),
            Err(FabricError::InvalidAction)
        );
    }

    ephemeral_actions_are_transport_bound_and_fresh();
    new_action_variants_do_not_renumber_existing_wire_actions();
    notification_actions_are_source_bound_bounded_and_fresh();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn audio_and_bluetooth_wire_is_versioned_bounded_and_canonical() {
    fn remote_audio_wire_is_versioned_bounded_and_capability_bearing() {
        let source = id("source");
        let target = id("target");
        let envelope = |action| PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        };
        let offer = || PersonalFabricAction::RemoteAudioOffer {
            session_id: [4; 16],
            protocol_version: REMOTE_AUDIO_PROTOCOL_VERSION,
            label: "All system audio".into(),
            sample_rate_hz: REMOTE_AUDIO_SAMPLE_RATE_HZ,
            channels: REMOTE_AUDIO_CHANNELS,
            frame_samples: REMOTE_AUDIO_FRAME_SAMPLES,
        };
        assert_eq!(
            envelope(offer()).validate_at(&source, &target, 10_100),
            Ok(())
        );
        let mut unsupported = offer();
        if let PersonalFabricAction::RemoteAudioOffer {
            protocol_version, ..
        } = &mut unsupported
        {
            *protocol_version = REMOTE_AUDIO_PROTOCOL_VERSION + 1;
        }
        assert_eq!(
            envelope(unsupported).validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioDecision {
                session_id: [4; 16],
                accepted: true,
                authorization: None,
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioFrame {
                session_id: [4; 16],
                authorization: [5; 16],
                sequence: 1,
                media_ts: 48_000,
                presentation_ns: 1_000_000_000 + REMOTE_AUDIO_GROUP_LATENCY_NS,
                pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES],
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioFrame {
                session_id: [4; 16],
                authorization: [5; 16],
                sequence: 2,
                media_ts: 0,
                presentation_ns: REMOTE_AUDIO_GROUP_LATENCY_NS,
                pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES + 4],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioCancel {
                session_id: [4; 16],
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioCancel {
                session_id: [0; 16],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioKeepalive {
                session_id: [4; 16],
                authorization: [5; 16],
                playout: None,
            })
            .validate_at(&source, &target, 10_100),
            Ok(())
        );
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioKeepalive {
                session_id: [4; 16],
                authorization: [0; 16],
                playout: None,
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction)
        );
    }

    fn retired_actions_from_an_older_device_are_refused() {
        let source = id("source");
        let target = id("target");
        for action in [
            PersonalFabricAction::RetiredBluetoothPrepare,
            PersonalFabricAction::RetiredBluetoothCancel,
            PersonalFabricAction::RetiredWakeDevice,
        ] {
            let envelope = PersonalFabricActionEnvelope {
                request_id: [3; 16],
                source: source.clone(),
                target: target.clone(),
                issued_unix_ms: 10_000,
                action,
            };
            assert_eq!(
                envelope.validate_at(&source, &target, 10_100),
                Err(FabricError::InvalidAction)
            );
        }
    }

    remote_audio_wire_is_versioned_bounded_and_capability_bearing();
    retired_actions_from_an_older_device_are_refused();
}

#[test]
fn remote_audio_frames_play_after_capture_within_the_latency_bound() {
    let source = id("source");
    let target = id("target");
    let envelope = |action| PersonalFabricActionEnvelope {
        request_id: [3; 16],
        source: source.clone(),
        target: target.clone(),
        issued_unix_ms: 10_000,
        action,
    };
    for presentation_ns in [999_999_999, 2_000_000_001] {
        assert_eq!(
            envelope(PersonalFabricAction::RemoteAudioFrame {
                session_id: [4; 16],
                authorization: [5; 16],
                sequence: 1,
                media_ts: 48_000,
                presentation_ns,
                pcm_s16le: vec![0; MAX_REMOTE_AUDIO_FRAME_BYTES],
            })
            .validate_at(&source, &target, 10_100),
            Err(FabricError::InvalidAction),
            "a frame played before its capture or beyond the latency bound"
        );
    }
}

#[test]
fn speaker_group_wire_rejects_zero_identities_and_bad_names() {
    use crate::fabric::speaker_group::{
        EndpointSelector, RoomPoint, SpeakerGroupId, SpeakerRole, StereoSide,
    };
    let source = id("source");
    let target = id("target");
    let envelope = |action| PersonalFabricActionEnvelope {
        request_id: [3; 16],
        source: source.clone(),
        target: target.clone(),
        issued_unix_ms: 10_000,
        action,
    };
    let check = |action| envelope(action).validate_at(&source, &target, 10_100);
    let invite = |group, name: &str, output, role| PersonalFabricAction::SpeakerGroupInvite {
        group,
        name: name.into(),
        output,
        role,
    };
    let group = SpeakerGroupId([1; 16]);
    let output = EndpointSelector([2; 16]);
    let right = SpeakerRole::Stereo(StereoSide::Right);
    assert_eq!(check(invite(group, "Living room", output, right)), Ok(()));
    for bad in [
        invite(SpeakerGroupId([0; 16]), "Living room", output, right),
        invite(group, "", output, right),
        invite(group, "line\nbreak", output, right),
        invite(group, "Living room", EndpointSelector([0; 16]), right),
        invite(
            group,
            "Living room",
            output,
            SpeakerRole::RoomPoint(RoomPoint {
                x_mm: i32::MAX,
                y_mm: 0,
                z_mm: 0,
            }),
        ),
        PersonalFabricAction::SpeakerGroupSessionOffer {
            session_id: [0; 16],
            group,
        },
        PersonalFabricAction::SpeakerGroupSessionDecision {
            session_id: [4; 16],
            group,
            authorization: Some([0; 16]),
        },
        PersonalFabricAction::SpeakerGroupSessionStop {
            session_id: [4; 16],
            authorization: Some([0; 16]),
        },
        PersonalFabricAction::SpeakerGroupLeave {
            group: SpeakerGroupId([0; 16]),
        },
    ] {
        assert_eq!(
            check(bad.clone()),
            Err(FabricError::InvalidAction),
            "{bad:?}"
        );
    }
    assert_eq!(
        check(PersonalFabricAction::SpeakerGroupSessionDecision {
            session_id: [4; 16],
            group,
            authorization: None,
        }),
        Ok(())
    );
}

#[test]
fn a_group_led_by_another_device_is_refused() {
    use crate::fabric::speaker_group::{
        EndpointSelector, SpeakerFabricState, SpeakerGroup, SpeakerGroupId, SpeakerGroupMember,
        SpeakerRole, SpeakerTrim, StereoSide,
    };
    let member = |device: &str, side| SpeakerGroupMember {
        device: id(device),
        output: EndpointSelector([9; 16]),
        role: SpeakerRole::Stereo(side),
        trim: SpeakerTrim::default(),
    };
    let group = SpeakerGroup {
        id: SpeakerGroupId([5; 16]),
        name: "Studio pair".into(),
        leader: id("tablet"),
        members: vec![
            member("laptop", StereoSide::Left),
            member("tablet", StereoSide::Right),
        ],
        stamp: stamp("laptop", 1),
    };
    let mut state = SpeakerFabricState {
        groups: vec![group],
        ..SpeakerFabricState::default()
    };
    assert_eq!(
        state.validate(&id("laptop")),
        Err(FabricError::ForeignAuthorship),
        "a device does not write a group another device leads"
    );
    state.groups[0].leader = id("laptop");
    assert_eq!(state.validate(&id("laptop")), Ok(()));
}
