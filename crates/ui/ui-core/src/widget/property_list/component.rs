//! [`PropertyList`] and its [`Property`] rows as described components.

use tilo_ui_base::{component::Component, component::Described, component::Event};

use super::{Property, PropertyList};

/// Facts about one thing, a label beside each value: a contact's details, a file's.
#[derive(Component, Default)]
#[component(name = "PropertyList", role = "List")]
pub struct PropertyListProps {
    /// The facts, in order.
    #[component(part(default))]
    pub properties: Vec<PropertyProps>,
    /// Shade every other row, for a long list read across.
    pub striped: bool,
    /// A secret hid itself: the window lost the person's focus (or the app sent one it could not
    /// show). The view clears the reveal it made, so its control reads "Reveal" again.
    #[component(event)]
    pub hidden: Event,
}

/// One fact of a property list: what it is, and its value.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "Property", role = "ListItem", label = "label")]
pub struct PropertyProps {
    /// What the fact is: "Due", "Owner".
    #[component(required, positional)]
    pub label: String,
    /// The fact.
    #[component(required, positional)]
    pub value: String,
    /// A trailing action that copies the value.
    pub copyable: bool,
    /// The value is a secret the app shows (a revealed password): the window hides it behind
    /// dots when it loses the person's focus, and shows it again only when the app sends a value
    /// it has not sent before.
    pub secret: bool,
}

impl From<PropertyProps> for Property {
    fn from(props: PropertyProps) -> Self {
        let mut property = Self::new(props.label, props.value);
        if props.secret {
            property = property.secret();
        }
        if props.copyable {
            property = property.copyable();
        }
        property
    }
}

impl Described for PropertyList {
    type Props = PropertyListProps;

    fn from_props(key: &str, props: PropertyListProps) -> Self {
        let mut list =
            Self::new(props.properties.into_iter().map(Property::from).collect()).with_id(key);
        list.hidden_event = props.hidden;
        if props.striped { list.striped() } else { list }
    }

    fn set_props(&mut self, props: PropertyListProps) -> bool {
        let rows = props.properties.into_iter().map(Property::from).collect();
        self.hidden_event = props.hidden;
        self.take_rows(rows);
        self.striped = props.striped;
        self.layout = super::ListLayout::empty();
        self.metrics = None;
        self.dirty = true;
        true
    }
}

impl tilo_ui_base::component::PartItem for PropertyProps {
    fn accepts() -> tilo_ui_base::component::Accepts {
        tilo_ui_base::component::Accepts::Component(PropertyProps::NAME.into())
    }

    fn read_all(
        source: &mut dyn tilo_ui_base::component::Source,
        part: &str,
    ) -> Result<Vec<Self>, String> {
        let mut read = Vec::new();
        source.components(part, &mut |child| {
            read.push(PropertyProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        Ok(read)
    }
}
