//! [`TimeField`] as a described component.

use crate::component::{Bound, Component, Described, Event};
use crate::widget::binding::derived;
use crate::widget::date_field::time_in;

use super::TimeField;

/// The time of day a day is due at: the hour and the minute it is due at, with "Remove time"
/// beside them, or a quiet "Add time" while it has none. It stands in the `time` slot of a
/// [`DateField`](crate::widget::DateField), so the day and its time read as one fact.
#[derive(Component)]
#[component(name = "TimeField", role = "Group")]
pub struct TimeFieldProps {
    /// The moment, as a bound field holds it (`09:00`, or the ISO text of an instant), and
    /// where a picked time of day goes.
    #[component(bind(ty = crate::component::Moment))]
    pub value: Bound<String>,
    /// A time was picked or removed: carries the time of day, or nothing for a removed one.
    #[component(event(value = crate::component::Moment))]
    pub change: Event,
}

impl Default for TimeFieldProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            change: Event::None,
        }
    }
}

/// What a picked time does: writes it into the bound moment, and reports it as the field's
/// `change` event.
fn reports(
    value: &Bound<String>,
    change: &Event,
) -> impl FnMut(Option<chrono::NaiveTime>) + 'static {
    let bound = value.clone();
    let held = value.state.clone();
    let change = change.clone();
    let mut run = matches!(&change, Event::Action(_))
        .then(|| change.clone().handler())
        .flatten();
    move |time| {
        // The time of day changes, the day it is on stays: the moment the field holds is one
        // value for the day and its time; a time of day on its own stays one.
        let said =
            crate::component::Value::Text(crate::widget::date_field::with_time(&held.get(), time));
        bound.edited(said.clone());
        change.fire(Some(said));
        if let Some(run) = run.as_mut() {
            run();
        }
    }
}

impl Described for TimeField {
    type Props = TimeFieldProps;

    fn from_props(key: &str, props: TimeFieldProps) -> Self {
        let TimeFieldProps { value, change } = props;
        Self::from_state(key, &value.state).on_change(reports(&value, &change))
    }

    /// The moment it shows, and what a picked one does, change where the field stands: the row
    /// it shows the time in stays the row it was.
    fn set_props(&mut self, props: TimeFieldProps) -> bool {
        let TimeFieldProps { value, change } = props;
        let follows = self
            .source
            .as_ref()
            .is_some_and(|state| state.same_state(&value.state));
        if !follows {
            let (shown, binding) = derived(&value.state, |text: &String| time_in(Some(text)));
            self.source = Some(value.state.clone());
            self.bound = Some(binding);
            self.set_value(shown);
        }
        self.on_change = Some(Box::new(reports(&value, &change)));
        true
    }
}
