//! What plays, as the desktop reads it: the player in front and its song,
//! the other players one can switch to, where its sound can go, and what
//! another of the person's devices hands this one. The panel shows it as its
//! now-playing card; the desktop sends each command to exactly the player it
//! names.

use media::handoff::HandoffSource;
use ui::widget::TransportArt;

/// The player the card steers, and what else it can choose from.
#[derive(Clone, Debug, Default, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is an independent capability the player reports"
)]
pub struct NowPlaying {
    pub title: String,
    /// Who it is by.
    pub subtitle: String,
    /// The app that plays it, where several players run.
    pub player_name: String,
    pub art: TransportArt,
    pub playing: bool,
    pub can_toggle: bool,
    pub can_previous: bool,
    pub can_next: bool,
    /// Where the song is, while the player says and can seek.
    pub position_ms: f64,
    /// How long it is; zero when the player doesn't say or can't seek.
    pub duration_ms: f64,
    /// Every running player, the one in front marked.
    pub players: Vec<MediaPlayer>,
    /// Where its sound can play, the current one marked.
    pub outputs: Vec<MediaOutput>,
    /// The speaker group it plays on, and how that stands.
    pub speakers: Option<String>,
    /// The session another of the person's devices hands this one, which
    /// the card offers to continue here.
    pub offer: Option<MediaOffer>,
}

/// A session another device hands this one, at the point it stopped.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaOffer {
    /// The handoff's request, 32 hex digits: the card names it when it asks
    /// to continue it here.
    pub request_id: String,
    /// The app that plays it, by package id.
    pub package_id: String,
    pub title: String,
    /// The device it comes from, as the person knows it.
    pub source_label: String,
    /// What it plays, as the locator this device resumes.
    pub source: HandoffSource,
    /// Where it stopped, in milliseconds.
    pub position_ms: f64,
    pub playing: bool,
}

/// What plays here, as another of the person's devices continues it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayingSource {
    /// The session the hand-over names: the player and its source.
    pub session_id: String,
    /// The app that plays it, by package id.
    pub app_id: String,
    pub title: String,
    /// The stream it plays from.
    pub url: String,
    /// Where it is, in milliseconds.
    pub position_ms: u64,
    /// It plays rather than stands paused.
    pub playing: bool,
}

/// One running player.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaPlayer {
    /// The bus name the desktop sends its commands to.
    pub id: String,
    pub name: String,
    pub active: bool,
}

/// One place its sound can go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaOutput {
    /// The destination's continuity token.
    pub token: String,
    pub label: String,
    pub current: bool,
}

impl NowPlaying {
    /// A song playing in one player with two outputs, for shots and tests.
    #[must_use]
    pub fn fixture() -> Self {
        Self {
            title: "Clair de lune".into(),
            subtitle: "Claude Debussy".into(),
            player_name: "Music".into(),
            art: TransportArt::Placeholder,
            playing: true,
            can_toggle: true,
            can_previous: true,
            can_next: true,
            position_ms: 72_000.0,
            duration_ms: 300_000.0,
            players: vec![
                MediaPlayer {
                    id: "org.mpris.MediaPlayer2.music".into(),
                    name: "Music".into(),
                    active: true,
                },
                MediaPlayer {
                    id: "org.mpris.MediaPlayer2.browser".into(),
                    name: "Browser".into(),
                    active: false,
                },
            ],
            outputs: vec![
                MediaOutput {
                    token: "local".into(),
                    label: "On this computer".into(),
                    current: true,
                },
                MediaOutput {
                    token: "living-room".into(),
                    label: "Living room".into(),
                    current: false,
                },
            ],
            speakers: None,
            offer: None,
        }
    }

    /// Whether the player can go to a point in the song.
    #[must_use]
    pub fn seekable(&self) -> bool {
        self.duration_ms > 0.0
    }
}
