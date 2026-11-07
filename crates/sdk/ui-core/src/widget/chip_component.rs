//! [`Chip`] as a described component: a tag, a filter or a token in a pill.

use crate::component::{Component, Described, Event};
use crate::icons::Icon;

use super::ChipStyle;
use super::{Chip, ChipLead};
use crate::widget::Tone;

/// A compact pill for a tag, a filter or a token: what a note is tagged with, a recipient, an
/// applied filter. Pressing it opens or applies what it names; a chip that can be taken away
/// has a trailing remove button; a filter chip shows whether it is on.
#[derive(Component, Default)]
#[component(name = "Chip", role = "Button", label = "label")]
pub struct ChipProps {
    /// What it names, in a word or two.
    #[component(required, positional)]
    pub label: String,
    /// A glyph before the label, for a chip that names an action rather than a value.
    pub icon: Option<Icon>,
    /// The tone the chip reads in: what it names is a status (a priority, a state of an item),
    /// and its pill is washed in that tone. `Neutral` for a chip that only names something.
    pub tone: Tone,
    /// Whether the filter it stands for is on; without it the chip is no filter.
    pub selected: Option<bool>,
    /// It was pressed: open or apply what it names.
    #[component(event)]
    pub press: Event,
    /// Its remove button was pressed: take it away. A chip with this has the button.
    #[component(event)]
    pub remove: Event,
}

impl Described for Chip {
    type Props = ChipProps;

    fn from_props(key: &str, props: ChipProps) -> Self {
        let ChipProps {
            label,
            icon,
            tone,
            selected,
            press,
            remove,
        } = props;
        let mut chip = Self::new(label).with_id(key.to_owned());
        if let Some(icon) = icon {
            chip = chip.with_lead(ChipLead::Glyph(icon));
        }
        if tone != Tone::Neutral {
            chip = chip.style(ChipStyle::Tinted(tone));
        }
        if let Some(on) = selected {
            chip = chip.selectable().selected(on);
        }
        chip = match press {
            Event::None => chip,
            Event::Action(binding) => chip.on_action(binding),
            Event::View(intent) => chip.on_view(intent.sender()),
        };
        if let Some(mut run) = remove.handler() {
            chip = chip.on_remove_view(move || run());
        }
        chip
    }

    /// What it names, the glyph or tone it reads in, whether it is chosen and what its press and
    /// its "×" do change where the chip stands: a chip under the pointer keeps its hover.
    fn set_props(&mut self, props: ChipProps) -> bool {
        let ChipProps {
            label,
            icon,
            tone,
            selected,
            press,
            remove,
        } = props;
        if label != self.label {
            self.label.clone_from(&label);
            self.footprint = None;
        }
        // A face leads the chip it names; another glyph leads one that names no face.
        if icon.is_some() || !matches!(self.lead, ChipLead::Face(_)) {
            self.lead = icon.map_or(ChipLead::None, ChipLead::Glyph);
        }
        self.style = if tone == Tone::Neutral {
            ChipStyle::Neutral
        } else {
            ChipStyle::Tinted(tone)
        };
        self.selectable = selected.is_some();
        self.set_selected(selected.unwrap_or(false));
        self.activation.set_event(press, &label);
        match remove.handler() {
            Some(mut run) => self.set_on_remove_view(move || run()),
            None => {
                self.removable = false;
                self.on_remove = None;
                self.remove_verb = None;
            }
        }
        self.dirty = true;
        true
    }
}
