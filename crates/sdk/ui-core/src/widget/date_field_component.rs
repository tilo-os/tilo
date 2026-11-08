//! [`DateField`] as a described component.

use crate::component::{Bound, Component, Described, Event, SlotViews};

use super::DateField;

/// A day, shown as the person reads a day (with the time it is due at) and picked from a
/// calendar that opens over it. A date that may be empty can be cleared, and the time it is due
/// at is picked in the same popover, in the `time` slot.
#[derive(Component)]
#[component(name = "DateField", role = "ComboBox", label = "label")]
pub struct DateFieldProps {
    /// The day, as the moment a component binds (its ISO text), and where a picked day goes.
    #[component(bind(ty = crate::component::Moment))]
    pub value: Bound<String>,
    /// The time of day the day is due at, when its type pairs one with it (`9:00`): what the
    /// chip says after the day. The field picks it in its `time` slot; it only says it here.
    pub at: Option<String>,
    /// Its spoken name: what the date is for.
    pub label: Option<String>,
    /// The date may be cleared: the calendar offers "Clear date".
    pub optional: bool,
    /// The field stands beside the entry that tells its day (a moment's field, whose hour the
    /// entry types): the chip shows the calendar icon alone, opening the calendar and the time.
    pub compact: bool,
    /// The control that picks the time of day the day is due at: `ui.TimeField(bind: t.at)`.
    #[component(slot)]
    pub time: Vec<crate::widget::Element>,
    /// A day was picked, or the date was cleared: carries the moment, or nothing for a cleared
    /// one.
    #[component(event(value = crate::component::Moment))]
    pub change: Event,
}

impl Default for DateFieldProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            at: None,
            label: None,
            optional: false,
            compact: false,
            time: Vec::new(),
            change: Event::None,
        }
    }
}

impl Described for DateField {
    type Props = DateFieldProps;

    fn from_props(key: &str, props: DateFieldProps) -> Self {
        let DateFieldProps {
            value,
            at,
            label,
            optional,
            compact,
            time,
            ..
        } = props;
        let shown = value.clone();
        let held = value.state.clone();
        Self::from_state(
            key,
            &value.state,
            optional,
            at.as_deref(),
            time,
            label.as_deref(),
            compact,
        )
        .on_change(move |day| {
            // The day changes, the time of day it is due at stays: a moment whose day is
            // picked keeps its time, and a day alone stays a day.
            let moment = super::with_day(&held.get(), day);
            shown.edited(crate::component::Value::Text(moment));
        })
    }

    /// A day or time set elsewhere shows in place through the binding; whether it may be
    /// cleared builds it again.
    fn set_props(&mut self, props: DateFieldProps) -> bool {
        if props.optional != self.is_optional() {
            return false;
        }
        self.set_at(super::time_in(props.at.as_deref()));
        true
    }

    /// The chip, the calendar and the time row stay as they are: the time row changes in
    /// place.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "time").then(|| self.popover_content_mut() as &mut dyn SlotViews)
    }
}
