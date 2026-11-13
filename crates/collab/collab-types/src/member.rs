//! A workspace's member list: a person as a surface shows them, and one row
//! per member — person, agent or bot — with its role and the colour it wears
//! everywhere.

use serde::{Deserialize, Serialize};

use tilo_action_policy::MemberId;

use crate::identity::AvatarBytes;
use crate::presence::PresenceHue;
use crate::role::Role;

/// A person, by the lowercase hex of their root public key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PersonId(pub String);

impl PersonId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for PersonId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for PersonId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl std::fmt::Display for PersonId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A person as a surface shows them, in the colour they wear there.
///
/// Every field is written, absent ones too: the collaboration wire is postcard, which reads
/// fields by position, so a field left out shifts every one after it and the frame does not
/// decode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    pub id: PersonId,
    pub name: String,
    #[serde(default)]
    pub avatar: Option<AvatarBytes>,
    /// Their own colour: the one they chose, else the one their identity derives
    /// ([`PresenceHue::of_identity`]).
    pub colour: PresenceHue,
}

impl Person {
    /// `id` under `name`, in the colour their identity derives.
    #[must_use]
    pub fn new(id: impl Into<PersonId>, name: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            colour: PresenceHue::of_identity(id.as_str()),
            id,
            name: name.into(),
            avatar: None,
        }
    }

    /// The same person in the colour they chose.
    #[must_use]
    pub fn wearing(mut self, colour: PresenceHue) -> Self {
        self.colour = colour;
        self
    }

    #[must_use]
    pub fn with_avatar(mut self, avatar: AvatarBytes) -> Self {
        self.avatar = Some(avatar);
        self
    }

    #[must_use]
    pub fn member_id(&self) -> MemberId {
        MemberId::Person(self.id.0.clone())
    }
}

/// One row of a workspace's member list.
///
/// Every field is written, absent ones too, as for [`Person`]: an app's roster reaches it over
/// the postcard collaboration wire, where a row that left out its role or avatar would not
/// decode and the app would lose its connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: MemberId,
    /// The role preset they hold: none for software, which acts on the grants
    /// its members gave it, and none for a person this list has no policy row
    /// for.
    #[serde(default)]
    pub role: Option<Role>,
    pub name: String,
    #[serde(default)]
    pub avatar: Option<AvatarBytes>,
    /// Assigned once, when they became a member; every surface reads it.
    pub colour: PresenceHue,
    /// When the workspace admitted them, Unix milliseconds.
    #[serde(default)]
    pub joined_at: Option<u64>,
    /// Whether this row is the reader's own.
    #[serde(default)]
    pub is_self: bool,
}

impl Member {
    /// `person`'s row in a workspace they hold `role` in.
    #[must_use]
    pub fn of_person(person: &Person, role: Option<Role>) -> Self {
        Self {
            id: person.member_id(),
            role,
            name: person.name.clone(),
            avatar: person.avatar.clone(),
            colour: person.colour,
            joined_at: None,
            is_self: false,
        }
    }

    /// An agent's or bot's row: its colour is assigned here, once, stepped clear
    /// of the person it acts for ([`PresenceHue::of_participant`]).
    #[must_use]
    pub fn of_software(
        id: MemberId,
        name: impl Into<String>,
        for_person: Option<&PersonId>,
    ) -> Self {
        Self {
            colour: assigned_colour(&id, for_person),
            id,
            role: None,
            name: name.into(),
            avatar: None,
            joined_at: None,
            is_self: false,
        }
    }

    #[must_use]
    pub fn claiming_self(mut self) -> Self {
        self.is_self = true;
        self
    }

    #[must_use]
    pub fn joined_at(mut self, joined_at: Option<u64>) -> Self {
        self.joined_at = joined_at;
        self
    }

    #[must_use]
    pub fn with_avatar(mut self, avatar: AvatarBytes) -> Self {
        self.avatar = Some(avatar);
        self
    }

    /// [`Self::colour`] as a roster's wire carries it: the sRGB colour packed
    /// `0x00RRGGBB`.
    #[must_use]
    pub fn packed_colour(&self) -> u32 {
        let (red, green, blue) = crate::presence::identity_rgb(self.colour);
        (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
    }
}

/// The admission time a roster's packed value names: `None` for `u64::MAX`.
#[must_use]
pub const fn joined_at_from_packed(packed: u64) -> Option<u64> {
    if packed == u64::MAX {
        None
    } else {
        Some(packed)
    }
}

/// The colour a member is given: a person's own, software's stepped clear of the
/// person it acts for. The one assignment; every surface reads what it stored.
#[must_use]
pub fn assigned_colour(id: &MemberId, for_person: Option<&PersonId>) -> PresenceHue {
    PresenceHue::of_participant(&id.key(), !id.is_person(), for_person.map(PersonId::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_wears_their_own_colour_and_software_one_clear_of_it() {
        let ada = Person::new("ada", "Ada");
        assert_eq!(ada.colour, PresenceHue::of_identity("ada"));
        assert_eq!(
            Person::new("ada", "Ada")
                .wearing(PresenceHue::Violet)
                .colour,
            PresenceHue::Violet
        );
        let helper = Member::of_software(MemberId::agent("helper"), "Helper", Some(&ada.id));
        assert_eq!(helper.colour, assigned_colour(&helper.id, Some(&ada.id)));
        assert_ne!(helper.colour, ada.colour);
        assert_eq!(
            helper.colour,
            Member::of_software(MemberId::agent("helper"), "Helper", Some(&ada.id)).colour,
            "the same agent is one colour however often it is worked out"
        );
    }

    #[test]
    fn a_member_row_is_written_as_it_is_read() {
        let row = Member::of_person(&Person::new("ada", "Ada"), Some(Role::Editor))
            .claiming_self()
            .joined_at(Some(7));
        let json = serde_json::to_string(&row).unwrap();
        assert_eq!(serde_json::from_str::<Member>(&json).unwrap(), row);
    }

    #[test]
    fn a_stored_id_is_spelt_once() {
        for id in [
            MemberId::person("a3"),
            MemberId::agent("helper"),
            MemberId::bot("bridge"),
        ] {
            assert_eq!(MemberId::parse(&id.key()).as_ref(), Some(&id), "{id}");
        }
        assert_eq!(MemberId::parse("ada"), Some(MemberId::person("ada")));
        assert_eq!(MemberId::parse("agent:"), None);
    }
}
