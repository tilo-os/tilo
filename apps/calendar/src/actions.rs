//! The calendar's items as JSON, the ids its items are published under, and
//! its calendar collections as live rows.
//!
//! The typed action arguments and the ids are the model's, shared with an
//! agent that reads the calendar ([`calendar_model::actions`]); what a window
//! publishes of a collection is this app's.

pub use calendar_model::actions::*;

pub(crate) fn calendar_collection_row(
    calendar: &CalendarCollectionResource,
) -> ui::framework::LiveRow {
    ui::framework::LiveRow::typed::<crate::rows::CalendarRow>(
        calendar_resource_id(&calendar.id),
        calendar.label.clone(),
    )
}

#[cfg(test)]
mod tests;
