//! What a workspace's shell is asked and answers: a command line run in a Terminal session, a
//! session's shell opened on a terminal for one device, and who a command runs as.

use std::sync::Arc;

use tilo_action_services::environment_commands::{CommandOutput, Person};
use tilo_action_services::server_account::Refusal;
use tilo_actions::remote::Caller;

/// What the caller of a command sees of it while it runs, and how it stops it.
#[derive(Clone, Copy, Default)]
pub struct Watching<'a> {
    /// What a command printed so far, each time that grew.
    pub seen: Option<&'a (dyn Fn(&CommandOutput) + Sync)>,
    /// Whether the caller asked the command to stop.
    pub stop: Option<&'a (dyn Fn() -> bool + Sync)>,
}

/// Where a host's workspaces run their shells.
pub trait WorkspaceShell: Send + Sync {
    /// Run `command` in `workspace`'s Terminal session `session` for `by`, who asked; `agent`
    /// when an agent asked, whose output is rendered for it and whose
    /// refused program is not waited for. `person` is who it runs as, whose
    /// name and address the workspace's VM programs record as the author of
    /// what they make. `watching` shows the caller what the command prints
    /// while it runs, and stops it when the caller asks.
    ///
    /// # Errors
    ///
    /// The session's shell could not be started.
    fn run(
        &self,
        workspace: &str,
        session: &str,
        command: &str,
        by: &Caller,
        agent: bool,
        person: Option<&Person>,
        watching: Watching<'_>,
    ) -> Result<ShellRan, String>;

    /// `caller` was let run commands in `workspace`'s session `session`: the person its
    /// programs' agents ask whether they may work in the workspace.
    fn started(&self, workspace: &str, session: &str, caller: &Caller) {
        let _ = (workspace, session, caller);
    }

    /// Start `opening`'s session shell on a terminal for `by`, who asked, and hand its stream
    /// to the device it names.
    ///
    /// # Errors
    ///
    /// This host opens no terminals, the session it names to come back to is gone, or the
    /// shell could not start.
    ///
    /// Each command line the shell runs from then on goes to `record` as it ends, attached or
    /// not, when this open started the shell; an open that came back to it records nothing
    /// more, its runs already going to the first.
    fn open(&self, opening: &Opening, by: &Caller, record: Recorder) -> Result<Opened, String> {
        let _ = (opening, by, record);
        Err("this host opens no terminals".into())
    }

    /// Keep `output`, a command's output, where the workspace's devices fetch
    /// its content, answering the reference that opens it, or `None` when
    /// this host keeps none.
    fn keep(&self, workspace: &str, output: &[u8]) -> Option<String> {
        let _ = (workspace, output);
        None
    }
}

/// Who a command `caller` asks for in `workspace` runs as: an agent by its own name, else the
/// person it acts for, as `names` (the workspace's member list) calls them. `host` is the
/// address the workspace is reached by, which their address is at (`<name>@<host>`). `None`
/// when the caller is neither a person nor an agent the workspace names.
///
/// Someone the list does not name, or names with no word, still signs: as a Tilo actor, never
/// as a key, since what a commit records of its author is read by people.
#[must_use]
pub fn person_of(
    caller: &Caller,
    names: &[(tilo_actions::MemberId, String)],
    host: &str,
) -> Option<Person> {
    /// The word `names` calls the member `id`, when it calls it one.
    fn named<'a>(names: &'a [(tilo_actions::MemberId, String)], id: &str) -> Option<&'a str> {
        names
            .iter()
            .find(|(member, name)| member.id() == id && !name.trim().is_empty())
            .map(|(_, name)| name.as_str())
    }
    let acting = caller.acting();
    if let Some(agent) = acting
        .member_id()
        .and_then(tilo_actions::MemberId::agent_id)
    {
        let name = named(names, agent)
            .or_else(|| caller.agent_name().filter(|name| !name.trim().is_empty()))
            .unwrap_or("Tilo agent");
        return Some(Person::of(name, host));
    }
    let person = acting
        .person_id()
        .filter(|id| !id.is_empty())
        .or_else(|| acting.acted_for_person())?;
    let name = named(names, person).unwrap_or("Tilo member");
    Some(Person::of(name, host))
}

/// A Terminal session's shell to open on a terminal, for one device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opening {
    pub workspace: String,
    /// The session whose shell it is: the one its commands run in.
    pub session: String,
    /// The device that takes its stream: the caller's own.
    pub device: [u8; 32],
    pub cols: u16,
    pub rows: u16,
    /// Who the shell runs as: the person or agent who opened it, as the workspace names them.
    pub person: Option<Person>,
    /// The session to come back to, by the id an earlier open answered.
    pub attach: Option<String>,
}

/// What records each command line a terminal's shell ran, as it ends.
pub type Recorder = Arc<dyn Fn(ShellRan) + Send + Sync>;

/// An opened terminal: the stream's one-use id, for the device that asked, and the session's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub stream: String,
    /// 32 hex characters.
    pub session: String,
}

/// How a command line went in a host's shell.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellRan {
    pub command: String,
    pub cwd: String,
    pub exit: i32,
    pub output: Vec<u8>,
    pub truncated: bool,
    /// It was stopped before it ended: a person's Ctrl+C, or an agent's
    /// cancel of the command it asked for.
    pub cancelled: bool,
    pub started_ms: i64,
    pub ended_ms: i64,
    /// Why the host would not run a program the command needed, typed.
    pub refusal: Option<Refusal>,
}
