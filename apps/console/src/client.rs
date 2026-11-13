//! Which MCP client a run or a grant comes from, as the name it goes by says: the API
//! server names a run "<client> #<number>" and a grant by the client's own name.

/// An MCP client, by the name it gave the door when it connected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Client(String);

/// The client `name` comes from: a run is named after its client and a number of the
/// person's runs ("Claude Code #3"); a grant carries the client's own name.
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
        assert_eq!(of("Cursor 2").name(), "Cursor 2");
        assert_eq!(of("Cursor #2 beta").name(), "Cursor #2 beta");
    }
}
