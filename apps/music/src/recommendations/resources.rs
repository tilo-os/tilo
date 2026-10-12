use ui::tilo_actions::Resource;
use ui::tilo_resource::Effect;

use super::{Input, suggestion, suggestions};
use crate::actions::{MusicEffect, Place};
use crate::model::Library;

pub const PREFERENCES: &str = "music.recommendation-preferences:library";

pub fn resource_by_id(lib: &Library, id: &str) -> Option<Resource> {
    let open = || {
        MusicEffect::Open {
            place: Place::View("recommendations".into()),
        }
        .bound()
    };
    if id == PREFERENCES {
        return Some(
            Resource::new(
                id,
                "music.recommendation-preferences",
                crate::i18n::t("music-recommendations-title"),
            )
            .with_action(open())
            .with_children(Input::ALL.into_iter().filter_map(|input| {
                resource_by_id(lib, &format!("music.recommendation-input:{}", input.key()))
            })),
        );
    }
    if let Some(key) = id.strip_prefix("music.recommendation-input:") {
        let input = Input::ALL.into_iter().find(|i| i.key() == key)?;
        let enabled = lib.recommendations.enabled(input);
        return Some(
            Resource::new(
                id,
                "music.recommendation-input",
                format!(
                    "{}: {}",
                    input.label(),
                    crate::i18n::t(if enabled {
                        "music-recommendations-enabled"
                    } else {
                        "music-recommendations-disabled"
                    })
                ),
            )
            .with_action(open()),
        );
    }
    if let Some(track) = id.strip_prefix("music.recommendation:") {
        return song_resource(lib, id, track);
    }
    if let Some(artist) = id.strip_prefix("music.recommendation-artist:") {
        let preferred = lib.recommendations.prefers(artist);
        if !preferred && lib.artist(artist).is_none() {
            return None;
        }
        return Some(
            Resource::new(
                id,
                "music.recommendation-artist",
                lib.artist(artist).map_or(artist, |a| a.name.as_str()),
            )
            .with_action(open()),
        );
    }
    None
}

fn song_resource(lib: &Library, id: &str, track: &str) -> Option<Resource> {
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
    let play = MusicEffect::Play {
        track: track.into(),
        queued: None,
    }
    .bound();
    let play = if lib.track(track).is_some() {
        play
    } else {
        play.unavailable(
            "missing_track",
            crate::i18n::t("music-recommendations-missing-track"),
        )
    };
    let row = Resource::new(id, "music.recommendation", label)
        .with_action(
            MusicEffect::Open {
                place: Place::View("recommendations".into()),
            }
            .bound(),
        )
        .with_action(play);
    Some(row)
}

pub fn resources(lib: &Library) -> Vec<Resource> {
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
    ids.filter_map(|id| resource_by_id(lib, &id)).collect()
}
