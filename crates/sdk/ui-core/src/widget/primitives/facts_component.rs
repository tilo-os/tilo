//! [`Facts`] and its facts as described components.

use crate::component::{Component, Described};
use crate::icons::Icon;

use super::{Fact, FactMark, Facts};

/// Facts in one row, each an optional glyph and its text, spaced apart with no separator ("12
/// files", "3 MB", "Edited today"); when the row is too narrow the least important give way
/// first.
#[derive(Component, Default)]
#[component(name = "Facts", role = "List")]
pub struct FactsProps {
    /// The facts, in order.
    #[component(slot(default))]
    pub facts: Vec<FactProps>,
}

/// One fact of a row: its text, and a glyph before it.
#[derive(Component, Default)]
#[component(name = "Fact", role = "ListItem", label = "text")]
pub struct FactProps {
    /// The fact, in a few words: "3 MB".
    #[component(required, positional)]
    pub text: String,
    /// A glyph before it, from the icon set.
    pub icon: Option<Icon>,
    /// How long it stays where the row is narrow: a higher one stays longer, and of equal ones
    /// the last gives way first.
    pub priority: i64,
}

crate::component::part_slot!(FactProps);

impl From<FactProps> for Fact {
    fn from(props: FactProps) -> Self {
        Self {
            mark: props.icon.map(FactMark::Icon),
            text: props.text,
            priority: u8::try_from(props.priority.clamp(0, 255)).unwrap_or(u8::MAX),
        }
    }
}

impl Described for Facts {
    type Props = FactsProps;

    fn from_props(_key: &str, props: FactsProps) -> Self {
        Self::new(props.facts.into_iter().map(Fact::from).collect())
    }

    fn set_props(&mut self, props: FactsProps) -> bool {
        self.set_facts(props.facts.into_iter().map(Fact::from).collect());
        true
    }
}
