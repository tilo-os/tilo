//! Which MCP client a run or a grant comes from, as the name it goes by
//! says: the front door names a run "<client> #<number>" and a grant by the
//! client's own name.
//!
//! The name a client gave the door is all the history and the door carry, so
//! this is where the app reads it once: the filter groups the runs by it, and
//! its faces wear the hue that tells one client from another.

/// An MCP client, by the name it gave the door when it connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Client(String);

/// The client `name` comes from: a run is named after its client and a
/// number of the person's runs ("Claude Code #3"); a grant carries the
/// client's own name.
pub(crate) fn of(name: &str) -> Client {
    Client(ui::presence::agent_client(name).to_owned())
}

impl Client {
    /// Its name, as it gave it: what the filter narrows to.
    pub(crate) fn name(&self) -> &str {
        &self.0
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
}
