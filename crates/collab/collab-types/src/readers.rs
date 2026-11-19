//! Who may read a document of a workspace, and the one derivation its content key is scoped by.
//!
//! A reader set is written as text, the way a workspace's resource store names it in its
//! document ids (`<readers>/<owner>#<n>`): [`EVERY_MEMBER`] for every member of the workspace, or
//! the people who read it, each by their root key in lowercase hex, sorted and joined by `,`.
//! Each set has one key group: every member's is the workspace's own group, and any other set's
//! is a group of its own ([`reader_group`]) whose keys only its people's devices hold.
//!
//! Content is sealed under its group's keys scoped to the document it belongs to
//! ([`content_artifact_id`]): the same key never opens another document's content, nor the
//! content of the same document id under other readers or in another workspace.

use std::collections::BTreeSet;

use crate::{ArtifactId, WorkspaceId, op::derived_uuid};

/// The reader set of every member of a workspace: what its own key group reads.
pub const EVERY_MEMBER: &str = "*";

const CONTENT_ARTIFACT_DOMAIN: &[u8] = b"tilo-content-artifact-v1\0";
const READER_GROUP_DOMAIN: &[u8] = b"tilo-reader-group-v1\0";

/// A reader set, read from its text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readers {
    /// Every member of the workspace.
    EveryMember,
    /// These people, by their root keys, and no one else.
    People(BTreeSet<[u8; 32]>),
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

    /// The set's text: what a document id and [`reader_group`] name it by.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::EveryMember => EVERY_MEMBER.to_owned(),
            Self::People(people) => people.iter().map(hex::encode).collect::<Vec<_>>().join(","),
        }
    }
}

/// The key group of `readers` in `workspace`: the workspace's own group for every member, else
/// a group of that set's alone, the same on every device. `None` when `readers` names no set.
#[must_use]
pub fn reader_group(workspace: WorkspaceId, readers: &str) -> Option<WorkspaceId> {
    match Readers::parse(readers)? {
        Readers::EveryMember => Some(workspace),
        Readers::People(_) => Some(WorkspaceId::from_uuid(derived_uuid(
            READER_GROUP_DOMAIN,
            &[&workspace.to_string(), readers],
        ))),
    }
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
