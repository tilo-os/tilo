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
    SetVolume(f64),
    ToggleFullscreen,
}

fn encode(op: &PlayerOperation) -> EncodedAction {
    EncodedAction::encode_operation(OPERATION_TYPE, op).unwrap_or_else(|_| EncodedAction {
        type_id: OPERATION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the small resource tree is kept together so labels, contracts, and hierarchy cannot drift"
)]
pub fn resources(
    title: &str,
    has_media: bool,
    can_control_transport: bool,
    _doc: &AutoCommit,
) -> Vec<Resource> {
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
            transport = transport.with_action(if !has_media {
                spec.unavailable("no_media", "Open a video before using playback controls")
            } else if can_control_transport {
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
    transport = transport.with_action(if !has_media {
        seek.unavailable("no_media", "Open a video before seeking")
    } else if can_control_transport {
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
    let mut audio = Resource::new("video-player.audio.current", "media.audio", "Audio");
    if let Ok(mute) = ActionSpec::operation(
        "mute",
        "Mute or unmute",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::ToggleMute,
    ) {
        audio = audio.with_action(if has_media {
            mute
        } else {
            mute.unavailable("no_media", "Open a video before changing audio")
        });
    }
    audio = audio.with_action(if has_media {
        volume
    } else {
        volume.unavailable("no_media", "Open a video before changing audio")
    });

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
        presentation = presentation.with_action(fullscreen);
    }

    let mut media = Resource::new("video-player.media.current", "media.video", title);
    if let Ok(close) = ActionSpec::operation(
        "close",
        "Close video",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::Close,
    ) {
        media = media.with_action(if has_media {
            close
        } else {
            close.unavailable("no_media", "No video is open")
        });
    }
    media = media
        .with_child(transport)
        .with_child(audio)
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

    #[test]
    fn resource_tree_is_stateful_hierarchical_and_strictly_typed() {
        let idle_resources = resources("No media", false, true, &AutoCommit::new());
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

        let loaded = resources("Clip.mp4", true, true, &AutoCommit::new());
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
        assert_eq!(media.children[1].kind, "media.audio");
        assert_eq!(media.children[2].kind, "media.presentation");
        let volume = media.children[1]
            .actions
            .iter()
            .find(|action| action.id == "set-volume")
            .unwrap();
        let invalid = serde_json::json!({"volume": 2.0})
            .as_object()
            .unwrap()
            .clone();
        assert!(volume.build_json(&invalid).is_err());
    }
}
