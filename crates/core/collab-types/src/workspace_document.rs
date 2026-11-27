//! Records that apps keep for the whole workspace, beside their other records.
//! Each kind has one document per workspace, reached on the owner's route.
//!
//! Other apps can join a shared workspace record from their own windows;
//! the workspace's grants on its owner still decide every read and change.

pub use app_fleet::{
    EARLIER_RECORDS, WORKSPACE_DOCUMENTS, WorkspaceDocument,
    derived_record as workspace_document_id,
};

/// The workspace document of `kind`, whichever app keeps it.
#[must_use]
pub fn workspace_document_of_kind(kind: &str) -> Option<&'static WorkspaceDocument> {
    WORKSPACE_DOCUMENTS.iter().find(|known| known.kind == kind)
}

/// The workspace document `app` keeps under `document`, when it is one.
#[must_use]
pub fn workspace_document(app: &str, document: &str) -> Option<&'static WorkspaceDocument> {
    let kind = crate::person_resource_type(document)?;
    WORKSPACE_DOCUMENTS
        .iter()
        .find(|known| known.app == app && known.kind == kind)
}

/// The kind the id `record` is bound to, whatever kind a write names for it: the kind a derived
/// id names, or the kind a fixed id is declared for. `None` for an id that names no kind.
#[must_use]
pub fn bound_kind(record: &str) -> Option<&str> {
    app_fleet::fixed_kind(record).or_else(|| crate::person_resource_type(record))
}

/// Whether records of `kind` are kept under keys of their own from their first write, as the
/// kind's owner declares: its app (`app_fleet::is_sealed`), or the system for a record it keeps.
#[must_use]
pub fn is_sealed_kind(kind: &str) -> bool {
    app_fleet::is_sealed(kind)
        || WORKSPACE_DOCUMENTS
            .iter()
            .any(|kept| kept.sealed && kept.kind == kind)
}

/// The sealed kind of the record `record` of `app`: the package's own record (`""`), kept as the
/// package's kind, or a record whose id is bound to its kind ([`bound_kind`]), when that kind is
/// sealed ([`is_sealed_kind`]). Only the kind decides: an app's name seals nothing.
#[must_use]
pub fn sealed_kind(app: &str, record: &str) -> Option<std::borrow::Cow<'static, str>> {
    if record.is_empty() {
        let kind = app_fleet::package_kind(app);
        return is_sealed_kind(&kind).then_some(std::borrow::Cow::Owned(kind));
    }
    let kind = bound_kind(record)?;
    is_sealed_kind(kind).then(|| std::borrow::Cow::Owned(kind.to_owned()))
}

/// The app whose records say which content the host stopped keeping: the
/// host itself, which keeps them as its own, not an app a person installs.
pub const CONTENT_REFUSAL_APP: &str = "tilo.host";
/// The kind of a record of content a host stopped keeping, with the limit it
/// went over and who can raise it. One record per refused content, keyed by
/// the record that named it and the content's address, so the same refusal
/// lands on the same record after a restart.
pub const CONTENT_REFUSAL_KIND: &str = "content.refusal";

/// The id of the record that says the content `hash` named by `record` of
/// `app` was refused: one per refused content, derived so every host of the
/// workspace writes the same one and a refusal is never made twice.
#[must_use]
pub fn content_refusal_document_id(app: &str, record: &str, hash: &str) -> String {
    let digest = blake3::hash(format!("{app}\0{record}\0{hash}").as_bytes()).to_hex();
    format!(
        "derived:{CONTENT_REFUSAL_KIND}.{}@tab:0000000000000000",
        digest.get(..32).unwrap_or(&digest)
    )
}

/// Whether `document` is the record of one content a host refused.
#[must_use]
pub fn is_content_refusal_document(document: &str) -> bool {
    document
        .strip_prefix("derived:")
        .is_some_and(|rest| rest.starts_with(CONTENT_REFUSAL_KIND))
}

/// Whether `app`'s `record` is decided as the app's own record (`""`), as its declaration says
/// ([`app_fleet::Readers::AppRecord`]).
#[must_use]
pub fn decided_as_app_record(app: &str, record: &str) -> bool {
    app_fleet::declared_record(app, record)
        .is_some_and(|kept| kept.readers == app_fleet::Readers::AppRecord)
}

/// Whether no one reads `app`'s `record`, as its declaration says
/// ([`app_fleet::Readers::NoOne`]): not its person, an owner, a role or a grant.
#[must_use]
pub fn is_unread_record(app: &str, record: &str) -> bool {
    app_fleet::declared_record(app, record)
        .is_some_and(|kept| kept.readers == app_fleet::Readers::NoOne)
}

/// A workspace record that even apps without a declared read of its owner may join.
#[must_use]
pub fn shared_workspace_document(app: &str, document: &str) -> Option<&'static WorkspaceDocument> {
    workspace_document(app, document).filter(|kept| kept.readers != app_fleet::Readers::AppRecord)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_listed_document_of_its_owner_is_a_workspace_document() {
        let emoji = workspace_document_id("tilo.workspace.emoji");
        assert_eq!(
            workspace_document("so.tilo.chat", &emoji).map(|document| document.kind),
            Some("tilo.workspace.emoji")
        );
        assert!(
            workspace_document("so.tilo.notes", &emoji).is_none(),
            "another app's document of the type"
        );
        assert!(
            workspace_document("so.tilo.chat", &workspace_document_id("chat.workspace")).is_none()
        );
        let agents = workspace_document_id("tilo.workspace.agent");
        assert_eq!(
            workspace_document("tilo.workspace", &agents).map(|document| document.kind),
            Some("tilo.workspace.agent"),
            "the workspace keeps its agents for every app of the workspace"
        );
        assert!(workspace_document("so.tilo.chat", &agents).is_none());
        assert!(workspace_document("so.tilo.notes", &agents).is_none());
        assert!(workspace_document("so.tilo.chat", "tab:0000000000000000").is_none());
        let personal = workspace_document_id("tilolang.personal");
        assert_eq!(
            workspace_document("tilo.host", &personal).map(|document| document.kind),
            Some("tilolang.personal")
        );
        assert!(
            workspace_document("so.tilo.lang.tasks", &personal).is_none(),
            "one document for every package of the language, not one per package"
        );
    }
}
