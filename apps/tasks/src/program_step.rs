//! A step a run took, in its issue's thread: the tilolang program it ran and what it answered,
//! drawn by the toolkit's [`ProgramStepView`] as the composer draws it, with the resources the
//! answer names as references that open where they live.

use ui::prelude::*;
use ui::tilo_reference::ProgramStep;
use ui::widget::{Badge, ChipStyle, ProgramStepView, StepAnswer};

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
    let dispatch = cx.dispatcher();
    let view = ProgramStepView::new(format!("step-{}-{}", message.run, message.at_ms), step)
        .answering(move |answer| {
            if let StepAnswer::Open(resource) = answer {
                dispatch.open_reference(
                    &resource.app,
                    resource.record.as_deref(),
                    &resource.reference,
                );
            }
        });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(heading))
            .push(view.into_widget()),
    )
}
