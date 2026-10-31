//! [`Link`] as a described component.

use crate::component::{Component, Described, Event, ResourceId};
use crate::tokens::TextRole;

use super::Link;

/// A hyperlink: primary ink with an underline, opening what it names when pressed, activated by
/// Enter or Space, or from assistive technology. A link that names a resource opens it; one
/// whose view gives it an `activate` acts.
#[derive(Component)]
#[component(name = "Link", role = "Link", label = "label", presents = "item")]
pub struct LinkProps {
    /// The address it opens (usually also what it says).
    #[component(required, positional)]
    pub url: String,
    /// What it shows, when that is not the address: "The docs".
    pub label: Option<String>,
    /// The resource it goes to: activating it opens the resource.
    pub item: Option<ResourceId>,
    /// What it is set in: `body` unless said.
    pub role: TextRole,
    /// It was pressed: what pressing it does, instead of opening its address.
    #[component(event)]
    pub activate: Event,
}

impl Default for LinkProps {
    fn default() -> Self {
        Self {
            url: String::new(),
            label: None,
            item: None,
            role: TextRole::Body,
            activate: Event::None,
        }
    }
}

impl Described for Link {
    type Props = LinkProps;

    fn from_props(key: &str, props: LinkProps) -> Self {
        let LinkProps {
            url,
            label,
            role,
            activate,
            ..
        } = props;
        let mut link = Self::new(&url).with_id(key).with_text_role(role);
        if let Some(label) = label {
            link = link.with_label(&label);
        }
        if let Some(mut pressed) = activate.handler() {
            link = link.on_click(move || pressed());
        }
        link
    }

    /// A link showing something else now takes the new address and what it says in place.
    fn set_props(&mut self, props: LinkProps) -> bool {
        if props.activate.is_none() != self.has_action() {
            return false;
        }
        self.set_url(&props.url);
        if let Some(label) = props.label {
            self.set_label(&label);
        }
        true
    }
}
