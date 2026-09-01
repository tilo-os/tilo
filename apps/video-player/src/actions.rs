//! Discoverable effect/view operations for the local video player.

use automerge::AutoCommit;
use qos_actions::{ActionSpec, EncodedAction, OperationKind, Resource};
use serde::{Deserialize, Serialize};

pub const OPERATION_TYPE: &str = "video-player.operation";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlayerOperation {
    OpenPicker,
    OpenPath(String),
    TogglePlayback,
    ToggleMute,
    SeekSeconds(f64),
}

fn encode(op: &PlayerOperation) -> EncodedAction {
    EncodedAction::encode_operation(OPERATION_TYPE, op).unwrap_or_else(|_| EncodedAction {
        type_id: OPERATION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

pub fn resources(title: &str, has_media: bool, _doc: &AutoCommit) -> Vec<Resource> {
    let mut player = Resource::new("video-player.current", "media.video", title)
        .with_action(
            ActionSpec::operation(
                "open",
                "Open video",
                OperationKind::Effect,
                OPERATION_TYPE,
                &PlayerOperation::OpenPicker,
            )
            .unwrap_or_else(|_| {
                ActionSpec::prompted_operation(
                    "open-path",
                    "Open video path",
                    "Video path",
                    "/path/to/video.mp4",
                    OperationKind::Effect,
                    |path| encode(&PlayerOperation::OpenPath(path.to_string())),
                )
            }),
        )
        .with_action(ActionSpec::prompted_operation(
            "open-path",
            "Open video path",
            "Video path",
            "/path/to/video.mp4",
            OperationKind::Effect,
            |path| encode(&PlayerOperation::OpenPath(path.to_string())),
        ));
    if has_media {
        for (id, label, op) in [
            (
                "play-pause",
                "Play or pause",
                PlayerOperation::TogglePlayback,
            ),
            ("mute", "Mute or unmute", PlayerOperation::ToggleMute),
        ] {
            if let Ok(spec) =
                ActionSpec::operation(id, label, OperationKind::View, OPERATION_TYPE, &op)
            {
                player = player.with_action(spec);
            }
        }
        player = player.with_action(ActionSpec::prompted_operation(
            "seek",
            "Seek to seconds",
            "Position in seconds",
            "30",
            OperationKind::View,
            |value| {
                let seconds = value.parse::<f64>().unwrap_or(0.0).max(0.0);
                encode(&PlayerOperation::SeekSeconds(seconds))
            },
        ));
    }
    vec![player]
}

pub fn decode(type_id: &str, payload: &[u8]) -> Result<Option<PlayerOperation>, String> {
    if type_id != OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}
