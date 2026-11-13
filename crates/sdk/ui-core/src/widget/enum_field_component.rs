//! [`EnumField`] as a described component.

use crate::component::{Bound, Component, Described};
use crate::icons::Icon;
use crate::widget::Tone;

use super::{EnumField, field};
use crate::widget::binding::derived;

crate::component::part_slot!(EnumOptionProps);

/// One case an [`EnumField`] offers: its name, what it reads as, and how it reads.
#[derive(Clone, Component, Default)]
#[component(name = "EnumOption", role = "MenuItem", label = "label")]
pub struct EnumOptionProps {
    /// The case's own name, as the app declares it: what an edit writes back, never read out.
    #[component(required, data)]
    pub name: String,
    /// What the case reads as, in a word or two: "High".
    #[component(required, positional)]
    pub label: String,
    /// The glyph the case names, when it names one.
    pub icon: Option<Icon>,
    /// The tone the case reads in: what the chip is washed in.
    pub tone: Tone,
}

/// A value among an enum's cases: the case it holds, as a chip in the case's glyph and
/// tone, opening the cases to pick another.
#[derive(Component, Default)]
#[component(name = "EnumField", role = "Button", label = "label")]
pub struct EnumFieldProps {
    /// The case it holds, by the case's name, and where a picked case goes.
    #[component(bind(ty = crate::component::EnumCase))]
    pub value: Bound<String>,
    /// The cases to pick from, in the enum's order.
    #[component(slot(default))]
    pub cases: Vec<EnumOptionProps>,
    /// What the field is for ("Priority"), as the app named it: what a screen reader hears
    /// before the case itself.
    pub label: Option<String>,
}

impl Described for EnumField {
    type Props = EnumFieldProps;

    fn from_props(key: &str, props: EnumFieldProps) -> Self {
        let EnumFieldProps {
            value,
            cases,
            label,
        } = props;
        // The field follows the case as the host keeps it in step, and sends a picked case
        // back through the same binding.
        let state = value.state.clone();
        Self::from_state(key, &state, cases, label, value)
    }

    /// The value it holds, the cases it offers and the name it is given change where the
    /// field stands: an open menu stays open on what it shows.
    fn set_props(&mut self, props: EnumFieldProps) -> bool {
        let EnumFieldProps {
            value,
            cases,
            label,
        } = props;
        let follows = self
            .source
            .as_ref()
            .is_some_and(|state| state.same_state(&value.state));
        let same_cases = self.cases.len() == cases.len()
            && self.cases.iter().zip(&cases).all(|(held, given)| {
                held.name == given.name
                    && held.label == given.label
                    && held.icon == given.icon
                    && held.tone == given.tone
            });
        let same_label = self.label == label;
        if !follows {
            let (chosen, binding) = derived(&value.state, |name: &String| name.clone());
            self.source = Some(value.state.clone());
            self.bound = Some(binding);
            self.value = chosen;
        }
        self.edits = value;
        if !same_cases || !same_label || !follows {
            self.cases = cases;
            self.label = label;
            self.inner = field(
                &self.id,
                &self.value,
                &self.cases,
                self.label.as_deref(),
                &self.edits,
            );
        }
        true
    }
}
