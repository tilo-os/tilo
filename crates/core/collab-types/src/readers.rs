//! Who may read a document of a workspace, and the one derivation its content key is scoped by.
//!
//! A reader set is written as text, the way a workspace's resource store names it in its
//! document ids (`<readers>/<owner>#<n>`): [`EVERY_MEMBER`] for every member of the workspace, or
//! the people who read it, each by their root key in lowercase hex, sorted and joined by `,`. A
//! set may begin with a governance role (`@role:moderator,<people>`): every member whose role
//! includes it reads it too, whoever holds the role when it is read, so the text never changes
//! when roles do. Each set has one key group: every member's is the workspace's own group, and
//! any other set's is a group of its own ([`reader_group`]) whose keys only its readers' devices
//! hold.
//!
//! A set may instead stay on one device (`@device:<person>`, [`Readers::Device`]): the person
//! who made its records reads them on the device that made them, and nowhere else. It has no key
//! group, so nothing seals it, and a document of it ([`stays_on_device`]) is never written to
//! disk, sent to another device or taken from one.
//!
//! Content is sealed under its group's keys scoped to the document it belongs to
//! ([`content_artifact_id`]): the same key never opens another document's content, nor the
//! content of the same document id under other readers or in another workspace.

use std::collections::BTreeSet;

/// The governance roles a reader set may name ([`Readers::Role`]).
pub use tilo_action_policy::Role;

use crate::{ArtifactId, WorkspaceId, op::derived_uuid};

/// The reader set of every member of a workspace: what its own key group reads.
pub const EVERY_MEMBER: &str = "*";

/// What begins a reader set read by the holders of a role: `@role:<role>`, then its people.
pub const ROLE_READERS: &str = "@role:";

/// What begins a reader set that stays on the device it was written on: `@device:<person>`.
pub const DEVICE_READERS: &str = "@device:";

/// What begins the name a document id gives a set that stays on its device ([`reader_set_name`]).
const DEVICE_NAME: char = '@';

const CONTENT_ARTIFACT_DOMAIN: &[u8] = b"tilo-content-artifact-v1\0";
const READER_GROUP_DOMAIN: &[u8] = b"tilo-reader-group-v1\0";

/// A reader set, read from its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readers {
    /// Every member of the workspace.
    EveryMember,
    /// These people, by their root keys, and no one else.
    People(BTreeSet<[u8; 32]>),
    /// Every member whose governance role includes `role` ([`Role::includes`]) at the moment
    /// the set is read, and these people (none, or more).
    Role {
        role: Role,
        people: BTreeSet<[u8; 32]>,
    },
    /// This person, on the device the record was written on, and no other device of theirs or
    /// anyone's.
    Device([u8; 32]),
}

/// Who a reader set other than every member's names: its people, and the role whose holders
/// read it beside them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedReaders {
    pub role: Option<Role>,
    pub people: BTreeSet<[u8; 32]>,
}

impl NamedReaders {
    /// Whether the person of root key `person`, whose role in the workspace is `role` (none
    /// for one who is no member), reads the set: they are one of its people, or a member whose
    /// role includes the set's.
    #[must_use]
    pub fn includes(&self, person: &[u8; 32], role: Option<Role>) -> bool {
        self.people.contains(person)
            || self
                .role
                .zip(role)
                .is_some_and(|(wanted, held)| held.includes(wanted))
    }
}

impl Readers {
    /// The set `text` names, when it is written canonically: [`EVERY_MEMBER`], or one person or
    /// more, sorted, each once. Any other spelling names no set, so one set has one text and
    /// so one group and one key.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text == EVERY_MEMBER {
            return Some(Self::EveryMember);
        }
        if let Some(person) = text.strip_prefix(DEVICE_READERS) {
            return match Self::parse_people(person)? {
                Self::People(people) if people.len() == 1 => {
                    people.into_iter().next().map(Self::Device)
                }
                _ => None,
            };
        }
        if let Some(rest) = text.strip_prefix(ROLE_READERS) {
            let (key, people) = rest.split_once(',').unwrap_or((rest, ""));
            let role = Role::parse(key).filter(|role| role.key() == key)?;
            let people = if people.is_empty() {
                BTreeSet::new()
            } else {
                match Self::parse_people(people)? {
                    Self::People(people) => people,
                    _ => return None,
                }
            };
            return Some(Self::Role { role, people });
        }
        Self::parse_people(text)
    }

    /// One person or more, sorted, each once, as [`Self::parse`] reads them.
    fn parse_people(text: &str) -> Option<Self> {
        let mut people = BTreeSet::new();
        let mut last: Option<[u8; 32]> = None;
        for person in text.split(',') {
            if person.len() != 64 || person.bytes().any(|byte| byte.is_ascii_uppercase()) {
                return None;
            }
            let mut key = [0_u8; 32];
            hex::decode_to_slice(person, &mut key).ok()?;
            if last.is_some_and(|last| last >= key) {
                return None;
            }
            last = Some(key);
            people.insert(key);
        }
        Some(Self::People(people))
    }

    /// The set of `people`; none reads as no set at all.
    #[must_use]
    pub fn people(people: impl IntoIterator<Item = [u8; 32]>) -> Option<Self> {
        let people: BTreeSet<[u8; 32]> = people.into_iter().collect();
        (!people.is_empty()).then_some(Self::People(people))
    }

    /// The set of the holders of `role` and of `people`.
    #[must_use]
    pub fn role(role: Role, people: impl IntoIterator<Item = [u8; 32]>) -> Self {
        Self::Role {
            role,
            people: people.into_iter().collect(),
        }
    }

    /// The set's text: what a document id and [`reader_group`] name it by.
    #[must_use]
    pub fn text(&self) -> String {
        let people = |people: &BTreeSet<[u8; 32]>| {
            people.iter().map(hex::encode).collect::<Vec<_>>().join(",")
        };
        match self {
            Self::EveryMember => EVERY_MEMBER.to_owned(),
            Self::People(named) => people(named),
            Self::Device(person) => format!("{DEVICE_READERS}{}", hex::encode(person)),
            Self::Role {
                role,
                people: named,
            } if named.is_empty() => {
                format!("{ROLE_READERS}{}", role.key())
            }
            Self::Role {
                role,
                people: named,
            } => {
                format!("{ROLE_READERS}{},{}", role.key(), people(named))
            }
        }
    }

    /// Who the set names, when it is not every member's.
    #[must_use]
    pub fn named(&self) -> Option<NamedReaders> {
        match self {
            Self::EveryMember => None,
            Self::People(people) => Some(NamedReaders {
                role: None,
                people: people.clone(),
            }),
            Self::Role { role, people } => Some(NamedReaders {
                role: Some(*role),
                people: people.clone(),
            }),
            Self::Device(person) => Some(NamedReaders {
                role: None,
                people: BTreeSet::from([*person]),
            }),
        }
    }
}

/// The key group of `readers` in `workspace`: the workspace's own group for every member, else
/// a group of that set's alone, the same on every device. `None` when `readers` names no set, or
/// one that stays on its device, which no key seals.
#[must_use]
pub fn reader_group(workspace: WorkspaceId, readers: &str) -> Option<WorkspaceId> {
    match Readers::parse(readers)? {
        Readers::EveryMember => Some(workspace),
        Readers::Device(_) => None,
        Readers::People(_) | Readers::Role { .. } => Some(WorkspaceId::from_uuid(derived_uuid(
            READER_GROUP_DOMAIN,
            &[&workspace.to_string(), readers],
        ))),
    }
}

/// The name a document id gives the reader set `readers`: [`EVERY_MEMBER`] as itself, any other
/// set as the first 16 hex digits of its BLAKE3 digest, so a long list of people is in no id. A
/// set that stays on its device is named so too, after `@`, so its documents say so by their id.
#[must_use]
pub fn reader_set_name(readers: &str) -> String {
    if readers == EVERY_MEMBER {
        return readers.to_owned();
    }
    let digest = blake3::hash(readers.as_bytes()).to_hex();
    let digest = digest.as_str().get(..16).unwrap_or_default();
    if readers.starts_with(DEVICE_READERS) {
        format!("{DEVICE_NAME}{digest}")
    } else {
        digest.to_owned()
    }
}

/// Whether `document` is a store document of a reader set that stays on its device
/// ([`Readers::Device`]): never written to disk, sealed, sent to another device or taken from
/// one.
#[must_use]
pub fn stays_on_device(document: &str) -> bool {
    store_document_readers(document).is_some_and(|name| name.starts_with(DEVICE_NAME))
}

/// The reader set's name a document of a workspace's resource store carries in its id
/// (`<readers>/<owner>#<n>`, or a content document `<readers>/<kind>/<id>~<n>`); `None` for any
/// other document, which is an app's own.
#[must_use]
pub fn store_document_readers(document: &str) -> Option<&str> {
    let (readers, rest) = document.split_once('/')?;
    let digest = readers.strip_prefix(DEVICE_NAME).unwrap_or(readers);
    let named = readers == EVERY_MEMBER
        || (digest.len() == 16 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let numbered = rest.rsplit_once(['#', '~']).is_some_and(|(_, number)| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    });
    (named && numbered).then_some(readers)
}

/// The kind and id of the resource whose content `document` holds, for a content document of a
/// workspace's resource store (`<readers>/<kind>/<id>~<n>`); `None` for any other document.
#[must_use]
pub fn store_content_resource(document: &str) -> Option<(&str, &str)> {
    store_document_readers(document)?;
    let (_, rest) = document.split_once('/')?;
    let (named, _) = rest.rsplit_once('~')?;
    let (kind, id) = named.split_once('/')?;
    (!kind.is_empty() && !id.is_empty()).then_some((kind, id))
}

/// What a document of resources a workspace's store placed (`<readers>/<owner>#<n>`) holds the
/// resources of: those nested in the resource `owner`, or of the kind `owner` and nested in
/// nothing. `None` for any other document.
#[must_use]
pub fn placed_owner(document: &str) -> Option<&str> {
    store_document_readers(document)?;
    let (_, placed) = document.split_once('/')?;
    Some(placed.rsplit_once('#')?.0)
}

/// Whether `document` is a document a workspace's resource store placed.
#[must_use]
pub fn is_store_document(document: &str) -> bool {
    store_document_readers(document).is_some()
}

/// What content of document `document` of `workspace`, read by `readers`, is sealed for: its
/// group's keys open it only under this id. Every sealed document derives its key scope here,
/// a document of an app as [`app_document_id`] names it.
#[must_use]
pub fn content_artifact_id(workspace: &str, readers: &str, document: &str) -> ArtifactId {
    ArtifactId(derived_uuid(
        CONTENT_ARTIFACT_DOMAIN,
        &[workspace, readers, document],
    ))
}

/// The id of `document` of `app` among its workspace's: an app id names no `/`, so no two
/// apps' documents share one.
#[must_use]
pub fn app_document_id(app: &str, document: &str) -> String {
    format!("{app}/{document}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set that stays on its device names its documents so, and has no key group to seal them
    /// for another device with.
    #[test]
    fn a_set_that_stays_on_its_device_has_no_key_group_and_says_so_in_its_documents() {
        let person = [7_u8; 32];
        let text = Readers::Device(person).text();
        assert_eq!(Readers::parse(&text), Some(Readers::Device(person)));
        assert_eq!(
            reader_group(WorkspaceId::from_uuid(uuid::Uuid::nil()), &text),
            None
        );
        let document = format!("{}/browser.Page#0", reader_set_name(&text));
        assert!(stays_on_device(&document), "{document}");
        assert!(is_store_document(&document));
        let shared = format!("{}/browser.Page#0", reader_set_name(&hex::encode(person)));
        assert!(!stays_on_device(&shared));
        assert!(!stays_on_device("*/browser.Page#0"));
        assert_eq!(Readers::parse("@device:ab"), None);
    }

    #[test]
    fn a_store_document_is_told_from_an_apps_own_document_by_its_name() {
        assert_eq!(store_document_readers("*/tasks.issue#0"), Some("*"));
        assert!(is_store_document("0123456789abcdef/board-7#12"));
        assert!(is_store_document("*/tasks.issue/i-1~0"));
        assert!(!is_store_document("derived:default@tab:0"));
        assert!(!is_store_document("notes/archive"));
        assert!(!is_store_document("*/tasks.issue"));
        assert_eq!(
            store_content_resource("*/files.tree/derived:files.tree@tab:0~0"),
            Some(("files.tree", "derived:files.tree@tab:0"))
        );
        assert_eq!(store_content_resource("*/tasks.issue#0"), None);
        assert_eq!(store_content_resource("derived:default@tab:0"), None);
        assert_eq!(reader_set_name(EVERY_MEMBER), EVERY_MEMBER);
        assert_eq!(reader_set_name(&"a".repeat(64)).len(), 16);
    }

    #[test]
    fn a_reader_set_has_one_text_one_group_and_its_own_key_scope() {
        let (ada, bob) = ([0xaa_u8; 32], [0xbb_u8; 32]);
        let both = Readers::people([bob, ada]).unwrap();
        let text = both.text();
        assert_eq!(Readers::parse(&text), Some(both));
        assert_eq!(Readers::parse(EVERY_MEMBER), Some(Readers::EveryMember));
        // Another spelling of the same people names no set.
        let reversed = format!("{},{}", hex::encode(bob), hex::encode(ada));
        assert_eq!(Readers::parse(&reversed), None);
        assert_eq!(Readers::parse(&text.to_uppercase()), None);
        assert_eq!(Readers::parse(""), None);
        assert_eq!(Readers::people([]), None);

        let workspace = WorkspaceId::new();
        assert_eq!(reader_group(workspace, EVERY_MEMBER), Some(workspace));
        let group = reader_group(workspace, &text).unwrap();
        assert_ne!(group, workspace);
        assert_eq!(reader_group(workspace, &text), Some(group));
        assert_ne!(reader_group(WorkspaceId::new(), &text), Some(group));
        assert_eq!(reader_group(workspace, &reversed), None);

        // A role's set is its own text, group and scope, read back as written.
        let moderators = Readers::role(Role::Moderator, [ada]);
        let held = moderators.text();
        assert_eq!(held, format!("@role:moderator,{}", hex::encode(ada)));
        assert_eq!(Readers::parse(&held), Some(moderators));
        let alone = Readers::role(Role::Moderator, []);
        assert_eq!(alone.text(), "@role:moderator");
        assert_eq!(Readers::parse("@role:moderator"), Some(alone));
        assert_eq!(Readers::parse("@role:Moderator"), None);
        assert_eq!(Readers::parse("@role:janitor"), None);
        assert_ne!(reader_group(workspace, &held), Some(group));
        assert_ne!(reader_group(workspace, &held), Some(workspace));
        let named = Readers::parse(&held).and_then(|set| set.named()).unwrap();
        assert!(named.includes(&ada, None), "its people read it");
        assert!(
            named.includes(&bob, Some(Role::Moderator)),
            "so does a moderator"
        );
        assert!(
            named.includes(&bob, Some(Role::Owner)),
            "and the owner, whose role includes it"
        );
        assert!(
            !named.includes(&bob, Some(Role::Editor)),
            "an editor does not"
        );
        assert!(!named.includes(&bob, None), "nor does one who is no member");

        let id = workspace.to_string();
        let scope = content_artifact_id(&id, &text, "doc");
        assert_ne!(scope, content_artifact_id(&id, EVERY_MEMBER, "doc"));
        assert_ne!(scope, content_artifact_id(&id, &text, "doc2"));
        assert_ne!(
            content_artifact_id(&id, EVERY_MEMBER, &app_document_id("so.tilo.notes", "a")),
            content_artifact_id(&id, EVERY_MEMBER, &app_document_id("so.tilo.tasks", "a")),
        );
    }
}
