//! [`Spinner`] as a described component.

use crate::component::{Component, Described};
use crate::tokens::TextRole;

use super::Spinner;

/// Something under way whose length is not known: four dots turning. It holds still under
/// reduced motion. Where the share done is known, a progress bar or ring says how far it is.
#[derive(Component, Default)]
#[component(name = "Spinner", role = "Spinner", label = "label")]
pub struct SpinnerProps {
    /// What is under way ("Loading messages"), as assistive technology names it.
    #[component(positional)]
    pub label: Option<String>,
    /// Size it to one line of this text, for a spinner beside a status line. Unless said it
    /// stands alone at its own size.
    pub beside: Option<TextRole>,
}

impl Described for Spinner {
    type Props = SpinnerProps;

    fn from_props(_key: &str, props: SpinnerProps) -> Self {
        let SpinnerProps { label, beside } = props;
        let spinner = beside.map_or_else(Self::new, Self::for_text);
        match label {
            Some(label) => spinner.with_label(label),
            None => spinner,
        }
    }

    /// Only its name changes in place; it keeps turning from where it was.
    fn set_props(&mut self, props: SpinnerProps) -> bool {
        if props.beside != self.text_role {
            return false;
        }
        self.label = props.label;
        true
    }
}
