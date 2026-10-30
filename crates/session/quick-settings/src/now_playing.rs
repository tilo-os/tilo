//! What plays, as the desktop reads it: the player in front and its song,
//! the other players one can switch to, and where its sound can go. The
//! panel shows it as its now-playing card; the desktop sends each command to
//! exactly the player it names.

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
        }
    }

    /// Whether the player can go to a point in the song.
    #[must_use]
    pub fn seekable(&self) -> bool {
        self.duration_ms > 0.0
    }
}
