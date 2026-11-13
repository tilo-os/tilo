//! What an actor's step is, as data: which kind of work it does, and on what.
//!
//! One vocabulary for every surface that says what a step is at — the line
//! beside an actor's face, the people list, the card drawn in the window the
//! step happens in, the words a reviewer reads on a run. A surface asks here
//! which kind of work the step is, and says it in the reader's own language:
//! a line is a catalogue message per kind, with the record or item it acts in
//! as its argument ("Writing in Trip plan", "Schreibt in Trip plan"), so a
//! language whose word order differs reads its own sentence rather than words
//! assembled in English.
//!
//! The kind is the action's own declaration: what its app says it does
//! ([`StepKind`] on its action), else the part of the record the invocation
//! names ([`StepKind::of_anchor`]), else the declared operation of the change
//! itself ([`StepKind::of_edit`]). Nothing here reads an English verb out of a
//! label: an action whose declaration names no kind is [`StepKind::Work`],
//! which says the record it works in and never invents a verb.

use serde::{Deserialize, Serialize};

use crate::entry::{Did, EditOp};
use crate::interaction::{InteractionAnchor, SemanticInteraction};
use crate::key::CREATE_ACTION;

/// The kind of work a step does, as its action declares it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepKind {
    /// It makes something new in the record.
    Create,
    /// It writes words into the record, at a place in it.
    Write,
    /// It changes what the record or one of its items says.
    Edit,
    /// It takes something away.
    Delete,
    /// It moves something elsewhere.
    Move,
    /// It only reads.
    Read,
    /// It runs something: a command, a program, a job.
    Run,
    /// Its declaration names no kind of work of its own.
    #[default]
    Work,
}

impl StepKind {
    /// The word a declaration names it by (`#[action(step = "create")]`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::Move => "move",
            Self::Read => "read",
            Self::Run => "run",
            Self::Work => "work",
        }
    }

    /// The kind `word` names.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        [
            Self::Create,
            Self::Write,
            Self::Edit,
            Self::Delete,
            Self::Move,
            Self::Read,
            Self::Run,
            Self::Work,
        ]
        .into_iter()
        .find(|kind| kind.word() == word)
    }

    /// The kind of step the change `op` is, as the change itself declares it:
    /// words written, an item added, something removed or moved.
    #[must_use]
    pub const fn of_edit(op: EditOp) -> Self {
        match op {
            EditOp::Splice | EditOp::Replace | EditOp::Mark => Self::Write,
            EditOp::Add
            | EditOp::Insert
            | EditOp::InsertRow
            | EditOp::InsertColumn
            | EditOp::React => Self::Create,
            EditOp::Remove
            | EditOp::RemoveRow
            | EditOp::RemoveColumn
            | EditOp::Unreact
            | EditOp::Forget => Self::Delete,
            EditOp::MoveTo => Self::Move,
            EditOp::Set | EditOp::SetCell => Self::Edit,
            EditOp::Settle => Self::Work,
        }
    }

    /// The kind of step an invocation is, from the part of the record it says
    /// it acts on: the characters of a text are words written into it, one of
    /// its items is that item changed, and the record as a whole names no kind
    /// of its own.
    #[must_use]
    pub const fn of_anchor(anchor: &InteractionAnchor) -> Option<Self> {
        match anchor {
            InteractionAnchor::Text { .. } | InteractionAnchor::Grid { .. } => Some(Self::Write),
            InteractionAnchor::ListItems { .. }
            | InteractionAnchor::Objects { .. }
            | InteractionAnchor::SemanticControl { .. } => Some(Self::Edit),
            // A timeline interval is a range of the record changed as a whole,
            // and a step on the record itself names no part to read a kind
            // from.
            InteractionAnchor::Timeline { .. } | InteractionAnchor::Resource => None,
        }
    }

    /// The kind of step a committed change is, from what it declares: a field
    /// edit's operation, a verb's own declaration ([`CREATE_ACTION`] makes
    /// something, a command parameter runs something), or a group naming what
    /// its members share. `None` for a step whose declaration names no kind of
    /// its own.
    #[must_use]
    pub fn of_did(what: &Did) -> Option<Self> {
        match what {
            Did::Edit(edit) => Some(Self::of_edit(edit.op)),
            Did::Verb(verb) => {
                if verb.action.action == CREATE_ACTION {
                    return Some(Self::Create);
                }
                // A parameter named for the command it runs: what the action
                // declared its input to be.
                verb.args
                    .keys()
                    .any(|name| COMMAND_PARAMS.contains(&name.as_str()))
                    .then_some(Self::Run)
            }
            Did::Group(group) => group.shared().and_then(Self::of_did),
            Did::History(_) => None,
        }
    }
}

/// The kind of step an invocation is: the kind its action declared, else the
/// part of the record it names ([`StepKind::of_anchor`]), else
/// [`StepKind::Work`].
#[must_use]
pub fn kind(declared: Option<StepKind>, anchor: &InteractionAnchor) -> StepKind {
    declared
        .or_else(|| StepKind::of_anchor(anchor))
        .unwrap_or(StepKind::Work)
}

/// One step, as every surface says it: which kind of work it is, and the one
/// name the line carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step<'a> {
    pub kind: StepKind,
    /// The item's title where the step names one, else the record's name as
    /// its app shows it ("Trip plan").
    pub record: Option<&'a str>,
    /// The app the record belongs to, said where a step makes something new
    /// in it and no title names what yet ("Tasks").
    pub app: Option<&'a str>,
    /// What a run runs, as the caller passed it.
    pub command: Option<&'a str>,
}

impl<'a> Step<'a> {
    /// A step of `kind` on `record`.
    #[must_use]
    pub fn new(kind: StepKind, record: Option<&'a str>) -> Self {
        Self {
            kind,
            record: nonempty(record),
            app: None,
            command: None,
        }
    }

    /// The same step, said in `app` where no record names it.
    #[must_use]
    pub fn in_app(mut self, app: Option<&'a str>) -> Self {
        self.app = nonempty(app);
        self
    }

    /// The same step, running `command`.
    #[must_use]
    pub fn running(mut self, command: Option<&'a str>) -> Self {
        self.command = nonempty(command);
        self
    }

    /// The step `interaction` is: its action's declared kind, else the part of
    /// its record the invocation names, on the item's title or the record's
    /// name.
    #[must_use]
    pub fn of_interaction(interaction: &'a SemanticInteraction) -> Step<'a> {
        Self {
            kind: kind(interaction.action.step, &interaction.anchor),
            record: nonempty(Some(interaction.resource.label.as_str())),
            app: nonempty(
                interaction
                    .action
                    .app
                    .as_ref()
                    .map(|app| app.label.as_str()),
            ),
            command: None,
        }
    }

    /// The catalogue message this step is said in, and its one argument: the
    /// name the line carries, and whether it is the record or what a run runs.
    /// A kind whose argument the step does not know says
    /// [`WORKING_ALONE`](self) instead of a sentence with a hole in it.
    #[must_use]
    pub fn message(&self) -> (&'static str, Option<(&'static str, &'a str)>) {
        fn named<'n>(
            key: &'static str,
            name: &'n str,
        ) -> (&'static str, Option<(&'static str, &'n str)>) {
            (key, Some(("record", name)))
        }
        match self.kind {
            StepKind::Create => match (self.record, self.app) {
                (Some(record), _) => named(CREATING, record),
                (None, Some(app)) => named(CREATING, app),
                (None, None) => (WORKING_ALONE, None),
            },
            StepKind::Write => self
                .record
                .map_or((WORKING_ALONE, None), |record| named(WRITING, record)),
            StepKind::Edit => self
                .record
                .map_or((WORKING_ALONE, None), |record| named(EDITING, record)),
            StepKind::Delete => self
                .record
                .map_or((WORKING_ALONE, None), |record| named(DELETING, record)),
            StepKind::Move => self
                .record
                .map_or((WORKING_ALONE, None), |record| named(MOVING, record)),
            StepKind::Read => self
                .record
                .map_or((WORKING_ALONE, None), |record| named(READING, record)),
            StepKind::Run => match (self.command, self.record) {
                (Some(command), _) => (RUNNING, Some(("command", command))),
                (None, Some(record)) => named(RUNNING_IN, record),
                (None, None) => (WORKING_ALONE, None),
            },
            StepKind::Work => self
                .record
                .map_or((WORKING_ALONE, None), |record| named(WORKING, record)),
        }
    }
}

/// The parameter names a verb declares for the command it runs: a verb
/// taking one is a step of [`StepKind::Run`], and the command is what a
/// person reads.
pub const COMMAND_PARAMS: &[&str] = &["command", "command_line", "cmd"];

/// A step that makes something new in the record or the app it names.
pub const CREATING: &str = "step-creating";
/// A step that writes words into the record.
pub const WRITING: &str = "step-writing";
/// A step that changes what the record or one of its items says.
pub const EDITING: &str = "step-editing";
/// A step that takes something away.
pub const DELETING: &str = "step-deleting";
/// A step that moves something elsewhere.
pub const MOVING: &str = "step-moving";
/// A step that only reads.
pub const READING: &str = "step-reading";
/// A step that runs a command.
pub const RUNNING: &str = "step-running";
/// A step that runs something in the record it names.
pub const RUNNING_IN: &str = "step-running-in";
/// A step of work its declaration names no kind of.
pub const WORKING: &str = "step-working";
/// The same where the step names nothing at all.
pub const WORKING_ALONE: &str = "step-working-alone";

/// `name` trimmed, unless that leaves nothing to say.
fn nonempty(name: Option<&str>) -> Option<&str> {
    name.map(str::trim).filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test the unwrap is the assertion"
    )]

    use super::*;
    use crate::interaction::{InteractionAction, InteractionActor, InteractionTextRange};
    use crate::{ActionApp, ActionResource, ActorKind, InteractionPhase, InvocationId};

    fn interaction(
        step: Option<StepKind>,
        anchor: InteractionAnchor,
        record: &str,
    ) -> SemanticInteraction {
        SemanticInteraction {
            version: crate::INTERACTION_VERSION,
            invocation: InvocationId(uuid::Uuid::from_u128(1)),
            sequence: 1,
            actor: InteractionActor::new("quill", ActorKind::Agent),
            phase: InteractionPhase::Applying,
            action: InteractionAction {
                id: "create".into(),
                label: "Create an issue".into(),
                app: Some(ActionApp {
                    id: "so.tilo.tasks".into(),
                    label: "Tasks".into(),
                }),
                step,
            },
            resource: ActionResource {
                id: "tasks.tracker".into(),
                kind: "tasks.tracker".into(),
                label: record.into(),
            },
            anchor,
            group: None,
        }
    }

    /// A step's kind is its action's own declaration where it names one, and
    /// the part of the record it acts on where it does not.
    #[test]
    fn a_steps_kind_is_declared_else_read_from_the_part_it_acts_on() {
        let text = InteractionAnchor::Text {
            region: "text".into(),
            ranges: vec![InteractionTextRange {
                block: "note".into(),
                start: 0,
                end: 0,
            }],
        };
        let created = interaction(Some(StepKind::Create), InteractionAnchor::Resource, "Tasks");
        assert_eq!(Step::of_interaction(&created).kind, StepKind::Create);
        let written = interaction(None, text, "Trip plan");
        let writing = Step::of_interaction(&written);
        assert_eq!(writing.kind, StepKind::Write);
        assert_eq!(writing.message(), (WRITING, Some(("record", "Trip plan"))));
        let items = InteractionAnchor::ListItems {
            ids: vec!["issue-1".into()],
        };
        assert_eq!(
            Step::of_interaction(&interaction(None, items, "Book the ferry")).kind,
            StepKind::Edit,
            "an item's own step changes that item"
        );
        assert_eq!(
            Step::of_interaction(&interaction(None, InteractionAnchor::Resource, "Issues")).kind,
            StepKind::Work
        );
    }

    /// A declared kind says the record, and the app where nothing is named
    /// yet; a kind with no name says only what it is.
    #[test]
    fn a_step_names_its_record_or_says_only_what_it_is() {
        let made = interaction(
            Some(StepKind::Create),
            InteractionAnchor::Resource,
            "Issues",
        );
        let created = Step::of_interaction(&made).in_app(Some(" Tasks "));
        assert_eq!(created.message(), (CREATING, Some(("record", "Issues"))));
        assert_eq!(
            Step::new(StepKind::Create, None)
                .in_app(Some("Tasks"))
                .message(),
            (CREATING, Some(("record", "Tasks")))
        );
        assert_eq!(
            Step::new(StepKind::Delete, None).message(),
            (WORKING_ALONE, None)
        );
        assert_eq!(
            Step::new(StepKind::Run, Some("Terminal")).running(Some("cargo build")),
            Step {
                kind: StepKind::Run,
                record: Some("Terminal"),
                app: None,
                command: Some("cargo build"),
            }
        );
        assert_eq!(
            Step::new(StepKind::Run, Some("Terminal")).message(),
            (RUNNING_IN, Some(("record", "Terminal")))
        );
    }

    /// A committed change names its kind by what it declared: the operation of
    /// a field edit, the platform's make, the command a verb runs.
    #[test]
    fn a_committed_change_names_its_kind_as_it_declared_it() {
        for (op, kind) in [
            (EditOp::Splice, StepKind::Write),
            (EditOp::Replace, StepKind::Write),
            (EditOp::Insert, StepKind::Create),
            (EditOp::Remove, StepKind::Delete),
            (EditOp::MoveTo, StepKind::Move),
            (EditOp::Set, StepKind::Edit),
        ] {
            assert_eq!(StepKind::of_edit(op), kind, "{op:?}");
        }
        let made = Did::verb(
            crate::ActionKey::new("so.tilo.notes", "notes.note", CREATE_ACTION),
            std::iter::empty::<(String, crate::Scalar)>(),
        );
        assert_eq!(StepKind::of_did(&made), Some(StepKind::Create));
        let command = Did::verb(
            crate::ActionKey::new("so.tilo.terminal", "terminal.session", "run"),
            [(
                "command".to_owned(),
                crate::Scalar::from_json(&serde_json::json!("cargo build")).unwrap(),
            )],
        );
        assert_eq!(StepKind::of_did(&command), Some(StepKind::Run));
        let renamed = Did::verb(
            crate::ActionKey::new("so.tilo.notes", "notes.note", "rename"),
            std::iter::empty::<(String, crate::Scalar)>(),
        );
        assert_eq!(StepKind::of_did(&renamed), None);
    }

    #[test]
    fn a_kind_reads_back_from_its_word() {
        for kind in [
            StepKind::Create,
            StepKind::Write,
            StepKind::Edit,
            StepKind::Delete,
            StepKind::Move,
            StepKind::Read,
            StepKind::Run,
            StepKind::Work,
        ] {
            assert_eq!(StepKind::of_word(kind.word()), Some(kind));
        }
        assert_eq!(StepKind::of_word("writing"), None);
    }
}
