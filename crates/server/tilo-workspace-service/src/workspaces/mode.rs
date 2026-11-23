//! The kill switch an operator turns on a host that has to shed load without
//! going down: the mode is one of three names written to `<state>/mode`, by
//! whoever holds the workspace's state, and read here, so a host takes one
//! without a restart.
//!
//! `normal` opens everything. `spectator-only` admits nobody new while the
//! people already in keep working and the host's public listing still answers.
//! `back-soon` pauses the host entirely: no entries, and nothing for a stranger
//! to read. An entry gates what the switch turns away: a device of someone the
//! workspace already has in it is not new and comes in as it does at any other
//! time.

use std::path::PathBuf;
use std::sync::Arc;

/// The file the operator writes the mode into, inside the state directory. It outlives a restart,
/// so the mode a host starts under is whatever the file last said.
pub const MODE_FILE: &str = "mode";
/// How long an entry the switch turned away asks to wait before it is tried again.
pub const PAUSED_RETRY_AFTER_SECS: u64 = 60;

/// How much of a host is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Everything works.
    #[default]
    Normal,
    /// Nobody new becomes a member, so a flood of vandals or a moderation emergency stops at
    /// once, while the people in the host keep working and its public listing still answers.
    SpectatorOnly,
    /// The host is paused entirely: no entries and no public listing.
    BackSoon,
}

impl Mode {
    /// Every mode, normal first.
    pub const ALL: [Self; 3] = [Self::Normal, Self::SpectatorOnly, Self::BackSoon];

    /// The name the operator writes and `/mode` answers.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::SpectatorOnly => "spectator-only",
            Self::BackSoon => "back-soon",
        }
    }

    /// The mode `text` names, or `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == text.trim())
    }

    /// Whether someone may join the host: come in as a member, or be admitted.
    #[must_use]
    pub const fn allows_joins(self) -> bool {
        matches!(self, Self::Normal)
    }

    /// Whether someone may watch what the host serves without joining it: its public listing,
    /// which any page reads.
    #[must_use]
    pub const fn allows_spectators(self) -> bool {
        !matches!(self, Self::BackSoon)
    }
}

/// The mode a host runs under: the one it started with, and the one an operator set since.
#[derive(Debug)]
pub struct HostMode {
    state: PathBuf,
    initial: Mode,
}

impl HostMode {
    /// The mode `state`'s file names, or `initial` while it names none.
    #[must_use]
    pub fn new(state: PathBuf, initial: Mode) -> Self {
        Self { state, initial }
    }

    /// [`HostMode::new`] shared between the host that reports the mode and the workspaces it
    /// serves, which read it as they admit.
    #[must_use]
    pub fn held(state: PathBuf, initial: Mode) -> Arc<Self> {
        Arc::new(Self::new(state, initial))
    }

    /// What the operator set: the file's mode now, or the one the host started with.
    #[must_use]
    pub fn current(&self) -> Mode {
        std::fs::read_to_string(self.state.join(MODE_FILE))
            .ok()
            .and_then(|text| Mode::parse(&text))
            .unwrap_or(self.initial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_names_the_mode_and_only_the_three_names_are_modes() {
        let dir = tempfile::tempdir().unwrap();
        let mode = HostMode::new(dir.path().to_owned(), Mode::Normal);
        assert_eq!(mode.current(), Mode::Normal);
        for named in Mode::ALL {
            std::fs::write(dir.path().join(MODE_FILE), named.as_str()).unwrap();
            assert_eq!(mode.current(), named, "{}", named.as_str());
        }
        std::fs::write(dir.path().join(MODE_FILE), "nonsense").unwrap();
        assert_eq!(
            mode.current(),
            Mode::Normal,
            "a file naming no mode is the one the host started with"
        );
        assert_eq!(Mode::parse(" back-soon\n"), Some(Mode::BackSoon));
        assert!(!Mode::SpectatorOnly.allows_joins());
        assert!(Mode::SpectatorOnly.allows_spectators());
        assert!(!Mode::BackSoon.allows_joins());
        assert!(!Mode::BackSoon.allows_spectators());
        assert!(Mode::Normal.allows_joins() && Mode::Normal.allows_spectators());
    }
}
