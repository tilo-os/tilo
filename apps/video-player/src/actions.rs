//! Discoverable effect/view operations for the local video player.

use automerge::AutoCommit;
use qos_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, Resource, ResultSpec, RetryPolicy,
};
use serde::{Deserialize, Serialize};

pub const OPERATION_TYPE: &str = "video-player.operation";
/// The contextual resource snapshot remains responsive even for containers
/// with an unusually large number of streams or chapters. Stable IDs still let
/// a host resolve a specific omitted item on demand.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlayerOperation {
    Open {
        path: Option<String>,
    },
    Close,
    TogglePlayback,
    ToggleMute,
    /// Bring the current player into view without changing playback state.
    Reveal,
    SeekSeconds(f64),
    SeekBySeconds(f64),
    Restart,
    SeekToEnd,
    SetVolume(f64),
    SetAutoplay(bool),
    SetLoop(bool),
    /// Choose an enumerated embedded audio stream.
    SelectAudioTrack {
        source_id: String,
        stream_id: String,
        index: u32,
    },
    /// Choose an embedded subtitle stream; `None` turns captions off.
    SelectSubtitleTrack {
        source_id: String,
        /// Requested subtitle stream. `None` turns captions off.
        target_stream_id: Option<String>,
        target_index: Option<u32>,
        /// Selection that was current when this action was published. This
        /// distinguishes "turn off" from "select the current track again".
        expected_stream_id: Option<String>,
        expected_index: Option<u32>,
    },
    /// A safe navigation action for a decoder-reported chapter.
    JumpToChapter {
        source_id: String,
        id: String,
        seconds: f64,
    },
    /// Set the shared transport speed in the bounded player range.
    SetPlaybackRate(f64),
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
                | Self::Reveal
                | Self::SeekSeconds(_)
                | Self::SeekBySeconds(_)
                | Self::Restart
                | Self::SeekToEnd
                | Self::SetVolume(_)
                | Self::SelectAudioTrack { .. }
                | Self::SelectSubtitleTrack { .. }
                | Self::JumpToChapter { .. }
                | Self::SetPlaybackRate(_)
        )
    }
}

/// State captured while publishing the player resource tree.
///
pub struct PlayerResourceCtx<'a> {
    /// Human-readable title of the open item, or the empty-state label.
    pub title: &'a str,
    /// Current source and collaboration authority state.
    pub media: MediaAvailability,
    /// The implemented, app-owned playback preferences.
    pub settings: PlaybackSettingsState,
    /// Decoder-reported audio streams. Empty means the active media contains
    /// none or discovery has not completed; it is never a fabricated default.
    pub audio_tracks: &'a [TrackState],
    /// Decoder-reported subtitle streams.
    pub subtitle_tracks: &'a [TrackState],
    /// Container chapters already materialized from a trusted source.
    pub chapters: &'a [ChapterState],
}

/// Media availability and shared-transport authority at publication time.
pub struct MediaAvailability {
    /// Whether a source is open (it may still be decoding its first frame).
    pub has_media: bool,
    /// Exact identity of the currently-open source. Dynamic targets are never
    /// published without it, so an old action cannot act on a later file.
    pub source_id: Option<String>,
    /// Whether this peer may change shared transport state.
    pub can_control_transport: bool,
}

/// Playback preferences that this app actually implements.
pub struct PlaybackSettingsState {
    /// Whether opening a new item starts playback automatically.
    pub autoplay: bool,
    /// Whether the host repeats the current item at its end.
    pub loop_enabled: bool,
    /// Current effective speed (`0.25..=4.0`).
    pub rate: f32,
}

/// Stream metadata the decoder actually reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackState {
    pub stream_id: String,
    pub index: u32,
    pub label: String,
    pub selected: bool,
}

/// A compact, deterministic URI/stream token for a resource address. The raw
/// identity remains in the operation payload and is compared before execution.
pub fn identity_token_for_action(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len().saturating_mul(2));
    for byte in value.bytes() {
        encoded.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        encoded.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap_or('0'));
    }
    encoded
}

pub fn audio_resource_id(source_id: &str, stream_id: &str) -> String {
    format!(
        "video-player.tracks.audio.{}.{}",
        identity_token_for_action(source_id),
        identity_token_for_action(stream_id)
    )
}

pub fn subtitle_resource_id(source_id: &str, stream_id: &str) -> String {
    format!(
        "video-player.tracks.subtitle.{}.{}",
        identity_token_for_action(source_id),
        identity_token_for_action(stream_id)
    )
}

pub fn chapter_resource_id(source_id: &str, chapter_id: &str) -> String {
    format!(
        "video-player.chapters.{}.{}",
        identity_token_for_action(source_id),
        identity_token_for_action(chapter_id)
    )
}

/// A stable chapter address and its playback target.
#[derive(Clone, Debug, PartialEq)]
pub struct ChapterState {
    pub id: String,
    pub label: String,
    pub seconds: f64,
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
    if let Ok(reveal) = ActionSpec::operation(
        "reveal",
        "Show player controls",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::Reveal,
    ) {
        transport = transport.with_action(if ctx.media.has_media {
            reveal
        } else {
            reveal.unavailable("no_media", "Open a video before showing its controls")
        });
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
        "Playback settings · {}× · autoplay {} · repeat {}",
        ctx.settings.rate,
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
    .with_action(if ctx.media.can_control_transport {
        loop_action
    } else {
        loop_action.unavailable(
            "following_host",
            "Only the collaborative host may change repeat playback",
        )
    });

    let rate = ActionSpec::typed_operation(
        "set-playback-rate",
        "Set playback speed",
        "1",
        OperationKind::View,
        OPERATION_TYPE,
        vec![
            ActionParam::required_owned(
                "rate",
                ParamType::Number,
                "Playback speed from 0.25× to 4×",
            )
            .with_schema(serde_json::json!({"minimum": 0.25, "maximum": 4.0})),
        ],
        |args: &ActionArgs| {
            encode(&PlayerOperation::SetPlaybackRate(
                args.number("rate").unwrap_or(1.0).clamp(0.25, 4.0),
            ))
        },
    );
    settings = settings.with_action(if !ctx.media.has_media {
        rate.unavailable("no_media", "Open a video before changing playback speed")
    } else if ctx.media.can_control_transport {
        rate
    } else {
        rate.unavailable(
            "following_host",
            "The collaborative host controls playback speed",
        )
    });

    let mut tracks = Resource::new(
        "video-player.tracks.current",
        "media.tracks",
        format!(
            "Audio and subtitles · {} audio · {} subtitles",
            ctx.audio_tracks.len(),
            ctx.subtitle_tracks.len()
        ),
    );
    for track in ctx.audio_tracks.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        let Some(source_id) = ctx.media.source_id.as_deref() else {
            break;
        };
        let action_id = format!(
            "select-audio-{}",
            identity_token_for_action(&track.stream_id)
        );
        let selected = if track.selected { " · selected" } else { "" };
        let spec = ActionSpec::typed_operation(
            action_id,
            format!("Use {}{selected}", track.label),
            "Select this embedded audio stream",
            OperationKind::View,
            OPERATION_TYPE,
            Vec::new(),
            {
                let source_id = source_id.to_string();
                let stream_id = track.stream_id.clone();
                let index = track.index;
                move |_| {
                    encode(&PlayerOperation::SelectAudioTrack {
                        source_id: source_id.clone(),
                        stream_id: stream_id.clone(),
                        index,
                    })
                }
            },
        )
        .retry_policy(RetryPolicy::AtMostOnce);
        tracks = tracks.with_child(
            Resource::new(
                audio_resource_id(source_id, &track.stream_id),
                "media.audio-track",
                format!("{}{}", track.label, selected),
            )
            .with_action(if ctx.media.has_media {
                spec
            } else {
                spec.unavailable("no_media", "Open a video before selecting an audio stream")
            }),
        );
    }
    for track in ctx.subtitle_tracks.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        let Some(source_id) = ctx.media.source_id.as_deref() else {
            break;
        };
        let action_id = format!(
            "select-subtitle-{}",
            identity_token_for_action(&track.stream_id)
        );
        let selected = if track.selected { " · selected" } else { "" };
        let expected_stream_id = ctx
            .subtitle_tracks
            .iter()
            .find(|candidate| candidate.selected)
            .map(|candidate| candidate.stream_id.clone());
        let expected_index = ctx
            .subtitle_tracks
            .iter()
            .find(|candidate| candidate.selected)
            .map(|candidate| candidate.index);
        let spec = ActionSpec::typed_operation(
            action_id,
            format!("Show {}{selected}", track.label),
            "Select this embedded subtitle stream",
            OperationKind::View,
            OPERATION_TYPE,
            Vec::new(),
            {
                let source_id = source_id.to_string();
                let stream_id = track.stream_id.clone();
                let index = track.index;
                move |_| {
                    encode(&PlayerOperation::SelectSubtitleTrack {
                        source_id: source_id.clone(),
                        target_stream_id: Some(stream_id.clone()),
                        target_index: Some(index),
                        expected_stream_id: expected_stream_id.clone(),
                        expected_index,
                    })
                }
            },
        )
        .retry_policy(RetryPolicy::AtMostOnce);
        tracks = tracks.with_child(
            Resource::new(
                subtitle_resource_id(source_id, &track.stream_id),
                "media.subtitle-track",
                format!("{}{}", track.label, selected),
            )
            .with_action(if ctx.media.has_media {
                spec
            } else {
                spec.unavailable("no_media", "Open a video before selecting subtitles")
            }),
        );
    }
    let selected_subtitle = ctx.subtitle_tracks.iter().find(|track| track.selected);
    let subtitles_off =
        ctx.media
            .source_id
            .as_deref()
            .zip(selected_subtitle)
            .map(|(source_id, track)| {
                ActionSpec::operation(
                    "disable-subtitles",
                    "Turn subtitles off",
                    OperationKind::View,
                    OPERATION_TYPE,
                    &PlayerOperation::SelectSubtitleTrack {
                        source_id: source_id.to_string(),
                        target_stream_id: None,
                        target_index: None,
                        expected_stream_id: Some(track.stream_id.clone()),
                        expected_index: Some(track.index),
                    },
                )
                .map(|spec| spec.retry_policy(RetryPolicy::AtMostOnce))
            });
    if let Some(Ok(spec)) = subtitles_off {
        tracks = tracks.with_action(if ctx.media.has_media {
            spec
        } else {
            spec.unavailable("no_media", "Open a video before changing subtitles")
        });
    }

    let mut chapters = Resource::new(
        "video-player.chapters.current",
        "media.chapters",
        format!("Chapters · {} available", ctx.chapters.len()),
    );
    for chapter in ctx.chapters.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        let Some(source_id) = ctx.media.source_id.as_deref() else {
            break;
        };
        let spec = ActionSpec::typed_operation(
            format!("jump-to-{}", identity_token_for_action(&chapter.id)),
            format!("Go to {}", chapter.label),
            format!("Seek to {:.1} seconds", chapter.seconds),
            OperationKind::View,
            OPERATION_TYPE,
            Vec::new(),
            {
                let source_id = source_id.to_string();
                let id = chapter.id.clone();
                let seconds = chapter.seconds;
                move |_| {
                    encode(&PlayerOperation::JumpToChapter {
                        source_id: source_id.clone(),
                        id: id.clone(),
                        seconds,
                    })
                }
            },
        )
        .retry_policy(RetryPolicy::AtMostOnce);
        chapters = chapters.with_child(
            Resource::new(
                chapter_resource_id(source_id, &chapter.id),
                "media.chapter",
                format!("{} · {:.1}s", chapter.label, chapter.seconds),
            )
            .with_action(if !ctx.media.has_media {
                spec.unavailable("no_media", "Open a video before navigating chapters")
            } else if ctx.media.can_control_transport {
                spec
            } else {
                spec.unavailable(
                    "following_host",
                    "The collaborative host controls timeline navigation",
                )
            }),
        );
    }

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

/// Lazily resolve one stable resource address omitted from the bounded
/// contextual snapshot. A caller receives the exact same resource/action
/// contract it would have received in the snapshot, and a stale stream or
/// chapter ID simply refuses to resolve.
pub fn resource(ctx: &PlayerResourceCtx<'_>, id: &str) -> Option<Resource> {
    fn find(resources: &[Resource], id: &str) -> Option<Resource> {
        for resource in resources {
            if resource.id.0 == id {
                return Some(resource.clone());
            }
            if let Some(found) = find(&resource.children, id) {
                return Some(found);
            }
        }
        None
    }

    let doc = AutoCommit::new();
    if let Some(found) = find(&resources(ctx, &doc), id) {
        return Some(found);
    }
    let one_track = |audio: bool, tracks: &[TrackState]| -> Option<Resource> {
        let source_id = ctx.media.source_id.as_deref()?;
        let track = tracks.iter().find(|track| {
            let expected = if audio {
                audio_resource_id(source_id, &track.stream_id)
            } else {
                subtitle_resource_id(source_id, &track.stream_id)
            };
            expected == id
        })?;
        let one = [track.clone()];
        let isolated = PlayerResourceCtx {
            title: ctx.title,
            media: MediaAvailability {
                has_media: ctx.media.has_media,
                source_id: ctx.media.source_id.clone(),
                can_control_transport: ctx.media.can_control_transport,
            },
            settings: PlaybackSettingsState {
                autoplay: ctx.settings.autoplay,
                loop_enabled: ctx.settings.loop_enabled,
                rate: ctx.settings.rate,
            },
            audio_tracks: if audio { &one } else { &[] },
            subtitle_tracks: if audio { &[] } else { &one },
            chapters: &[],
        };
        find(&resources(&isolated, &doc), id)
    };
    one_track(true, ctx.audio_tracks)
        .or_else(|| one_track(false, ctx.subtitle_tracks))
        .or_else(|| {
            let source_id = ctx.media.source_id.as_deref()?;
            let chapter = ctx
                .chapters
                .iter()
                .find(|chapter| chapter_resource_id(source_id, &chapter.id) == id)?;
            let one = [chapter.clone()];
            let isolated = PlayerResourceCtx {
                title: ctx.title,
                media: MediaAvailability {
                    has_media: ctx.media.has_media,
                    source_id: ctx.media.source_id.clone(),
                    can_control_transport: ctx.media.can_control_transport,
                },
                settings: PlaybackSettingsState {
                    autoplay: ctx.settings.autoplay,
                    loop_enabled: ctx.settings.loop_enabled,
                    rate: ctx.settings.rate,
                },
                audio_tracks: &[],
                subtitle_tracks: &[],
                chapters: &one,
            };
            find(&resources(&isolated, &doc), id)
        })
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
                source_id: has_media.then(|| "fixture://clip".to_string()),
                can_control_transport: true,
            },
            settings: PlaybackSettingsState {
                autoplay: true,
                loop_enabled: false,
                rate: 1.0,
            },
            audio_tracks: &[],
            subtitle_tracks: &[],
            chapters: &[],
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
        let idle_loop = idle_media.children[5]
            .actions
            .iter()
            .find(|action| action.id == "set-loop")
            .expect("repeat preference action");
        assert!(
            idle_loop.contract.availability.is_available(),
            "repeat is a persisted preference and is meaningful before a video opens"
        );
        assert!(
            !PlayerOperation::SetLoop(true).requires_media(),
            "out-of-band callers may configure repeat before opening media"
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
        let rate = media.children[5]
            .actions
            .iter()
            .find(|action| action.id == "set-playback-rate")
            .expect("playback speed is a real action");
        assert!(rate.contract.availability.is_available());
        let encoded = rate
            .build_json(
                &serde_json::json!({"rate": 1.5})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap();
        assert_eq!(
            decode(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PlayerOperation::SetPlaybackRate(1.5))
        );
    }

    #[test]
    fn omitted_decoder_metadata_resolves_lazily_by_its_stable_address() {
        let audio: Vec<TrackState> = (0..CONTEXTUAL_CHILD_LIMIT as u32 + 2)
            .map(|index| TrackState {
                stream_id: format!("audio-stream-{index}"),
                index,
                label: format!("Áudio {index}"),
                selected: index == 0,
            })
            .collect();
        let chapters = vec![ChapterState {
            id: "intro-夏".into(),
            label: "Introduction 夏".into(),
            seconds: 12.5,
        }];
        let mut context = ctx(true);
        context.audio_tracks = &audio;
        context.chapters = &chapters;
        let snapshot = resources(&context, &AutoCommit::new());
        let tracks = &snapshot[0].children[0].children[3];
        assert_eq!(tracks.children.len(), CONTEXTUAL_CHILD_LIMIT);
        let target = audio_resource_id(
            "fixture://clip",
            &format!("audio-stream-{}", CONTEXTUAL_CHILD_LIMIT + 1),
        );
        let resolved = resource(&context, &target).expect("lazy audio resource");
        assert_eq!(resolved.id.0, target);
        assert!(resource(&context, "video-player.tracks.audio.999").is_none());
        let chapter = resource(&context, &chapter_resource_id("fixture://clip", "intro-夏"))
            .expect("chapter");
        assert_eq!(chapter.label, "Introduction 夏 · 12.5s");
    }

    #[test]
    fn disable_subtitles_encodes_none_as_the_target_not_the_expected_track() {
        let subtitles = [TrackState {
            stream_id: "caption-en".into(),
            index: 3,
            label: "English captions".into(),
            selected: true,
        }];
        let mut context = ctx(true);
        context.subtitle_tracks = &subtitles;
        let tree = resources(&context, &AutoCommit::new());
        let tracks = &tree[0].children[0].children[3];
        let off = tracks
            .actions
            .iter()
            .find(|action| action.id == "disable-subtitles")
            .unwrap();
        let encoded = off.build_args([]).unwrap();
        assert_eq!(
            decode(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PlayerOperation::SelectSubtitleTrack {
                source_id: "fixture://clip".into(),
                target_stream_id: None,
                target_index: None,
                expected_stream_id: Some("caption-en".into()),
                expected_index: Some(3),
            })
        );
    }
}
