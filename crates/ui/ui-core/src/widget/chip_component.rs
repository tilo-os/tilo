//! [`Chip`] as a described component: a tag, a filter or a token in a pill.

use crate::component::{Component, Described, Event, Person};
use crate::icons::Icon;

use super::ChipStyle;
use super::{Chip, ChipLead};
use crate::widget::{Press, Tone};

/// A compact pill for a tag, a filter or a token: what a note is tagged with, a recipient, an applied
/// filter.
#[derive(Component)]
#[component(name = "Chip", role = "Button", label = "label")]
pub struct ChipProps {
    /// What it names, in a word or two.
    #[component(required, positional)]
    pub label: String,
    /// A glyph before the label, for a chip that names an action rather than a value.
    pub icon: Option<Icon>,
    /// Who it names, as their face before the label: the member, so their face wears the picture
    /// they set, as the bar's faces do. The first is the one shown.
    pub faces: Vec<Person>,
    /// The tone the chip reads in: what it names is a status (a priority, a state of an item), and its pill
    /// is washed in that tone.
    pub tone: Tone,
    /// Whether the filter it stands for is on; without it the chip is no filter.
    pub selected: Option<bool>,
    /// Whether it can be pressed now.
    pub enabled: bool,
    /// Why it cannot be pressed now, where the label does not say (an action whose requirement
    /// does not hold): spoken as its description beside the name, and shown on hover.
    pub description: Option<String>,
    /// What it says is news: a screen reader announces it when it appears or changes, without the
    /// person moving focus — `polite` waits for a pause, `assertive` speaks at once.
    pub live: Option<crate::a11y::Politeness>,
    /// It was pressed: open or apply what it names.
    #[component(event(press))]
    pub press: Event,
    /// Its remove button was pressed: take it away.
    #[component(event(press))]
    pub remove: Event,
}

/// A chip is pressable until something says otherwise.
impl Default for ChipProps {
    fn default() -> Self {
        Self {
            label: String::new(),
            icon: None,
            faces: Vec::new(),
            tone: Tone::default(),
            selected: None,
            enabled: true,
            description: None,
            live: None,
            press: Event::None,
            remove: Event::None,
        }
    }
}

impl Described for Chip {
    type Props = ChipProps;

    fn from_props(key: &str, props: ChipProps) -> Self {
        let ChipProps {
            label,
            icon,
            faces,
            tone,
            selected,
            enabled,
            description,
            live,
            press,
            remove,
        } = props;
        let mut chip = Self::new(label).with_id(key.to_owned());
        if let Some(politeness) = live {
            chip = chip.with_live(politeness);
        }
        if let Some(who) = faces.into_iter().next() {
            chip = chip.with_face_of(who);
        } else if let Some(icon) = icon {
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
            Event::View(intent) => chip.on_open(intent.sender()),
        };
        if let Some(why) = description.filter(|why| !why.trim().is_empty()) {
            chip = chip.with_description(why.clone()).tooltip(why);
        }
        chip = chip.with_enabled(enabled);
        chip = chip.with_remove_press(Press::of(remove));
        chip
    }

    fn set_props(&mut self, props: ChipProps) -> bool {
        let ChipProps {
            label,
            icon,
            faces,
            tone,
            selected,
            enabled,
            description,
            live,
            press,
            remove,
        } = props;
        self.live = live;
        if label != self.label {
            self.label.clone_from(&label);
            self.footprint = None;
        }
        if let Some(who) = faces.into_iter().next() {
            self.face_hue = Some(who.hue);
            self.lead = ChipLead::Face(crate::presence::presence_colour(
                crate::presence::identity_rgb(who.hue),
            ));
            self.face = Some(who);
        } else if icon.is_some() || !matches!(self.lead, ChipLead::Face(_)) {
            self.face = None;
            self.face_hue = None;
            self.lead = icon.map_or(ChipLead::None, ChipLead::Glyph);
        }
        self.style = if tone == Tone::Neutral {
            ChipStyle::Neutral
        } else {
            ChipStyle::Tinted(tone)
        };
        self.selectable = selected.is_some();
        self.set_selected(selected.unwrap_or(false));
        self.activation.set_event(press);
        self.set_enabled(enabled);
        self.set_description(description.filter(|why| !why.trim().is_empty()));
        let remove = Press::of(remove);
        if remove.is_none() {
            self.removable = false;
            self.on_remove = None;
            self.remove_press = Press::default();
        } else {
            self.set_remove_press(remove);
        }
        self.dirty = true;
        true
    }
}
