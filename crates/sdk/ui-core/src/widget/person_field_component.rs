//! [`PersonField`] as a described component.

use crate::component::{Bound, Component, Described, Person};

use super::PersonField;

/// A form's person, shown as an avatar and name and picked from the workspace's members and
/// agents; "Me" is the person the view is shown to, "No one" clears it.
#[derive(Component, Default)]
#[component(name = "PersonField", role = "ComboBox")]
pub struct PersonFieldProps {
    /// The person's identity and the binding a pick writes (empty for nobody).
    #[component(bind(ty = Person))]
    pub value: Bound<String>,
    /// The people offered after "Me" and "No one": the workspace's members and agents.
    pub people: Option<Vec<Person>>,
    /// The person the view is shown to: the list's "Me".
    pub me: Option<Person>,
}

impl Described for PersonField {
    type Props = PersonFieldProps;

    fn from_props(key: &str, props: PersonFieldProps) -> Self {
        let PersonFieldProps { value, people, me } = props;
        let shown = value.clone();
        Self::from_state(key, &value.state, people.unwrap_or_default(), me).on_change(move |who| {
            shown.edited(crate::component::Value::Text(
                who.map_or_else(String::new, |who| who.identity),
            ));
        })
    }

    /// Who may be picked and what they are called; the person held shows in place through the
    /// binding.
    fn set_props(&mut self, props: PersonFieldProps) -> bool {
        props.people.unwrap_or_default() == self.people() && props.me.as_ref() == self.me()
    }
}
