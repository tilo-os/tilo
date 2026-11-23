//! What plays, which of them a media key reaches, and what each control asks a Playback to do.
//!
//! A [`PlayerView`] is one Playback as the system's controls show it. The [`Board`] keeps
//! every one that is active and remembers which was touched last, so a hardware key reaches
//! the thing a person last listened to or watched rather than a stale window's. Every control
//! becomes a [`Command`] the caller runs as the Playback's own action, and the command names
//! only that action: nothing here reaches a decoder or an app.

use std::collections::HashMap;

/// Whether a Playback stands, plays, or has nothing at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Stopped,
    Playing,
    Paused,
}

impl Status {
    /// The word MPRIS's `PlaybackStatus` reads.
    #[must_use]
    pub fn as_mpris(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Playing => "Playing",
            Self::Paused => "Paused",
        }
    }
}

/// What a control asks one Playback to do: the same actions a person or an agent calls on the
/// Playback resource.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    /// Play if paused, pause if playing.
    Toggle,
    Stop,
    Next,
    Previous,
    /// Move by this many microseconds, positive forward.
    SeekBy(i64),
    /// Move to this position, microseconds from the start.
    SeekTo(i64),
    /// Set the volume, `0.0..=1.0`.
    SetVolume(f64),
    /// Silence, or bring back what was silent.
    Mute,
}

/// A hardware media key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    PlayPause,
    Next,
    Previous,
    VolumeUp,
    VolumeDown,
    Mute,
}

/// How far one volume key moves the level.
const VOLUME_STEP: f64 = 0.05;

/// One Playback as the system's media controls show it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "what a Playback can do now, as MPRIS states it: independent flags"
)]
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerView {
    /// The Playback's stable id: what a command comes back naming.
    pub id: String,
    /// The app playing, as the desktop's widget shows it ("Video", "Music").
    pub identity: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Where its artwork is, or empty.
    pub art_url: String,
    pub status: Status,
    /// Where the playhead stood when this was read, microseconds.
    pub position_us: i64,
    /// How long what plays runs, microseconds; zero when it does not say or has no end.
    pub length_us: i64,
    pub volume: f64,
    pub rate: f64,
    pub can_next: bool,
    pub can_prev: bool,
    pub can_seek: bool,
    pub can_play: bool,
    pub can_pause: bool,
}

impl Default for PlayerView {
    fn default() -> Self {
        Self {
            id: String::new(),
            identity: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            art_url: String::new(),
            status: Status::Stopped,
            position_us: 0,
            length_us: 0,
            volume: 0.8,
            rate: 1.0,
            can_next: false,
            can_prev: false,
            can_seek: false,
            can_play: true,
            can_pause: true,
        }
    }
}

impl PlayerView {
    /// Whether two views differ in a field an MPRIS client reads as metadata.
    #[must_use]
    pub fn metadata_differs(&self, other: &Self) -> bool {
        self.id != other.id
            || self.title != other.title
            || self.artist != other.artist
            || self.album != other.album
            || self.art_url != other.art_url
            || self.length_us != other.length_us
    }
}

/// What happened to one Playback between two reads of the board.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// It became active: its player appears at the desktop.
    Started,
    /// It moved: its properties are published again.
    Updated,
    /// It stopped or went away: its player leaves the desktop.
    Stopped,
}

/// One Playback's change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub id: String,
}

/// Every Playback that is active, and which was touched last.
#[derive(Default)]
pub struct Board {
    players: Vec<PlayerView>,
    /// Ids by how recently a person listened to or watched them, most recent first.
    recent: Vec<String>,
    /// The last view kept for each id, so a change is read against what was shown.
    last: HashMap<String, PlayerView>,
}

impl Board {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The Playbacks active now, in the order the app named them.
    #[must_use]
    pub fn players(&self) -> &[PlayerView] {
        &self.players
    }

    /// What changed since the last read, and which Playback it is about.
    pub fn update(&mut self, next: Vec<PlayerView>) -> Vec<Change> {
        let mut changes = Vec::new();
        let mut now: HashMap<String, PlayerView> = HashMap::with_capacity(next.len());
        for view in &next {
            now.insert(view.id.clone(), view.clone());
        }
        // What went away.
        for (id, before) in &self.last {
            if now.contains_key(id) {
                continue;
            }
            if before.status != Status::Stopped {
                changes.push(Change {
                    kind: ChangeKind::Stopped,
                    id: id.clone(),
                });
            }
        }
        for view in &next {
            match self.last.get(&view.id) {
                None => changes.push(Change {
                    kind: ChangeKind::Started,
                    id: view.id.clone(),
                }),
                Some(before) if before != view => changes.push(Change {
                    kind: ChangeKind::Updated,
                    id: view.id.clone(),
                }),
                Some(_) => {}
            }
            // What a person is on now is the one that began playing, moved to another item, or
            // jumped back (a seek to the start): a bare key reaches it.
            let before = self.last.get(&view.id);
            let became_playing = view.status == Status::Playing
                && !before.is_some_and(|before| before.status == Status::Playing);
            let another_item = before.is_some_and(|before| {
                before.title != view.title
                    || before.artist != view.artist
                    || before.album != view.album
            });
            let jumped_back = before.is_some_and(|before| {
                view.position_us.saturating_add(1_000_000) < before.position_us
            });
            if became_playing || another_item || jumped_back {
                Self::touch(&mut self.recent, &view.id);
            }
        }
        self.recent.retain(|id| now.contains_key(id));
        self.last = now;
        self.players = next;
        changes
    }

    /// Move `id` to the front of the recency list.
    fn touch(recent: &mut Vec<String>, id: &str) {
        recent.retain(|held| held != id);
        recent.insert(0, id.to_owned());
    }

    /// The Playback a key reaches with nothing else to go on: the most recently touched one
    /// still active, else the first.
    #[must_use]
    pub fn most_recent(&self) -> Option<&PlayerView> {
        let id = self
            .recent
            .iter()
            .find(|id| self.players.iter().any(|view| &view.id == *id))
            .or_else(|| self.players.first().map(|view| &view.id))?;
        self.players.iter().find(|view| &view.id == id)
    }

    /// A key pressed, as the Playback it reaches and what it asks: the most recently touched
    /// player that can take it. A key with nowhere to go reaches none.
    pub fn press(&mut self, key: Key) -> Option<(String, Command)> {
        let target = match key {
            Key::Next => self.most_recent_that(|view| view.can_next),
            Key::Previous => self.most_recent_that(|view| view.can_prev),
            _ => self.most_recent().cloned(),
        }?;
        let command = match key {
            Key::PlayPause => Command::Toggle,
            Key::Next => Command::Next,
            Key::Previous => Command::Previous,
            Key::Mute => Command::Mute,
            Key::VolumeUp | Key::VolumeDown => {
                let step = if key == Key::VolumeUp {
                    VOLUME_STEP
                } else {
                    -VOLUME_STEP
                };
                Command::SetVolume((target.volume + step).clamp(0.0, 1.0))
            }
        };
        self.note_command(&target.id, command);
        Some((target.id, command))
    }

    /// A control asked on `id`'s player, as what it may do to the Playback.
    ///
    /// The view is moved at once so a desktop that reads straight back sees what it asked:
    /// the Playback's own action is the truth, and the next read corrects anything here.
    pub fn note_command(&mut self, id: &str, command: Command) {
        Self::touch(&mut self.recent, id);
        if let Some(view) = self.players.iter_mut().find(|view| view.id == id) {
            match command {
                Command::SetVolume(level) => view.volume = level.clamp(0.0, 1.0),
                Command::Mute => view.volume = 0.0,
                _ => {}
            }
        }
        if let Some(before) = self.last.get_mut(id)
            && let Some(view) = self.players.iter().find(|view| view.id == id)
        {
            *before = view.clone();
        }
    }

    /// The most recently touched active Playback that `passes`.
    fn most_recent_that(&self, passes: impl Fn(&PlayerView) -> bool) -> Option<PlayerView> {
        let ordered = self
            .recent
            .iter()
            .filter_map(|id| self.players.iter().find(|view| &view.id == id))
            .chain(self.players.iter());
        let mut seen = Vec::new();
        for view in ordered {
            if seen.contains(&view.id) {
                continue;
            }
            seen.push(view.id.clone());
            if passes(view) {
                return Some(view.clone());
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing(id: &str) -> PlayerView {
        PlayerView {
            id: id.to_owned(),
            identity: id.to_owned(),
            title: format!("{id}'s item"),
            status: Status::Playing,
            can_next: true,
            can_prev: true,
            ..PlayerView::default()
        }
    }

    #[test]
    fn a_started_player_appears_and_a_stopped_one_leaves() {
        let mut board = Board::new();
        let changes = board.update(vec![playing("a")]);
        assert_eq!(
            changes,
            vec![Change {
                kind: ChangeKind::Started,
                id: "a".into()
            }]
        );
        let changes = board.update(vec![]);
        assert_eq!(
            changes,
            vec![Change {
                kind: ChangeKind::Stopped,
                id: "a".into()
            }]
        );
    }

    #[test]
    fn a_media_key_reaches_the_player_touched_last() {
        let mut board = Board::new();
        board.update(vec![playing("music"), playing("video")]);
        // Both start playing in one read: the later one is where the person is.
        assert_eq!(
            board.most_recent().map(|view| view.id.as_str()),
            Some("video")
        );
        // Music starts a new item: it is what a person is on now.
        let mut music = playing("music");
        music.title = "another".into();
        music.position_us = 0;
        board.update(vec![music, playing("video")]);
        assert_eq!(
            board.most_recent().map(|view| view.id.as_str()),
            Some("music")
        );
        assert_eq!(
            board.press(Key::PlayPause),
            Some(("music".into(), Command::Toggle))
        );
    }

    #[test]
    fn a_skipping_key_reaches_a_player_that_can_skip() {
        let mut board = Board::new();
        let mut video = playing("video");
        video.can_next = false;
        board.update(vec![playing("music"), video]);
        // Video was touched last but cannot skip, so Next reaches Music.
        assert_eq!(
            board.press(Key::Next),
            Some(("music".into(), Command::Next))
        );
        assert_eq!(
            board.press(Key::Previous),
            Some(("music".into(), Command::Previous))
        );
    }

    #[test]
    fn the_volume_key_moves_the_most_recent_and_says_the_level() {
        let mut board = Board::new();
        let mut music = playing("music");
        music.volume = 0.5;
        board.update(vec![music]);
        let level = |board: &mut Board| match board.press(Key::VolumeUp) {
            Some((_, Command::SetVolume(level))) => level,
            other => panic!("the volume key reached {other:?}"),
        };
        assert!((level(&mut board) - 0.55).abs() < 1e-9);
        // The view moved too, so the next press builds on it.
        assert!((level(&mut board) - 0.6).abs() < 1e-9);
        assert!(
            (board.players()[0].volume - 0.6).abs() < 1e-9,
            "the player shows the level it was asked to"
        );
    }

    #[test]
    fn a_key_with_nothing_playing_reaches_none() {
        let mut board = Board::new();
        assert_eq!(board.press(Key::PlayPause), None);
    }
}
