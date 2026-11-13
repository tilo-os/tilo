//! [`Link`] as a described component.

use crate::component::{Component, Described, Event, ResourceId};
use crate::tokens::TextRole;

use super::Link;

/// A hyperlink: primary ink with an underline, opening what it names when pressed.
#[derive(Component)]
#[component(name = "Link", role = "Link", label = "label", presents = "item")]
pub struct LinkProps {
    /// The address it opens (usually also what it says).
    #[component(required, positional)]
    #[component(data)]
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

    fn set_props(&mut self, props: LinkProps) -> bool {
        self.set_url(&props.url);
        self.set_label(props.label.as_deref().unwrap_or(&props.url));
        self.text_role = props.role;
        self.click = props.activate.handler();
        self.dirty = true;
        true
    }
}
