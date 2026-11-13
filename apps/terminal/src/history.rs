//! The person's command history across every session (docs/app-model/ush.md, "Search and
//! history").

use ui::tilo_resource::{
    self, ActionCx, Atom, Collection, Id, JsonSchema, Model, Refusal, resource,
};

pub const MAX_REMEMBERED: usize = 10_000;

#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct CommandHistory {
    /// Runs, oldest first. Only the sessions that ran them add them.
    #[model(readonly)]
    pub runs: Collection<Remembered>,
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
    pub actor: Atom,
    /// The session that ran it: its record's address.
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
    pub actor: String,
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
        if ran.command.trim().is_empty() || ran.command.len() > crate::resource::MAX_COMMAND_BYTES {
            return Err(Refusal::new("a run names its command line"));
        }
        self.keep(cx, ran);
        Ok(())
    }

    pub fn matching<'a>(&'a self, text: &'a str) -> impl Iterator<Item = &'a Remembered> {
        self.runs
            .values()
            .rev()
            .filter(move |run| run.command.0.contains(text))
    }

    fn keep(&mut self, cx: &mut ActionCx, ran: Ran) {
        if self
            .runs
            .values()
            .any(|remembered| remembered.run == ran.run)
        {
            return;
        }
        while self.runs.len() >= MAX_REMEMBERED {
            let Some(oldest) = self.runs.ids().next().cloned() else {
                break;
            };
            self.runs.remove(&oldest);
        }
        self.runs.push(
            cx.id(),
            Remembered {
                command: Atom(ran.command),
                cwd: Atom(ran.cwd),
                environment: ran.environment.map(Atom),
                exit: ran.exit,
                ended_ms: ran.ended_ms,
                actor: Atom(ran.actor),
                session: Atom(ran.session),
                run: ran.run,
            },
        );
    }
}
