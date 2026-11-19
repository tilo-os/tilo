//! What Tube shows that the document does not hold, published beneath the resource as rows of
//! declared types: the open watch page with its comments, the fetched catalogue (videos, creators,
//! playlists, the completed search), the workspace's watch history and the person's playback preferences, and the
//! paired Grayjay devices. A row says what it is by its type, and offers its type's effects
//! bound to it.

use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_resource;
use ui::tilo_resource::RowBindings;

use crate::action::SettingFlag;
use crate::actions::{TubeEffect, comment_key, next_quality, resource_label};
use crate::model::{Category, Route, Settings, SkipMode, TubeModel};
use crate::sync::{PeerEntry, SyncStatus, SyncView};

const MAX_ROWS: usize = 100;
const MAX_COMMENTS: usize = 50;

/// The catalogue and the watch page a row groups: videos, creators, playlists,
/// or one search's results.
#[derive(tilo_resource::Row)]
#[row(kind = "tube.collection", label = "Collection")]
pub(crate) struct Collection;

/// One video watched in the workspace.
#[derive(tilo_resource::Row)]
#[row(
    kind = "tube.history-entry",
    label = "Watched video",
    effects = TubeEffect
)]
pub(crate) struct HistoryEntry;

/// The workspace's watch history.
#[derive(tilo_resource::Row)]
#[row(kind = "tube.history", label = "Watch history", effects = TubeEffect)]
pub(crate) struct History;

/// The playback preferences, and the settings they hold.
#[derive(tilo_resource::Row)]
#[row(kind = "tube.settings", label = "Playback settings")]
pub(crate) struct PlaybackSettings;

/// One playback setting the person may change.
#[derive(tilo_resource::Row)]
#[row(kind = "tube.setting", label = "Playback setting", effects = TubeEffect)]
pub(crate) struct Setting;

/// The Grayjay device sync, and the devices it pairs.
#[derive(tilo_resource::Row)]
#[row(
    kind = "tube.device-sync",
    label = "Grayjay device sync",
    effects = TubeEffect
)]
pub(crate) struct DeviceSync;

/// One paired Grayjay device.
#[derive(tilo_resource::Row)]
#[row(kind = "tube.device", label = "Grayjay device", effects = TubeEffect)]
pub(crate) struct Device;

/// What a video's row offers about that video: open, share and mark watched,
/// each naming it.
fn video_bindings(video_id: &str) -> RowBindings {
    let video = serde_json::Value::from(video_id);
    RowBindings::new()
        .bind(TubeEffect::OPEN_VIDEO, [("video", video.clone())])
        .bind(TubeEffect::SHARE, [("video", video.clone())])
        .bind(TubeEffect::MARK_WATCHED, [("video", video)])
}

fn video_row(model: &TubeModel, id: String, video_id: &str) -> Option<LiveRow> {
    let video = model.video(video_id)?;
    Some(LiveRow::typed_with::<crate::model::Video>(
        id,
        resource_label(&video.title),
        &video_bindings(video_id),
    ))
}

fn creator_row(model: &TubeModel, id: String, channel_id: &str) -> Option<LiveRow> {
    let channel = model.channel(channel_id)?;
    let bindings = RowBindings::new().bind(
        TubeEffect::OPEN_CREATOR,
        [("creator", serde_json::Value::from(channel_id))],
    );
    Some(LiveRow::typed_with::<crate::model::Channel>(
        id,
        resource_label(&channel.name),
        &bindings,
    ))
}

fn playlist_row(model: &TubeModel, id: String, playlist_id: &str) -> Option<LiveRow> {
    let playlist = model.playlist(playlist_id)?;
    let bindings = RowBindings::new().bind(
        TubeEffect::OPEN_PLAYLIST,
        [("playlist", serde_json::Value::from(playlist_id))],
    );
    Some(LiveRow::typed_with::<crate::model::Playlist>(
        id,
        resource_label(&playlist.title),
        &bindings,
    ))
}

fn comment_label(author: &str, text: &str) -> String {
    let mut line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 48 {
        line = line.chars().take(45).collect::<String>() + "…";
    }
    if author.is_empty() {
        line
    } else {
        format!("{author}: {line}")
    }
}

fn current_watch(model: &TubeModel) -> Option<LiveRow> {
    let Route::Watch(video_id) = &model.route else {
        return None;
    };
    let video = model.video(video_id)?;
    // This page shows the video, so it offers what only a watch page can:
    // playing it, opening its creator, and skipping the segment playing now
    // (which its status says is not there when none is).
    let bindings = video_bindings(video_id)
        .offer(TubeEffect::TOGGLE_PLAYBACK)
        .bind(
            TubeEffect::OPEN_CREATOR,
            [("creator", serde_json::Value::from(video.channel_id.clone()))],
        )
        .offer(TubeEffect::SKIP_SEGMENT);
    let mut row = LiveRow::typed_with::<crate::model::Video>(
        format!("tube.current:{video_id}"),
        resource_label(&video.title),
        &bindings,
    );
    for comment in video.comments.iter().take(MAX_COMMENTS) {
        row = row.row(comment_row(video_id, comment));
    }
    Some(row)
}

fn comment_row(video_id: &str, comment: &crate::model::Comment) -> LiveRow {
    LiveRow::typed::<crate::model::Comment>(
        format!("tube.comment:{}", comment_key(video_id, comment)),
        resource_label(&comment_label(&comment.author, &comment.text)),
    )
}

fn collection(id: &str, label: &str, rows: impl Iterator<Item = LiveRow>) -> LiveRow {
    rows.take(MAX_ROWS)
        .fold(LiveRow::typed::<Collection>(id, label), LiveRow::row)
}

fn history(model: &TubeModel) -> LiveRow {
    // The row is the history itself, so it clears it.
    model
        .history
        .iter()
        .take(MAX_ROWS)
        .map(|video_id| history_row(model, video_id))
        .fold(
            LiveRow::typed::<History>("tube.history", "Watch history"),
            LiveRow::row,
        )
}

fn history_row(model: &TubeModel, video_id: &str) -> LiveRow {
    let label = model.video(video_id).map_or_else(
        || format!("Watched video {video_id}"),
        |video| video.title.clone(),
    );
    let video = serde_json::Value::from(video_id);
    let bindings = RowBindings::new()
        .bind(TubeEffect::OPEN_VIDEO, [("video", video.clone())])
        .bind(TubeEffect::FORGET_WATCHED, [("video", video)]);
    LiveRow::typed_with::<HistoryEntry>(
        format!("tube.history:{video_id}"),
        resource_label(&label),
        &bindings,
    )
}

fn search_results(model: &TubeModel) -> LiveRow {
    let query = model.search_hits_query.trim();
    let label = if query.is_empty() {
        "Search results".to_owned()
    } else {
        resource_label(&format!("Search results: {query}"))
    };
    if query.is_empty() {
        return LiveRow::typed::<Collection>("tube.search-results", label);
    }
    let hit = |kind: &str, id: &str| format!("tube.search-result:{kind}:{id}");
    let videos = model
        .search_hits
        .iter()
        .filter_map(|id| video_row(model, hit("video", id), id));
    let playlists = model
        .search_playlist_hits
        .iter()
        .filter_map(|id| playlist_row(model, hit("playlist", id), id));
    let creators = model
        .search_channel_hits
        .iter()
        .filter_map(|id| creator_row(model, hit("channel", id), id));
    collection(
        "tube.search-results",
        &label,
        videos.chain(playlists).chain(creators),
    )
}

fn preferences(settings: &Settings) -> LiveRow {
    let switches = SettingFlag::ALL
        .into_iter()
        .map(|flag| switch_row(settings, flag));
    let segments = Category::ALL
        .into_iter()
        .map(|category| segment_row(settings, category));
    switches
        .chain([quality_row(settings)])
        .chain(segments)
        .fold(
            LiveRow::typed::<PlaybackSettings>("tube.settings", "Playback settings"),
            LiveRow::row,
        )
}

fn switch_row(settings: &Settings, flag: SettingFlag) -> LiveRow {
    let enabled = flag.get(settings);
    // The row's press flips this switch, which it knows and the effect's
    // caller does not.
    let bindings = RowBindings::new().bind(
        TubeEffect::SET_PREFERENCE,
        [
            ("preference", serde_json::Value::from(flag.id())),
            ("enabled", serde_json::Value::from(!enabled)),
        ],
    );
    LiveRow::typed_with::<Setting>(
        format!("tube.setting:{}", flag.id()),
        format!("{}: {}", flag.name(), if enabled { "on" } else { "off" }),
        &bindings,
    )
}

fn quality_row(settings: &Settings) -> LiveRow {
    let bindings = RowBindings::new().bind(
        TubeEffect::SET_PREFERRED_QUALITY,
        [(
            "quality",
            serde_json::Value::from(next_quality(&settings.preferred_quality)),
        )],
    );
    LiveRow::typed_with::<Setting>(
        "tube.setting:preferred-quality",
        format!("Preferred quality: {}", settings.preferred_quality),
        &bindings,
    )
}

fn segment_row(settings: &Settings, category: Category) -> LiveRow {
    let stored = settings
        .categories
        .get(category.api_key())
        .copied()
        .unwrap_or(SkipMode::Skip);
    let bindings = RowBindings::new().bind(
        TubeEffect::SET_SEGMENT_HANDLING,
        [
            ("category", serde_json::Value::from(category.api_key())),
            ("mode", serde_json::Value::from(stored.next().key())),
        ],
    );
    LiveRow::typed_with::<Setting>(
        format!("tube.setting:sponsor-category:{}", category.api_key()),
        format!(
            "SponsorBlock {}: {}",
            crate::i18n::t(category.message()),
            crate::i18n::t(stored.message())
        ),
        &bindings,
    )
}

fn devices(view: &SyncView) -> LiveRow {
    let unavailable = matches!(view.status, SyncStatus::Unavailable);
    let label = if unavailable {
        "Grayjay device sync (unavailable)"
    } else {
        "Grayjay device sync"
    };
    let mut sync = LiveRow::typed::<DeviceSync>("tube.device-sync", label);
    if unavailable {
        return sync;
    }
    for peer in view.peers.iter().filter(|peer| peer.revoked_at.is_none()) {
        sync = sync.row(device_row(peer));
    }
    sync
}

fn device_row(peer: &PeerEntry) -> LiveRow {
    LiveRow::typed::<Device>(
        format!("tube.device:{}", peer.id),
        resource_label(&peer.name),
    )
}

/// The id `value` names, as the row that published it names it
/// (`tube.video:x` for a catalogue video, `tube.search-result:video:x` for a
/// search hit) or as the catalogue knows it: a row's press and a control's
/// reach the same video, creator, playlist or device.
pub(crate) fn row_entity(value: &str) -> &str {
    for prefix in [
        "tube.search-result:video:",
        "tube.search-result:channel:",
        "tube.search-result:playlist:",
        "tube.video:",
        "tube.history:",
        "tube.current:",
        "tube.channel:",
        "tube.playlist:",
        "tube.device:",
    ] {
        if let Some(id) = value.strip_prefix(prefix) {
            return id;
        }
    }
    value
}

pub(crate) fn live_rows(model: &TubeModel, view: &SyncView) -> Vec<LiveRow> {
    current_watch(model)
        .into_iter()
        .chain([
            collection(
                "tube.videos",
                "Videos",
                model.videos.iter().filter_map(|video| {
                    video_row(model, format!("tube.video:{}", video.id), &video.id)
                }),
            ),
            collection(
                "tube.creators",
                "Creators",
                model.channels.iter().filter_map(|channel| {
                    creator_row(model, format!("tube.channel:{}", channel.id), &channel.id)
                }),
            ),
            collection(
                "tube.playlists",
                "Playlists",
                model.playlists.iter().filter_map(|playlist| {
                    playlist_row(
                        model,
                        format!("tube.playlist:{}", playlist.id),
                        &playlist.id,
                    )
                }),
            ),
            history(model),
            search_results(model),
            preferences(&model.settings),
            devices(view),
        ])
        .collect()
}

pub(crate) fn live_row(model: &TubeModel, view: &SyncView, id: &str) -> Option<LiveRow> {
    if let Some(video_id) = id.strip_prefix("tube.video:") {
        return video_row(model, id.to_owned(), video_id);
    }
    if let Some(channel_id) = id.strip_prefix("tube.channel:") {
        return creator_row(model, id.to_owned(), channel_id);
    }
    if let Some(playlist_id) = id.strip_prefix("tube.playlist:") {
        return playlist_row(model, id.to_owned(), playlist_id);
    }
    if let Some(video_id) = id.strip_prefix("tube.history:") {
        return model
            .history
            .iter()
            .any(|watched| watched == video_id)
            .then(|| history_row(model, video_id));
    }
    if let Some(comment) = id.strip_prefix("tube.comment:") {
        let Route::Watch(video_id) = &model.route else {
            return None;
        };
        return model
            .video(video_id)?
            .comments
            .iter()
            .find(|candidate| comment_key(video_id, candidate) == comment)
            .map(|comment| comment_row(video_id, comment));
    }
    if let Some(rest) = id.strip_prefix("tube.search-result:") {
        let (kind, entity) = rest.split_once(':')?;
        let listed = match kind {
            "video" => &model.search_hits,
            "playlist" => &model.search_playlist_hits,
            "channel" => &model.search_channel_hits,
            _ => return None,
        };
        if model.search_hits_query.trim().is_empty() || !listed.iter().any(|hit| hit == entity) {
            return None;
        }
        return match kind {
            "video" => video_row(model, id.to_owned(), entity),
            "playlist" => playlist_row(model, id.to_owned(), entity),
            _ => creator_row(model, id.to_owned(), entity),
        };
    }
    if id == "tube.setting:preferred-quality" {
        return Some(quality_row(&model.settings));
    }
    if let Some(category) = id.strip_prefix("tube.setting:sponsor-category:") {
        return Category::from_api_key(category)
            .map(|category| segment_row(&model.settings, category));
    }
    if let Some(flag) = id
        .strip_prefix("tube.setting:")
        .and_then(SettingFlag::from_id)
    {
        return Some(switch_row(&model.settings, flag));
    }
    if let Some(device) = id.strip_prefix("tube.device:") {
        if matches!(view.status, SyncStatus::Unavailable) {
            return None;
        }
        return view
            .peers
            .iter()
            .find(|peer| peer.id == device && peer.revoked_at.is_none())
            .map(device_row);
    }
    live_rows(model, view)
        .into_iter()
        .find(|row| row.id() == id)
}

pub(crate) fn effect_statuses(model: &TubeModel, view: &SyncView) -> Vec<EffectStatus> {
    let mut statuses = vec![EffectStatus::unavailable(
        TubeEffect::DOWNLOAD,
        "download_storage_unavailable",
        "Tube has no host-granted download storage capability",
    )];
    if !matches!(model.route, Route::Watch(_)) {
        statuses.push(EffectStatus::unavailable(
            TubeEffect::SEEK,
            "no_video",
            "Open a video to seek in it",
        ));
    }
    if model.show_skip_end().is_none() {
        statuses.push(EffectStatus::unavailable(
            TubeEffect::SKIP_SEGMENT,
            "no_segment",
            "No segment to skip is playing",
        ));
    }
    if matches!(view.status, SyncStatus::Unavailable) {
        let reason = if view.summary.trim().is_empty() {
            crate::i18n::t(crate::sync::capability().summary_key)
        } else {
            view.summary.clone()
        };
        statuses.push(EffectStatus::unavailable(
            TubeEffect::PAIR_DEVICE,
            "grayjay_sync_unavailable",
            reason.clone(),
        ));
        statuses.push(EffectStatus::unavailable(
            TubeEffect::SYNC_DEVICES,
            "grayjay_sync_unavailable",
            reason,
        ));
    }
    statuses
}

/// What one of Tube's rows offers right now: a device waiting for approval is
/// not revoked, a device with access is not approved again, and nothing syncs
/// without an active device or a reachable Grayjay.
pub(crate) fn row_status(model: &TubeModel, view: &SyncView, id: &str) -> Vec<EffectStatus> {
    let mut statuses = Vec::new();
    if id.starts_with("tube.current:") && model.show_skip_end().is_none() {
        // The watch page always offers to skip the segment playing now; with
        // none playing there is nothing to skip.
        statuses.push(
            EffectStatus::unavailable(
                TubeEffect::SKIP_SEGMENT,
                "no_segment",
                "No segment to skip is playing",
            )
            .on(id),
        );
    }
    if id == "tube.device-sync" {
        let reason = if matches!(view.status, SyncStatus::Unavailable) {
            Some(if view.summary.trim().is_empty() {
                crate::i18n::t(crate::sync::capability().summary_key)
            } else {
                view.summary.clone()
            })
        } else if !view.peers.iter().any(PeerEntry::is_active) {
            Some("No paired device is active".to_owned())
        } else {
            None
        };
        if let Some(reason) = reason {
            statuses.push(
                EffectStatus::unavailable(
                    TubeEffect::SYNC_DEVICES,
                    "grayjay_sync_unavailable",
                    reason,
                )
                .on(id),
            );
        }
    } else if let Some(device) = id.strip_prefix("tube.device:") {
        let peer = view
            .peers
            .iter()
            .find(|peer| peer.id == device && peer.revoked_at.is_none());
        if let Some(peer) = peer {
            let (effect, reason) = if peer.pending_approval {
                (
                    TubeEffect::REVOKE_DEVICE,
                    "This device is waiting for approval",
                )
            } else {
                (TubeEffect::APPROVE_DEVICE, "This device already has access")
            };
            statuses.push(EffectStatus::unavailable(effect, "device_access", reason).on(id));
        }
    }
    statuses
}
