//! [`Avatar`] as a described component.

use crate::component::{Component, Described, Person};
use crate::icons::Icon;

use super::Avatar;

/// Who someone is, at a glance: their picture, or their initials on their own colour. A thing
/// or a group shows a glyph on a neutral square.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "Avatar", role = "Image", label = "name")]
pub struct AvatarProps {
    /// The person or agent it shows.
    #[component(positional)]
    pub who: Option<Person>,
    /// What it is called when it shows no one: a group, a thing. Its initials and colour come
    /// from it.
    pub name: Option<String>,
    /// A glyph instead of initials, on a neutral square: a group, an app.
    pub glyph: Option<Icon>,
    /// A square rather than a circle: an account or a site, not a person.
    pub square: bool,
}

impl AvatarProps {
    /// What the avatar is called and whose colour it takes.
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
        avatar
    }

    fn set_props(&mut self, props: AvatarProps) -> bool {
        let (name, identity) = props.name_and_identity();
        self.name = name;
        self.identity = identity;
        self.glyph = props.glyph;
        self.neutral = props.glyph.is_some();
        self.square = props.square || props.glyph.is_some();
        self.dirty = true;
        true
    }
}
