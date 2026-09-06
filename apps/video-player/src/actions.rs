//! Discoverable effect/view operations for the local video player.

use automerge::AutoCommit;
use qos_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, Resource, ResultSpec, RetryPolicy,
};
use serde::{Deserialize, Serialize};

pub const OPERATION_TYPE: &str = "video-player.operation";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlayerOperation {
    Open { path: Option<String> },
    Close,
    TogglePlayback,
    ToggleMute,
    SeekSeconds(f64),
    SeekBySeconds(f64),
    Restart,
    SeekToEnd,
    SetVolume(f64),
    SetAutoplay(bool),
    SetLoop(bool),
    ShowTracksUnavailable,
    ShowChaptersUnavailable,
    SetPlaybackRateUnavailable(f64),
    ToggleFullscreen,
}

impl PlayerOperation {
    /// Whether this operation is meaningful only while a media source is open.
    pub fn requires_media(&self) -> bool {
        matches!(
            self,
            Self::Close
                | Self::TogglePlayback
                | Self::ToggleMute
                | Self::SeekSeconds(_)
                | Self::SeekBySeconds(_)
                | Self::Restart
                | Self::SeekToEnd
                | Self::SetVolume(_)
                | Self::SetLoop(_)
        )
    }
}

/// State captured while publishing the player resource tree.
///
/// It deliberately describes only capabilities the shared decoder currently
/// provides. Container stream metadata, chapters, and rate control remain
/// visible as explicitly unavailable rather than being guessed from a file.
pub struct PlayerResourceCtx<'a> {
    /// Human-readable title of the open item, or the empty-state label.
    pub title: &'a str,
    /// Current source and collaboration authority state.
    pub media: MediaAvailability,
    /// The implemented, app-owned playback preferences.
    pub settings: PlaybackSettingsState,
}

/// Media availability and shared-transport authority at publication time.
pub struct MediaAvailability {
    /// Whether a source is open (it may still be decoding its first frame).
    pub has_media: bool,
    /// Whether this peer may change shared transport state.
    pub can_control_transport: bool,
}

/// Playback preferences that this app actually implements.
pub struct PlaybackSettingsState {
    /// Whether opening a new item starts playback automatically.
    pub autoplay: bool,
    /// Whether the host repeats the current item at its end.
    pub loop_enabled: bool,
}

fn encode(op: &PlayerOperation) -> EncodedAction {
    EncodedAction::encode_operation(OPERATION_TYPE, op).unwrap_or_else(|_| EncodedAction {
        type_id: OPERATION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

/// Publish a limitation as a normal, searchable action descriptor. The action
/// is unavailable at the protocol boundary, and its payload is still decoded
/// defensively if an untrusted caller tries to bypass discovery.
fn unavailable_action(
    id: &'static str,
    label: &'static str,
    operation: PlayerOperation,
    code: &'static str,
    reason: &'static str,
) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "",
        OperationKind::View,
        OPERATION_TYPE,
        Vec::new(),
        move |_| encode(&operation),
    )
    .unavailable(code, reason)
}

struct UnavailableResourceSpec {
    id: &'static str,
    kind: &'static str,
    label: &'static str,
    action_id: &'static str,
    action_label: &'static str,
    operation: PlayerOperation,
    code: &'static str,
    reason: &'static str,
}

fn unavailable_resource(spec: UnavailableResourceSpec) -> Resource {
    Resource::new(spec.id, spec.kind, spec.label).with_action(unavailable_action(
        spec.action_id,
        spec.action_label,
        spec.operation,
        spec.code,
        spec.reason,
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "the small resource tree is kept together so labels, contracts, and hierarchy cannot drift"
)]
pub fn resources(ctx: &PlayerResourceCtx<'_>, _doc: &AutoCommit) -> Vec<Resource> {
    let open_contract = ActionContract {
        retry: RetryPolicy::AtMostOnce,
        tags: vec!["filesystem".into(), "media".into()],
        ..ActionContract::default()
    };
    let mut app = Resource::new(
        "video-player.application",
        "video-player.application",
        "Video Player",
    )
    .with_action(
        ActionSpec::typed_operation(
            "open",
            "Open video",
            "path (omit to use the system file picker)",
            OperationKind::Effect,
            OPERATION_TYPE,
            vec![
                ActionParam::optional_owned("path", ParamType::Text, "Direct video path")
                    .with_schema(serde_json::json!({"minLength": 1})),
            ],
            |args: &ActionArgs| {
                encode(&PlayerOperation::Open {
                    path: args
                        .text("path")
                        .filter(|p| !p.trim().is_empty())
                        .map(str::to_string),
                })
            },
        )
        .with_contract(ActionContract {
            execution: ExecutionMode::Background { cancellable: false },
            result: ResultSpec::Job,
            permission: qos_actions::PermissionRequirement::Read,
            ..open_contract
        }),
    );

    let mut transport = Resource::new(
        "video-player.transport.current",
        "media.transport",
        "Playback",
    );
    for (id, label, op) in [(
        "play-pause",
        "Play or pause",
        PlayerOperation::TogglePlayback,
    )] {
        if let Ok(spec) = ActionSpec::operation(id, label, OperationKind::View, OPERATION_TYPE, &op)
        {
            // A retry would toggle twice, so it must never be declared
            // idempotent merely because it is a view operation.
            let spec = spec.retry_policy(RetryPolicy::AtMostOnce);
            transport = transport.with_action(if !ctx.media.has_media {
                spec.unavailable("no_media", "Open a video before using playback controls")
            } else if ctx.media.can_control_transport {
                spec
            } else {
                spec.unavailable(
                    "following_host",
                    "This player is following the collaborative host",
                )
            });
        }
    }
    let seek = ActionSpec::typed_operation(
        "seek",
        "Seek to seconds",
        "30",
        OperationKind::View,
        OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "seconds",
                ParamType::Number,
                "Non-negative playback position in seconds",
            )
            .with_schema(serde_json::json!({"minimum": 0})),
        ],
        |args: &ActionArgs| {
            encode(&PlayerOperation::SeekSeconds(
                args.number("seconds").unwrap_or(0.0).max(0.0),
            ))
        },
    );
    transport = transport.with_action(if !ctx.media.has_media {
        seek.unavailable("no_media", "Open a video before seeking")
    } else if ctx.media.can_control_transport {
        seek
    } else {
        seek.unavailable(
            "following_host",
            "This player is following the collaborative host",
        )
    });

    let volume = ActionSpec::typed_operation(
        "set-volume",
        "Set volume",
        "0.8",
        OperationKind::View,
        OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "volume",
                ParamType::Number,
                "Volume from 0 (silent) to 1 (full)",
            )
            .with_schema(serde_json::json!({"minimum": 0, "maximum": 1})),
        ],
        |args: &ActionArgs| {
            encode(&PlayerOperation::SetVolume(
                args.number("volume").unwrap_or(0.8).clamp(0.0, 1.0),
            ))
        },
    );
    let seek_by = ActionSpec::typed_operation(
        "seek-by",
        "Seek by seconds",
        "10",
        OperationKind::View,
        OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "seconds",
                ParamType::Number,
                "Signed offset from the current position (at most one hour)",
            )
            .with_schema(serde_json::json!({"minimum": -3600, "maximum": 3600})),
        ],
        |args: &ActionArgs| {
            encode(&PlayerOperation::SeekBySeconds(
                args.number("seconds").unwrap_or(0.0).clamp(-3600.0, 3600.0),
            ))
        },
    )
    .retry_policy(RetryPolicy::AtMostOnce);
    let mut navigation = Resource::new(
        "video-player.navigation.current",
        "media.navigation",
        "Timeline navigation",
    )
    .with_action(if !ctx.media.has_media {
        seek_by.unavailable("no_media", "Open a video before navigating its timeline")
    } else if ctx.media.can_control_transport {
        seek_by
    } else {
        seek_by.unavailable(
            "following_host",
            "This player is following the collaborative host",
        )
    });
    for (id, label, op) in [
        ("restart", "Restart", PlayerOperation::Restart),
        ("jump-to-end", "Jump to end", PlayerOperation::SeekToEnd),
    ] {
        if let Ok(spec) = ActionSpec::operation(id, label, OperationKind::View, OPERATION_TYPE, &op)
        {
            navigation = navigation.with_action(if !ctx.media.has_media {
                spec.unavailable("no_media", "Open a video before navigating its timeline")
            } else if ctx.media.can_control_transport {
                spec
            } else {
                spec.unavailable(
                    "following_host",
                    "This player is following the collaborative host",
                )
            });
        }
    }

    let mut audio = Resource::new("video-player.audio.current", "media.audio", "Audio");
    if let Ok(mute) = ActionSpec::operation(
        "mute",
        "Mute or unmute",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::ToggleMute,
    ) {
        // A retry would restore the prior state, so mute is at-most-once.
        let mute = mute.retry_policy(RetryPolicy::AtMostOnce);
        audio = audio.with_action(if ctx.media.has_media {
            mute
        } else {
            mute.unavailable("no_media", "Open a video before changing audio")
        });
    }
    audio = audio.with_action(if ctx.media.has_media {
        volume
    } else {
        volume.unavailable("no_media", "Open a video before changing audio")
    });

    let autoplay = ActionSpec::typed_operation(
        "set-autoplay",
        "Set autoplay",
        "true",
        OperationKind::View,
        OPERATION_TYPE,
        vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether newly opened videos begin playing automatically",
        )],
        |args: &ActionArgs| {
            encode(&PlayerOperation::SetAutoplay(
                args.boolean("enabled").unwrap_or(true),
            ))
        },
    );
    let loop_action = ActionSpec::typed_operation(
        "set-loop",
        "Set repeat current video",
        "false",
        OperationKind::View,
        OPERATION_TYPE,
        vec![ActionParam::required_owned(
            "enabled",
            ParamType::Boolean,
            "Whether the current video repeats after it finishes",
        )],
        |args: &ActionArgs| {
            encode(&PlayerOperation::SetLoop(
                args.boolean("enabled").unwrap_or(false),
            ))
        },
    );
    let settings_label = format!(
        "Playback settings · autoplay {} · repeat {}",
        if ctx.settings.autoplay { "on" } else { "off" },
        if ctx.settings.loop_enabled {
            "on"
        } else {
            "off"
        }
    );
    let mut settings = Resource::new(
        "video-player.settings.current",
        "media.player-settings",
        settings_label,
    )
    .with_action(autoplay)
    .with_action(if !ctx.media.has_media {
        loop_action.unavailable("no_media", "Open a video before enabling repeat")
    } else if ctx.media.can_control_transport {
        loop_action
    } else {
        loop_action.unavailable(
            "following_host",
            "Only the collaborative host may change repeat playback",
        )
    });

    let tracks = unavailable_resource(UnavailableResourceSpec {
        id: "video-player.tracks.current",
        kind: "media.tracks",
        label: "Audio and subtitle tracks",
        action_id: "show-track-metadata",
        action_label: "Track metadata unavailable",
        operation: PlayerOperation::ShowTracksUnavailable,
        code: "track_metadata_unavailable",
        reason: "The shared decoder does not yet enumerate or switch embedded audio/subtitle tracks",
    });
    let chapters = unavailable_resource(UnavailableResourceSpec {
        id: "video-player.chapters.current",
        kind: "media.chapters",
        label: "Chapters",
        action_id: "show-chapters",
        action_label: "Chapter metadata unavailable",
        operation: PlayerOperation::ShowChaptersUnavailable,
        code: "chapter_metadata_unavailable",
        reason: "The shared decoder does not yet expose container chapter metadata",
    });
    settings = settings.with_action(unavailable_action(
        "set-playback-rate",
        "Set playback speed",
        PlayerOperation::SetPlaybackRateUnavailable(1.0),
        "playback_rate_unavailable",
        "The shared decoder does not yet support playback-rate control",
    ));

    let mut presentation = Resource::new(
        "video-player.presentation.current",
        "media.presentation",
        "Presentation",
    );
    if let Ok(fullscreen) = ActionSpec::operation(
        "fullscreen",
        "Enter or exit fullscreen",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::ToggleFullscreen,
    ) {
        // Retrying an edge-triggered fullscreen toggle would exit again.
        presentation = presentation.with_action(fullscreen.retry_policy(RetryPolicy::AtMostOnce));
    }

    let mut media = Resource::new("video-player.media.current", "media.video", ctx.title);
    if let Ok(close) = ActionSpec::operation(
        "close",
        "Close video",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::Close,
    ) {
        media = media.with_action(if ctx.media.has_media {
            close
        } else {
            close.unavailable("no_media", "No video is open")
        });
    }
    media = media
        .with_child(transport)
        .with_child(navigation)
        .with_child(audio)
        .with_child(tracks)
        .with_child(chapters)
        .with_child(settings)
        .with_child(presentation);
    app = app.with_child(media);
    vec![app]
}

/// Decode a Video Player operation payload when `type_id` belongs to this app.
///
/// # Errors
///
/// Returns an error when a Video Player payload is not valid JSON for
/// [`PlayerOperation`]. Unknown operation types return `Ok(None)`.
pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<PlayerOperation>, String> {
    if type_id != OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(has_media: bool) -> PlayerResourceCtx<'static> {
        PlayerResourceCtx {
            title: if has_media { "Clip.mp4" } else { "No media" },
            media: MediaAvailability {
                has_media,
                can_control_transport: true,
            },
            settings: PlaybackSettingsState {
                autoplay: true,
                loop_enabled: false,
            },
        }
    }

    #[test]
    fn resource_tree_is_stateful_hierarchical_and_strictly_typed() {
        let idle_resources = resources(&ctx(false), &AutoCommit::new());
        let actions = &idle_resources[0].actions;
        let open = actions.iter().find(|a| a.id == "open").unwrap();
        assert_eq!(
            open.contract.execution,
            qos_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(open.contract.result, qos_actions::ResultSpec::Job);
        let idle_media = &idle_resources[0].children[0];
        assert_eq!(idle_media.label, "No media");
        assert!(
            !idle_media.children[0].actions[0]
                .contract
                .availability
                .is_available()
        );

        let loaded = resources(&ctx(true), &AutoCommit::new());
        let media = &loaded[0].children[0];
        let transport = &media.children[0];
        let play = transport
            .actions
            .iter()
            .find(|a| a.id == "play-pause")
            .unwrap();
        assert!(play.contract.availability.is_available());
        let seek = transport.actions.iter().find(|a| a.id == "seek").unwrap();
        assert_eq!(seek.params()[0].ty, ParamType::Number);
        let mut args = serde_json::Map::new();
        args.insert("seconds".into(), serde_json::json!(12.5));
        let encoded = seek.build_json(&args).unwrap();
        assert_eq!(
            decode(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PlayerOperation::SeekSeconds(12.5))
        );
        assert_eq!(media.children[1].kind, "media.navigation");
        assert_eq!(media.children[2].kind, "media.audio");
        assert_eq!(media.children[3].kind, "media.tracks");
        assert_eq!(media.children[4].kind, "media.chapters");
        assert_eq!(media.children[5].kind, "media.player-settings");
        assert_eq!(media.children[6].kind, "media.presentation");
        let volume = media.children[2]
            .actions
            .iter()
            .find(|action| action.id == "set-volume")
            .unwrap();
        let invalid = serde_json::json!({"volume": 2.0})
            .as_object()
            .unwrap()
            .clone();
        assert!(volume.build_json(&invalid).is_err());

        let seek_by = media.children[1]
            .actions
            .iter()
            .find(|action| action.id == "seek-by")
            .expect("relative seek action");
        let encoded = seek_by
            .build_json(
                &serde_json::json!({"seconds": -10})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            decode(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PlayerOperation::SeekBySeconds(-10.0))
        );
        assert_eq!(seek_by.contract.retry, RetryPolicy::AtMostOnce);
        assert_eq!(play.contract.retry, RetryPolicy::AtMostOnce);
        assert!(
            !media.children[3].actions[0]
                .contract
                .availability
                .is_available(),
            "unsupported tracks remain discoverable but fail closed"
        );
    }
}
