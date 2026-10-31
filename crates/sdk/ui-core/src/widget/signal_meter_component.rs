//! [`SignalMeter`] as a described component.

use crate::component::{Component, Described};
use crate::widget::SignalLevel;

use super::SignalMeter;

crate::component::choice!(SignalLevel, "SignalLevel", {
    Unknown => "unknown",
    Weak => "weak",
    Fair => "fair",
    Good => "good",
    Strong => "strong",
});

/// How strong a radio signal is (a Wi-Fi network, a mobile network), as four bars of rising
/// height, one icon square in size. It names itself by its level: "Strong signal".
#[derive(Component, Default)]
#[component(name = "SignalMeter", role = "Image")]
pub struct SignalMeterProps {
    /// How strong the signal is: `unknown` fills no bar.
    #[component(required, positional)]
    pub level: SignalLevel,
}

impl Described for SignalMeter {
    type Props = SignalMeterProps;

    fn from_props(_key: &str, props: SignalMeterProps) -> Self {
        Self::new(props.level)
    }
}
