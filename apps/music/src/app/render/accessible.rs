//! The accessibility tree of Music's pages: one node per track, album,
//! artist, playlist and download a person can browse, with the actions
//! each row offers.

use super::super::CacheState;

pub(super) fn accessible_track(
    lib: &crate::model::Library,
    track: &crate::model::Track,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    use ui::a11y::{AccessNode, AccessState, Role};

    let liked = lib.is_liked(&track.id);
    let mut node = AccessNode::new(Role::ListItem)
        .with_hashed_key(format!("track:{}", track.id))
        .with_name(&track.title)
        .with_description(format!(
            "{}; {}; {}",
            lib.artist_name(&track.artist_id),
            lib.album_title(&track.album_id),
            ui::format::timecode(u64::from(track.duration_s))
        ))
        .with_position(position, total)
        .with_children(vec![
            AccessNode::new(Role::Button)
                .with_name(format!("Play {}", track.title))
                .with_action_target(format!("music.play-track.{}", track.id)),
            AccessNode::new(Role::ToggleButton)
                .with_name(format!("Like {}", track.title))
                .with_state(AccessState::new().checked(liked))
                .with_action_target(format!("music.toggle-like.{}", track.id)),
        ]);
    if !lib.tracks.contains_key(&track.id) {
        node.children.truncate(1);
    }
    node
}

pub(super) fn accessible_download(
    lib: &crate::model::Library,
    track: &crate::model::Track,
    state: CacheState,
    failure: Option<&str>,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    use ui::a11y::{AccessNode, Role};

    let (description, action_name, action_target) = if let Some(error) = failure {
        (
            format!(
                "{}; download failed: {error}. Retry the offline download.",
                lib.artist_name(&track.artist_id)
            ),
            format!("Retry offline download for {}", track.title),
            format!("music.retry-offline.{}", track.id),
        )
    } else if matches!(state, CacheState::Downloading(_)) {
        (
            format!(
                "{}; offline download in progress",
                lib.artist_name(&track.artist_id)
            ),
            format!("Cancel offline download for {}", track.title),
            format!("music.remove-offline.{}", track.id),
        )
    } else {
        (
            format!(
                "{}; available offline; {}",
                lib.artist_name(&track.artist_id),
                ui::format::timecode(u64::from(track.duration_s))
            ),
            format!("Remove offline download for {}", track.title),
            format!("music.remove-offline.{}", track.id),
        )
    };
    let mut children = vec![
        AccessNode::new(Role::Button)
            .with_name(action_name)
            .with_action_target(action_target),
    ];
    if failure.is_none() && matches!(state, CacheState::Cached) {
        children.insert(
            0,
            AccessNode::new(Role::Button)
                .with_name(format!("Play {}", track.title))
                .with_action_target(format!("music.play-track.{}", track.id)),
        );
    }
    AccessNode::new(Role::ListItem)
        .with_hashed_key(format!("download:{}", track.id))
        .with_name(&track.title)
        .with_description(description)
        .with_position(position, total)
        .with_children(children)
}

pub(super) fn accessible_album(
    lib: &crate::model::Library,
    album: &crate::model::Album,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    ui::a11y::AccessNode::new(ui::a11y::Role::ListItem)
        .with_hashed_key(format!("album:{}", album.id))
        .with_name(&album.title)
        .with_description(format!(
            "{}; {}",
            lib.artist_name(&album.artist_id),
            album.year
        ))
        .with_position(position, total)
        .with_action_target(format!("music.open-album.{}", album.id))
}

pub(super) fn accessible_artist(
    artist: &crate::model::Artist,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    ui::a11y::AccessNode::new(ui::a11y::Role::ListItem)
        .with_hashed_key(format!("artist:{}", artist.id))
        .with_name(&artist.name)
        .with_description(format!(
            "{}; {} monthly listeners",
            artist.genre, artist.monthly_listeners
        ))
        .with_position(position, total)
        .with_action_target(format!("music.open-artist.{}", artist.id))
}

pub(super) fn accessible_playlist(
    playlist: &crate::model::Playlist,
    position: usize,
    total: usize,
) -> ui::a11y::AccessNode {
    let resource = crate::actions::playlist_resource_id(&playlist.id);
    ui::a11y::AccessNode::new(ui::a11y::Role::ListItem)
        .with_hashed_key(format!("playlist:{}", playlist.id))
        .with_name(&playlist.name)
        .with_description(format!("{} songs", playlist.len()))
        .with_position(position, total)
        .with_action_target(format!("music.open-playlist.{}", playlist.id))
        .with_children(vec![
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_name(format!("Rename {}", playlist.name))
                .with_action_target(format!("music.rename-playlist.{resource}")),
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_name(format!("Delete {}", playlist.name))
                .with_action_target(format!("music.delete-playlist.{resource}")),
        ])
}

pub(super) fn accessible_playlist_track(
    lib: &crate::model::Library,
    playlist: &crate::model::Playlist,
    track: &crate::model::Track,
    position: usize,
) -> ui::a11y::AccessNode {
    use ui::a11y::{AccessNode, Role};
    let total = playlist.len();
    let resource = crate::actions::playlist_track_resource_id(&playlist.id, &track.id);
    let mut node = accessible_track(lib, track, position, total);
    let mut actions = vec![
        AccessNode::new(Role::Button)
            .with_name(format!("Remove {} from {}", track.title, playlist.name))
            .with_action_target(format!("music.remove-playlist-track.{resource}")),
    ];
    if position > 1 {
        actions.push(
            AccessNode::new(Role::Button)
                .with_name(format!("Move {} up", track.title))
                .with_action_target(format!("music.move-up-playlist-track.{resource}")),
        );
    }
    if position < total {
        actions.push(
            AccessNode::new(Role::Button)
                .with_name(format!("Move {} down", track.title))
                .with_action_target(format!("music.move-down-playlist-track.{resource}")),
        );
    }
    node.children.extend(actions);
    node
}
