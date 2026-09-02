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
    let open_contract = ActionContract {
        retry: RetryPolicy::AtMostOnce,
        tags: vec!["filesystem".into(), "media".into()],
        ..ActionContract::default()
    };
    let mut player = Resource::new("video-player.current", "media.video", title).with_action(
        ActionSpec::typed_operation(
            "open",
            "Open video",
            "path (omit to use the system file picker)",
            OperationKind::Effect,
            OPERATION_TYPE,
            vec![ActionParam::optional_owned(
                "path",
                ParamType::Text,
                "Direct video path",
            )],
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
            ..open_contract
        }),
    );
    for (id, label, op) in [
        (
            "play-pause",
            "Play or pause",
            PlayerOperation::TogglePlayback,
        ),
        ("mute", "Mute or unmute", PlayerOperation::ToggleMute),
    ] {
        if let Ok(spec) = ActionSpec::operation(id, label, OperationKind::View, OPERATION_TYPE, &op)
        {
            player = player.with_action(if has_media {
                spec
            } else {
                spec.unavailable("no_media", "Open a video before using playback controls")
            });
        }
    }
    let seek = ActionSpec::typed_operation(
        "seek",
        "Seek to seconds",
        "30",
        OperationKind::View,
        OPERATION_TYPE,
        vec![ActionParam::required_owned(
            "seconds",
            ParamType::Number,
            "Non-negative playback position in seconds",
        )],
        |args: &ActionArgs| {
            encode(&PlayerOperation::SeekSeconds(
                args.number("seconds").unwrap_or(0.0).max(0.0),
            ))
        },
    );
    player = player.with_action(if has_media {
        seek
    } else {
        seek.unavailable("no_media", "Open a video before seeking")
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_player_keeps_controls_discoverable_with_reasons_and_typed_seek() {
        let resources = resources("No media", false, &AutoCommit::new());
        let actions = &resources[0].actions;
        let open = actions.iter().find(|a| a.id == "open").unwrap();
        assert_eq!(
            open.contract.execution,
            qos_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(open.contract.result, qos_actions::ResultSpec::Job);
        let play = actions.iter().find(|a| a.id == "play-pause").unwrap();
        assert!(!play.contract.availability.is_available());
        let seek = actions.iter().find(|a| a.id == "seek").unwrap();
        assert_eq!(seek.params()[0].ty, ParamType::Number);
        let mut args = serde_json::Map::new();
        args.insert("seconds".into(), serde_json::json!(12.5));
        let encoded = seek.build_json(&args).unwrap();
        assert_eq!(
            decode(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PlayerOperation::SeekSeconds(12.5))
        );
    }
}
