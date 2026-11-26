//! [`Rule`] as a described component.

use tilo_ui_base::{component::Component, component::Described};

use super::Rule;

/// A horizontal rule of a size the view gives it, its ends ticked: what a view measures (a map's
/// scale bar) rather than a divider that fills its row.
#[derive(Component, Default)]
#[component(name = "Rule", role = "Separator", label = "label")]
pub struct RuleProps {
    /// How long it draws, in logical pixels. Without one it fills the row it stands in.
    pub length: Option<f64>,
    /// A tick stands at each end, as a measurement's ends do.
    pub ticks: bool,
    /// What it is, for assistive technology.
    pub label: Option<String>,
}

impl Described for Rule {
    type Props = RuleProps;

    fn from_props(_key: &str, props: RuleProps) -> Self {
        let rule = match props.length {
            Some(length) if length > 0.0 => Self::sized(length),
            _ => Self::filling(),
        };
        rule.with_ticks(props.ticks)
    }
}
