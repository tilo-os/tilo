//! `PersonField`: a form's person, shown as their avatar and name and picked from a list.

use std::cell::Cell;
use std::rc::Rc;

use crate::component::Person;
use crate::icons::Icon;
use crate::widget::arrange::Stack;
use crate::widget::binding::{BoxedBinding, Pulled, derived};
use crate::widget::{AppModel, Button, ButtonRole, Decorator, ModelPath, PopoverButton, Widget};

#[path = "person_field_component.rs"]
mod component;
pub use component::PersonFieldProps;

type Picked = Rc<Cell<Option<Option<Person>>>>;

/// Who the bound value took while the list was open: shown once it closes, so a person set
/// elsewhere never moves the list from under the person picking.
#[derive(Clone)]
enum Waiting {
    /// The binding took nobody since the field last showed somebody.
    Nothing,
    /// The identity to show now, `None` for nobody.
    Whom(Option<String>),
}

pub struct PersonField {
    inner: PopoverButton<Stack>,
    id: String,
    value: Option<String>,
    people: Vec<Person>,
    me: Option<Person>,
    picked: Picked,
    on_change: Option<Box<dyn FnMut(Option<Person>)>>,
    bound: Option<BoxedBinding<Option<String>>>,
    waiting: Waiting,
}

impl PersonField {
    /// A field holding `who` (an identity, none for nobody), offering `people` and `me`.
    #[must_use]
    pub fn new(id: &str, who: Option<&str>, people: Vec<Person>, me: Option<Person>) -> Self {
        let picked: Picked = Rc::default();
        Self {
            inner: field(id, who, &people, me.as_ref(), &picked),
            id: id.to_owned(),
            value: who.map(str::to_owned),
            people,
            me,
            picked,
            on_change: None,
            bound: None,
            waiting: Waiting::Nothing,
        }
    }

    /// A field following the identity `state` holds as its text (`""` for nobody): a person
    /// set there shows in place.
    pub(super) fn from_state(
        id: &str,
        state: &crate::LocalState<String>,
        people: Vec<Person>,
        me: Option<Person>,
    ) -> Self {
        let (who, binding) = derived(state, |text: &String| {
            (!text.is_empty()).then(|| text.clone())
        });
        let mut field = Self::new(id, who.as_deref(), people, me);
        field.bound = Some(binding);
        field
    }

    /// Call `f` with the person the person picked, or `None` when they cleared it.
    #[must_use]
    pub fn on_change<F: FnMut(Option<Person>) + 'static>(mut self, f: F) -> Self {
        self.on_change = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn value(&self) -> Option<&str> {
        self.value.as_deref()
    }

    #[must_use]
    pub fn people(&self) -> &[Person] {
        &self.people
    }

    #[must_use]
    pub fn me(&self) -> Option<&Person> {
        self.me.as_ref()
    }

    /// What the field calls the person it holds: their name, or their identity when the
    /// workspace no longer names them.
    #[must_use]
    pub fn who_name(&self, identity: Option<&str>) -> String {
        let Some(identity) = identity else {
            return crate::i18n::widget_format("person-field-none", &[]);
        };
        self.people
            .iter()
            .chain(self.me.iter())
            .find(|who| who.identity == identity)
            .map_or_else(|| identity.to_owned(), |who| who.name.clone())
    }

    /// Show the person `who`, set elsewhere. An open list stays open on what it shows.
    pub fn set_value(&mut self, who: Option<&str>) {
        if who == self.value() || self.inner.is_open() {
            return;
        }
        self.value = who.map(str::to_owned);
        self.inner = field(&self.id, who, &self.people, self.me.as_ref(), &self.picked);
    }

    fn take_pick(&mut self) -> bool {
        let Some(picked) = self.picked.take() else {
            return false;
        };
        self.waiting = Waiting::Nothing;
        self.inner.close();
        let identity = picked.as_ref().map(|who| who.identity.clone());
        if identity != self.value {
            self.value = identity;
            self.inner = field(
                &self.id,
                self.value.as_deref(),
                &self.people,
                self.me.as_ref(),
                &self.picked,
            );
            if let Some(on_change) = &mut self.on_change {
                on_change(picked);
            }
        }
        true
    }

    fn show_waiting(&mut self) -> bool {
        if self.inner.is_open() {
            return false;
        }
        let Waiting::Whom(who) = std::mem::replace(&mut self.waiting, Waiting::Nothing) else {
            return false;
        };
        if who == self.value {
            return false;
        }
        self.set_value(who.as_deref());
        true
    }
}

fn field(
    id: &str,
    who: Option<&str>,
    people: &[Person],
    me: Option<&Person>,
    picked: &Picked,
) -> PopoverButton<Stack> {
    let mut content = Stack::stack();
    if let Some(me) = me {
        content = content.push(Box::new(entry(
            &format!("{id}-me"),
            &crate::i18n::widget_format("person-field-me", &[]),
            me,
            Some(me.clone()),
            picked,
        )));
    }
    let clear = Button::new(
        &format!("{id}-none"),
        &crate::i18n::widget_format("person-field-none", &[]),
    )
    .role(ButtonRole::Plain)
    .with_placeholder();
    let on_clear = Rc::clone(picked);
    let clear = clear.on_view(move || on_clear.set(Some(None)));
    content = content.push(Box::new(clear));
    for person in people {
        if me.is_some_and(|me| me.identity == person.identity) {
            continue;
        }
        content = content.push(Box::new(entry(
            &format!("{id}-{}", person.identity),
            &person.name,
            person,
            Some(person.clone()),
            picked,
        )));
    }
    let name = who.map_or_else(String::new, |identity| {
        people
            .iter()
            .chain(me)
            .find(|person| person.identity == identity)
            .map_or_else(|| identity.to_owned(), |person| person.name.clone())
    });
    let label = if name.is_empty() {
        crate::i18n::widget_format("person-field-none", &[])
    } else {
        name
    };
    let chip = match who {
        Some(identity) => Button::new(id, &label)
            .with_avatar(
                people
                    .iter()
                    .chain(me)
                    .find(|person| person.identity == identity)
                    .map_or(identity, |person| person.name.as_str()),
                identity,
                people
                    .iter()
                    .chain(me)
                    .find(|person| person.identity == identity)
                    .map_or_else(
                        || crate::tokens::PresenceHue::of_identity(identity),
                        |person| person.hue,
                    ),
            )
            .showing_choice(),
        None => Button::new(id, &label)
            .with_icon(Icon::User)
            .with_placeholder()
            .showing_choice(),
    };
    chip.with_popover(content)
}

fn entry(id: &str, label: &str, who: &Person, picked: Option<Person>, chosen: &Picked) -> Button {
    let on_pick = Rc::clone(chosen);
    Button::new(id, label)
        .role(ButtonRole::Plain)
        .with_avatar(&who.name, &who.identity, who.hue)
        .on_view(move || on_pick.set(Some(picked.clone())))
}

impl Decorator for PersonField {
    type Inner = PopoverButton<Stack>;

    fn inner(&self) -> &PopoverButton<Stack> {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut PopoverButton<Stack> {
        &mut self.inner
    }

    /// The person the bound value took shows in place; one the person is picking is not taken
    /// from under the list.
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        if let Some(bound) = &mut self.bound
            && let Pulled::Value(who) = bound.pull(state, path)
        {
            if self.inner.is_open() {
                self.waiting = Waiting::Whom(who);
            } else {
                self.set_value(who.as_deref());
            }
        }
        self.inner.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let picked = self.take_pick();
        let shown = self.show_waiting();
        let inner = self.inner.update(state, path);
        picked || shown || inner
    }

    fn name(&self) -> &'static str {
        "PersonField"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn people() -> (Vec<Person>, Person) {
        let mia = Person {
            identity: "scenario:Mia".to_owned(),
            name: "Mia".to_owned(),
            hue: crate::tokens::PresenceHue::Violet,
        };
        let sam = Person {
            identity: "scenario:Sam".to_owned(),
            name: "Sam".to_owned(),
            hue: crate::tokens::PresenceHue::Cyan,
        };
        (vec![mia.clone(), sam], mia)
    }

    #[test]
    fn a_picked_person_is_shown_and_reported_and_clearing_reports_nobody() {
        let (people, mia) = people();
        let reported: Rc<Cell<Option<Option<Person>>>> = Rc::default();
        let seen = Rc::clone(&reported);
        let mut field = PersonField::new("assignee", None, people, Some(mia.clone()))
            .on_change(move |who| seen.set(Some(who)));
        assert_eq!(field.who_name(field.value()), "No one");
        field.picked.set(Some(Some(mia.clone())));
        assert!(field.take_pick());
        assert_eq!(field.value(), Some("scenario:Mia"));
        assert_eq!(field.who_name(field.value()), "Mia");
        assert_eq!(reported.take(), Some(Some(mia)));
        field.picked.set(Some(None));
        assert!(field.take_pick());
        assert_eq!(field.value(), None);
        assert_eq!(field.who_name(field.value()), "No one");
        assert_eq!(reported.take(), Some(None));
    }

    #[test]
    fn a_person_from_elsewhere_is_shown_without_being_reported() {
        let (people, mia) = people();
        let reported: Rc<Cell<bool>> = Rc::default();
        let seen = Rc::clone(&reported);
        let mut field = PersonField::new("assignee", None, people, Some(mia))
            .on_change(move |_| seen.set(true));
        field.set_value(Some("scenario:Sam"));
        assert_eq!(field.value(), Some("scenario:Sam"));
        assert_eq!(field.who_name(field.value()), "Sam");
        assert!(!field.take_pick());
        assert!(!reported.get());
    }

    #[test]
    fn a_bound_identity_shows_and_one_set_while_the_list_is_open_waits_for_it() {
        let (people, mia) = people();
        let state = crate::LocalState::new("scenario:Mia".to_owned());
        let mut field = PersonField::from_state("assignee", &state, people, Some(mia));
        assert_eq!(
            field.value(),
            Some("scenario:Mia"),
            "the person the binding holds is shown"
        );
        state.set("scenario:Sam".to_owned());
        Decorator::sync(&mut field, &(), &());
        assert_eq!(
            field.value(),
            Some("scenario:Sam"),
            "a person set elsewhere shows in place"
        );
        field.inner_mut().open();
        state.set(String::new());
        Decorator::sync(&mut field, &(), &());
        assert_eq!(
            field.value(),
            Some("scenario:Sam"),
            "a change while the list is open is not shown yet"
        );
        field.inner_mut().close();
        assert!(Decorator::update(&mut field, &(), &()));
        assert_eq!(
            field.value(),
            None,
            "who it took shows once the list closes"
        );
    }
}
