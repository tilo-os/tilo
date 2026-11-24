//! The session's own actions, offered to a turn's agent as steps of the broker
//! (`docs/designs/app-language/shell.md` §8.1, "Any action").
//!
//! The agent answering a person's turn arranges the screen as the REPL does: the
//! layout's `place`, `place_at` and `open`, the windows and the notices. Those are
//! published by the session on its own API, on the loop's thread, and this is the
//! provider its broker answers them under, so one catalogue names them for every caller
//! that has a broker: the MCP server, a launched agent, and a task's program alike.
//!
//! Each step is decided where it lands: the broker admits it as the turn's step (the
//! person's grants, the agent's, the issue's), and the session carries it out on the loop's
//! pass, attributed to the agent and the person who asked, as a change the person takes
//! back like any other.
//!
//! An app reaches the shell's invitation the same way, as the person using it: what it shows
//! is shared by inviting people into the workspace it is in — the shell's own invite sheet
//! ([`SHOW_INVITE`]), and the invitation it makes ([`ISSUE_INVITATION`]) — and nothing else of
//! the shell.

/// The provider the session answers its own actions under: the same name its own API
/// publishes them as, so a catalogue that names a session action names it here too.
pub const PROVIDER: &str = "so.tilo.session";

/// The resource the session publishes inviting on: the links into a workspace, the invite sheet,
/// and adding another device or an agent of the person's.
pub const INVITE_RESOURCE: &str = "session:invite";

/// The action making an invitation into a workspace: the link to hand to whoever is
/// being invited, which their device joins with. An app offers inviting what it shows by asking
/// this, and gets the invitation the invite sheet makes.
pub const ISSUE_INVITATION: &str = "issue-invitation";

/// The action opening the invite sheet on a workspace: where a link into it is made, the
/// part it brings someone in at picked and the people the directory knows offered. An app offers
/// sharing what it shows by asking this, so a folder's share and the bar's invite button are one
/// sheet.
pub const SHOW_INVITE: &str = "show-invite";

/// The action showing how a new device of the person's joins them: a desktop opens Settings'
/// devices page, where "Add a device" prints the ticket the new device logs in with. An app that
/// lists what acts as the person (Settings' Devices) offers adding to it this way.
pub const SHOW_PAIR_DEVICE: &str = "show-pair-device";

/// The resource the person's own tilolang line is run on: a line typed in a conversation, run as
/// the person at this device. What it reads and changes is theirs, and History credits them,
/// never an agent.
pub const PROGRAMS_RESOURCE: &str = "session:programs";

/// The action running one tilolang line as the invoking person: `line`, its source text. It
/// answers the program's output in the person's words, or why it failed — a parse or type error
/// with its place in the line, or the system's own word for a refusal.
pub const RUN_LINE: &str = "run-line";

/// The target of [`RUN_LINE`]: the session's own provider, on [`PROGRAMS_RESOURCE`]. A composer
/// asks it as the person typing it.
#[must_use]
pub fn run_line_target() -> super::RemoteActionTarget {
    super::RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: super::ActionAddress::new(PROGRAMS_RESOURCE, RUN_LINE),
    }
}

/// The resource the person's layout is published as: the layout itself, which a place is put
/// on, brought forward and closed through, and which carries the system's `open`.
pub const LAYOUT_RESOURCE: &str = "shell:layout";

/// The system's `open` on the layout: resource `resource` placed with its own default view
/// beside the place showing `beside` (the conversation of the turn that asked) or beside the
/// place in front when it names none (`docs/designs/app-language/one-app-model.md` §3, "`open(r)`
/// places r with its default view, beside where it was asked, for every app"). A resource's own
/// `open`, an app's `open` for an app this session serves, and this step are one act.
pub const OPEN: &str = "open";

/// The action showing how an agent or another computer of the person's connects as them:
/// the same setup their own card's "Connect an agent or computer" opens ([`CONNECT_AGENT`]). An app
/// that lists what acts as the person (Settings' Devices and access) offers adding to it this way.
pub const SHOW_CONNECT: &str = "show-connect";

/// The host action that shows how an agent or a computer connects as the person: a page shows the
/// setup its first screen shows (a `tilo login` prompt with a fresh code, Tilo's MCP address); a
/// desktop, which has no login code to hand out, opens Console's Connect place. The person's card
/// and [`SHOW_CONNECT`] both choose it.
pub const CONNECT_AGENT: &str = "connect-agent";

/// The host action that shows the link another device of the person's joins with, as a code its
/// camera reads: a page shows its own pairing sheet, with a fresh link each time it is asked. The
/// person's search and [`SHOW_PAIR_DEVICE`] both choose it.
pub const PAIR_DEVICE: &str = "pair-device";

/// The target the inviting action `action` is invoked at: the session's own provider, on
/// [`INVITE_RESOURCE`]. An app invokes one of these as the person using it, through the action
/// broker.
#[must_use]
pub fn target(action: &str) -> super::RemoteActionTarget {
    super::RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: super::ActionAddress::new(INVITE_RESOURCE, action),
    }
}

/// The resource the session publishes the agents it runs on.
pub const AGENTS_RESOURCE: &str = "session:agents";

/// The target the agents' action `action` is invoked at: the session's own provider, on
/// [`AGENTS_RESOURCE`]. An app invokes one of these as the person using it, through the action
/// API.
#[must_use]
pub fn agents_target(action: &str) -> super::RemoteActionTarget {
    super::RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: super::ActionAddress::new(AGENTS_RESOURCE, action),
    }
}
