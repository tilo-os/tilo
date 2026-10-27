//! What a control pressing an [`Action`] is bound to in Music's published
//! surface, where the action is one.

use super::{Action, MusicApp, View};
use crate::model::Library;

/// What an [`Action`] is in Music's published surface, where it is one:
/// a control pressing it is bound to that, so agents, scripts and the
/// palette press the same thing.
pub(super) enum Published {
    Effect(crate::actions::MusicEffect),
    Edit(ui::tilo_resource::Edit<Library>),
}

impl Action {
    /// Whether the action only changes what this window shows: a menu, a
    /// view, where a control lives.
    pub(super) fn only_shows(&self) -> bool {
        matches!(
            self,
            Self::Nav(_)
                | Self::Back
                | Self::OpenPlaylistMenu(_)
                | Self::OpenSpotifyPlaylistMenu(_)
                | Self::OpenTrackMenu { .. }
                | Self::OpenAddToPlaylist(_)
                | Self::OpenPlayerMenu(_)
                | Self::OpenFacet(_)
                | Self::SetQuery(_)
                | Self::ShowControl { .. }
                | Self::ToggleQueue
        )
    }

    pub(super) fn published(&self) -> Option<Published> {
        use crate::actions::{MusicEffect, Place};
        let effect = match self {
            Self::Recommendation(edit) => return Some(Published::Edit(edit.clone())),
            Self::SpotifyEffect(effect) => effect.clone(),
            Self::Nav(View::Album(album)) => MusicEffect::OpenAlbum {
                album: album.clone(),
            },
            Self::Nav(View::Artist(artist)) => MusicEffect::OpenArtist {
                artist: artist.clone(),
            },
            Self::Nav(View::Playlist(playlist)) => MusicEffect::Open {
                place: Place::Playlist(playlist.clone()),
            },
            Self::Nav(view) => MusicEffect::Open {
                place: Place::View(top_level_id(view)?.to_owned()),
            },
            Self::TogglePlay => MusicEffect::PlayPause,
            Self::Next => MusicEffect::Next,
            Self::Prev => MusicEffect::Previous,
            Self::ToggleShuffle => MusicEffect::ToggleShuffle,
            Self::CycleRepeat => MusicEffect::CycleRepeat,
            Self::ToggleMute => MusicEffect::ToggleMute,
            Self::ToggleLike(track) => MusicEffect::ToggleLike {
                track: track.clone(),
            },
            Self::AddToQueue(track) => MusicEffect::AddToQueue {
                track: track.clone(),
            },
            Self::PlayNext(track) => MusicEffect::PlayNext {
                track: track.clone(),
            },
            _ => return None,
        };
        Some(Published::Effect(effect))
    }
}

/// The published id of a top-level view, as [`MusicApp::top_level_view`]
/// reads it back.
fn top_level_id(view: &View) -> Option<&'static str> {
    crate::actions::NAVIGATION_VIEWS
        .iter()
        .map(|(id, _)| *id)
        .find(|id| MusicApp::top_level_view(id).as_ref() == Some(view))
}
