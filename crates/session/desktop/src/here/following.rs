//! Who follows whom: each person's page says whose view it follows on its
//! live presence ([`super::LiveMember::following`]), so every roster can
//! say "Following Ada" on a row, and "Following you" on the row of someone
//! following this person.

use super::{HereState, Person};

impl HereState {
    /// Follow the member `publisher` from now on (`None`: stop), for this
    /// page's presence to say. Returns whether that changed.
    pub fn set_following(&mut self, publisher: Option<String>) -> bool {
        if self.following == publisher {
            return false;
        }
        self.following = publisher;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    /// Whose view this page follows, by their publisher.
    #[must_use]
    pub fn following(&self) -> Option<&str> {
        self.following.as_deref()
    }

    /// Who follows whom here, `(follower, followed)`, each as the roster
    /// names them: a row of `people`, or this page's own publisher for this
    /// person. A device of someone counts as them; a pair naming anyone not
    /// here is left out.
    #[must_use]
    pub fn following_pairs(&self, people: &[Person]) -> Vec<(String, String)> {
        let own = self.own_id();
        let row = |publisher: &str| -> Option<String> {
            if self.is_own_person(publisher) {
                return own.map(str::to_owned);
            }
            let person = self.person_of(publisher);
            people
                .iter()
                .find(|row| !row.agent && self.person_of(&row.id) == person)
                .map(|row| row.id.clone())
        };
        let mut pairs: Vec<(String, String)> = self
            .members
            .iter()
            .flatten()
            .filter(|member| member.kind != "agent" && !self.is_own_person(&member.publisher))
            .filter_map(|member| {
                let target = member.following.as_deref()?;
                Some((row(&member.publisher)?, row(target)?))
            })
            .chain(
                own.zip(self.following.as_deref())
                    .and_then(|(own, target)| Some((own.to_owned(), row(target)?))),
            )
            .filter(|(follower, followed)| follower != followed)
            .collect();
        pairs.sort();
        pairs.dedup();
        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::super::LiveMember;
    use super::*;

    fn member(publisher: &str, following: Option<&str>) -> LiveMember {
        LiveMember {
            publisher: publisher.into(),
            kind: "person".into(),
            label: publisher.to_uppercase(),
            focus: None,
            activity: None,
            place: None,
            user: None,
            wave: None,
            layout: None,
            following: following.map(str::to_owned),
        }
    }

    fn person(id: &str) -> Person {
        Person {
            id: id.into(),
            label: id.to_uppercase(),
            rgb: (1, 2, 3),
            agent: false,
            tab: None,
            place: None,
            status: None,
            recent: None,
            screen: None,
        }
    }

    /// Ben follows Ada, Cy follows this page's person, and this page
    /// follows Ben: each pair names rows of the roster, this person by
    /// their own publisher; someone following a person not here says
    /// nothing.
    #[test]
    fn every_roster_says_who_follows_whom() {
        let mut here = HereState::default();
        here.set_members(
            Some(member("me", None)),
            Some(vec![
                member("me", None),
                member("ada", None),
                member("ben", Some("ada")),
                member("cy", Some("me")),
                member("dee", Some("gone")),
            ]),
        );
        let people = [person("ada"), person("ben"), person("cy"), person("dee")];
        assert!(here.set_following(Some("ben".into())));
        assert!(!here.set_following(Some("ben".into())), "no change");
        assert_eq!(
            here.following_pairs(&people),
            vec![
                ("ben".into(), "ada".into()),
                ("cy".into(), "me".into()),
                ("me".into(), "ben".into()),
            ]
        );
        here.set_following(None);
        assert_eq!(here.following(), None);
        assert_eq!(here.following_pairs(&people).len(), 2);
    }
}
