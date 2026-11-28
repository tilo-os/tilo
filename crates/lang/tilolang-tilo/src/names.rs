//! How a view names the workspace's members.
//!
//! The document holds ids, the same on every replica; the workspace's members' resource knows the names:
//! what it calls each member, the instance's own person among them. A tilolang host names
//! people by its last read of that resource ([`HostNames`]).
//!
//! Naming is by what a member is ([`named`]): a person by their name, an app by its own
//! name, an agent by the name it goes by. A history row, a presence mark and a catch-up line say
//! who acted, never an internal id and never part of one.

use std::cell::RefCell;
use std::rc::Rc;

use collab_types::history::authors::{MemberNames, UNKNOWN_AUTHOR_NAME, name_words};

use crate::runtime::ids::MemberId;
use crate::runtime::{Actor, NameHost};

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
        Actor::Member(MemberId::Agent { id, .. }) => id
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
/// [`UNKNOWN_AUTHOR_NAME`](collab_types::history::authors::UNKNOWN_AUTHOR_NAME), never
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

/// The names a view shows people by: what the host read of the workspace's members' resource
/// names each ([`Host::member_rows`](crate::host::Host::member_rows)), and what its embedder
/// knows otherwise. The sheet a view draws is the one place a name is resolved, so a rename, a
/// new member or one removed moves every view with the host's next read.
pub struct HostNames {
    /// The instance's host, not held: the host's runtime holds these names, and a strong hold
    /// here would keep the host, and the instance's connection, after the instance ended.
    host: std::rc::Weak<RefCell<Option<crate::host::Host>>>,
    otherwise: Rc<dyn NameHost>,
}

impl HostNames {
    /// The names read from `host`'s last members read, and what its embedder knows otherwise.
    #[must_use]
    pub fn new(otherwise: Rc<dyn NameHost>, host: &Rc<RefCell<Option<crate::host::Host>>>) -> Self {
        Self {
            host: Rc::downgrade(host),
            otherwise,
        }
    }

    /// The row the workspace keeps for `member`, as the host last read it.
    fn row(&self, member: &MemberId) -> Option<collab_types::Member> {
        let host = self.host.upgrade()?;
        let host = host.borrow();
        host.as_ref()?.member_rows().get(member).cloned()
    }
}

impl NameHost for HostNames {
    fn name(&self, who: &Actor) -> String {
        let Some(member) = who.member_id() else {
            return self.otherwise.name(who);
        };
        if let Some(row) = self.row(member)
            && !row.name.trim().is_empty()
        {
            return row.name;
        }
        self.otherwise.name(who)
    }

    /// The picture the workspace keeps for `who`, as the address a face reads it from: shown
    /// wherever a view shows them as an author. `None` shows their initials.
    fn picture(&self, who: &Actor) -> Option<String> {
        let member = who.member_id()?;
        let avatar = self.row(member)?.avatar?;
        collab_types::photo_url(&avatar.0).or_else(|| self.otherwise.picture(who))
    }

    /// The colour the workspace paints `who` in, so a byline or a chip in a view wears the
    /// colour of their face on the bar and their caret in every other app.
    fn hue(&self, who: &Actor) -> Option<u8> {
        let member = who.member_id()?;
        let slot = self.row(member)?.colour.slot();
        Some(u8::try_from(slot).unwrap_or_default())
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
        MemberId::agent(id, "ada").into()
    }

    fn app(id: &str) -> Actor {
        Actor::App(AppId::from(id))
    }

    /// What a window with nobody named yet calls each actor: by what they are, never by part
    /// of a key.
    #[test]
    fn a_member_nobody_named_is_named_by_what_they_are_never_by_part_of_their_id() {
        let names = Installed;
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
            assert_eq!(names.name(who), *expected, "{who:?}: {why}");
        }
    }
}

#[cfg(test)]
mod host_names_tests {
    use super::*;
    use crate::runtime::ids::AppId;

    fn person(id: &str) -> Actor {
        MemberId::person(id).into()
    }

    fn agent(id: &str) -> Actor {
        MemberId::agent(id, "ada").into()
    }

    /// A view names people as the host last read the workspace's members' resource, and falls
    /// back to what its embedder knows where the resource names nobody or the host read
    /// nothing yet.
    #[test]
    fn a_member_is_named_by_what_they_are_until_the_members_resource_names_them() {
        let host: Rc<RefCell<Option<crate::host::Host>>> = Rc::default();
        let names = HostNames::new(Rc::new(Installed), &host);
        assert_eq!(
            names.name(&person(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )),
            "Unknown member",
            "before the members are read, nobody is named by their key"
        );
        assert_eq!(names.name(&agent("planner")), "Planner");
        assert_eq!(
            names.name(&Actor::App(AppId::from("so.tilo.lang.today"))),
            "Today",
            "an app is its own name"
        );
        // Nothing a host read is what a test stages: the rows are the replacement for the
        // roster's `tell`, tested where the host is opened over a store
        // (`tests/workspace_members.rs`), and their parse in `collab_types::Member::members_of`.
    }
}
