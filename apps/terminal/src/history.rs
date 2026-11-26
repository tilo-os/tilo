//! The workspace's command history across sessions (docs/app-model/ush.md, "Search and
//! history").

use ui::tilo_resource::{
    self, ActionCx, Atom, Collection, Id, JsonSchema, Model, Refusal, resource,
};

pub const MAX_REMEMBERED: usize = 10_000;
/// The history's record, named by its kind, which binds the id to it: the kind is sealed
/// (`[resources] sealed`), so the record has keys of its own from its first write.
pub(crate) const DOCUMENT: &str = "derived:terminal.history@tab:0000000000000000";
#[cfg(feature = "desktop")]
pub(crate) type HistoryRecord = ui::framework::WorkspaceResource<CommandHistory>;

#[cfg(feature = "desktop")]
pub(crate) fn is_device_shell(target: Option<&str>) -> bool {
    matches!(
        target.and_then(crate::target::SessionTarget::decode),
        Some(crate::target::SessionTarget::Host | crate::target::SessionTarget::LinuxVm { .. })
    )
}

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct CommandHistory {
    /// Runs, oldest first. Only the sessions that ran them add them.
    #[model(readonly)]
    pub remembered: Collection<Remembered>,
}

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Remembered {
    /// Command
    #[model(label)]
    pub command: Atom,
    /// Working directory
    pub cwd: Atom,
    /// Where it ran: the session's encoded target.
    pub environment: Option<Atom>,
    /// Exit status
    pub exit: i32,
    /// Ended, in milliseconds since the Unix epoch.
    pub ended_ms: i64,
    /// Who ran it.
    pub actor: Option<ui::tilo_resource::MemberId>,
    /// The session that ran it: its resource's address.
    pub session: Atom,
    /// The run, an item of the session's `runs`.
    pub run: Id,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, JsonSchema)]
pub struct Ran {
    pub command: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub exit: i32,
    pub ended_ms: i64,
    pub actor: Option<ui::tilo_resource::MemberId>,
    pub session: String,
    pub run: Id,
}

impl CommandHistory {
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "terminal.history",
    read_content = CommandHistory::read_document
)]
impl CommandHistory {
    #[action(housekeeping)]
    pub fn remember(
        &mut self,
        cx: &mut ActionCx,
        #[param("The run, as its session recorded it", flatten)] ran: Ran,
    ) -> Result<(), Refusal> {
        self.keep(cx, ran, false)
    }

    /// Place an older workspace run in its chronological position.
    #[action(external, writes = remembered)]
    pub fn import_run(
        &mut self,
        cx: &mut ActionCx,
        #[param("The previous run", flatten)] ran: Ran,
    ) -> Result<(), Refusal> {
        self.keep(cx, ran, true)
    }

    pub fn matching<'a>(&'a self, text: &'a str) -> impl Iterator<Item = &'a Remembered> {
        self.remembered
            .values()
            .rev()
            .filter(move |run| run.command.0.contains(text))
    }

    fn keep(&mut self, cx: &mut ActionCx, ran: Ran, historical: bool) -> Result<(), Refusal> {
        if ran.command.trim().is_empty() || ran.command.len() > crate::resource::MAX_COMMAND_BYTES {
            return Err(Refusal::new("a run names its command line"));
        }
        if self
            .remembered
            .values()
            .any(|remembered| remembered.run == ran.run)
        {
            return Ok(());
        }
        let len = self.remembered.len();
        let position = if historical {
            self.remembered
                .values()
                .position(|run| run.ended_ms > ran.ended_ms)
                .unwrap_or(len)
        } else {
            len
        };
        if position == 0 && len >= MAX_REMEMBERED {
            return Ok(());
        }
        self.remembered.insert(
            position,
            cx.id(),
            Remembered {
                command: Atom(ran.command),
                cwd: Atom(ran.cwd),
                environment: ran.environment.map(Atom),
                exit: ran.exit,
                ended_ms: ran.ended_ms,
                actor: ran.actor,
                session: Atom(ran.session),
                run: ran.run,
            },
        );
        while self.remembered.len() > MAX_REMEMBERED {
            let Some(oldest) = self.remembered.ids().next().cloned() else {
                break;
            };
            self.remembered.remove(&oldest);
        }
        Ok(())
    }
}
