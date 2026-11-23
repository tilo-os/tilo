//! [`DisclosureRow`] as a described component.

use crate::component::{Component, Described, Event, PartViews, Person, Value};
use crate::icons::Icon;
use crate::tokens::Rhythm;
use crate::widget::arrange::{Arranged, FlexLayout, Part, Stack};
use crate::widget::{Element, SignalLevel};

use super::{DisclosureRow, Leading};

/// A row whose body opens under it (a device's details, a run's log, a setting's options), its
/// chevron turning as the rows below move down; a press, Enter or Space opens and closes it.
#[derive(Component, Default)]
#[component(name = "DisclosureRow", role = "ListItem", label = "title")]
pub struct DisclosureRowProps {
    /// What the row is, on its first line.
    #[component(required, positional)]
    pub title: String,
    /// A line under the title, in secondary ink.
    pub secondary: Option<String>,
    /// A quiet line at the far end of the title's line: who did it and when.
    pub side: Option<String>,
    /// A glyph before the title: the kind of thing it is.
    pub icon: Option<Icon>,
    /// A person before the title, as an avatar: who did the work the row shows.
    pub who: Option<Person>,
    /// How strong a radio network's signal is, before the title, for a row that is a network.
    pub signal: Option<SignalLevel>,
    /// What opens under the row, in order.
    #[component(part(default))]
    pub body: Vec<Element>,
    /// One control on the row's trailing edge, before the chevron (a device's Connect): it acts
    /// without opening the row.
    #[component(part)]
    pub trailing: Option<Element>,
    /// Whether the row is open; unless given the row keeps it.
    pub expanded: Option<bool>,
    /// One quiet line among content rather than a row of a list: the caption's size and ink,
    /// as tall as that line (an answer's steps, folded under it).
    pub dense: bool,
    /// The pace the body unfolds at, and a view's open or close: the theme's unless said.
    pub motion: Option<crate::motion::Feel>,
    /// The row was opened or closed: whether it is open now is the value.
    #[component(event(value = bool))]
    pub toggle: Event,
}

fn leading(
    icon: Option<Icon>,
    who: Option<&Person>,
    signal: Option<SignalLevel>,
) -> Option<Leading> {
    match (who, signal, icon) {
        (Some(who), _, _) => Some(Leading::Avatar {
            name: who.name.clone(),
            identity: who.identity.id().to_owned(),
        }),
        (None, Some(level), _) => Some(Leading::Signal(level)),
        (None, None, Some(icon)) => Some(Leading::Icon(icon)),
        (None, None, None) => None,
    }
}

impl Described for DisclosureRow {
    type Props = DisclosureRowProps;

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        if part != "body" {
            return None;
        }
        let column = self
            .body
            .as_any_mut()?
            .downcast_mut::<Arranged<FlexLayout>>()?;
        Some(column)
    }

    fn from_props(key: &str, props: DisclosureRowProps) -> Self {
        let DisclosureRowProps {
            title,
            secondary,
            side,
            icon,
            who,
            signal,
            body,
            trailing,
            expanded,
            dense,
            motion,
            toggle,
        } = props;
        let column = body.into_iter().fold(
            Stack::stack()
                .fill_width()
                .gap(Rhythm::Related)
                .with_id(format!("{key}.body")),
            |column, view| column.push_part(Part::new(view)),
        );
        let mut row = Self::new(title, Box::new(column))
            .with_id(key)
            .secondary(secondary.unwrap_or_default())
            .secondary_side(side.unwrap_or_default());
        row.leading = leading(icon, who.as_ref(), signal);
        if let Some(control) = trailing {
            row = row.trailing(control);
        }
        if !toggle.is_none() {
            row = row.on_open_toggle(move |open| toggle.fire(Some(Value::Bool(open))));
        }
        if expanded == Some(true) {
            row = row.opened();
        }
        if dense {
            row = row.dense();
        }
        row.with_feel(motion.unwrap_or(crate::motion::Feel::Standard))
    }

    /// Its lines take their new text in place, and a changed `expanded` opens or closes it on
    /// the disclose motion; its body's views change through [`Self::part_views`].
    fn set_props(&mut self, props: DisclosureRowProps) -> bool {
        let DisclosureRowProps {
            title,
            secondary,
            side,
            icon,
            who,
            signal,
            body: _,
            trailing: _,
            expanded,
            dense,
            motion,
            toggle: _,
        } = props;
        if dense != self.is_dense() {
            return false;
        }
        self.set_feel(motion.unwrap_or(crate::motion::Feel::Standard));
        self.title = title;
        self.secondary = secondary.unwrap_or_default();
        self.side = side.unwrap_or_default();
        self.leading = leading(icon, who.as_ref(), signal);
        if let Some(open) = expanded {
            self.state.show(open, self.motion);
        }
        self.dirty = true;
        true
    }
}
