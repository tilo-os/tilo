use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};

use super::{Input, Preferences};
use crate::actions::MusicAction;

const INPUT: &str = "recommendations/input/";
const ARTIST: &str = "recommendations/preferred-artist/";
const DISMISSED: &str = "recommendations/dismissed/";

pub(crate) fn seed(doc: &mut AutoCommit, prefs: &Preferences) -> Result<(), String> {
    for input in Input::ALL {
        write(
            doc,
            &MusicAction::SetRecommendationInput {
                input,
                enabled: prefs.enabled(input),
            },
        )?;
    }
    for artist in &prefs.preferred_artists {
        write(
            doc,
            &MusicAction::SetRecommendationArtist {
                artist: artist.clone(),
                preferred: true,
            },
        )?;
    }
    for track in &prefs.dismissed {
        write(
            doc,
            &MusicAction::SetRecommendationDismissed {
                track: track.clone(),
                dismissed: true,
            },
        )?;
    }
    Ok(())
}

pub(crate) fn read(doc: &AutoCommit) -> Preferences {
    use automerge::ReadDoc as _;
    let mut prefs = Preferences::default();
    for (input, enabled) in [
        (Input::Likes, &mut prefs.likes),
        (Input::SavedAlbums, &mut prefs.saved_albums),
        (Input::FollowedArtists, &mut prefs.followed_artists),
    ] {
        *enabled =
            ui::doc::read_bool(doc, &ROOT, format!("{INPUT}{}", input.key())).unwrap_or(true);
    }
    for key in doc
        .keys(ROOT)
        .filter(|key| ui::doc::read_bool(doc, &ROOT, key).unwrap_or(false))
    {
        if let Some(id) = key.strip_prefix(ARTIST) {
            prefs.preferred_artists.insert(id.into());
        }
        if let Some(id) = key.strip_prefix(DISMISSED) {
            prefs.dismissed.insert(id.into());
        }
    }
    prefs
}

/// Absolute scalar entries avoid competing object creation when two devices
/// first edit preferences in an older library with no recommendation schema.
pub(crate) fn write(doc: &mut AutoCommit, action: &MusicAction) -> Result<(), String> {
    let (key, value) = match action {
        MusicAction::SetRecommendationInput { input, enabled } => {
            (format!("{INPUT}{}", input.key()), *enabled)
        }
        MusicAction::SetRecommendationDismissed { track, dismissed } => {
            (format!("{DISMISSED}{track}"), *dismissed)
        }
        MusicAction::SetRecommendationArtist { artist, preferred } => {
            (format!("{ARTIST}{artist}"), *preferred)
        }
        _ => return Err("not a recommendation preference action".into()),
    };
    doc.put(ROOT, key, value).map_err(|e| e.to_string())
}
