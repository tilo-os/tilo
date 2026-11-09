//! Which MCP client a run or a grant comes from, as the name it goes by
//! says: the front door names a run "<client> #<number>" and a grant by the
//! client's own name.
//!
//! The name a client gave the door is all the history and the door carry, so
//! this is where the app reads it once: the filter groups the runs by it, and
//! its faces wear the hue that tells one client from another.

use ui::tokens::PresenceHue;

/// An MCP client, by the name it gave the door when it connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Client(String);

/// The client `name` comes from: a run is named after its client and a
/// number of the person's runs ("Claude Code #3"); a grant carries the
/// client's own name.
pub(crate) fn of(name: &str) -> Client {
    let stem = name
        .rsplit_once(' ')
        .filter(|(_, number)| {
            number.strip_prefix('#').is_some_and(|digits| {
                !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
            })
        })
        .map_or(name, |(stem, _)| stem);
    Client(stem.trim().to_owned())
}

impl Client {
    /// Its name, as it gave it: what the filter narrows to.
    pub(crate) fn name(&self) -> &str {
        &self.0
    }

    /// The hue its faces wear, so a run reads as its client's at a glance:
    /// warm for Claude Code, cool for Codex, and none for a client this
    /// build does not know, which keeps the plain agent square.
    pub(crate) fn hue(&self) -> Option<PresenceHue> {
        let name = self.0.to_ascii_lowercase();
        if name.contains("claude") {
            Some(PresenceHue::Orange)
        } else if name.contains("codex") {
            Some(PresenceHue::Sky)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::of;

    #[test]
    fn a_run_is_its_clients_and_a_grant_is_the_client_itself() {
        assert_eq!(of("Claude Code #3").name(), "Claude Code");
        assert_eq!(of("Codex #12").name(), "Codex");
        assert_eq!(of("Claude Code").name(), "Claude Code");
        // A client whose own name ends in a number keeps it: only a run's
        // "#<digits>" suffix names a number of runs.
        assert_eq!(of("Cursor 2").name(), "Cursor 2");
        assert_eq!(of("Cursor #2 beta").name(), "Cursor #2 beta");
    }

    #[test]
    fn a_client_is_tinted_when_its_name_says_which_it_is() {
        assert!(of("Claude Code #1").hue().is_some());
        assert!(of("codex").hue().is_some());
        assert_eq!(of("Cursor").hue(), None);
        assert_ne!(of("Claude Code").hue(), of("Codex").hue());
    }
}
