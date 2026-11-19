use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_resource::row::RowBindings;

use super::{Input, suggestion, suggestions};
use crate::actions::{MusicEffect, Place};
use crate::model::Library;

pub const PREFERENCES: &str = "music.recommendation-preferences:library";

/// The row's Open of the recommendations page it belongs to.
fn open_bindings() -> RowBindings {
    crate::actions::open_place(Place::View("recommendations".into()))
}

pub fn row_by_id(lib: &Library, id: &str) -> Option<LiveRow> {
    if id == PREFERENCES {
        let mut row = LiveRow::typed_with::<crate::row_types::RecommendationPreferences>(
            id,
            crate::i18n::t("music-recommendations-title"),
            &open_bindings(),
        );
        for input in Input::ALL {
            if let Some(child) =
                row_by_id(lib, &format!("music.recommendation-input:{}", input.key()))
            {
                row = row.row(child);
            }
        }
        return Some(row);
    }
    if let Some(key) = id.strip_prefix("music.recommendation-input:") {
        let input = Input::ALL.into_iter().find(|i| i.key() == key)?;
        let enabled = lib.recommendations.enabled(input);
        return Some(
            LiveRow::typed_with::<crate::row_types::RecommendationInput>(
                id,
                format!(
                    "{}: {}",
                    input.label(),
                    crate::i18n::t(if enabled {
                        "music-recommendations-enabled"
                    } else {
                        "music-recommendations-disabled"
                    })
                ),
                &open_bindings(),
            ),
        );
    }
    if let Some(track) = id.strip_prefix("music.recommendation:") {
        return song_row(lib, id, track);
    }
    if let Some(artist) = id.strip_prefix("music.recommendation-artist:") {
        let preferred = lib.recommendations.prefers(artist);
        if !preferred && lib.artist(artist).is_none() {
            return None;
        }
        return Some(
            LiveRow::typed_with::<crate::row_types::RecommendationArtist>(
                id,
                lib.artist(artist).map_or(artist, |a| a.name.as_str()),
                &open_bindings(),
            ),
        );
    }
    None
}

fn song_row(lib: &Library, id: &str, track: &str) -> Option<LiveRow> {
    let dismissed = lib.recommendations.is_dismissed(track);
    let suggestion = suggestion(lib, track);
    if suggestion.is_none() && !dismissed {
        return None;
    }
    let title = lib.track(track).map_or(track, |t| t.title.as_str());
    let label = suggestion.map_or_else(
        || title.to_owned(),
        |s| format!("{title} — {}", s.reason.label(lib)),
    );
    let bindings = open_bindings().bind(
        MusicEffect::PLAY,
        crate::actions::play_arguments(track, None),
    );
    Some(LiveRow::typed_with::<crate::row_types::Recommendation>(
        id, label, &bindings,
    ))
}

/// What a recommended song's row says of its effects now: one the library no
/// longer holds cannot be played here.
pub fn row_status(lib: &Library, id: &str) -> Vec<EffectStatus> {
    let Some(track) = id.strip_prefix("music.recommendation:") else {
        return Vec::new();
    };
    if lib.track(track).is_some() {
        return Vec::new();
    }
    vec![
        EffectStatus::unavailable(
            MusicEffect::PLAY,
            "missing_track",
            crate::i18n::t("music-recommendations-missing-track"),
        )
        .on(id),
    ]
}

pub fn rows(lib: &Library) -> Vec<LiveRow> {
    let picks = suggestions(lib, 6);
    let artists = picks
        .iter()
        .filter_map(|pick| lib.track(&pick.track).map(|track| track.artist_id.clone()))
        .chain(lib.recommendations.preferred_artists.iter().cloned())
        .collect::<std::collections::BTreeSet<_>>();
    let ids = std::iter::once(PREFERENCES.to_owned())
        .chain(
            picks
                .into_iter()
                .map(|s| format!("music.recommendation:{}", s.track)),
        )
        .chain(
            lib.recommendations
                .dismissed
                .iter()
                .map(|id| format!("music.recommendation:{id}")),
        )
        .chain(
            artists
                .into_iter()
                .map(|id| format!("music.recommendation-artist:{id}")),
        );
    ids.filter_map(|id| row_by_id(lib, &id)).collect()
}
