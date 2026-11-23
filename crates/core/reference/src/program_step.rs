//! One step of an agent's task, as everyone who follows the task sees it: the tilolang program
//! the agent ran and what it answered (docs/designs/app-language/agents.md, "A task is many
//! programs"; agent-ux.md, "Working shows work").
//!
//! The step is what the agent's task records on its issue and what the composer and Tasks draw:
//! the program as code, the resources its answer names as references that open, what it changed
//! as rows that undo, and what waits for the person as a preview to confirm. It carries no
//! authority: opening a reference, undoing a change and confirming a preview each go through the
//! ordinary paths, decided again where they land.

use serde::{Deserialize, Serialize};
use tilo_resource::JsonSchema;

use crate::ResourceAt;

/// The longest program a step keeps, in characters.
pub const MAX_PROGRAM_CHARS: usize = 4_000;
/// The most answer lines a step keeps.
pub const MAX_ANSWER_LINES: usize = 40;
/// The longest answer line a step keeps, in characters.
pub const MAX_ANSWER_LINE_CHARS: usize = 300;
/// The most references, changes or previews a step keeps, each.
pub const MAX_STEP_ITEMS: usize = 24;

/// How a step's program ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepOutcome {
    /// It ran to its end.
    #[default]
    Done,
    /// It did not compile; the answer is the diagnostics, which say what is valid.
    Rejected,
    /// It would change many things or destroy something: nothing ran, and it waits for the
    /// person to confirm what [`ProgramStep::previews`] shows.
    Preview,
    /// An action in it asks the person first; the step waits for their answer.
    Asked,
    /// It stopped: a trap, or an action that failed.
    Failed,
}

/// A resource a step changed, and how: a row the person can undo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepChange {
    /// What changed.
    pub resource: ResourceAt,
    /// The action that changed it, as its app labels it ("Move to Done").
    pub action: String,
}

/// An action the step proposes and the person decides: a card that previews it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StepPreview {
    /// The suggested invocation the app recorded, which the person confirms or declines through
    /// its app.
    pub invocation: ResourceAt,
    /// The action, as its app labels it.
    pub action: String,
    /// What it would do, in a line, when the app says.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
}

/// One program an agent's task ran, and what came of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProgramStep {
    /// The program, as the agent wrote it.
    pub program: String,
    pub outcome: StepOutcome,
    /// What it answered, line by line, as tilolang: output pastes back as input.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answer: Vec<String>,
    /// The resources the answer names, in the order it names them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<ResourceAt>,
    /// What it changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<StepChange>,
    /// What it proposes and waits on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previews: Vec<StepPreview>,
    /// What a previewed program would change once it is confirmed: nothing ran yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub planned: Vec<StepChange>,
}

impl ProgramStep {
    /// The step within the bounds a record keeps, cut on character boundaries; a cut answer
    /// says how many lines it left out, so a reader never takes a part for the whole.
    #[must_use]
    pub fn bounded(mut self) -> Self {
        self.program = cut(&self.program, MAX_PROGRAM_CHARS);
        let lines = self.answer.len();
        self.answer.truncate(MAX_ANSWER_LINES);
        for line in &mut self.answer {
            *line = cut(line, MAX_ANSWER_LINE_CHARS);
        }
        if lines > MAX_ANSWER_LINES {
            // The last line kept gives way to saying so, so it is left out too.
            let left = lines.saturating_sub(MAX_ANSWER_LINES).saturating_add(1);
            if let Some(last) = self.answer.last_mut() {
                *last = format!("// {left} more lines");
            }
        }
        self.refs.truncate(MAX_STEP_ITEMS);
        self.changes.truncate(MAX_STEP_ITEMS);
        self.previews.truncate(MAX_STEP_ITEMS);
        self.planned.truncate(MAX_STEP_ITEMS);
        self
    }
}

fn cut(text: &str, most: usize) -> String {
    text.chars().take(most).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResourceRef;

    #[test]
    fn a_step_reads_back_from_what_it_writes() {
        let step = ProgramStep {
            program: "board.cards.filter(.column.title != \"Done\")".into(),
            outcome: StepOutcome::Done,
            answer: vec!["@board.Card:c1 \"Write the release notes\"".into()],
            refs: vec![ResourceAt {
                app: "so.tilo.lang.board".into(),
                record: None,
                reference: ResourceRef::whole("board.Card", "tl:so.tilo.lang.board:c1", "Write"),
            }],
            changes: Vec::new(),
            previews: Vec::new(),
            planned: Vec::new(),
        };
        let written = serde_json::to_string(&step).unwrap();
        assert!(!written.contains("changes"), "{written}");
        assert_eq!(serde_json::from_str::<ProgramStep>(&written).unwrap(), step);
    }

    #[test]
    fn a_long_answer_says_what_it_left_out() {
        let step = ProgramStep {
            program: "x".repeat(4_010),
            answer: (0..50).map(|n| n.to_string()).collect(),
            ..ProgramStep::default()
        }
        .bounded();
        assert_eq!(step.program.chars().count(), MAX_PROGRAM_CHARS);
        assert_eq!(step.answer.len(), MAX_ANSWER_LINES);
        assert_eq!(step.answer.last().unwrap(), "// 11 more lines");
    }
}
