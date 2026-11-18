//! `PersonField`: a form's person, shown as their avatar and name and picked from a list.

use std::cell::Cell;
use std::rc::Rc;

use crate::LocalState;
use crate::component::Person;
use crate::icons::Icon;
use crate::tokens::TextRole;
use crate::widget::arrange::Stack;
use crate::widget::binding::{BoxedBinding, Pulled, derived};
use crate::widget::{
    AppModel, Button, ButtonRole, Decorator, ModelPath, PopoverButton, Text, TextInput, When,
    Widget,
};
use tilo_actions::Actor;

#[path = "person_field_component.rs"]
mod component;
pub use component::PersonFieldProps;

type Picked = Rc<Cell<Option<Option<Person>>>>;

/// Whether `what` is offered again by `query`: its name or its identity holds the words typed,
/// whatever case they were typed in.
fn matches(query: &str, name: &str, identity: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || name.to_lowercase().contains(&query)
        || identity.to_lowercase().contains(&query)
}

/// Whether the row `name` (who `identity` is) shows while `query` narrows the list.
fn shown_matching(
    query: &LocalState<String>,
    name: &str,
    identity: &str,
) -> impl FnMut() -> bool + 'static {
    let query = query.clone();
    let name = name.to_owned();
    let identity = identity.to_owned();
    move || matches(&query.get(), &name, &identity)
}

/// Who the bound value took while the list was open: shown once it closes, so a person set
/// elsewhere never moves the list from under the person picking.
#[derive(Clone)]
enum Waiting {
    /// The binding took nobody since the field last showed somebody.
    Nothing,
    /// Who to show now, `None` for nobody.
    Whom(Option<Actor>),
}

pub struct PersonField {
    inner: PopoverButton<Stack>,
    id: String,
    value: Option<Actor>,
    people: Vec<Person>,
    me: Option<Person>,
    picked: Picked,
    /// The words typed into the open list's search field: it narrows who is offered.
    query: LocalState<String>,
    /// Whether the list was open at the last update, so opening it can start a fresh search.
    was_open: bool,
    on_change: Option<Box<dyn FnMut(Option<Person>)>>,
    bound: Option<BoxedBinding<Option<Actor>>>,
    waiting: Waiting,
}

impl PersonField {
    /// A field holding `who` (none for nobody), offering `people` and `me`.
    #[must_use]
    pub fn new(id: &str, who: Option<&Actor>, people: Vec<Person>, me: Option<Person>) -> Self {
        let picked: Picked = Rc::default();
        let query = LocalState::new(String::new());
        Self {
            inner: field(id, who, &people, me.as_ref(), &picked, &query),
            id: id.to_owned(),
            value: who.cloned(),
            people,
            me,
            picked,
            query,
            was_open: false,
            on_change: None,
            bound: None,
            waiting: Waiting::Nothing,
        }
    }

    /// A field following the actor `state` holds as its text ([`crate::component::actor_text`],
    /// `""` for nobody): a person set there shows in place.
    pub(super) fn from_state(
        id: &str,
        state: &crate::LocalState<String>,
        people: Vec<Person>,
        me: Option<Person>,
    ) -> Self {
        let (who, binding) = derived(state, |text: &String| crate::component::actor_of_text(text));
        let mut field = Self::new(id, who.as_ref(), people, me);
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
    pub fn value(&self) -> Option<&Actor> {
        self.value.as_ref()
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
    pub fn who_name(&self, identity: Option<&Actor>) -> String {
        let Some(identity) = identity else {
            return crate::i18n::widget_format("person-field-none", &[]);
        };
        self.people
            .iter()
            .chain(self.me.iter())
            .find(|who| who.identity == *identity)
            .map_or_else(|| identity.id().to_owned(), |who| who.name.clone())
    }

    /// Show the person `who`, set elsewhere. An open list stays open on what it shows.
    pub fn set_value(&mut self, who: Option<&Actor>) {
        if who == self.value() || self.inner.is_open() {
            return;
        }
        self.value = who.cloned();
        self.inner = field(
            &self.id,
            who,
            &self.people,
            self.me.as_ref(),
            &self.picked,
            &self.query,
        );
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
                self.value.as_ref(),
                &self.people,
                self.me.as_ref(),
                &self.picked,
                &self.query,
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
        self.set_value(who.as_ref());
        true
    }
}

fn field(
    id: &str,
    who: Option<&Actor>,
    people: &[Person],
    me: Option<&Person>,
    picked: &Picked,
    query: &LocalState<String>,
) -> PopoverButton<Stack> {
    let name = who.map_or_else(String::new, |identity| {
        people
            .iter()
            .chain(me)
            .find(|person| person.identity == *identity)
            .map_or_else(|| identity.id().to_owned(), |person| person.name.clone())
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
                    .find(|person| person.identity == *identity)
                    .cloned()
                    .unwrap_or_else(|| Person::new(identity.clone(), identity.id())),
            )
            .showing_choice(),
        None => Button::new(id, &label)
            .with_icon(Icon::User)
            .with_placeholder()
            .showing_choice(),
    };
    chip.with_popover(picker(id, people, me, picked, query, false))
}

/// The open list: a search field over who may be picked, narrowed as it is typed, and a row
/// that clears the field. Opened, the search takes the keyboard, so typing narrows at once.
fn picker(
    id: &str,
    people: &[Person],
    me: Option<&Person>,
    picked: &Picked,
    query: &LocalState<String>,
    focus: bool,
) -> Stack {
    let mut content = Stack::stack();
    let mut search = TextInput::from_state(query)
        .with_id(&format!("{id}-search"))
        .with_placeholder(crate::i18n::LabelKey::new("person-field-search"))
        .with_clear_button();
    if focus {
        search = search.wants_focus();
    }
    content = content.push(Box::new(search));
    if let Some(me) = me {
        let label = crate::i18n::widget_format("person-field-me", &[]);
        content = content.push(Box::new(When::new(
            Box::new(entry(
                &format!("{id}-me"),
                &label,
                me,
                Some(me.clone()),
                picked,
            )),
            shown_matching(query, &label, me.identity.id()),
        )));
    }
    let none_label = crate::i18n::widget_format("person-field-none", &[]);
    let on_clear = Rc::clone(picked);
    let clear = Button::new(&format!("{id}-none"), &none_label)
        .role(ButtonRole::Plain)
        .with_icon(Icon::X)
        .with_placeholder()
        .on_view(move || on_clear.set(Some(None)));
    content = content.push(Box::new(When::new(
        Box::new(clear),
        shown_matching(query, &none_label, ""),
    )));
    for person in people {
        if me.is_some_and(|me| me.identity == person.identity) {
            continue;
        }
        content = content.push(Box::new(When::new(
            Box::new(entry(
                &format!("{id}-{}", person.identity.id()),
                &person.name,
                person,
                Some(person.clone()),
                picked,
            )),
            shown_matching(query, &person.name, person.identity.id()),
        )));
    }
    // What the list says while the search holds nobody: a line, never a row that does nothing.
    let no_matches = crate::i18n::widget_format("search-no-matches", &[]);
    let notice = Text::new(&no_matches)
        .with_role(TextRole::Caption)
        .dimmed()
        .with_id(format!("{id}-no-matches"));
    let names: Vec<(String, String)> = me
        .iter()
        .map(|me| {
            (
                crate::i18n::widget_format("person-field-me", &[]),
                me.identity.id().to_owned(),
            )
        })
        .chain(
            people
                .iter()
                .filter(|person| me.is_none_or(|me| me.identity != person.identity))
                .map(|person| (person.name.clone(), person.identity.id().to_owned())),
        )
        .chain(std::iter::once((
            crate::i18n::widget_format("person-field-none", &[]),
            String::new(),
        )))
        .collect();
    let query_for_notice = query.clone();
    content = content.push(Box::new(When::new(Box::new(notice), move || {
        let typed = query_for_notice.get();
        !typed.trim().is_empty()
            && !names
                .iter()
                .any(|(name, identity)| matches(&typed, name, identity))
    })));
    content
}

fn entry(id: &str, label: &str, who: &Person, picked: Option<Person>, chosen: &Picked) -> Button {
    let on_pick = Rc::clone(chosen);
    Button::new(id, label)
        .role(ButtonRole::Plain)
        .with_avatar(who.clone())
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
                self.set_value(who.as_ref());
            }
        }
        self.inner.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let picked = self.take_pick();
        let shown = self.show_waiting();
        // Opening starts a fresh search, its field taking the keyboard: the list is narrowed
        // from the first letter typed without reaching for the field first.
        let open = self.inner.is_open();
        let mut started = false;
        if open != self.was_open {
            self.was_open = open;
            if open {
                self.query.set(String::new());
                *self.inner.popover_mut().content_mut() = picker(
                    &self.id,
                    &self.people,
                    self.me.as_ref(),
                    &self.picked,
                    &self.query,
                    true,
                );
                started = true;
            }
        }
        let inner = self.inner.update(state, path);
        picked || shown || started || inner
    }

    fn name(&self) -> &'static str {
        "PersonField"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mia_id() -> Actor {
        tilo_actions::MemberId::person("mia").into()
    }

    fn sam_id() -> Actor {
        tilo_actions::MemberId::person("sam").into()
    }

    fn people() -> (Vec<Person>, Person) {
        let mia = Person {
            identity: mia_id(),
            name: "Mia".to_owned(),
            hue: crate::tokens::PresenceHue::Violet,
        };
        let sam = Person {
            identity: sam_id(),
            name: "Sam".to_owned(),
            hue: crate::tokens::PresenceHue::Cyan,
        };
        (vec![mia.clone(), sam], mia)
    }

    tilo_test_env::scenario! {
        fn a_person_field_shows_and_reports_who_was_picked;

        phase a_picked_person_is_shown_and_reported_and_clearing_reports_nobody;
        {
            let (people, mia) = people();
            let reported: Rc<Cell<Option<Option<Person>>>> = Rc::default();
            let seen = Rc::clone(&reported);
            let mut field = PersonField::new("assignee", None, people, Some(mia.clone()))
                .on_change(move |who| seen.set(Some(who)));
            assert_eq!(field.who_name(field.value()), "No one");
            field.picked.set(Some(Some(mia.clone())));
            assert!(field.take_pick());
            assert_eq!(field.value(), Some(&mia_id()));
            assert_eq!(field.who_name(field.value()), "Mia");
            assert_eq!(reported.take(), Some(Some(mia)));
            field.picked.set(Some(None));
            assert!(field.take_pick());
            assert_eq!(field.value(), None);
            assert_eq!(field.who_name(field.value()), "No one");
            assert_eq!(reported.take(), Some(None));
        }

        phase a_person_from_elsewhere_is_shown_without_being_reported;
        {
            let (people, mia) = people();
            let reported: Rc<Cell<bool>> = Rc::default();
            let seen = Rc::clone(&reported);
            let mut field = PersonField::new("assignee", None, people, Some(mia))
                .on_change(move |_| seen.set(true));
            field.set_value(Some(&sam_id()));
            assert_eq!(field.value(), Some(&sam_id()));
            assert_eq!(field.who_name(field.value()), "Sam");
            assert!(!field.take_pick());
            assert!(!reported.get());
        }

        phase a_bound_identity_shows_and_one_set_while_the_list_is_open_waits_for_it;
        {
            let (people, mia) = people();
            let state = crate::LocalState::new(crate::component::actor_text(&mia_id()));
            let doc = automerge::AutoCommit::new();
            let mut field = PersonField::from_state("assignee", &state, people, Some(mia));
            assert_eq!(
                field.value(),
                Some(&mia_id()),
                "the person the binding holds is shown"
            );
            state.set(crate::component::actor_text(&sam_id()));
            Decorator::sync(&mut field, &doc, &automerge::ROOT);
            assert_eq!(
                field.value(),
                Some(&sam_id()),
                "a person set elsewhere shows in place"
            );
            field.inner_mut().open();
            state.set(String::new());
            Decorator::sync(&mut field, &doc, &automerge::ROOT);
            assert_eq!(
                field.value(),
                Some(&sam_id()),
                "a change while the list is open is not shown yet"
            );
            field.inner_mut().close();
            assert!(Decorator::update(&mut field, &doc, &automerge::ROOT));
            assert_eq!(
                field.value(),
                None,
                "who it took shows once the list closes"
            );
        }
    }
}

/// The picker's search matches a person by their name or by who they are, whatever case the
/// words are typed in, and always offers the "No one" row while nothing is typed.
#[test]
fn the_search_matches_names_and_identities() {
    use crate::LocalState;

    assert!(matches("", "Mia", "mia"));
    assert!(matches("  ", "Mia", "mia"), "only blanks narrow nothing");
    assert!(matches("mi", "Mia", "mia"));
    assert!(matches("MIA", "Mia", "mia"));
    assert!(matches("mia", "Mia", "mia"), "the identity matches too");
    assert!(!matches("sam", "Mia", "mia"));
    let query = LocalState::new("mi".to_owned());
    let mut shown = shown_matching(&query, "Mia", "mia");
    assert!(shown());
    query.set("sam".to_owned());
    assert!(!shown());
}
