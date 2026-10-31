//! The shared surface of an [`ActionProposal`]: what it would change, what it
//! sends, which model answers, and the decision.
//!
//! Built from the design system's own parts — [`Column`], [`Text`] and
//! [`Button`] — so it carries the toolkit's focus, accessibility and theming
//! rather than a copy of them. What the person is about to send stands in
//! full above the decision: not a summary and not a count, the text itself.

use std::cell::RefCell;
use std::rc::Rc;

use super::{ActionProposal, DiffTag, MAX_DIFF_LINES, ProposalDecision, ProposalTarget, titled};
use crate::i18n::ArgValue;
use crate::tokens::TextRole;
use crate::widget::{Button, ButtonRole, Column, Text};

/// A proposal on screen: its title, the model's note, the change as a diff
/// (or a value's before and after), which model answered and where it ran,
/// exactly what is sent, and Accept and Reject.
pub struct ActionProposalView<R: tilo_resource::Resource> {
    proposal: ActionProposal,
    target: ProposalTarget<R>,
    sheet: Column,
    decision: Option<ProposalDecision>,
}

impl<R: tilo_resource::Resource> ActionProposalView<R> {
    /// The proposal `proposal` in front of the person, which accepting
    /// writes at `target`.
    #[must_use]
    pub fn new(proposal: ActionProposal, target: ProposalTarget<R>) -> Self {
        let mut sheet = Column::new().with_rhythm(crate::tokens::Rhythm::Related);
        let mut index = 0;
        let push = |sheet: Column, widget: Text, index: &mut usize| {
            let id = format!("proposal-{index}");
            *index = index.saturating_add(1);
            sheet.push(Box::new(widget.with_id(id)))
        };
        sheet = push(
            sheet,
            Text::new(&proposal.titled()).with_role(TextRole::Title),
            &mut index,
        );
        if let Some(note) = proposal.note.as_deref() {
            sheet = push(sheet, Text::new(note).with_role(TextRole::Body), &mut index);
        }
        let diff = proposal.change.diff();
        for line in diff.iter().take(MAX_DIFF_LINES) {
            sheet = push(sheet, diff_text(line), &mut index);
        }
        let left_out = diff.len().saturating_sub(MAX_DIFF_LINES);
        if left_out > 0 {
            sheet = push(
                sheet,
                Text::with_args(
                    "proposal-diff-more",
                    &[(
                        "count",
                        ArgValue::from(i64::try_from(left_out).unwrap_or(i64::MAX)),
                    )],
                ),
                &mut index,
            );
        }
        sheet = push(
            sheet,
            Text::new(&proposal.model.placed()).with_role(TextRole::Caption),
            &mut index,
        );
        // What is sent, in full: the person reads it before it goes.
        for part in proposal
            .context
            .instruction
            .iter()
            .chain(&proposal.context.parts)
        {
            sheet = push(
                sheet,
                Text::new(&part.titled()).with_role(TextRole::Caption),
                &mut index,
            );
            sheet = push(
                sheet,
                Text::new(&part.text).with_role(TextRole::Code),
                &mut index,
            );
        }
        if let Some(refusal) = proposal.refusal() {
            sheet = push(
                sheet,
                Text::new(&refusal).with_role(TextRole::Caption),
                &mut index,
            );
        }
        Self {
            proposal,
            target,
            sheet,
            decision: None,
        }
    }

    /// The proposal on screen.
    pub fn proposal(&self) -> &ActionProposal {
        &self.proposal
    }

    /// What the person decided through this view, once they did.
    pub fn decision(&self) -> Option<&ProposalDecision> {
        self.decision.as_ref()
    }

    /// The one action accepting runs, once the person has accepted.
    pub fn accepted(&self) -> Option<super::ProposalAccept<R>> {
        matches!(self.decision, Some(ProposalDecision::Accept { .. }))
            .then(|| self.proposal.accept_action(self.target.clone()))
    }

    /// The action that writes what the person accepted, to submit through
    /// the app's own door — `cx.dispatcher().submit(...)` — as the record's
    /// one field edit: one change, one History row, one undo step. `Ok(None)`
    /// when nothing was accepted, so rejecting submits nothing at all.
    ///
    /// # Errors
    ///
    /// Why the answer cannot be written, when it cannot.
    pub fn invoke(&self) -> Result<Option<tilo_resource::Invoke<R>>, String> {
        match self.accepted() {
            Some(accept) => accept.invoke().map(Some),
            None => Ok(None),
        }
    }

    /// Put Accept and Reject under the proposal. `on_decision` runs once,
    /// from whichever the person picks.
    #[must_use]
    pub fn with_decisions(self, on_decision: impl FnMut(ProposalDecision) + 'static) -> Self {
        let changed = Rc::new(RefCell::new(on_decision));
        let change = self.proposal.change.clone();
        let accept = Button::new("proposal-accept", "proposal-accept")
            .role(ButtonRole::Primary)
            .with_enabled(self.proposal.refusal().is_none() && change.changes_anything())
            .on_effect({
                let changed = Rc::clone(&changed);
                move || {
                    (*changed.borrow_mut())(ProposalDecision::Accept {
                        change: change.clone(),
                    });
                }
            });
        let reject = Button::new("proposal-reject", "proposal-reject").on_effect(move || {
            (*changed.borrow_mut())(ProposalDecision::Reject);
        });
        let label = Text::new(&titled("proposal-decisions")).with_role(TextRole::Label);
        let sheet = self
            .sheet
            .push(Box::new(label))
            .push(Box::new(accept))
            .push(Box::new(reject));
        Self { sheet, ..self }
    }

    /// The laid-out sheet itself, for a dialog or a page to hold.
    pub fn into_widget(mut self) -> Box<dyn crate::widget::Widget> {
        let sheet = std::mem::take(&mut self.sheet);
        Box::new(sheet)
    }
}

/// One diff line as the person reads it: the mark that says how it stands,
/// then the line as it is.
fn diff_text(line: &super::DiffLine) -> Text {
    let mark = match line.tag {
        DiffTag::Same => "  ",
        DiffTag::Added => "+ ",
        DiffTag::Removed => "- ",
    };
    let mut text = Text::new(&format!("{mark}{}", line.text)).with_role(TextRole::Code);
    if line.tag != DiffTag::Same {
        text = text.strong();
    }
    text
}
