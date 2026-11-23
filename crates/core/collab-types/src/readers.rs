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

/// The name a document id gives the reader set `readers`: [`EVERY_MEMBER`] as itself, any other
/// set as the first 16 hex digits of its BLAKE3 digest, so a long list of people is in no id.
#[must_use]
pub fn reader_set_name(readers: &str) -> String {
    if readers == EVERY_MEMBER {
        return readers.to_owned();
    }
    let digest = blake3::hash(readers.as_bytes()).to_hex();
    digest.as_str().get(..16).unwrap_or_default().to_owned()
}

/// The reader set's name a document of a workspace's resource store carries in its id
/// (`<readers>/<owner>#<n>`, or a content document `<readers>/<kind>/<id>~<n>`); `None` for any
/// other document, which is an app's own.
#[must_use]
pub fn store_document_readers(document: &str) -> Option<&str> {
    let (readers, rest) = document.split_once('/')?;
    let named = readers == EVERY_MEMBER
        || (readers.len() == 16 && readers.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let numbered = rest.rsplit_once(['#', '~']).is_some_and(|(_, number)| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    });
    (named && numbered).then_some(readers)
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

    #[test]
    fn a_store_document_is_told_from_an_apps_own_document_by_its_name() {
        assert_eq!(store_document_readers("*/tasks.issue#0"), Some("*"));
        assert!(is_store_document("0123456789abcdef/board-7#12"));
        assert!(is_store_document("*/tasks.issue/i-1~0"));
        assert!(!is_store_document("derived:default@tab:0"));
        assert!(!is_store_document("notes/archive"));
        assert!(!is_store_document("*/tasks.issue"));
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
