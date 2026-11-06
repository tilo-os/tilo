//! What Music keeps going outside its window: the song it plays, as the bar's
//! pill names it and as its floating card shows it, and the controls pressed
//! on either, handed back to the app.
//!
//! The shell draws the one [`Summary`] shape, so the pill and the card can
//! never disagree about the song; what a card shows of a song — its cover, the
//! point it stands at and what is left of it — travels in that summary too,
//! the cover as a small `data:` picture the shell's image store reads. The
//! song that plays is the activity's id, so putting its card away lasts until
//! the song changes.

use serde::Deserialize;
use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

/// What Music shows of the song it plays.
#[derive(Clone, Debug, PartialEq)]
pub struct Playing {
    /// The track, which is also the activity's id: the card the person put
    /// away stays away until another song plays.
    pub track: String,
    pub title: String,
    /// Who it is by, empty when the catalogue does not say.
    pub artist: String,
    pub playing: bool,
    /// Where the playhead stands and how long the song is, in milliseconds;
    /// the length is zero when the app does not know it.
    pub position_ms: f64,
    pub length_ms: f64,
    /// Its cover as a picture address (a small `data:` picture), empty while
    /// there is none: the card then draws the song's glyph in its place.
    pub artwork: String,
}

/// A control pressed on the song's pill or card.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Command {
    /// The control's id, as the summary named it.
    pub command: String,
    /// The activity it was pressed on: a press for a song that has since
    /// changed is not this song's.
    #[serde(default)]
    pub activity_id: Option<String>,
    /// How far into the song a seek points, from 0 to 1.
    #[serde(default)]
    pub fraction: Option<f64>,
}

/// The song as the shell's live activity: its title, who it is by, the cover
/// and where the playhead stands, with the transport beside them.
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

/// What Music shows now, as the shell reads it.
#[must_use]
pub fn published(playing: Option<&Playing>) -> Published {
    playing.map_or_else(Published::default, |playing| {
        Published::of(summary(playing))
    })
}

/// Show `playing` as Music's live activity, or end it when nothing plays.
pub fn publish(playing: Option<&Playing>) {
    let shown = playing.and_then(|playing| serde_json::to_value(published(Some(playing))).ok());
    ui::collaboration::channel_live_activity::show(shown);
}

/// The controls pressed on the song since the last call.
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
