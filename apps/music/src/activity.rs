//! What Music keeps going outside its window: the playing song as the summary the shell
//! draws as a pill and a card, and the controls pressed on them.

use serde::Deserialize;
use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

/// What Music shows of the song it plays, as the shell draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Playing {
    /// The track, also the activity's id: a card put away stays away until the song changes.
    pub track: String,
    pub title: String,
    /// Who it is by, empty when the catalogue does not say.
    pub artist: String,
    pub playing: bool,
    /// The playhead and the song's length in milliseconds, zero when unknown.
    pub position_ms: f64,
    pub length_ms: f64,
    /// The cover as a small `data:` picture; empty draws the song's glyph instead.
    pub artwork: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Command {
    pub command: String,
    /// The activity it was pressed on: a press for a song that has changed is not its own.
    #[serde(default)]
    pub activity_id: Option<String>,
    /// How far into the song a seek points, from 0 to 1.
    #[serde(default)]
    pub fraction: Option<f64>,
}

/// The song as the shell's activity: title, artist, cover, playhead and transport.
#[must_use]
pub fn summary(playing: &Playing) -> Summary {
    let mut summary = Summary::new(Kind::Music, playing.title.clone(), "")
        .with_id(playing.track.clone())
        .with_icon("music")
        .with_control(
            Control::new("previous", "Previous", ControlKind::Secondary).with_icon("skip-back"),
        )
        .with_control(
            Control::new(
                "play-pause",
                if playing.playing { "Pause" } else { "Play" },
                ControlKind::Primary,
            )
            .with_icon(if playing.playing { "pause" } else { "play" }),
        )
        .with_control(
            Control::new("next", "Next", ControlKind::Secondary).with_icon("skip-forward"),
        );
    if !playing.artist.is_empty() {
        summary = summary.with_subtitle(playing.artist.clone());
    }
    if !playing.artwork.is_empty() {
        summary = summary.with_artwork(playing.artwork.clone());
    }
    if playing.length_ms > 0.0 {
        let length_ms = playing.length_ms;
        let (position, length) = (playing.position_ms.clamp(0.0, length_ms), length_ms);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a recording's length in seconds is far inside u64"
        )]
        let seconds = (length / 1000.0).round() as u64;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a fraction of a song is inside f32's range with room to spare"
        )]
        let progress = (position / length) as f32;
        summary = summary.with_length(seconds).with_progress(progress);
    }
    summary
}

#[must_use]
pub fn published(playing: Option<&Playing>) -> Published {
    playing.map_or_else(Published::default, |playing| {
        Published::of(summary(playing))
    })
}

pub fn publish(playing: Option<&Playing>) {
    let shown = playing.and_then(|playing| serde_json::to_value(published(Some(playing))).ok());
    ui::collaboration::channel_live_activity::show(shown);
}

#[must_use]
pub fn take_commands() -> Vec<Command> {
    ui::collaboration::channel_live_activity::take_controls()
        .into_iter()
        .filter_map(|control| serde_json::from_value(control).ok())
        .collect()
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
