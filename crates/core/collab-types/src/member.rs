//! A workspace's member list: a person as a surface shows them, and one row
//! per member — person, agent or bot — with its role and the colour it wears
//! everywhere.

use serde::{Deserialize, Serialize};

use tilo_member_types::{MemberId, PersonId};

use crate::identity::AvatarBytes;
use crate::presence::PresenceHue;
use crate::role::Role;

/// `word` as a roster carries a colour, `rrggbb` or `#rrggbb`, as the presence hue nearest it.
#[must_use]
pub fn hue_of_word(word: &str) -> Option<PresenceHue> {
    let digits = word.trim().trim_start_matches('#');
    let value = u32::from_str_radix(digits.get(..6)?, 16).ok()?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "each channel is a byte of the six hex digits read"
    )]
    let (red, green, blue) = ((value >> 16) as u8, (value >> 8) as u8, value as u8);
    PresenceHue::nearest_to_srgb(red, green, blue)
}

/// A person as a surface shows them, in the colour they wear there.
///
/// Every field is written, absent ones too: the collaboration wire is postcard, which reads
/// fields by position, so a field left out shifts every one after it and the frame does not
/// decode.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Person {
    /// Them: their bare root key, the one spelling every record stores.
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
        MemberId::Person(self.id.clone())
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
    /// Whether the workspace removed them: the row is kept so their name and
    /// role still read where a change of theirs is shown.
    #[serde(default)]
    pub removed: bool,
    /// Whether the workspace's members made them an administrator
    /// ([`crate::role::Role::Owner`] is the owner's alone).
    #[serde(default)]
    pub administrator: bool,
    /// How many permissions the workspace gives them beyond their role
    /// ([`tilo_action_policy::WorkspacePolicy::extra_grants`]).
    #[serde(default)]
    pub grants: u32,
}

impl Member {
    /// The workspace's members as its `workspace/members` resource answers them, `me` the member
    /// the reading is for: each row's actor, the name the workspace keeps, the role it gives,
    /// whether it removed them, when it admitted them and the picture they set. A store-held
    /// members' record names bare actors, with no such row around them, and each is a member the
    /// workspace names not.
    #[must_use]
    pub fn members_of(content: &serde_json::Value, me: &MemberId) -> Vec<Self> {
        content
            .get("people")
            .and_then(serde_json::Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| Self::from_members_row(row, me))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One row of [`Self::members_of`]: its `actor` with what the workspace says of them, or the
    /// actor alone where the store kept no row.
    #[must_use]
    pub fn from_members_row(row: &serde_json::Value, me: &MemberId) -> Option<Self> {
        let who: tilo_member_types::Actor =
            serde_json::from_value(row.get("actor").cloned().unwrap_or_else(|| row.clone()))
                .ok()?;
        let id = who.member_id()?;
        let name = row
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let role = row
            .get("role")
            .and_then(|role| serde_json::from_value::<Role>(role.clone()).ok());
        let mut member = match id.as_person() {
            Some(person) => Self::of_person(&Person::new(person.as_str(), name), role),
            None => Self::of_software(id.clone(), name),
        };
        // The colour the row carries, where its writer gave one (a member's own), so a face
        // wears the workspace's colour rather than one this device derives again.
        if let Some(colour) = row
            .get("colour")
            .and_then(serde_json::Value::as_str)
            .and_then(hue_of_word)
        {
            member.colour = colour;
        }
        member.removed = row
            .get("removed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        member.joined_at = row.get("joined_at").and_then(serde_json::Value::as_u64);
        member.avatar = row
            .get("photo")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::photo::avatar_of);
        member.is_self = *id == *me;
        Some(member)
    }

    /// This member's row of the workspace's `workspace/members` resource, the one shape every
    /// surface reads them in ([`Self::from_members_row`] reads it back): their actor, name and role,
    /// whether the workspace removed them, whether they own it (`owner`) or administer it, when it
    /// admitted them and the picture they set.
    #[must_use]
    pub fn members_row(&self, owner: bool) -> serde_json::Value {
        let mut row = serde_json::Map::new();
        let mut put = |field: &str, value: serde_json::Value| {
            row.insert(field.to_owned(), value);
        };
        put(
            "actor",
            serde_json::json!(tilo_member_types::Actor::Member(self.id.clone())),
        );
        put("name", serde_json::json!(self.name));
        // The colour they wear, as the roster's own row carries it: every surface draws their
        // face in this one hue, the chosen colour it holds rather than one a device derives.
        let (red, green, blue) = crate::presence::identity_rgb(self.colour);
        put(
            "colour",
            serde_json::json!(format!("{red:02x}{green:02x}{blue:02x}")),
        );
        put("removed", serde_json::json!(self.removed));
        put("owner", serde_json::json!(owner));
        put("administrator", serde_json::json!(self.administrator));
        if let Some(role) = self.role {
            put("role", serde_json::json!(role));
        }
        if let Some(joined) = self.joined_at {
            put("joined_at", serde_json::json!(joined));
        }
        if let Some(photo) = self
            .avatar
            .as_ref()
            .and_then(|avatar| crate::photo_url(&avatar.0))
        {
            put("photo", serde_json::json!(photo));
        }
        serde_json::Value::Object(row)
    }

    /// `id`'s row named `name`, with no role the workspace has decided and the colour their kind
    /// gives them.
    #[must_use]
    pub fn named(id: MemberId, name: impl Into<String>) -> Self {
        let name = name.into();
        match id {
            MemberId::Person(person) => Self::of_person(&Person::new(person, name), None),
            id => Self::of_software(id, name),
        }
    }

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
            removed: false,
            administrator: false,
            grants: 0,
        }
    }

    /// An agent's or bot's row, in the colour what it is wears ([`PresenceHue::of_member`]).
    #[must_use]
    pub fn of_software(id: MemberId, name: impl Into<String>) -> Self {
        Self {
            colour: PresenceHue::of_member(&id),
            id,
            role: None,
            name: name.into(),
            avatar: None,
            joined_at: None,
            is_self: false,
            removed: false,
            administrator: false,
            grants: 0,
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

    /// Their root key, when they are a person: the hex of their id, decoded.
    #[must_use]
    pub fn root(&self) -> Option<[u8; 32]> {
        self.id.as_person()?.root()
    }

    /// [`Self::colour`] as a roster's wire carries it: the sRGB colour packed
    /// `0x00RRGGBB`.
    #[must_use]
    pub fn packed_colour(&self) -> u32 {
        let (red, green, blue) = crate::presence::identity_rgb(self.colour);
        (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)
    }
}

/// The colour a roster row's packed `0x00RRGGBB` ([`Member::packed_colour`]) names: the
/// presence hue nearest it, `None` for `0` or a grey.
#[must_use]
pub fn colour_from_packed(packed: u32) -> Option<crate::presence::PresenceHue> {
    if packed == 0 {
        return None;
    }
    let [_, r, g, b] = packed.to_be_bytes();
    crate::presence::PresenceHue::nearest_to_srgb(r, g, b)
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

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// A member as a stored row: its colour, its encoding and the spelling of its id.
        fn a_member_row_keeps_its_colour_and_its_stored_spelling;

        phase a_person_and_software_wear_the_colour_of_who_they_are;
        {
            let ada = Person::new("ada", "Ada");
            assert_eq!(ada.colour, PresenceHue::of_identity("ada"));
            assert_eq!(
                Person::new("ada", "Ada")
                    .wearing(PresenceHue::Violet)
                    .colour,
                PresenceHue::Violet
            );
            let helper = Member::of_software(MemberId::agent("helper", "ada"), "Helper");
            assert_eq!(helper.colour, PresenceHue::of_member(&helper.id));
            assert_eq!(
                helper.colour,
                Member::of_software(MemberId::agent("helper", "ada"), "Helper").colour,
                "the same agent is one colour however often it is worked out"
            );
        }

        phase a_member_row_is_written_as_it_is_read;
        {
            let row = Member::of_person(&Person::new("ada", "Ada"), Some(Role::Editor))
                .claiming_self()
                .joined_at(Some(7));
            let json = serde_json::to_string(&row).unwrap();
            assert_eq!(serde_json::from_str::<Member>(&json).unwrap(), row);
        }

        phase a_row_carries_the_colour_its_reader_draws_the_member_in;
        {
            let row = Member::of_person(
                &Person::new("ada", "Ada").wearing(PresenceHue::Violet),
                Some(Role::Editor),
            );
            let json = row.members_row(false);
            let colour = json
                .get("colour")
                .and_then(serde_json::Value::as_str)
                .expect("a member's row names the colour they wear");
            assert_eq!(
                hue_of_word(colour),
                Some(PresenceHue::Violet),
                "the colour a row writes reads back as the hue it was written from"
            );
            let read = Member::from_members_row(&json, &MemberId::person("ada"))
                .expect("a members resource's row reads as a member");
            assert_eq!(
                read.colour,
                PresenceHue::Violet,
                "the reader draws them in the workspace's colour, not one it derives again"
            );
        }

        phase a_members_resource_names_each_row_it_answers;
        {
            // The workspace's `workspace/members` resource: each row's actor with the name the
            // workspace keeps, its role, join time and picture; a store-held record names bare
            // actors and makes the reader their own where it names them.
            let photo = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAMAAAAoyzS7AAAAA1BMVEX///+nxBvIAAAAAXRSTlMAQObYZgAAAAxJREFUeNoBgAEAAP//AwABWgH+AAAAAElFTkSuQmCC";
            let content = serde_json::json!({
                "people": [
                    {
                        "actor": serde_json::to_value(tilo_member_types::Actor::Member(
                            MemberId::person("ada")
                        ))
                        .unwrap(),
                        "name": "Ada",
                        "role": "editor",
                        "joined_at": 7,
                        "photo": photo,
                    },
                    {
                        "actor": serde_json::to_value(tilo_member_types::Actor::Member(
                            MemberId::agent("scout", "ada")
                        ))
                        .unwrap(),
                        "name": "Scout",
                        "removed": true,
                    },
                ],
            });
            let rows = Member::members_of(&content, &MemberId::person("ada"));
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].name, "Ada");
            assert_eq!(rows[0].role, Some(Role::Editor));
            assert_eq!(rows[0].joined_at, Some(7));
            assert_eq!(
                rows[0].avatar.as_ref().map(|avatar| avatar.0.len()),
                crate::photo::avatar_of(photo).map(|avatar| avatar.0.len())
            );
            assert!(rows[0].is_self);
            assert_eq!(rows[1].name, "Scout");
            assert!(rows[1].removed);
            assert!(!rows[1].is_self);
            // A record held in a process for a device of its own names bare actors.
            let bare = serde_json::json!({
                "people": [serde_json::to_value(tilo_member_types::Actor::Member(
                    MemberId::person("bo")
                ))
                .unwrap()],
            });
            let rows = Member::members_of(&bare, &MemberId::person("ada"));
            assert_eq!(rows.len(), 1);
            assert!(rows[0].name.is_empty(), "a bare actor names nobody");
            assert!(!rows[0].is_self);
        }
    }
}
