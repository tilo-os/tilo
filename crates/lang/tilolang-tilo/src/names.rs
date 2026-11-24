//! How a view names the workspace's members.
//!
//! The document holds ids, the same on every replica; the workspace's member list knows the names:
//! what it calls each member, the instance's own person among them. A tilolang host names
//! people by the list its instance's connection heard last ([`Told`]).
//!
//! Naming is by what a member is ([`named`]): a person by their name, an app by its own
//! name, an agent by the name it goes by. A history row, a presence mark and a catch-up line say
//! who acted, never an internal id and never part of one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use collaboration::history_source::authors::{MemberNames, UNKNOWN_AUTHOR_NAME, name_words};

use crate::runtime::ids::MemberId;
use crate::runtime::{Actor, NameHost};
use tilo_action_policy::PersonId;

/// The name of `who` to a reader who calls people, apps and agents as `names` does: a sample
/// person or agent a scenario named by that name, an app acting by itself (one of its triggers,
/// or an update to a later build) as the app, and anyone else as `names` calls them.
#[must_use]
pub fn named(who: &Actor, names: &dyn MemberNames) -> String {
    match who {
        Actor::Member(MemberId::Person(id)) => names.person(id).unwrap_or_else(|| {
            if id.is_empty() || id.root().is_some() {
                UNKNOWN_AUTHOR_NAME.to_owned()
            } else {
                id.as_str().to_owned()
            }
        }),
        Actor::Member(MemberId::Agent(id)) => id
            .strip_prefix(SCENARIO)
            .map_or_else(|| names.agent(id), str::to_owned),
        Actor::Member(MemberId::Bot(id)) => names.person(id).unwrap_or_else(|| name_words(id)),
        Actor::Automation(id) => name_words(id.as_str()),
        Actor::App(app) => names.app(app.as_str()),
        // Someone with no identity in the workspace has no name in it.
        Actor::Visitor(_) => "Visitor".to_owned(),
    }
}

/// What the id of a sample agent a scenario named starts with.
const SCENARIO: &str = "scenario:";

/// The name of `who` as the session's catalogue says it, by what they are: an app by its
/// installed name, an agent by its name, a sample person a scenario named (`person("Mia")`, whose
/// id is that name rather than a root key) by that name, and a person nobody named as
/// [`UNKNOWN_AUTHOR_NAME`](collaboration::history_source::authors::UNKNOWN_AUTHOR_NAME), never
/// their key.
#[must_use]
pub fn installed(who: &Actor) -> String {
    named(who, &Installed)
}

/// The names the session's catalogue gives, and no roster of its own: what a member reads as
/// before the workspace's member list named them, or when it never does.
pub struct Installed;

impl MemberNames for Installed {}

impl NameHost for Installed {
    fn name(&self, who: &Actor) -> String {
        installed(who)
    }
}

/// What the member list calls the members, and the colour it paints each in.
#[derive(Default, PartialEq, Eq)]
struct Said {
    me: Option<String>,
    others: HashMap<MemberId, String>,
    hues: HashMap<MemberId, u8>,
    pictures: HashMap<MemberId, String>,
}

/// The names an instance shows people by: what its host heard the member list call them
/// ([`Told::tell`]) first, and what its embedder knows otherwise.
pub struct Told {
    me: PersonId,
    said: RefCell<Said>,
    otherwise: Rc<dyn NameHost>,
}

impl Told {
    /// The names for the person `me`, falling back to `otherwise`.
    #[must_use]
    pub fn new(me: PersonId, otherwise: Rc<dyn NameHost>) -> Self {
        Self {
            me,
            said: RefCell::default(),
            otherwise,
        }
    }

    /// The member list calls the person `me` and each of `others` by id now, paints each member
    /// of `hues` in its presence palette slot, and shows each of `pictures` as the address of
    /// their face. Whether any of it changed.
    pub fn tell(
        &self,
        me: String,
        others: Vec<(MemberId, String)>,
        hues: Vec<(MemberId, u8)>,
        pictures: Vec<(MemberId, String)>,
    ) -> bool {
        let next = Said {
            me: Some(me).filter(|me| !me.trim().is_empty()),
            others: others
                .into_iter()
                .filter(|(_, name)| !name.trim().is_empty())
                .collect(),
            hues: hues.into_iter().collect(),
            pictures: pictures
                .into_iter()
                .filter(|(_, picture)| !picture.trim().is_empty())
                .collect(),
        };
        if *self.said.borrow() == next {
            return false;
        }
        *self.said.borrow_mut() = next;
        true
    }
}

impl NameHost for Told {
    fn name(&self, who: &Actor) -> String {
        let said = self.said.borrow();
        let Some(member) = who.member_id() else {
            drop(said);
            return self.otherwise.name(who);
        };
        if member.as_person() == Some(&self.me)
            && let Some(me) = &said.me
        {
            return me.clone();
        }
        if let Some(name) = said.others.get(member) {
            return name.clone();
        }
        drop(said);
        self.otherwise.name(who)
    }

    /// The picture the member list gives `who`, as the address a face reads it from: shown
    /// wherever a view shows them as an author. `None` shows their initials.
    fn picture(&self, who: &Actor) -> Option<String> {
        let member = who.member_id()?;
        self.said
            .borrow()
            .pictures
            .get(member)
            .cloned()
            .or_else(|| self.otherwise.picture(who))
    }

    /// The colour the member list paints `who` in, so a byline or a chip in a view wears the
    /// colour of their face on the bar and their caret in every other app.
    fn hue(&self, who: &Actor) -> Option<u8> {
        let member = who.member_id()?;
        self.said
            .borrow()
            .hues
            .get(member)
            .copied()
            .or_else(|| self.otherwise.hue(who))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ids::AppId;

    fn person(id: &str) -> Actor {
        MemberId::person(id).into()
    }

    fn agent(id: &str) -> Actor {
        MemberId::agent(id).into()
    }

    fn app(id: &str) -> Actor {
        Actor::App(AppId::from(id))
    }

    /// What a window with nobody named yet calls each actor: by what they are, never by part
    /// of a key.
    #[test]
    fn a_member_nobody_named_is_named_by_what_they_are_never_by_part_of_their_id() {
        let told = Told::new("me-id".into(), Rc::new(Installed));
        let rows: &[(Actor, &str, &str)] = &[
            (agent("planner"), "Planner", "an agent by its name"),
            (
                person("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
                "Unknown member",
                "a key nobody named is never shown, in full or cut short",
            ),
            (person(""), "Unknown member", "nobody"),
            (
                person("Mia"),
                "Mia",
                "a scenario's sample person, by the name it gave",
            ),
            (
                agent("scenario:Scout"),
                "Scout",
                "a scenario's sample agent",
            ),
            (app("so.tilo.lang.today"), "Today", "an app by its own name"),
            (app("so.tilo.notes"), "Notes", "an app by its own name"),
            (
                app("org.example.Editor"),
                "Editor",
                "an app installed nowhere here reads as the words its id makes",
            ),
            (
                agent(""),
                "Unknown member",
                "an agent that gave no name is unknown, as a person is",
            ),
            (
                app("org.gnome.TextEditor"),
                "Text Editor",
                "a camel-case hump is a space",
            ),
            (
                app("so.tilo.lang.read-later"),
                "Read Later",
                "a dash is a space, and each word is capitalised",
            ),
            (
                MemberId::bot("helper").into(),
                "Helper",
                "a bot nobody named, by its own name",
            ),
            (
                Actor::automation("morning"),
                "Morning",
                "an automation by its name",
            ),
        ];
        for (who, expected, why) in rows {
            assert_eq!(told.name(who), *expected, "{who:?}: {why}");
        }
    }

    #[test]
    fn a_member_is_named_as_the_window_said_and_by_what_they_are_when_nobody_did() {
        let id = "95522b97c3d1e0f4a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
        let told = Told::new(id.into(), Rc::new(Installed));
        assert_eq!(
            told.name(&person(id)),
            "Unknown member",
            "a key nobody named is never shown, in full or cut short"
        );
        assert_eq!(told.name(&agent("planner")), "Planner");
        assert_eq!(
            told.name(&app("so.tilo.lang.today")),
            "Today",
            "an app is its own name"
        );
        assert!(told.tell(
            "Ada".to_owned(),
            vec![(MemberId::person("ben-id"), "Ben".to_owned())],
            Vec::new(),
            Vec::new(),
        ));
        assert!(!told.tell(
            "Ada".to_owned(),
            vec![(MemberId::person("ben-id"), "Ben".to_owned())],
            Vec::new(),
            Vec::new(),
        ));
        assert_eq!(told.name(&person(id)), "Ada");
        assert_eq!(told.name(&person("ben-id")), "Ben");
    }

    /// A member wears in a view the colour the member list paints them in, which travels with
    /// them to the window; one the list said nothing of is left to the window's rule.
    #[test]
    fn a_member_wears_the_colour_the_member_list_paints_them_in() {
        let told = Told::new("ada-id".into(), Rc::new(Installed));
        assert_eq!(told.hue(&person("ben-id")), None, "nothing said yet");
        assert!(told.tell(
            "Ada".to_owned(),
            vec![(MemberId::person("ben-id"), "Ben".to_owned())],
            vec![
                (MemberId::person("ben-id"), 1),
                (MemberId::agent("planner"), 9),
                (MemberId::person("ada-id"), 4),
            ],
            Vec::new(),
        ));
        assert_eq!(told.hue(&person("ben-id")), Some(1));
        assert_eq!(told.hue(&agent("planner")), Some(9));
        assert_eq!(
            told.hue(&person("ada-id")),
            Some(4),
            "the person at the window too"
        );
        assert_eq!(
            told.hue(&app("so.tilo.lang.today")),
            None,
            "an app is no member"
        );
    }
}

#[cfg(test)]
mod picture_tests {
    use super::*;

    fn person(id: &str) -> Actor {
        MemberId::person(id).into()
    }

    fn agent(id: &str) -> Actor {
        MemberId::agent(id).into()
    }

    /// A member's face draws the picture the member list gives them, and shows initials until
    /// it gives one.
    #[test]
    fn a_member_wears_the_picture_the_member_list_gives_them() {
        let told = Told::new("ada-id".into(), Rc::new(Installed));
        assert_eq!(told.picture(&person("ben-id")), None, "nothing said yet");
        let picture = "data:image/png;base64,iVBORw0KGgo=".to_owned();
        assert!(told.tell(
            "Ada".to_owned(),
            vec![(MemberId::person("ben-id"), "Ben".to_owned())],
            Vec::new(),
            vec![(MemberId::person("ben-id"), picture.clone())],
        ));
        assert_eq!(
            told.picture(&person("ben-id")).as_deref(),
            Some(picture.as_str())
        );
        assert_eq!(
            told.picture(&agent("planner")),
            None,
            "an agent nobody pictured shows its mark"
        );
    }
}
