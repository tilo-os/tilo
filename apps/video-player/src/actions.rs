//! What Video Player offers the palette, agents and scripts: the effects of
//! the player on this device, the shared library's edits and rows, and the
//! live rows of what the player shows (the open video's tracks and
//! chapters, this device's queue and recent videos).

use tilo_actions::{ActionParam, ActionSpec, ExecutionLocus, ParamType, Resource};
use ui::framework::LiveRow;
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::{ActionDecl, Effect};

use crate::resource::VideoPlayer;
use crate::shared::{SharedAction, SharedLibrary};

/// The published tree stays responsive even for containers with an unusually
/// large number of streams or chapters, or a long library. Stable ids still
/// let a caller reach an omitted row.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 32;

/// What Video Player does on this device: work with no document delta,
/// invoked from its own controls, the palette, an agent or a script alike.
/// A `track`, `chapter` or `item` is the id of the live row that offers it,
/// which names one stream of one opened source, so a delayed invocation
/// never reaches a later video's streams.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "video-player.effect")]
pub enum VideoEffect {
    /// Open video.
    ///
    /// Opens a video file or stream on this device and answers once its
    /// first picture has decoded.
    #[effect(job, permission = "read", device_only)]
    Open {
        #[param(
            "A video's path or stream URL on this device; omit to choose with the file picker"
        )]
        path: Option<String>,
    },
    /// Play this video.
    ///
    /// Opens a video of this device's queue or recent videos and answers
    /// once its first picture has decoded.
    #[effect(row, job, permission = "read", device_only)]
    PlayQueued {
        #[param("The queue or recent video's item id")]
        item: String,
    },
    /// Remove from queue.
    ///
    /// Removes the entry from this device's queue; the video file stays.
    #[effect(row, permission = "read")]
    RemoveQueued {
        #[param("The queued video's item id")]
        item: String,
    },
    /// Clear recent videos.
    ///
    /// Forgets the videos played on this device; no file is deleted.
    #[effect(destructive, permission = "read")]
    ClearRecent,
    /// Show library.
    #[effect(view)]
    ShowLibrary {
        #[param("queue | recent | shared")]
        tab: String,
    },
    /// Close library.
    #[effect(view)]
    HideLibrary,
    /// Close video.
    #[effect(view)]
    Close,
    /// Play or pause.
    #[effect(view)]
    TogglePlayback,
    /// Mute or unmute.
    #[effect(view)]
    ToggleMute,
    /// Show player controls.
    #[effect(view)]
    Reveal,
    /// Seek to seconds.
    #[effect(view)]
    Seek {
        #[param("Non-negative playback position in seconds")]
        seconds: f64,
    },
    /// Seek by seconds.
    #[effect(view)]
    SeekBy {
        #[param("Signed offset from the current position (at most one hour)")]
        seconds: f64,
    },
    /// Restart.
    #[effect(view)]
    Restart,
    /// Jump to end.
    #[effect(view)]
    JumpToEnd,
    /// Set volume.
    #[effect(view)]
    SetVolume {
        #[param("Volume from 0 (silent) to 1 (full)")]
        volume: f64,
    },
    /// Set autoplay.
    #[effect(view)]
    SetAutoplay {
        #[param("Whether newly opened videos begin playing")]
        enabled: bool,
    },
    /// Set repeat current video.
    #[effect(view)]
    SetLoop {
        #[param("Whether the current video repeats after it finishes")]
        enabled: bool,
    },
    /// Set playback speed.
    #[effect(view)]
    SetPlaybackRate {
        #[param("Playback speed from 0.25 to 4")]
        rate: f64,
    },
    /// Use this audio track.
    #[effect(view, row)]
    SelectAudioTrack {
        #[param("The audio track row's id")]
        track: String,
    },
    /// Show these subtitles.
    #[effect(view, row)]
    SelectSubtitles {
        #[param("The subtitle track row's id")]
        track: String,
    },
    /// Turn subtitles off.
    #[effect(view)]
    SubtitlesOff,
    /// Go to chapter.
    #[effect(view, row)]
    JumpToChapter {
        #[param("The chapter row's id")]
        chapter: String,
    },
    /// Jump to timestamp.
    ///
    /// Seeks the matching video on this device to a shared review note.
    #[effect(view, row)]
    JumpToAnnotation {
        #[param("The annotation's id")]
        annotation: String,
    },
    /// Open Picture in Picture.
    #[effect(view)]
    PictureInPicture,
    /// Enter or exit fullscreen.
    #[effect(view)]
    ToggleFullscreen,
}

impl VideoEffect {
    /// Whether this effect is meaningful only while a video is open.
    pub fn requires_media(&self) -> bool {
        matches!(
            self,
            Self::Close
                | Self::TogglePlayback
                | Self::ToggleMute
                | Self::Reveal
                | Self::Seek { .. }
                | Self::SeekBy { .. }
                | Self::Restart
                | Self::JumpToEnd
                | Self::SetVolume { .. }
                | Self::SetPlaybackRate { .. }
                | Self::SelectAudioTrack { .. }
                | Self::SelectSubtitles { .. }
                | Self::SubtitlesOff
                | Self::JumpToChapter { .. }
                | Self::JumpToAnnotation { .. }
                | Self::PictureInPicture
        )
    }
}

/// Stream metadata the decoder actually reported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackState {
    pub stream_id: String,
    pub index: u32,
    pub label: String,
    pub selected: bool,
}

/// A stable chapter address and its playback target.
#[derive(Clone, Debug, PartialEq)]
pub struct ChapterState {
    pub id: String,
    pub label: String,
    pub seconds: f64,
}

/// A queue or recent row. `id` is an opaque per-device token; neither it nor
/// a label contains a filesystem path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalMediaItemState {
    pub id: String,
    pub title: String,
    pub current: bool,
}

/// What the player shows that the document does not hold, for its live rows.
pub struct PlayerRows<'a> {
    /// The open video's title, when one is open.
    pub title: Option<&'a str>,
    /// Exact identity of the open source; stream and chapter rows are named
    /// by it.
    pub source_id: Option<&'a str>,
    pub audio_tracks: &'a [TrackState],
    pub subtitle_tracks: &'a [TrackState],
    pub chapters: &'a [ChapterState],
    pub queue: Vec<LocalMediaItemState>,
    pub recent: Vec<LocalMediaItemState>,
}

/// A compact, deterministic token for a row id. The raw identity is resolved
/// against the player's state before anything acts on it.
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

/// The shared library's named edits. Ids are minted from the origin's clock
/// when the edit is built, so a replay keeps them.
pub(crate) fn declarations() -> Vec<ActionDecl<VideoPlayer>> {
    let text =
        |name: &'static str, what: &'static str| ActionParam::required(name, ParamType::Text, what);
    vec![
        ActionDecl::new(
            "create-playlist",
            "Create shared playlist",
            vec![text("name", "Playlist name")],
            |args, now_ms| {
                Ok(SharedAction::CreatePlaylist {
                    id: format!("playlist-{now_ms}"),
                    name: required_text(args, "name")?,
                })
            },
        )
        .describe("Create a playlist everyone in the room sees."),
        ActionDecl::new(
            "create-annotation",
            "Add timestamp annotation",
            vec![
                text(
                    "media_ref",
                    "Opaque shareable media identity; never a device path",
                ),
                ActionParam::required(
                    "time_ms",
                    ParamType::Integer,
                    "Absolute timestamp in milliseconds",
                ),
                ActionParam::optional(
                    "text",
                    ParamType::Text,
                    "Annotation text; omit for a bookmark",
                ),
            ],
            |args, now_ms| {
                Ok(SharedAction::CreateAnnotation {
                    id: format!("annotation-{now_ms}"),
                    media_ref: required_text(args, "media_ref")?,
                    time_ms: args.number("time_ms").unwrap_or_default().max(0.0) as u64,
                    text: args.text("text").unwrap_or_default().to_owned(),
                })
            },
        )
        .describe("Add a review note at a time in a shared video."),
    ]
}

fn required_text(args: &tilo_actions::ActionArgs, name: &str) -> Result<String, String> {
    args.text(name)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("`{name}` is required"))
}

fn shared_spec(
    id: &str,
    label: &str,
    params: Vec<ActionParam>,
    build: impl Fn(&tilo_actions::ActionArgs) -> SharedAction + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec::typed(
        id,
        label,
        "",
        ExecutionLocus::ClientLocal,
        params,
        move |args| {
            tilo_actions::EncodedAction::encode(crate::shared::ACTION_TYPE, &build(args))
                .unwrap_or_else(|_| tilo_actions::EncodedAction {
                    type_id: crate::shared::ACTION_TYPE.into(),
                    payload: Vec::new(),
                })
        },
    )
    .reversible()
}

fn immediate_spec(id: &str, label: &str, action: &SharedAction) -> Option<ActionSpec> {
    ActionSpec::immediate(id, label, crate::shared::ACTION_TYPE, action)
        .ok()
        .map(|spec| spec.destructive().reversible())
}

/// The shared library beneath the resource: every playlist with its items
/// and every timestamp annotation, each with the edits it takes and the
/// effects bound to it (show it, jump to it).
pub(crate) fn document_children(state: &SharedLibrary) -> Vec<Resource> {
    vec![shared_library(state, CONTEXTUAL_CHILD_LIMIT)]
}

/// The shared row `id`, when the bounded children leave it out.
pub(crate) fn document_child(state: &SharedLibrary, id: &str) -> Option<Resource> {
    find(&[shared_library(state, crate::shared::MAX_SHARED_ROWS)], id)
}

fn find(resources: &[Resource], id: &str) -> Option<Resource> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

fn show_shared() -> ActionSpec {
    VideoEffect::ShowLibrary {
        tab: "shared".into(),
    }
    .bound()
}

#[allow(
    clippy::too_many_lines,
    reason = "playlist and annotation rows stay adjacent to the edits they take"
)]
fn shared_library(state: &SharedLibrary, limit: usize) -> Resource {
    let mut playlists = Resource::new(
        "video-player.playlists.shared",
        "media.playlists",
        format!("Shared playlists · {}", state.playlists.len()),
    )
    .with_action(show_shared());
    for playlist in state.playlists.iter().take(limit) {
        let id = playlist.id.clone();
        let before = playlist.name.clone();
        let mut resource = Resource::new(
            playlist_resource_id(&playlist.id),
            "media.playlist",
            playlist.name.clone(),
        )
        .with_action(show_shared())
        .with_action(shared_spec(
            "rename",
            "Rename playlist",
            vec![ActionParam::required_owned(
                "name",
                ParamType::Text,
                "New playlist name",
            )],
            move |args| SharedAction::RenamePlaylist {
                id: id.clone(),
                before: before.clone(),
                after: args.text("name").unwrap_or_default().into(),
            },
        ));
        if let Some(delete) = immediate_spec(
            "delete",
            "Delete playlist",
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
            resource = resource.with_action(delete);
        }
        let playlist_id = playlist.id.clone();
        resource = resource.with_action(shared_spec(
            "add-item",
            "Add media to playlist",
            vec![
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
            move |args| {
                let media_ref = args.text("media_ref").unwrap_or_default();
                let position = args.number("position").unwrap_or_default() as i64;
                SharedAction::AddPlaylistItem {
                    id: format!("item-{}-{position}", identity_token_for_action(media_ref)),
                    playlist_id: playlist_id.clone(),
                    media_ref: media_ref.into(),
                    title: args.text("title").unwrap_or_default().into(),
                    position,
                }
            },
        ));
        for item in state
            .items
            .iter()
            .filter(|item| item.playlist_id == playlist.id)
            .take(limit)
        {
            let id = item.id.clone();
            let before = item.position;
            let mut child = Resource::new(
                playlist_item_resource_id(&item.id),
                "media.playlist-item",
                item.title.clone(),
            )
            .with_action(shared_spec(
                "move",
                "Move playlist item",
                vec![ActionParam::required_owned(
                    "position",
                    ParamType::Number,
                    "New absolute sortable position",
                )],
                move |args| SharedAction::MovePlaylistItem {
                    id: id.clone(),
                    before,
                    after: args.number("position").unwrap_or(before as f64) as i64,
                },
            ));
            if let Some(remove) = immediate_spec(
                "remove",
                "Remove from playlist",
                &SharedAction::RemovePlaylistItem {
                    id: item.id.clone(),
                },
            ) {
                child = child.with_action(remove);
            }
            resource = resource.with_child(child);
        }
        playlists = playlists.with_child(resource);
    }
    let mut annotations = Resource::new(
        "video-player.annotations.shared",
        "media.annotations",
        format!("Timestamp annotations · {}", state.annotations.len()),
    )
    .with_action(show_shared());
    for annotation in state.annotations.iter().take(limit) {
        let label = if annotation.text.is_empty() {
            format!("Bookmark · {}ms", annotation.time_ms)
        } else {
            format!("{} · {}ms", annotation.text, annotation.time_ms)
        };
        let id = annotation.id.clone();
        let before = annotation.text.clone();
        let move_id = annotation.id.clone();
        let before_ms = annotation.time_ms;
        let mut resource = Resource::new(
            annotation_resource_id(&annotation.id),
            "media.annotation",
            label,
        )
        .with_action(show_shared())
        .with_action(
            VideoEffect::JumpToAnnotation {
                annotation: annotation.id.clone(),
            }
            .bound(),
        )
        .with_action(shared_spec(
            "edit",
            "Edit annotation",
            vec![ActionParam::required_owned(
                "text",
                ParamType::Text,
                "New annotation text; empty is a bookmark",
            )],
            move |args| SharedAction::EditAnnotation {
                id: id.clone(),
                before: before.clone(),
                after: args.text("text").unwrap_or_default().into(),
            },
        ))
        .with_action(shared_spec(
            "move",
            "Move annotation",
            vec![ActionParam::required_owned(
                "time_ms",
                ParamType::Number,
                "New absolute timestamp in milliseconds",
            )],
            move |args| SharedAction::MoveAnnotation {
                id: move_id.clone(),
                before_ms,
                after_ms: args.number("time_ms").unwrap_or(before_ms as f64).max(0.0) as u64,
            },
        ));
        if let Some(delete) = immediate_spec(
            "delete",
            "Delete annotation",
            &SharedAction::DeleteAnnotation {
                id: annotation.id.clone(),
            },
        ) {
            resource = resource.with_action(delete);
        }
        annotations = annotations.with_child(resource);
    }
    Resource::new(
        "video-player.shared-library",
        "media.shared-library",
        "Shared library",
    )
    .with_child(playlists)
    .with_child(annotations)
}

/// The live rows of what the player shows: the open video with its audio
/// and subtitle tracks and chapters, this device's queue and its recent
/// videos, each bounded to `limit` rows.
pub(crate) fn live_rows(rows: &PlayerRows<'_>, limit: usize) -> Vec<LiveRow> {
    let mut published = Vec::new();
    if let Some(title) = rows.title {
        let mut media = LiveRow::new("video-player.media.current", "media.video", title);
        if rows.source_id.is_some() {
            let mut tracks = LiveRow::new(
                "video-player.tracks.current",
                "media.tracks",
                format!(
                    "Audio and subtitles · {} audio · {} subtitles",
                    rows.audio_tracks.len(),
                    rows.subtitle_tracks.len()
                ),
            );
            if rows.subtitle_tracks.iter().any(|track| track.selected) {
                tracks = tracks.effect(&VideoEffect::SubtitlesOff);
            }
            let tracks = audio_rows(rows)
                .take(limit)
                .chain(subtitle_rows(rows).take(limit))
                .fold(tracks, LiveRow::row);
            let chapters = chapter_rows(rows).take(limit).fold(
                LiveRow::new(
                    "video-player.chapters.current",
                    "media.chapters",
                    format!("Chapters · {} available", rows.chapters.len()),
                ),
                LiveRow::row,
            );
            media = media.row(tracks).row(chapters);
        }
        published.push(media);
    }
    let queue = LiveRow::new(
        "video-player.queue.local",
        "media.queue",
        format!(
            "Queue · {} session item{}",
            rows.queue.len(),
            if rows.queue.len() == 1 { "" } else { "s" }
        ),
    )
    .effect(&VideoEffect::ShowLibrary {
        tab: "queue".into(),
    });
    published.push(queue_rows(rows).take(limit).fold(queue, LiveRow::row));
    let mut recent = LiveRow::new(
        "video-player.history.local",
        "media.history",
        format!("Recent videos · {} on this device", rows.recent.len()),
    )
    .effect(&VideoEffect::ShowLibrary {
        tab: "recent".into(),
    });
    if !rows.recent.is_empty() {
        recent = recent.effect(&VideoEffect::ClearRecent);
    }
    published.push(recent_rows(rows).take(limit).fold(recent, LiveRow::row));
    published
}

/// The live row `id`: a collection, or any of its rows, also one the
/// bounded tree leaves out.
pub(crate) fn live_row(rows: &PlayerRows<'_>, id: &str) -> Option<LiveRow> {
    audio_rows(rows)
        .chain(subtitle_rows(rows))
        .chain(chapter_rows(rows))
        .chain(queue_rows(rows))
        .chain(recent_rows(rows))
        .find(|row| row.id() == id)
}

fn track_label(track: &TrackState) -> String {
    if track.selected {
        format!("{} · selected", track.label)
    } else {
        track.label.clone()
    }
}

fn audio_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    let source_id = rows.source_id.unwrap_or_default();
    rows.audio_tracks
        .iter()
        .filter(move |_| !source_id.is_empty())
        .map(move |track| {
            let id = audio_resource_id(source_id, &track.stream_id);
            LiveRow::new(id.clone(), "media.audio-track", track_label(track))
                .effect(&VideoEffect::SelectAudioTrack { track: id })
        })
}

fn subtitle_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    let source_id = rows.source_id.unwrap_or_default();
    rows.subtitle_tracks
        .iter()
        .filter(move |_| !source_id.is_empty())
        .map(move |track| {
            let id = subtitle_resource_id(source_id, &track.stream_id);
            LiveRow::new(id.clone(), "media.subtitle-track", track_label(track))
                .effect(&VideoEffect::SelectSubtitles { track: id })
        })
}

fn chapter_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    let source_id = rows.source_id.unwrap_or_default();
    rows.chapters
        .iter()
        .filter(move |_| !source_id.is_empty())
        .map(move |chapter| {
            let id = chapter_resource_id(source_id, &chapter.id);
            LiveRow::new(
                id.clone(),
                "media.chapter",
                format!("{} · {:.1}s", chapter.label, chapter.seconds),
            )
            .effect(&VideoEffect::JumpToChapter { chapter: id })
        })
}

fn queue_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    rows.queue.iter().map(|item| {
        let suffix = if item.current { " · playing" } else { "" };
        LiveRow::new(
            queue_item_resource_id(&item.id),
            "media.queue-item",
            format!("{}{suffix}", item.title),
        )
        .effect(&VideoEffect::PlayQueued {
            item: item.id.clone(),
        })
        .effect(&VideoEffect::RemoveQueued {
            item: item.id.clone(),
        })
    })
}

fn recent_rows<'a>(rows: &'a PlayerRows<'_>) -> impl Iterator<Item = LiveRow> + 'a {
    rows.recent.iter().map(|item| {
        LiveRow::new(
            recent_item_resource_id(&item.id),
            "media.history-item",
            item.title.clone(),
        )
        .effect(&VideoEffect::PlayQueued {
            item: item.id.clone(),
        })
    })
}
