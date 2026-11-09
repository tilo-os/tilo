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
//!
//! An app reaches the desk's invitation the same way, as the person using it: what it shows
//! is shared by inviting people into the workspace it is in — the desk's own invite sheet
//! ([`SHOW_INVITE`]), and the invitation it makes ([`ISSUE_INVITATION`]) — and nothing else of
//! the desk.

/// The provider the session answers its own actions under: the same name its own door
/// publishes them as, so a catalogue that names a session action names it here too.
pub const PROVIDER: &str = "so.tilo.session";

/// The resource the session publishes its desk as: the actions the person at this device takes
/// on the workspace itself — the invitation into it, the surface up, the agents working here —
/// rather than on a record of an app's.
pub const DESK_RESOURCE: &str = "session:desk";

/// The desk's action making an invitation into a workspace: the link to hand to whoever is
/// being invited, which their device joins with. An app offers inviting what it shows by asking
/// this, and gets the invitation the invite sheet makes.
pub const ISSUE_INVITATION: &str = "issue-invitation";

/// The desk's action opening its invite sheet on a workspace: where a link into it is made, the
/// part it brings someone in at picked and the people the directory knows offered. An app offers
/// sharing what it shows by asking this, so a folder's share and the bar's invite button are one
/// sheet.
pub const SHOW_INVITE: &str = "show-invite";
