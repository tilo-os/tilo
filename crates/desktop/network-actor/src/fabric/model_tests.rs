use super::*;
use crate::fabric::{DeviceId, FabricStamp};

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
            MediaSourceLocator::RetiredPersonalTransfer {
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
        let cancel = postcard::to_allocvec(&PersonalFabricAction::RemoteAudioCancel {
            session_id: [1; 16],
        })
        .unwrap();
        assert_eq!(cancel.first(), Some(&6));
    }

    ephemeral_actions_are_transport_bound_and_fresh();
    new_action_variants_do_not_renumber_existing_wire_actions();
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

    remote_audio_wire_is_versioned_bounded_and_capability_bearing();
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
fn a_retired_speaker_group_action_is_refused() {
    let source = id("source");
    let target = id("target");
    let check = |action| {
        PersonalFabricActionEnvelope {
            request_id: [3; 16],
            source: source.clone(),
            target: target.clone(),
            issued_unix_ms: 10_000,
            action,
        }
        .validate_at(&source, &target, 10_100)
    };
    for retired in [
        PersonalFabricAction::RetiredSpeakerGroupInvite,
        PersonalFabricAction::RetiredSpeakerGroupDecision,
        PersonalFabricAction::RetiredSpeakerGroupLeave,
        PersonalFabricAction::RetiredSpeakerGroupSessionOffer,
        PersonalFabricAction::RetiredSpeakerGroupSessionDecision,
        PersonalFabricAction::RetiredSpeakerGroupSessionStop,
    ] {
        assert_eq!(
            check(retired.clone()),
            Err(FabricError::InvalidAction),
            "{retired:?}"
        );
    }
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
