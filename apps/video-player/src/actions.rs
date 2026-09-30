//! Discoverable effect/view operations for the local video player.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionRegistry, ActionSpec, EncodedAction,
    ExecutionMode, OperationKind, ParamType, Resource, ResultSpec, RetryPolicy,
};

use crate::shared::SharedAction;

pub const OPERATION_TYPE: &str = "video-player.operation";
/// The contextual resource snapshot remains responsive even for containers
/// with an unusually large number of streams or chapters. Stable IDs still let
/// a host resolve a specific omitted item on demand.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 32;

/// The player library is a device-local presentation. Its explicit navigation
/// operations are safe to repeat after a delayed host response; they neither
/// replay a document mutation nor touch a decoder, portal, or external device.
fn local_presentation_contract() -> ActionContract {
    ActionContract {
        tags: vec!["device-local".into(), "media".into(), "presentation".into()],
        ..ActionContract::view()
    }
}

/// Register every durable document action published by Video Player.
pub fn register(registry: &mut ActionRegistry) {
    crate::shared::register(registry);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PlayerOperation {
    Open {
        path: Option<String>,
    },
    /// Play an opaque item already retained in this device-local queue.
    OpenQueueItem {
        item_id: String,
    },
    /// Remove a queued local item without deleting its underlying media file.
    RemoveQueueItem {
        item_id: String,
    },
    /// Forget local recent-playback labels; never deletes source files.
    ClearRecentHistory,
    /// Reveal the device-local queue and recent-history panel.
    RevealLibrary,
    /// Reveal the recent-history view of the device-local library.
    RevealRecentLibrary,
    RevealSharedLibrary,
    /// Close the app-owned library panel without changing media state.
    HideLibrary,
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
    /// Navigate to a collaborative review marker that belongs to the media
    /// currently open on this device. The durable annotation is checked again
    /// against the document before seeking, so search results cannot send a
    /// later video to a stale timestamp.
    JumpToAnnotation {
        id: String,
        media_ref: String,
        time_ms: u64,
    },
    /// Set the shared transport speed in the bounded player range.
    SetPlaybackRate(f64),
    OpenPictureInPicture,
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
                | Self::JumpToAnnotation { .. }
                | Self::SetPlaybackRate(_)
                | Self::OpenPictureInPicture
        )
    }
}

/// A publication context with no open media — used by
/// [`crate::resource::VideoPlayer`] for `local_operations` / `children` when
/// the live decoder state is not available.
pub fn empty_resource_ctx() -> PlayerResourceCtx<'static> {
    PlayerResourceCtx {
        title: "Video Player",
        media: MediaAvailability {
            has_media: false,
            source_id: None,
            media_ref: None,
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
        library: LocalLibraryState::default(),
    }
}

/// Seed a document with `state` so [`resources`] can rebuild the shared
/// playlist / annotation subtree for `Resource::children`.
pub fn doc_from_library(state: &crate::shared::SharedLibrary) -> AutoCommit {
    use tilo_actions::Action;
    let mut doc = AutoCommit::new();
    for playlist in &state.playlists {
        let _ = SharedAction::CreatePlaylist {
            id: playlist.id.clone(),
            name: playlist.name.clone(),
        }
        .apply(&mut doc);
    }
    for item in &state.items {
        let _ = SharedAction::AddPlaylistItem {
            id: item.id.clone(),
            playlist_id: item.playlist_id.clone(),
            media_ref: item.media_ref.clone(),
            title: item.title.clone(),
            position: item.position,
        }
        .apply(&mut doc);
    }
    for annotation in &state.annotations {
        let _ = SharedAction::CreateAnnotation {
            id: annotation.id.clone(),
            media_ref: annotation.media_ref.clone(),
            time_ms: annotation.time_ms,
            text: annotation.text.clone(),
        }
        .apply(&mut doc);
    }
    doc
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
    /// Queue and history are local session state. They are published as
    /// canonical resources for this client, never collaborative objects.
    pub library: LocalLibraryState,
}

/// Media availability and shared-transport authority at publication time.
pub struct MediaAvailability {
    /// Whether a source is open (it may still be decoding its first frame).
    pub has_media: bool,
    /// Exact identity of the currently-open source. Dynamic targets are never
    /// published without it, so an old action cannot act on a later file.
    pub source_id: Option<String>,
    /// Stable opaque identity for the underlying source. Unlike `source_id`,
    /// this intentionally survives closing and reopening the same local media
    /// so collaborative bookmarks remain useful without publishing a path.
    pub media_ref: Option<String>,
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

/// A local queue or recent-history row. `id` is an opaque per-device token;
/// neither it nor a resource label contains a filesystem path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalMediaItemState {
    pub id: String,
    pub title: String,
    pub current: bool,
}

/// Queue rows are session-local; recent rows are a bounded device-local
/// profile. Neither enters the collab document because a row can carry a
/// portal-granted file handle or absolute path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalLibraryState {
    pub queue: Vec<LocalMediaItemState>,
    pub recent: Vec<LocalMediaItemState>,
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

pub fn queue_item_resource_id(item_id: &str) -> String {
    format!(
        "video-player.queue.item.{}",
        identity_token_for_action(item_id)
    )
}

pub fn recent_item_resource_id(item_id: &str) -> String {
    format!(
        "video-player.history.item.{}",
        identity_token_for_action(item_id)
    )
}

pub fn playlist_resource_id(id: &str) -> String {
    format!("video-player.playlist.{}", identity_token_for_action(id))
}

pub fn playlist_item_resource_id(id: &str) -> String {
    format!(
        "video-player.playlist-item.{}",
        identity_token_for_action(id)
    )
}

pub fn annotation_resource_id(id: &str) -> String {
    format!("video-player.annotation.{}", identity_token_for_action(id))
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
/// The presentation tree: transport, tracks, local queue, and the shared library.
/// [`resources`] publishes this through [`VideoPlayer`](crate::resource::VideoPlayer)
/// and keeps these children, which depend on the live decoder and this device.
pub(crate) fn application_tree(ctx: &PlayerResourceCtx<'_>, doc: &AutoCommit) -> Vec<Resource> {
    let open_contract = ActionContract {
        retry: RetryPolicy::AtMostOnce,
        // A portal-granted path and its decoder/cache state are meaningful
        // only on this device. The direct-path parameter is intentionally
        // never mirrored into a collaborative document or resource ID.
        tags: vec![
            "device-local".into(),
            "filesystem".into(),
            "media".into(),
            "sensitive-path".into(),
        ],
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
            permission: tilo_actions::PermissionRequirement::Read,
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
    if let Ok(pip) = ActionSpec::operation(
        "picture-in-picture",
        "Open Picture in Picture",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::OpenPictureInPicture,
    ) {
        presentation = presentation.with_action(if ctx.media.has_media {
            pip.retry_policy(RetryPolicy::AtMostOnce)
        } else {
            pip.unavailable("no_media", "Open a video before using Picture in Picture")
        });
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
    let local_library_contract = ActionContract {
        retry: RetryPolicy::AtMostOnce,
        tags: vec!["device-local".into(), "media".into()],
        ..ActionContract::default()
    };
    // Presentational navigation has no hardware, file-system or document
    // effect. It is safe—and useful—to retry it after a delayed host response,
    // unlike queue/history mutation which remains at-most-once above.
    let local_presentation_contract = local_presentation_contract();
    let mut queue = Resource::new(
        "video-player.queue.local",
        "media.queue",
        format!(
            "Queue · {} session item{}",
            ctx.library.queue.len(),
            if ctx.library.queue.len() == 1 {
                ""
            } else {
                "s"
            }
        ),
    );
    for item in ctx.library.queue.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        let item_id = item.id.clone();
        let play = ActionSpec::typed_operation(
            "play",
            "Play queued video",
            "Open this local queue item and wait for its first decoded frame",
            OperationKind::Effect,
            OPERATION_TYPE,
            Vec::new(),
            move |_| {
                encode(&PlayerOperation::OpenQueueItem {
                    item_id: item_id.clone(),
                })
            },
        )
        .with_contract(ActionContract {
            execution: ExecutionMode::Background { cancellable: false },
            result: ResultSpec::Job,
            ..local_library_contract.clone()
        });
        let remove_id = item.id.clone();
        let remove = ActionSpec::typed_operation(
            "remove",
            "Remove from queue",
            "Remove this local queue entry without deleting its video file",
            OperationKind::View,
            OPERATION_TYPE,
            Vec::new(),
            move |_| {
                encode(&PlayerOperation::RemoveQueueItem {
                    item_id: remove_id.clone(),
                })
            },
        )
        .with_contract(local_library_contract.clone());
        let suffix = if item.current { " · playing" } else { "" };
        queue = queue.with_child(
            Resource::new(
                queue_item_resource_id(&item.id),
                "media.queue-item",
                format!("{}{suffix}", item.title),
            )
            .with_action(play)
            .with_action(remove),
        );
    }
    let mut recent = Resource::new(
        "video-player.history.local",
        "media.history",
        format!(
            "Recent videos · {} on this device",
            ctx.library.recent.len(),
        ),
    );
    if !ctx.library.recent.is_empty()
        && let Ok(clear) = ActionSpec::operation(
            "clear",
            "Clear recent history",
            OperationKind::View,
            OPERATION_TYPE,
            &PlayerOperation::ClearRecentHistory,
        )
    {
        recent = recent.with_action(clear.with_contract(local_library_contract.clone()));
    }
    for item in ctx.library.recent.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        let item_id = item.id.clone();
        let play = ActionSpec::typed_operation(
            "play",
            "Play again",
            "Open this local recent item and wait for its first decoded frame",
            OperationKind::Effect,
            OPERATION_TYPE,
            Vec::new(),
            move |_| {
                encode(&PlayerOperation::OpenQueueItem {
                    item_id: item_id.clone(),
                })
            },
        )
        .with_contract(ActionContract {
            execution: ExecutionMode::Background { cancellable: false },
            result: ResultSpec::Job,
            ..local_library_contract.clone()
        });
        recent = recent.with_child(
            Resource::new(
                recent_item_resource_id(&item.id),
                "media.history-item",
                item.title.clone(),
            )
            .with_action(play),
        );
    }
    if let Ok(reveal_library) = ActionSpec::operation(
        "show-library",
        "Show queue and recent videos",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::RevealLibrary,
    ) {
        app = app.with_action(reveal_library.with_contract(local_presentation_contract.clone()));
    }
    if let Ok(hide_library) = ActionSpec::operation(
        "hide-library",
        "Close library",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::HideLibrary,
    ) {
        app = app.with_action(hide_library.with_contract(local_presentation_contract.clone()));
    }
    if let Ok(show_queue) = ActionSpec::operation(
        "show",
        "Show queue",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::RevealLibrary,
    ) {
        queue = queue.with_action(show_queue.with_contract(local_presentation_contract.clone()));
    }
    if let Ok(show_recent) = ActionSpec::operation(
        "show",
        "Show recent videos",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::RevealRecentLibrary,
    ) {
        recent = recent.with_action(show_recent.with_contract(local_presentation_contract));
    }
    let shared = shared_resources(ctx, doc, CONTEXTUAL_CHILD_LIMIT);
    app = app
        .with_child(media)
        .with_child(queue)
        .with_child(recent)
        .with_child(shared);
    vec![app]
}

/// Standalone / `--shots` door. The resource publishes the application root;
/// this peer's decoder and device library replace the static children.
pub fn resources(ctx: &PlayerResourceCtx<'_>, doc: &AutoCommit) -> Vec<Resource> {
    let state = crate::shared::read(doc);
    let mut published = ui::tilo_resource::publish::<crate::resource::VideoPlayer>(
        "video-player.application",
        "Video Player",
        &state,
    );
    if let Some(live) = application_tree(ctx, doc).into_iter().next() {
        published.children = live.children;
    }
    vec![published]
}

/// Live decoder track/chapter projection for the palette — not CRDT.
///
/// Standalone [`resources`] nests these under `media.video`. Hosted
/// [`PublishesResources`](ui::resources::PublishesResources) returns the same
/// slices alone so agents keep audio/subtitle/chapter rows without a second
/// application or shared-library root beside [`crate::resource::VideoPlayer`].
pub fn live_track_resources(ctx: &PlayerResourceCtx<'_>, doc: &AutoCommit) -> Vec<Resource> {
    let Some(root) = application_tree(ctx, doc).into_iter().next() else {
        return Vec::new();
    };
    let Some(media) = root
        .children
        .into_iter()
        .find(|child| child.kind == "media.video")
    else {
        return Vec::new();
    };
    media
        .children
        .into_iter()
        .filter(|child| child.kind == "media.tracks" || child.kind == "media.chapters")
        .filter(|child| !child.children.is_empty() || !child.actions.is_empty())
        .collect()
}

fn encode_shared(action: &SharedAction) -> EncodedAction {
    EncodedAction::encode(crate::shared::ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: crate::shared::ACTION_TYPE.into(),
        payload: Vec::new(),
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "playlist and annotation action contracts stay adjacent to their resource hierarchy"
)]
fn shared_resources(ctx: &PlayerResourceCtx<'_>, doc: &AutoCommit, limit: usize) -> Resource {
    let state = crate::shared::read(doc);
    let mut playlists = Resource::new(
        "video-player.playlists.shared",
        "media.playlists",
        format!("Shared playlists · {}", state.playlists.len()),
    );
    if let Ok(show) = ActionSpec::operation(
        "show",
        "Show shared library",
        OperationKind::View,
        OPERATION_TYPE,
        &PlayerOperation::RevealSharedLibrary,
    ) {
        playlists
            .actions
            .push(show.with_contract(local_presentation_contract()));
    }
    playlists.actions.push(
        ActionSpec::typed(
            "create",
            "Create shared playlist",
            "name",
            tilo_actions::ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required_owned(
                    "id",
                    ParamType::Text,
                    "Globally unique stable playlist ID",
                ),
                ActionParam::required_owned("name", ParamType::Text, "Playlist name"),
            ],
            |args| {
                encode_shared(&SharedAction::CreatePlaylist {
                    id: args.text("id").unwrap_or_default().into(),
                    name: args.text("name").unwrap_or_default().into(),
                })
            },
        )
        .reversible(),
    );
    for playlist in state.playlists.iter().take(limit) {
        let mut resource = Resource::new(
            playlist_resource_id(&playlist.id),
            "media.playlist",
            playlist.name.clone(),
        );
        if let Ok(show) = ActionSpec::operation(
            "show",
            "Show shared playlist",
            OperationKind::View,
            OPERATION_TYPE,
            &PlayerOperation::RevealSharedLibrary,
        ) {
            resource.actions.push(show);
        }
        let id = playlist.id.clone();
        let before = playlist.name.clone();
        resource.actions.push(
            ActionSpec::typed(
                "rename",
                "Rename playlist",
                "name",
                tilo_actions::ExecutionLocus::ClientLocal,
                vec![ActionParam::required_owned(
                    "name",
                    ParamType::Text,
                    "New playlist name",
                )],
                move |args| {
                    encode_shared(&SharedAction::RenamePlaylist {
                        id: id.clone(),
                        before: before.clone(),
                        after: args.text("name").unwrap_or_default().into(),
                    })
                },
            )
            .reversible(),
        );
        if let Ok(delete) = ActionSpec::immediate(
            "delete",
            "Delete playlist",
            crate::shared::ACTION_TYPE,
            &SharedAction::DeletePlaylist {
                id: playlist.id.clone(),
                observed_item_ids: state
                    .items
                    .iter()
                    .filter(|item| item.playlist_id == playlist.id)
                    .map(|item| item.id.clone())
                    .collect(),
            },
        ) {
            resource.actions.push(delete.destructive().reversible());
        }
        resource.actions.push(
            ActionSpec::typed(
                "add-item",
                "Add media to playlist",
                "title",
                tilo_actions::ExecutionLocus::ClientLocal,
                vec![
                    ActionParam::required_owned(
                        "id",
                        ParamType::Text,
                        "Globally unique stable item ID",
                    ),
                    ActionParam::required_owned(
                        "media_ref",
                        ParamType::Text,
                        "Opaque shareable media identity; never a device path",
                    ),
                    ActionParam::required_owned("title", ParamType::Text, "Media title"),
                    ActionParam::required_owned(
                        "position",
                        ParamType::Number,
                        "Absolute sortable position",
                    ),
                ],
                {
                    let playlist_id = playlist.id.clone();
                    move |args| {
                        encode_shared(&SharedAction::AddPlaylistItem {
                            id: args.text("id").unwrap_or_default().into(),
                            playlist_id: playlist_id.clone(),
                            media_ref: args.text("media_ref").unwrap_or_default().into(),
                            title: args.text("title").unwrap_or_default().into(),
                            position: args.number("position").unwrap_or_default() as i64,
                        })
                    }
                },
            )
            .reversible(),
        );
        for item in state
            .items
            .iter()
            .filter(|item| item.playlist_id == playlist.id)
            .take(limit)
        {
            let mut child = Resource::new(
                playlist_item_resource_id(&item.id),
                "media.playlist-item",
                item.title.clone(),
            );
            let id = item.id.clone();
            let before = item.position;
            child.actions.push(
                ActionSpec::typed(
                    "move",
                    "Move playlist item",
                    "position",
                    tilo_actions::ExecutionLocus::ClientLocal,
                    vec![ActionParam::required_owned(
                        "position",
                        ParamType::Number,
                        "New absolute sortable position",
                    )],
                    move |args| {
                        encode_shared(&SharedAction::MovePlaylistItem {
                            id: id.clone(),
                            before,
                            after: args.number("position").unwrap_or(before as f64) as i64,
                        })
                    },
                )
                .reversible(),
            );
            if let Ok(remove) = ActionSpec::immediate(
                "remove",
                "Remove from playlist",
                crate::shared::ACTION_TYPE,
                &SharedAction::RemovePlaylistItem {
                    id: item.id.clone(),
                },
            ) {
                child.actions.push(remove.destructive().reversible());
            }
            resource.children.push(child);
        }
        playlists.children.push(resource);
    }
    let mut annotations = Resource::new(
        "video-player.annotations.shared",
        "media.annotations",
        format!("Timestamp annotations · {}", state.annotations.len()),
    );
    annotations.actions.push(
        ActionSpec::typed(
            "create",
            "Add timestamp annotation",
            "text",
            tilo_actions::ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required_owned(
                    "id",
                    ParamType::Text,
                    "Globally unique stable annotation ID",
                ),
                ActionParam::required_owned(
                    "media_ref",
                    ParamType::Text,
                    "Opaque shareable media identity; never a device path",
                ),
                ActionParam::required_owned(
                    "time_ms",
                    ParamType::Number,
                    "Absolute timestamp in milliseconds",
                ),
                ActionParam::required_owned(
                    "text",
                    ParamType::Text,
                    "Annotation text; empty creates a bookmark",
                ),
            ],
            |args| {
                encode_shared(&SharedAction::CreateAnnotation {
                    id: args.text("id").unwrap_or_default().into(),
                    media_ref: args.text("media_ref").unwrap_or_default().into(),
                    time_ms: args.number("time_ms").unwrap_or_default().max(0.0) as u64,
                    text: args.text("text").unwrap_or_default().into(),
                })
            },
        )
        .reversible(),
    );
    for annotation in state.annotations.iter().take(limit) {
        let label = if annotation.text.is_empty() {
            format!("Bookmark · {}ms", annotation.time_ms)
        } else {
            format!("{} · {}ms", annotation.text, annotation.time_ms)
        };
        let mut resource = Resource::new(
            annotation_resource_id(&annotation.id),
            "media.annotation",
            label,
        );
        if let Ok(show) = ActionSpec::operation(
            "show",
            "Show timestamp annotation",
            OperationKind::View,
            OPERATION_TYPE,
            &PlayerOperation::RevealSharedLibrary,
        ) {
            resource.actions.push(show);
        }
        let jump = ActionSpec::typed_operation(
            "jump",
            "Jump to timestamp",
            "Seek to this review marker in the matching video on this device",
            OperationKind::View,
            OPERATION_TYPE,
            Vec::new(),
            {
                let id = annotation.id.clone();
                let media_ref = annotation.media_ref.clone();
                let time_ms = annotation.time_ms;
                move |_| {
                    encode(&PlayerOperation::JumpToAnnotation {
                        id: id.clone(),
                        media_ref: media_ref.clone(),
                        time_ms,
                    })
                }
            },
        )
        .retry_policy(RetryPolicy::AtMostOnce);
        resource.actions.push(if !ctx.media.has_media {
            jump.unavailable(
                "no_media",
                "Open the matching video before jumping to this marker",
            )
        } else if ctx.media.media_ref.as_deref() != Some(annotation.media_ref.as_str()) {
            jump.unavailable(
                "different_media",
                "This marker belongs to a different video on this device",
            )
        } else if !ctx.media.can_control_transport {
            jump.unavailable(
                "following_host",
                "The collaborative host controls timeline navigation",
            )
        } else {
            jump
        });
        let id = annotation.id.clone();
        let before = annotation.text.clone();
        resource.actions.push(
            ActionSpec::typed(
                "edit",
                "Edit annotation",
                "text",
                tilo_actions::ExecutionLocus::ClientLocal,
                vec![ActionParam::required_owned(
                    "text",
                    ParamType::Text,
                    "New annotation text; empty is a bookmark",
                )],
                move |args| {
                    encode_shared(&SharedAction::EditAnnotation {
                        id: id.clone(),
                        before: before.clone(),
                        after: args.text("text").unwrap_or_default().into(),
                    })
                },
            )
            .reversible(),
        );
        let id = annotation.id.clone();
        let before_ms = annotation.time_ms;
        resource.actions.push(
            ActionSpec::typed(
                "move",
                "Move annotation",
                "time_ms",
                tilo_actions::ExecutionLocus::ClientLocal,
                vec![ActionParam::required_owned(
                    "time_ms",
                    ParamType::Number,
                    "New absolute timestamp in milliseconds",
                )],
                move |args| {
                    encode_shared(&SharedAction::MoveAnnotation {
                        id: id.clone(),
                        before_ms,
                        after_ms: args.number("time_ms").unwrap_or(before_ms as f64).max(0.0)
                            as u64,
                    })
                },
            )
            .reversible(),
        );
        if let Ok(delete) = ActionSpec::immediate(
            "delete",
            "Delete annotation",
            crate::shared::ACTION_TYPE,
            &SharedAction::DeleteAnnotation {
                id: annotation.id.clone(),
            },
        ) {
            resource.actions.push(delete.destructive().reversible());
        }
        annotations.children.push(resource);
    }
    Resource::new(
        "video-player.shared-library",
        "media.shared-library",
        "Shared library",
    )
    .with_child(playlists)
    .with_child(annotations)
}

/// Lazily resolve one stable resource address omitted from the bounded
/// contextual snapshot. A caller receives the exact same resource/action
/// contract it would have received in the snapshot, and a stale stream or
/// chapter ID simply refuses to resolve.
pub fn resource(ctx: &PlayerResourceCtx<'_>, doc: &AutoCommit, id: &str) -> Option<Resource> {
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

    if let Some(found) = find(&resources(ctx, doc), id) {
        return Some(found);
    }
    if let Some(found) = find(
        &[shared_resources(ctx, doc, crate::shared::MAX_SHARED_ROWS)],
        id,
    ) {
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
                media_ref: ctx.media.media_ref.clone(),
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
            library: LocalLibraryState::default(),
        };
        find(&resources(&isolated, doc), id)
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
                    media_ref: ctx.media.media_ref.clone(),
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
                library: LocalLibraryState::default(),
            };
            find(&resources(&isolated, doc), id)
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
    use tilo_actions::Action;

    fn ctx(has_media: bool) -> PlayerResourceCtx<'static> {
        PlayerResourceCtx {
            title: if has_media { "Clip.mp4" } else { "No media" },
            media: MediaAvailability {
                has_media,
                source_id: has_media.then(|| "fixture://clip".to_string()),
                media_ref: has_media.then(|| "fixture-media://clip".to_string()),
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
            library: LocalLibraryState::default(),
        }
    }

    fn registry() -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
    }

    /// A shared library holding one playlist with one item and one review
    /// marker on the open clip, authored through the shared actions.
    fn shared_seed() -> AutoCommit {
        let mut doc = AutoCommit::new();
        for action in [
            SharedAction::CreatePlaylist {
                id: "p".into(),
                name: "Watch later".into(),
            },
            SharedAction::AddPlaylistItem {
                id: "i".into(),
                playlist_id: "p".into(),
                media_ref: "fixture-media://clip".into(),
                title: "Clip".into(),
                position: 10,
            },
            SharedAction::CreateAnnotation {
                id: "a".into(),
                media_ref: "fixture-media://clip".into(),
                time_ms: 1000,
                text: "Check the cut".into(),
            },
        ] {
            tilo_actions::apply_local(&mut doc, &action, encode_shared(&action)).unwrap();
        }
        doc
    }

    ui::action_contract_tests! {
        seed: shared_seed(),
        registry: registry(),
        resources: |doc| resources(&ctx(true), doc),
        palette: [
            "Open video",
            "Play or pause",
            "Create shared playlist",
            "Rename playlist",
            "Delete playlist",
            "Remove from playlist",
            "Add timestamp annotation",
            "Jump to timestamp",
            "Delete annotation",
        ],
        replay: [
            create_playlist => |_| encode_shared(&SharedAction::CreatePlaylist {
                id: "p2".into(),
                name: "Road trip".into(),
            }),
            rename_playlist => |_| encode_shared(&SharedAction::RenamePlaylist {
                id: "p".into(),
                before: "Watch later".into(),
                after: "Tonight".into(),
            }),
            add_item => |_| encode_shared(&SharedAction::AddPlaylistItem {
                id: "i2".into(),
                playlist_id: "p".into(),
                media_ref: "fixture-media://other".into(),
                title: "Other".into(),
                position: 20,
            }),
            move_item => |_| encode_shared(&SharedAction::MovePlaylistItem {
                id: "i".into(),
                before: 10,
                after: 30,
            }),
            create_annotation => |_| encode_shared(&SharedAction::CreateAnnotation {
                id: "a2".into(),
                media_ref: "fixture-media://clip".into(),
                time_ms: 2500,
                text: String::new(),
            }),
            edit_annotation => |_| encode_shared(&SharedAction::EditAnnotation {
                id: "a".into(),
                before: "Check the cut".into(),
                after: "Cut is fine".into(),
            }),
            move_annotation => |_| encode_shared(&SharedAction::MoveAnnotation {
                id: "a".into(),
                before_ms: 1000,
                after_ms: 1250,
            }),
        ],
        undo: [
            create_playlist => |_| encode_shared(&SharedAction::CreatePlaylist {
                id: "p2".into(),
                name: "Road trip".into(),
            }),
            delete_playlist => |_| encode_shared(&SharedAction::DeletePlaylist {
                id: "p".into(),
                observed_item_ids: vec!["i".into()],
            }),
            edit_annotation => |_| encode_shared(&SharedAction::EditAnnotation {
                id: "a".into(),
                before: "Check the cut".into(),
                after: "Cut is fine".into(),
            }),
        ],
        encodes: [
            delete_playlist => (&playlist_resource_id("p"), "delete", |_, _| {
                encode_shared(&SharedAction::DeletePlaylist {
                    id: "p".into(),
                    observed_item_ids: vec!["i".into()],
                })
            }),
            rename_playlist => (&playlist_resource_id("p"), "rename", arg: "Tonight", |_, _| {
                encode_shared(&SharedAction::RenamePlaylist {
                    id: "p".into(),
                    before: "Watch later".into(),
                    after: "Tonight".into(),
                })
            }),
            remove_item => (&playlist_item_resource_id("i"), "remove", |_, _| {
                encode_shared(&SharedAction::RemovePlaylistItem { id: "i".into() })
            }),
            edit_annotation => (&annotation_resource_id("a"), "edit", arg: "Cut is fine", |_, _| {
                encode_shared(&SharedAction::EditAnnotation {
                    id: "a".into(),
                    before: "Check the cut".into(),
                    after: "Cut is fine".into(),
                })
            }),
            delete_annotation => (&annotation_resource_id("a"), "delete", |_, _| {
                encode_shared(&SharedAction::DeleteAnnotation { id: "a".into() })
            }),
        ],
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn player_resource_tree_is_typed_bounded_and_lazily_addressed() {
        // Case `resource_tree_is_stateful_hierarchical_and_strictly_typed`.
        {
            let idle_resources = resources(&ctx(false), &AutoCommit::new());
            let actions = &idle_resources[0].actions;
            let open = actions.iter().find(|a| a.id == "open").unwrap();
            assert_eq!(
                open.contract.execution,
                tilo_actions::ExecutionMode::Background { cancellable: false }
            );
            assert_eq!(open.contract.result, tilo_actions::ResultSpec::Job);
            assert_eq!(open.locus, tilo_actions::ExecutionLocus::ClientLocal);
            assert!(open.contract.tags.iter().any(|tag| tag == "device-local"));
            assert!(
                open.contract.tags.iter().any(|tag| tag == "sensitive-path"),
                "direct local paths must stay explicitly device-local and sensitive"
            );
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
        // Case `omitted_decoder_metadata_resolves_lazily_by_its_stable_address`.
        {
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
            let doc = AutoCommit::new();
            let resolved = resource(&context, &doc, &target).expect("lazy audio resource");
            assert_eq!(resolved.id.0, target);
            assert!(resource(&context, &doc, "video-player.tracks.audio.999").is_none());
            let chapter = resource(
                &context,
                &doc,
                &chapter_resource_id("fixture://clip", "intro-夏"),
            )
            .expect("chapter");
            assert_eq!(chapter.label, "Introduction 夏 · 12.5s");
        }
        // Case `shared_entities_are_bounded_and_lazily_materialized_with_typed_actions`.
        {
            let mut doc = AutoCommit::new();
            for index in 0..(CONTEXTUAL_CHILD_LIMIT + 2) {
                SharedAction::CreatePlaylist {
                    id: format!("p-{index}"),
                    name: format!("Playlist {index}"),
                }
                .apply(&mut doc)
                .unwrap();
            }
            let tree = resources(&ctx(false), &doc);
            let shared = tree[0]
                .children
                .iter()
                .find(|child| child.id.0 == "video-player.shared-library")
                .unwrap();
            let playlists = &shared.children[0];
            assert_eq!(playlists.children.len(), CONTEXTUAL_CHILD_LIMIT);
            assert!(
                playlists.actions.iter().any(|action| {
                    action.id == "create" && action.kind == OperationKind::Document
                })
            );
            assert!(playlists.actions.iter().any(|action| {
                action.id == "show"
                    && action.kind == OperationKind::View
                    && action.contract.retry == RetryPolicy::Idempotent
            }));
            let late_id = playlist_resource_id(&format!("p-{}", CONTEXTUAL_CHILD_LIMIT + 1));
            let late = resource(&ctx(false), &doc, &late_id).expect("lazy shared playlist");
            assert_eq!(late.id.0, late_id);
            assert!(
                late.actions
                    .iter()
                    .any(|action| action.id == "rename" && action.kind == OperationKind::Document)
            );
            assert!(
                late.actions
                    .iter()
                    .any(|action| action.id == "show" && action.kind == OperationKind::View)
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn player_actions_encode_canonical_targets() {
        // Case `local_queue_and_device_history_have_opaque_canonical_actions`.
        {
            let mut context = ctx(true);
            context.library = LocalLibraryState {
                queue: vec![LocalMediaItemState {
                    id: "local-queue-local-media-0a2b-open-0".into(),
                    title: "Private clip.mp4".into(),
                    current: true,
                }],
                recent: vec![LocalMediaItemState {
                    id: "local-queue-local-media-9f3c-open-0".into(),
                    title: "Earlier clip.webm".into(),
                    current: false,
                }],
            };
            let root = resources(&context, &AutoCommit::new()).remove(0);
            let queue = root
                .children
                .iter()
                .find(|resource| resource.id.0 == "video-player.queue.local")
                .expect("local queue resource");
            let queue_item = &queue.children[0];
            assert_eq!(
                queue_item.id.0,
                queue_item_resource_id("local-queue-local-media-0a2b-open-0")
            );
            let play = queue_item
                .actions
                .iter()
                .find(|action| action.id == "play")
                .unwrap();
            assert_eq!(play.locus, tilo_actions::ExecutionLocus::ClientLocal);
            assert_eq!(
                play.contract.execution,
                ExecutionMode::Background { cancellable: false }
            );
            assert!(play.contract.tags.iter().any(|tag| tag == "device-local"));
            let recent = root
                .children
                .iter()
                .find(|resource| resource.id.0 == "video-player.history.local")
                .expect("recent resource");
            assert!(recent.actions.iter().any(|action| action.id == "clear"));
            assert_eq!(
                recent.children[0].id.0,
                recent_item_resource_id("local-queue-local-media-9f3c-open-0")
            );
        }
        // Case `matching_shared_marker_exposes_a_canonical_timeline_jump`.
        {
            let mut doc = AutoCommit::new();
            SharedAction::CreateAnnotation {
                id: "review-cut".into(),
                media_ref: "fixture-media://clip".into(),
                time_ms: 12_500,
                text: "Check the edit".into(),
            }
            .apply(&mut doc)
            .expect("fixture marker writes");
            let resource = resource(&ctx(true), &doc, &annotation_resource_id("review-cut"))
                .expect("annotation resource resolves");
            let jump = resource
                .actions
                .iter()
                .find(|action| action.id == "jump")
                .expect("matching marker can jump");
            assert!(jump.contract.availability.is_available());
            let encoded = jump.build_json(&serde_json::Map::new()).unwrap();
            assert_eq!(
                decode(&encoded.type_id, &encoded.payload).unwrap(),
                Some(PlayerOperation::JumpToAnnotation {
                    id: "review-cut".into(),
                    media_ref: "fixture-media://clip".into(),
                    time_ms: 12_500,
                })
            );
        }
        // Case `disable_subtitles_encodes_none_as_the_target_not_the_expected_track`.
        {
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
}
