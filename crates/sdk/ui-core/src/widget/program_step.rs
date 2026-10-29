//! `ProgramStepView`: one step of an agent's task as everyone who follows it sees it
//! (`docs/designs/app-language/agent-ux.md`, "Working shows work"): the tilolang program the
//! agent ran, read-only and coloured in a raised well; what it answered, line by line, with the
//! resource each line names as a reference chip that opens it; what it changed, as rows that
//! undo; and what it proposes, as cards the person confirms or declines.
//!
//! The view holds no authority. Opening, undoing, confirming and declining are the host's
//! callbacks, which go through the ordinary paths as the person; without a callback the view
//! offers no control for it.

use std::rc::Rc;

use tilo_reference::{ProgramStep, ResourceAt, StepChange, StepOutcome, StepPreview};
use tilolang::lexer::{self, Tok};

use crate::i18n::widget_format;
use crate::icons::Icon;
use crate::text_render::TextWrapMode;
use crate::tokens::{Rhythm, TextRole};
use crate::widget::code_editor::{CodeEditor, Language};
use crate::widget::{
    Actions, Button, ButtonRole, Card, Chip, Column, ControlSize, DisclosureGroup, DisclosureRow,
    Element, ListRow, Row, Surface, SurfaceLevel, Text, Tone,
};

/// An answer longer than this many lines shows its first [`SHOWN_LINES`] and folds the rest.
const FOLD_AFTER: usize = 6;
/// How many lines a folded answer shows.
const SHOWN_LINES: usize = 4;

type OnResource = Rc<dyn Fn(&ResourceAt)>;
type OnChange = Rc<dyn Fn(&StepChange)>;
type OnPreview = Rc<dyn Fn(&StepPreview)>;

/// One program step of an agent's task, with the host's callbacks for what the person does
/// with it. Build it with [`ProgramStepView::new`] and turn it into a widget with
/// [`ProgramStepView::into_widget`].
pub struct ProgramStepView {
    id: String,
    step: ProgramStep,
    on_open: Option<OnResource>,
    on_undo: Option<OnChange>,
    on_confirm: Option<OnPreview>,
    on_decline: Option<OnPreview>,
    disclosure: Option<DisclosureGroup>,
}

impl ProgramStepView {
    /// `step`, its controls named after `id` (unique among the steps shown together).
    #[must_use]
    pub fn new(id: impl Into<String>, step: ProgramStep) -> Self {
        Self {
            id: id.into(),
            step,
            on_open: None,
            on_undo: None,
            on_confirm: None,
            on_decline: None,
            disclosure: None,
        }
    }

    /// Open a resource the answer names, where it lives.
    #[must_use]
    pub fn on_open(mut self, open: impl Fn(&ResourceAt) + 'static) -> Self {
        self.on_open = Some(Rc::new(open));
        self
    }

    /// Undo what the step changed in one resource.
    #[must_use]
    pub fn on_undo(mut self, undo: impl Fn(&StepChange) + 'static) -> Self {
        self.on_undo = Some(Rc::new(undo));
        self
    }

    /// Confirm what the step proposes.
    #[must_use]
    pub fn on_confirm(mut self, confirm: impl Fn(&StepPreview) + 'static) -> Self {
        self.on_confirm = Some(Rc::new(confirm));
        self
    }

    /// Decline what the step proposes.
    #[must_use]
    pub fn on_decline(mut self, decline: impl Fn(&StepPreview) + 'static) -> Self {
        self.on_decline = Some(Rc::new(decline));
        self
    }

    /// Keep whether a long answer is unfolded in `group`, by this step's id, so a view that
    /// rebuilds its steps leaves the answer as the person left it.
    #[must_use]
    pub fn with_disclosure(mut self, group: &DisclosureGroup) -> Self {
        self.disclosure = Some(group.clone());
        self
    }

    /// The step as a widget: the program, then the answer, the changes and the proposals.
    #[must_use]
    pub fn into_widget(self) -> Element {
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Related)
            .push(self.program());
        if let Some(answer) = self.answer() {
            column = column.push(answer);
        }
        for (index, change) in self.step.changes.iter().enumerate() {
            column = column.push(self.change(index, change));
        }
        for (index, preview) in self.step.previews.iter().enumerate() {
            column = column.push(self.preview(index, preview));
        }
        if !self.step.planned.is_empty() {
            column = column.push(Box::new(
                Text::new(&widget_format("program-step-planned", &[]))
                    .with_role(TextRole::Caption)
                    .with_wrap_mode(TextWrapMode::WordOrGlyph),
            ));
            for (index, planned) in self.step.planned.iter().enumerate() {
                column = column.push(self.planned(index, planned));
            }
        }
        Box::new(column)
    }

    /// The program, read-only and coloured, in a raised well.
    fn program(&self) -> Element {
        let editor = CodeEditor::local()
            .with_language(Language::Tilolang)
            .with_gutter(false)
            .with_current_line_highlight(false)
            .with_initial_value(self.step.program.trim_end())
            .with_accessible_name(&widget_format("program-step-program", &[]))
            .with_id(&format!("{}-program", self.id))
            .read_only();
        Box::new(Surface::new(SurfaceLevel::Raised, Box::new(editor)).padded(Rhythm::Tight))
    }

    /// What the step answered: each line in the code role and secondary ink, a line led by a
    /// reference showing it as a chip; a refused or failed step's diagnostics in the danger
    /// status ink. A long answer folds after its first lines.
    fn answer(&self) -> Option<Element> {
        let lines = &self.step.answer;
        if lines.is_empty() {
            return None;
        }
        let diagnostics = matches!(
            self.step.outcome,
            StepOutcome::Rejected | StepOutcome::Failed
        );
        let line_of = |line: &str| -> Element {
            if diagnostics {
                diagnostic_line(line)
            } else {
                self.answer_line(line)
            }
        };
        let column = |lines: &[String]| {
            lines.iter().fold(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_rhythm(Rhythm::Tight),
                |column, line| column.push(line_of(line)),
            )
        };
        if lines.len() <= FOLD_AFTER {
            return Some(Box::new(column(lines)));
        }
        let (shown, folded) = lines.split_at(SHOWN_LINES);
        let title = widget_format("program-step-show-all", &[("count", lines.len().into())]);
        let key = format!("{}-answer", self.id);
        let rest: Element = Box::new(column(folded));
        let row = match &self.disclosure {
            Some(group) => DisclosureRow::keyed(title, rest, group, &key),
            None => DisclosureRow::new(title, rest),
        }
        .with_id(key);
        Some(Box::new(column(shown).push(Box::new(row))))
    }

    /// One answer line: led by a reference this step names, the reference as a chip that
    /// opens it and the rest of the line beside it; any other line as it reads.
    fn answer_line(&self, line: &str) -> Element {
        let Some((resource, rest)) = leading_reference(line, &self.step.refs) else {
            return code_line(line);
        };
        let mut chip = Chip::reference(resource.reference.label.clone(), &resource.reference.id);
        if let Some(open) = &self.on_open {
            let (open, resource) = (Rc::clone(open), resource.clone());
            chip = chip.on_click(move || open(&resource));
        }
        let mut row = Row::new().with_rhythm(Rhythm::Related).push(Box::new(chip));
        if !rest.is_empty() {
            row = row.push(code_line(&labelled_only(rest)));
        }
        Box::new(row)
    }

    /// A change as a row: what was done, to what, and Undo.
    fn change(&self, index: usize, change: &StepChange) -> Element {
        let id = format!("{}-change-{index}", self.id);
        let mut row = ListRow::new(id.clone(), change.action.clone())
            .secondary(change.resource.reference.label.clone())
            .icon(Icon::Pencil);
        if let Some(undo) = &self.on_undo {
            let (undo, change) = (Rc::clone(undo), change.clone());
            row = row.trailing(Box::new(
                Button::new(
                    &format!("{id}-undo"),
                    &widget_format("program-step-undo", &[]),
                )
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .on_click(move || undo(&change)),
            ));
        }
        Box::new(row)
    }

    /// A change a preview would make, as a row: what, to what. Nothing to undo: it has not run.
    fn planned(&self, index: usize, planned: &StepChange) -> Element {
        Box::new(
            ListRow::new(
                format!("{}-planned-{index}", self.id),
                planned.action.clone(),
            )
            .secondary(planned.resource.reference.label.clone())
            .icon(Icon::Pencil),
        )
    }

    /// A proposal as an object card: the action, what it would do, Confirm and Decline.
    fn preview(&self, index: usize, preview: &StepPreview) -> Element {
        let id = format!("{}-preview-{index}", self.id);
        let summary = if preview.summary.trim().is_empty() {
            widget_format("program-step-waits", &[])
        } else {
            preview.summary.clone()
        };
        let mut card = Card::new(Box::new(
            Text::new(&summary)
                .with_role(TextRole::Body)
                .with_wrap_mode(TextWrapMode::WordOrGlyph),
        ))
        .with_title(preview.action.clone());
        if let (Some(confirm), Some(decline)) = (&self.on_confirm, &self.on_decline) {
            let (confirm, decline) = (Rc::clone(confirm), Rc::clone(decline));
            let (confirmed, declined) = (preview.clone(), preview.clone());
            card = card.with_footer(
                Actions::new(
                    Button::new(
                        &format!("{id}-approve"),
                        &widget_format("program-step-approve", &[]),
                    )
                    .role(ButtonRole::Primary)
                    .on_click(move || confirm(&confirmed)),
                )
                .secondary(
                    Button::new(
                        &format!("{id}-decline"),
                        &widget_format("program-step-decline", &[]),
                    )
                    .role(ButtonRole::Secondary)
                    .on_click(move || decline(&declined)),
                ),
            );
        }
        Box::new(card)
    }
}

/// A line of code in the answer's ink.
fn code_line(line: &str) -> Element {
    Box::new(
        Text::new(line)
            .with_role(TextRole::Code)
            .with_wrap_mode(TextWrapMode::WordOrGlyph)
            .dimmed(),
    )
}

/// A line of a refused or failed step's diagnostics: what the compiler or the host said, in
/// the danger status ink, without the comment marker it is carried in.
fn diagnostic_line(line: &str) -> Element {
    let said = line.strip_prefix("//").map_or(line, str::trim_start);
    Box::new(
        Text::new(said)
            .with_role(TextRole::Caption)
            .with_wrap_mode(TextWrapMode::WordOrGlyph)
            .tone(Tone::Danger),
    )
}

/// `fields` with each reference literal that its label follows left out: `column:
/// @board.Column:root/columns:todo "To do"` reads `column: "To do"`. The chip beside them
/// already says which record the line is; the fields' references are read by their labels.
fn labelled_only(fields: &str) -> String {
    let (tokens, _) = lexer::lex(fields);
    let mut shown = String::with_capacity(fields.len());
    let mut from = 0;
    for pair in tokens.windows(2) {
        if let (Tok::Ref(_), Tok::Str(_)) = (&pair[0].tok, &pair[1].tok)
            && let (Some(kept), true) = (
                fields.get(from..pair[0].span.start),
                pair[1].span.start <= fields.len(),
            )
        {
            shown.push_str(kept);
            from = pair[1].span.start;
        }
    }
    shown.push_str(fields.get(from..).unwrap_or(""));
    shown
}

/// The reference `line` starts with, among `refs`, and what follows it: the rest of the line
/// after the reference literal and the label it repeats.
fn leading_reference<'a>(
    line: &'a str,
    refs: &'a [ResourceAt],
) -> Option<(&'a ResourceAt, &'a str)> {
    let (tokens, _) = lexer::lex(line);
    let mut tokens = tokens.into_iter();
    let first = tokens.next()?;
    let Tok::Ref(literal) = &first.tok else {
        return None;
    };
    let resource = refs.iter().find(|resource| {
        resource
            .reference
            .id
            .strip_suffix(literal.id.as_str())
            .is_some_and(|head| head.ends_with(':'))
            && resource
                .reference
                .kind
                .strip_suffix(literal.ty.as_str())
                .is_some_and(|head| head.is_empty() || head.ends_with('.'))
    })?;
    let mut end = first.span.end.min(line.len());
    // The label the literal is followed by is the chip's own.
    if let Some(next) = tokens.next()
        && let Tok::Str(pieces) = &next.tok
        && let [lexer::StrPiece::Lit(label)] = pieces.as_slice()
        && *label == resource.reference.label
    {
        end = next.span.end.min(line.len());
    }
    Some((resource, line.get(end..).unwrap_or("").trim_start()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_reference::ResourceRef;

    fn card() -> ResourceAt {
        ResourceAt {
            app: "so.tilo.lang.board".into(),
            record: None,
            reference: ResourceRef::whole(
                "tl.so.tilo.lang.board.Card",
                "tl:so.tilo.lang.board:root/cards:c1",
                "Write the release notes",
            ),
        }
    }

    #[test]
    fn an_answer_line_led_by_a_reference_shows_it_as_a_chip() {
        let refs = [card()];
        let line = r#"@board.Card:root/cards:c1 "Write the release notes" column: @board.Column:root/columns:todo "To do""#;
        let (resource, rest) = leading_reference(line, &refs).expect("the card leads the line");
        assert_eq!(resource, &refs[0]);
        assert_eq!(
            rest, r#"column: @board.Column:root/columns:todo "To do""#,
            "the label is the chip's, the fields stay"
        );
        assert_eq!(labelled_only(rest), r#"column: "To do""#);
        assert_eq!(labelled_only("due: 2026-10-30"), "due: 2026-10-30");
        assert!(leading_reference("// 3 items", &refs).is_none());
        assert!(
            leading_reference(r#"@board.Card:root/cards:c9 "Other""#, &refs).is_none(),
            "a reference the step does not name stays code"
        );
    }

    #[test]
    fn the_view_offers_only_what_its_host_can_do() {
        fn ids(widget: &dyn crate::widget::Widget, out: &mut Vec<String>) {
            if let Some(id) = widget.test_id() {
                out.push(id.to_owned());
            }
            for child in widget.children() {
                ids(child, out);
            }
        }
        let step = ProgramStep {
            program: "board.move(@board.Card:root/cards:c1, column: \"Done\")".into(),
            outcome: StepOutcome::Done,
            answer: (0..9).map(|n| format!("// line {n}")).collect(),
            refs: vec![card()],
            changes: vec![StepChange {
                resource: card(),
                action: "Move to Done".into(),
            }],
            previews: vec![StepPreview {
                invocation: ResourceAt {
                    app: "so.tilo.lang.todo".into(),
                    record: None,
                    reference: ResourceRef::whole("tl.invocation", "inv-1", "Check"),
                },
                action: "Check the todo".into(),
                summary: String::new(),
            }],
            planned: Vec::new(),
        };
        let mut bare = Vec::new();
        ids(
            ProgramStepView::new("s1", step.clone())
                .into_widget()
                .as_ref(),
            &mut bare,
        );
        assert!(bare.iter().any(|id| id == "s1-answer"), "{bare:?}");
        assert!(!bare.iter().any(|id| id.ends_with("-undo")), "{bare:?}");
        assert!(!bare.iter().any(|id| id.ends_with("-approve")), "{bare:?}");

        let mut full = Vec::new();
        ids(
            ProgramStepView::new("s1", step)
                .on_undo(|_| {})
                .on_confirm(|_| {})
                .on_decline(|_| {})
                .into_widget()
                .as_ref(),
            &mut full,
        );
        for id in [
            "s1-change-0-undo",
            "s1-preview-0-approve",
            "s1-preview-0-decline",
        ] {
            assert!(full.iter().any(|shown| shown == id), "{id} in {full:?}");
        }
    }
}
