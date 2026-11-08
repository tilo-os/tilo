//! The session's own actions, offered to a turn's agent as steps of the broker
//! (`docs/designs/app-language/shell.md` §8.1, "Any action").
//!
//! The agent answering a person's turn arranges the screen as the REPL does: the
//! arrangement's `place`, `place_at` and `open`, the windows and the notices. Those are
//! published by the session on its own door, on the loop's thread, and this is the
//! provider its broker answers them under, so one catalogue names them for every caller
//! that has a broker: the agent door, a launched agent, and a task's program alike.
//!
//! Each step is decided where it lands: the broker admits it as the turn's step (the
//! person's grants, the agent's, the issue's), and the session carries it out on the loop's
//! pass, attributed to the agent and the person who asked, as a change the person takes
//! back like any other.

/// The provider the session answers its own actions under: the same name its own door
/// publishes them as, so a catalogue that names a session action names it here too.
pub const PROVIDER: &str = "so.tilo.session";
