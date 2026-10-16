//! What Tube shows that the document does not hold, published beneath the
//! resource as live rows: the open watch page with its comments, the fetched
//! catalogue (videos, creators, playlists, the completed search), the
//! person's watch history and playback preferences, and the paired Grayjay
//! devices.
//!
//! A row offers the effects bound to its entity (open *this* video). The
//! edits on it — liking a video, queueing it, subscribing to its creator,
//! liking or replying to a comment — are the resource's declarations,
//! naming the entity by the id after the row id's `tube.<kind>:` prefix.

use ui::framework::{EffectStatus, LiveRow};

use crate::action::SettingFlag;
use crate::actions::{TubeEffect, comment_key, next_quality, resource_label};
use crate::model::{Category, Route, Settings, SkipMode, TubeModel};
use crate::sync::{PeerRecord, SyncStatus, SyncView};

const MAX_ROWS: usize = 100;
const MAX_COMMENTS: usize = 50;

fn video_row(model: &TubeModel, id: String, video_id: &str) -> Option<LiveRow> {
    let video = model.video(video_id)?;
    Some(
        LiveRow::new(id, "tube.video", resource_label(&video.title))
            .effect(&TubeEffect::OpenVideo {
                video: video_id.to_owned(),
            })
            .effect(&TubeEffect::Share {
                video: video_id.to_owned(),
            })
            .effect(&TubeEffect::MarkWatched {
                video: video_id.to_owned(),
            }),
    )
}

fn creator_row(model: &TubeModel, id: String, channel_id: &str) -> Option<LiveRow> {
    let channel = model.channel(channel_id)?;
    Some(
        LiveRow::new(id, "tube.channel", resource_label(&channel.name)).effect(
            &TubeEffect::OpenCreator {
                creator: channel_id.to_owned(),
            },
        ),
    )
}

fn playlist_row(model: &TubeModel, id: String, playlist_id: &str) -> Option<LiveRow> {
    let playlist = model.playlist(playlist_id)?;
    Some(
        LiveRow::new(id, "tube.playlist", resource_label(&playlist.title)).effect(
            &TubeEffect::OpenPlaylist {
                playlist: playlist_id.to_owned(),
            },
        ),
    )
}

/// One-line comment preview for the row label.
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

/// The open watch page, `tube.current:ID`: play or pause it, share it, its
/// creator, and its comments as `tube.comment:ID` rows.
fn current_watch(model: &TubeModel) -> Option<LiveRow> {
    let Route::Watch(video_id) = &model.route else {
        return None;
    };
    let video = model.video(video_id)?;
    let mut row = video_row(model, format!("tube.current:{video_id}"), video_id)?
        .effect(&TubeEffect::TogglePlayback)
        .effect(&TubeEffect::OpenCreator {
            creator: video.channel_id.clone(),
        });
    for comment in video.comments.iter().take(MAX_COMMENTS) {
        row = row.row(comment_row(video_id, comment));
    }
    Some(row)
}

fn comment_row(video_id: &str, comment: &crate::model::Comment) -> LiveRow {
    LiveRow::new(
        format!("tube.comment:{}", comment_key(video_id, comment)),
        "tube.comment",
        resource_label(&comment_label(&comment.author, &comment.text)),
    )
}

fn collection(id: &str, label: &str, rows: impl Iterator<Item = LiveRow>) -> LiveRow {
    rows.take(MAX_ROWS)
        .fold(LiveRow::new(id, "tube.collection", label), LiveRow::row)
}

fn history(model: &TubeModel) -> LiveRow {
    collection(
        "tube.history",
        "Watch history",
        model
            .history
            .iter()
            .map(|video_id| history_row(model, video_id)),
    )
    .effect(&TubeEffect::ClearHistory)
}

fn history_row(model: &TubeModel, video_id: &str) -> LiveRow {
    let label = model.video(video_id).map_or_else(
        || format!("Watched video {video_id}"),
        |video| video.title.clone(),
    );
    LiveRow::new(
        format!("tube.history:{video_id}"),
        "tube.history-entry",
        resource_label(&label),
    )
    .effect(&TubeEffect::OpenVideo {
        video: video_id.to_owned(),
    })
    .effect(&TubeEffect::ForgetWatched {
        video: video_id.to_owned(),
    })
}

/// The completed search's results in order, so "the third result" means the
/// same thing to an agent as on screen.
fn search_results(model: &TubeModel) -> LiveRow {
    let query = model.search_hits_query.trim();
    let label = if query.is_empty() {
        "Search results".to_owned()
    } else {
        resource_label(&format!("Search results: {query}"))
    };
    if query.is_empty() {
        return LiveRow::new("tube.search-results", "tube.collection", label);
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

/// The person's playback preferences, `tube.settings`: flip each switch, and
/// cycle the preferred quality and each segment category's handling.
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
            LiveRow::new("tube.settings", "tube.settings", "Playback settings"),
            LiveRow::row,
        )
}

fn switch_row(settings: &Settings, flag: SettingFlag) -> LiveRow {
    let enabled = flag.get(settings);
    LiveRow::new(
        format!("tube.setting:{}", flag.id()),
        "tube.setting",
        format!("{}: {}", flag.name(), if enabled { "on" } else { "off" }),
    )
    .effect(&TubeEffect::SetPreference {
        preference: flag.id().to_owned(),
        enabled: !enabled,
    })
}

fn quality_row(settings: &Settings) -> LiveRow {
    LiveRow::new(
        "tube.setting:preferred-quality",
        "tube.setting",
        format!("Preferred quality: {}", settings.preferred_quality),
    )
    .effect(&TubeEffect::SetPreferredQuality {
        quality: next_quality(&settings.preferred_quality),
    })
}

fn segment_row(settings: &Settings, category: Category) -> LiveRow {
    let stored = settings
        .categories
        .get(category.api_key())
        .copied()
        .unwrap_or(SkipMode::Skip);
    LiveRow::new(
        format!("tube.setting:sponsor-category:{}", category.api_key()),
        "tube.setting",
        format!("SponsorBlock {}: {}", category.label(), stored.label()),
    )
    .effect(&TubeEffect::SetSegmentHandling {
        category: category.api_key().to_owned(),
        mode: stored.next().key().to_owned(),
    })
}

/// Grayjay device sync: sync now, and approve or revoke each device.
fn devices(view: &SyncView) -> LiveRow {
    let unavailable = matches!(view.status, SyncStatus::Unavailable);
    let mut row = LiveRow::new(
        "tube.device-sync",
        "tube.device-sync",
        if unavailable {
            "Grayjay device sync (unavailable)"
        } else {
            "Grayjay device sync"
        },
    );
    if unavailable {
        return row;
    }
    if view.peers.iter().any(PeerRecord::is_active) {
        row = row.effect(&TubeEffect::SyncDevices);
    }
    for peer in view.peers.iter().filter(|peer| peer.revoked_at.is_none()) {
        row = row.row(device_row(peer));
    }
    row
}

fn device_row(peer: &PeerRecord) -> LiveRow {
    let device = LiveRow::new(
        format!("tube.device:{}", peer.id),
        "tube.device",
        resource_label(&peer.name),
    );
    if peer.pending_approval {
        device.effect(&TubeEffect::ApproveDevice {
            device: peer.id.clone(),
        })
    } else {
        device.effect(&TubeEffect::RevokeDevice {
            device: peer.id.clone(),
        })
    }
}

/// Every live row, bounded: the watch page first, as what the person is
/// looking at.
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

/// The live row `id`, including the entities beyond the published page.
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

/// The effects whose availability the surface reports: downloads have no
/// storage to go to, and device sync may be unavailable on this device.
pub(crate) fn effect_statuses(view: &SyncView) -> Vec<EffectStatus> {
    let mut statuses = vec![EffectStatus::unavailable(
        TubeEffect::DOWNLOAD,
        "download_storage_unavailable",
        "Tube has no host-granted download storage capability",
    )];
    if matches!(view.status, SyncStatus::Unavailable) {
        let reason = if view.summary.trim().is_empty() {
            crate::sync::capability().summary.to_string()
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
