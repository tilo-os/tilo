//! A step a run took, in its issue's thread: the tilolang program it ran, and what it answered,
//! with the resources the answer names as references that open.

use ui::prelude::*;
use ui::tilo_reference::ProgramStep;
use ui::widget::{Badge, ChipStyle};

use crate::model::{Message, Tracker};
use crate::tr;

/// The thread row of a [`crate::model::MessageKind::Step`]: `heading` names who ran it.
pub(crate) fn step_row(cx: &ViewCx<Tracker>, heading: Row, message: &Message) -> Element {
    let heading = heading.push(Box::new(
        Badge::text(tr("tasks-message-step")).style(ChipStyle::Neutral),
    ));
    let step: ProgramStep = serde_json::from_str(&message.step).unwrap_or_else(|_| ProgramStep {
        program: message.text.clone(),
        ..ProgramStep::default()
    });
    let mut column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .push(Box::new(heading))
        .push(Box::new(Text::new(&step.program).with_role(TextRole::Code)));
    for line in &step.answer {
        column = column.push(Box::new(Text::new(line).with_role(TextRole::Code).dimmed()));
    }
    for resource in &step.refs {
        column = column.push(Box::new(cx.reference(
            &resource.app,
            resource.record.as_deref(),
            &resource.reference,
        )));
    }
    Box::new(column)
}
