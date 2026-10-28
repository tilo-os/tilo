//! [`PropertyList`] and its [`Property`] rows as described components.

use crate::component::{Component, Described};

use super::{Property, PropertyList};

/// Facts about one thing, a label beside each value: a contact's details, a file's.
#[derive(Component, Default)]
#[component(name = "PropertyList", role = "List")]
pub struct PropertyListProps {
    /// The facts, in order.
    #[component(slot(default))]
    pub properties: Vec<PropertyProps>,
    /// Shade every other row, for a long list read across.
    pub striped: bool,
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
}

impl From<PropertyProps> for Property {
    fn from(props: PropertyProps) -> Self {
        let property = Self::new(props.label, props.value);
        if props.copyable {
            property.copyable()
        } else {
            property
        }
    }
}

impl Described for PropertyList {
    type Props = PropertyListProps;

    fn from_props(key: &str, props: PropertyListProps) -> Self {
        let list =
            Self::new(props.properties.into_iter().map(Property::from).collect()).with_id(key);
        if props.striped { list.striped() } else { list }
    }
}

/// [`PropertyProps`] in a slot of any number of them.
impl crate::component::SlotType for Vec<PropertyProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(PropertyProps::NAME.into())
    }

    fn fill(
        &mut self,
        source: &mut dyn crate::component::Source,
        slot: &str,
    ) -> Result<(), String> {
        self.clear();
        let mut read = Vec::new();
        source.components(slot, &mut |child| {
            read.push(PropertyProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        *self = read;
        Ok(())
    }
}
