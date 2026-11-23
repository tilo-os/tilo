//! [`Stepper`] as a described component.

use crate::component::{Component, Described, Event, Value};

use super::Stepper;

/// Where a person is in a short sequence: a setup wizard's steps, an import's. Each step is a
/// station with a disc and its name; a done one carries a check and may be gone back to. Too
/// narrow for its stations, it reads "Step 2 of 4" over the current step.
#[derive(Component, Default)]
#[component(name = "Stepper", role = "Group")]
pub struct StepperProps {
    /// The steps, in order.
    #[component(required)]
    pub steps: Vec<String>,
    /// Which step is current, counted from 0. None before the first step starts: every station
    /// waits.
    pub current: Option<i64>,
    /// The step gone back to: carries its index. Without it, no station takes the pointer.
    #[component(event(value = i64))]
    pub station: Event,
}

impl Described for Stepper {
    type Props = StepperProps;

    fn from_props(key: &str, props: StepperProps) -> Self {
        let StepperProps {
            steps,
            current,
            station,
        } = props;
        let mut stepper = Self::new(steps)
            .with_id(key)
            .current(current.and_then(|index| usize::try_from(index).ok()));
        if !station.is_none() {
            stepper = stepper.on_station(move |index| {
                station.fire(Some(Value::Int(i64::try_from(index).unwrap_or(i64::MAX))));
            });
        }
        stepper
    }

    /// The current step changes in place; other steps are built again.
    fn set_props(&mut self, props: StepperProps) -> bool {
        if props.steps.len() != self.count() {
            return false;
        }
        self.set_current(props.current.and_then(|index| usize::try_from(index).ok()));
        true
    }
}
