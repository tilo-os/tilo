//! [`EnumField`] as a described component.

use crate::component::{Bound, Component, Described};
use crate::icons::Icon;
use crate::widget::Tone;

use super::EnumField;

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

/// A value among an enum's cases: the case it holds, as a chip in the case's glyph and tone,
/// opening the cases to pick another.
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
}
