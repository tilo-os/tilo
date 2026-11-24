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
    /// An empty avatar placeholder: a message that continues its author's run lines up under the
    /// first one's avatar without repeating it.
    pub placeholder: bool,
    /// Where the person's picture is: a content address (`tilo-blob://<hash>`) or a `data:`
    /// address a record carries, read as `ui.Image` reads one. The initials show until it is here.
    pub picture: Option<String>,
}

impl AvatarProps {
    /// What it is called, and the identity its colour comes from.
    #[must_use]
    pub fn name_and_identity(&self) -> (String, String) {
        self.who.as_ref().map_or_else(
            || {
                let name = self.name.clone().unwrap_or_default();
                (name.clone(), name)
            },
            |who| (who.name.clone(), who.identity.id().to_owned()),
        )
    }
}

impl Described for Avatar {
    type Props = AvatarProps;

    fn from_props(_key: &str, props: AvatarProps) -> Self {
        let (name, identity) = props.name_and_identity();
        let mut avatar = match props.who {
            Some(who) => Self::of(who),
            None => Self::person(name, identity),
        };
        if let Some(glyph) = props.glyph {
            avatar = avatar.with_glyph(glyph);
        }
        if props.square {
            avatar = avatar.square();
        }
        if props.placeholder {
            avatar = avatar.placeholder();
        }
        if let Some(picture) = props.picture.filter(|url| !url.is_empty()) {
            avatar = avatar.with_picture_url(picture);
        }
        avatar
    }

    fn set_props(&mut self, props: AvatarProps) -> bool {
        let (name, identity) = props.name_and_identity();
        let picture = props
            .picture
            .clone()
            .or_else(|| props.who.as_ref().and_then(|who| who.picture.clone()))
            .filter(|url| !url.is_empty());
        self.name = name;
        self.identity = identity;
        self.who = props.who;
        self.glyph = props.glyph;
        self.neutral = props.glyph.is_some();
        self.square = props.square || props.glyph.is_some();
        self.placeholder = props.placeholder;
        let shown = match &self.picture {
            super::Picture::Lazy { url, .. } => Some(url.clone()),
            super::Picture::None => None,
        };
        if picture != shown {
            self.image = None;
            match picture {
                Some(url) => {
                    self.picture = super::Picture::Lazy {
                        store: crate::pictures::Pictures::window().store().clone(),
                        url,
                    };
                }
                None => self.picture = super::Picture::None,
            }
        }
        self.dirty = true;
        true
    }
}
