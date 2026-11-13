//! [`Avatar`] as a described component.

use crate::component::{Component, Described, Person};
use crate::icons::Icon;

use super::Avatar;

/// Who someone is, at a glance: their picture, or their initials on their own colour.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "Avatar", role = "Image", label = "name")]
pub struct AvatarProps {
    /// The person or agent it shows.
    #[component(positional)]
    pub who: Option<Person>,
    /// What it is called when it shows no one: a group, a thing.
    pub name: Option<String>,
    /// A glyph instead of initials, on a neutral square: a group, an app.
    pub glyph: Option<Icon>,
    /// A square rather than a circle: an account or a site, not a person.
    pub square: bool,
    /// The avatar's room with none in it: a message that continues its author's run lines up under the
    /// first one's avatar without repeating it.
    pub room: bool,
}

impl AvatarProps {
    #[must_use]
    pub fn name_and_identity(&self) -> (String, String) {
        self.who.as_ref().map_or_else(
            || {
                let name = self.name.clone().unwrap_or_default();
                (name.clone(), name)
            },
            |who| (who.name.clone(), who.identity.clone()),
        )
    }
}

impl Described for Avatar {
    type Props = AvatarProps;

    fn from_props(_key: &str, props: AvatarProps) -> Self {
        let (name, identity) = props.name_and_identity();
        let mut avatar = Self::person(name, identity);
        if let Some(glyph) = props.glyph {
            avatar = avatar.with_glyph(glyph);
        }
        if props.square {
            avatar = avatar.square();
        }
        if props.room {
            avatar = avatar.room();
        }
        avatar
    }

    fn set_props(&mut self, props: AvatarProps) -> bool {
        let (name, identity) = props.name_and_identity();
        self.name = name;
        self.identity = identity;
        self.glyph = props.glyph;
        self.neutral = props.glyph.is_some();
        self.square = props.square || props.glyph.is_some();
        self.room = props.room;
        self.dirty = true;
        true
    }
}
