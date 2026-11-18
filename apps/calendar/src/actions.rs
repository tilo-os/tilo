//! The calendar's items as JSON, the ids its items are published under, and
//! its calendar collections as live rows.
//!
//! The typed action arguments and the ids are the model's, shared with an
//! agent that reads the calendar ([`calendar_model::actions`]); what a window
//! publishes of a collection is this app's.

pub use calendar_model::actions::*;

use calendar_model::model::Palette;
use ui::tilo_resource::Effect as _;

use crate::resource::CalendarEffect;

pub(crate) fn calendar_collection_row(
    calendar: &CalendarCollectionResource,
) -> ui::framework::LiveRow {
    ui::framework::LiveRow::new(
        calendar_resource_id(&calendar.id),
        "calendar.collection",
        calendar.label.clone(),
    )
    .operation(
        CalendarEffect::ReadCalendar {
            id: calendar.id.clone(),
        }
        .bound()
        .reads_document(),
    )
    .effect(&CalendarEffect::Open {
        item: calendar_resource_id(&calendar.id),
    })
    .effect(&CalendarEffect::SetVisibility {
        id: calendar.id.clone(),
        visible: !calendar.visible,
    })
    .operation(
        CalendarEffect::SetColor {
            id: calendar.id.clone(),
            color: Palette::from_index(calendar.color.unwrap_or_default())
                .name()
                .to_ascii_lowercase(),
        }
        .bound_asking(&["color"]),
    )
}

#[cfg(test)]
mod tests;
