//! A member's own record in a workspace: one record of a kind per member
//! (what they are writing and have not sent, their mailbox, their settings),
//! kept in the workspace beside its other records and synced with it to that
//! member's devices, and to nobody else's.
//!
//! Which kinds are a member's own is what the app's manifest declares
//! (`[resources] own`). A write of one carries its kind and its person on the
//! wire ([`OwnResource`]), so a host, a device on a direct link and a policy
//! decide from that alone: the record's id names neither. The id is derived
//! from both, so every device of the person finds the same record without
//! asking, and a host checks that a write's record is the one its kind and
//! person name ([`OwnResource::names`]).

use serde::{Deserialize, Serialize};

/// Domain separation for the id derivation.
const DOMAIN: &str = "tilo own record v1";

/// A member's own record of a workspace: its kind, which the app's manifest
/// declares each member's own, and its person, by their user root key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OwnResource {
    /// The record kind (`settings.root`, `mail.mailbox`).
    pub kind: String,
    /// The user root key of the person whose record it is.
    pub owner: [u8; 32],
}

impl OwnResource {
    /// `owner`'s record of `kind`; `None` when `kind` is not a label.
    #[must_use]
    pub fn new(kind: &str, owner: [u8; 32]) -> Option<Self> {
        let label = kind.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && kind.len() <= 64
            && kind
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        label.then(|| Self {
            kind: kind.to_owned(),
            owner,
        })
    }

    /// The record of `kind` of the person a person
    /// id names (the hex of their user root key); `None` for anything else.
    #[must_use]
    pub fn of_person(person: &str, kind: &str) -> Option<Self> {
        let mut owner = [0; 32];
        hex::decode_to_slice(person, &mut owner).ok()?;
        Self::new(kind, owner)
    }

    /// The person's id: the lowercase hex of
    /// their user root key.
    #[must_use]
    pub fn person(&self) -> String {
        hex::encode(self.owner)
    }

    /// The record's id: a version-8 uuid derived from its kind and person,
    /// the same on every device of theirs, naming neither.
    #[must_use]
    pub fn document_id(&self) -> String {
        let mut hasher = blake3::Hasher::new_derive_key(DOMAIN);
        hasher.update(&self.owner);
        hasher.update(self.kind.as_bytes());
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
        format!(
            "uuid:{}",
            uuid::Builder::from_custom_bytes(bytes).into_uuid()
        )
    }

    /// Whether `record` is this record's id ([`Self::document_id`]).
    #[must_use]
    pub fn names(&self, record: &str) -> bool {
        self.document_id() == record
    }

    /// The person `owner`'s own record `record` of `app` is, of a kind the
    /// app's manifest declares each member's own ([`own_kinds`]); `None` for
    /// any other record. What a device of that person writes into the
    /// record's frames: nobody else derives the id.
    #[must_use]
    pub fn recognise(app: &str, record: &str, owner: [u8; 32]) -> Option<Self> {
        own_kinds(app)
            .into_iter()
            .filter_map(|kind| Self::new(&kind, owner))
            .find(|own| own.names(record))
    }
}

/// The kinds `app`'s manifest declares each member keeps as their own
/// (`[resources] own`, a tilolang package's `records: .own`): a built-in
/// app's or package's as the build carries them, a package installed since as
/// its surface marks them ([`crate::app_surface::own_resource_types`]).
#[must_use]
pub fn own_kinds(app: &str) -> Vec<String> {
    let mut kinds: Vec<String> = app_fleet::own_kinds(app).map(str::to_owned).collect();
    for kind in crate::app_surface::own_resource_types(app) {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_own_records_id_is_its_kind_and_persons_alone() {
        let ada = OwnResource::new("chat.drafts", [1; 32]).unwrap();
        assert_eq!(
            ada.document_id(),
            OwnResource::new("chat.drafts", [1; 32])
                .unwrap()
                .document_id()
        );
        assert!(ada.names(&ada.document_id()));
        assert!(crate::document::DocumentId::parse(&ada.document_id()).is_some());
        let bo = OwnResource::new("chat.drafts", [2; 32]).unwrap();
        let other_kind = OwnResource::new("mail.mailbox", [1; 32]).unwrap();
        assert_ne!(ada.document_id(), bo.document_id());
        assert_ne!(ada.document_id(), other_kind.document_id());
        assert!(!ada.document_id().contains("chat.drafts"));
        assert!(!ada.document_id().contains(&ada.person()));
        assert_eq!(
            OwnResource::of_person(&ada.person(), "chat.drafts"),
            Some(ada)
        );
        for kind in ["", "1kind", "a@b", &"k".repeat(65)] {
            assert_eq!(OwnResource::new(kind, [1; 32]), None, "{kind}");
        }
        assert_eq!(OwnResource::of_person("not hex", "chat.drafts"), None);
    }

    #[test]
    fn a_device_recognises_its_own_record_of_a_kind_the_manifest_declares() {
        let mailbox = OwnResource::new("mail.mailbox", [3; 32]).unwrap();
        assert_eq!(
            OwnResource::recognise("so.tilo.mail", &mailbox.document_id(), [3; 32]),
            Some(mailbox.clone())
        );
        assert_eq!(
            OwnResource::recognise("so.tilo.mail", &mailbox.document_id(), [4; 32]),
            None,
            "another person derives another id"
        );
        let package = OwnResource::new("tl.so.tilo.shell", [3; 32]).unwrap();
        assert_eq!(
            OwnResource::recognise("so.tilo.shell", &package.document_id(), [3; 32]),
            Some(package),
            "a built-in tilolang package declaring `records: .own` keeps its record kind as each member's own"
        );
    }
}
