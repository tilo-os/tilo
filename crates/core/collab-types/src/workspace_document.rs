//! Records that apps keep for the whole workspace, beside their other records.
//! Each kind has one document per workspace, reached on the owner's route.
//!
//! Other apps can join a shared workspace record from their own windows;
//! the workspace's grants on its owner still decide every read and change.

/// A record an app keeps for the whole workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceDocument {
    /// The app that keeps it: whose surface and grants decide its changes.
    pub app: &'static str,
    /// Its resource kind.
    pub kind: &'static str,
    /// The id it is kept under, where that is not the one its kind derives
    /// ([`workspace_document_id`]). Either way the id is bound to the kind ([`bound_kind`]).
    pub fixed: Option<&'static str>,
    /// Whether it is kept under keys of its own from its first write ([`is_sealed_kind`]).
    pub sealed: bool,
}

impl WorkspaceDocument {
    const fn kept(app: &'static str, kind: &'static str) -> Self {
        Self {
            app,
            kind,
            fixed: None,
            sealed: false,
        }
    }
}

/// The records apps keep for their whole workspace.
pub const WORKSPACE_DOCUMENTS: &[WorkspaceDocument] = &[
    // What a workspace's VM session keeps for its next one, its settings by name: the one
    // record its device writes besides its window's (`tilo-workspace-service::environment`).
    WorkspaceDocument::kept("so.tilo.browser", "browser.environment"),
    // The workspace's own emoji, which any view draws for `:name:`
    // (`tilo-workspace-emoji`).
    WorkspaceDocument::kept("so.tilo.chat", "tilo.workspace.emoji"),
    // Which vault items the workspace's Linux machine hands the programs it
    // starts, and in which variable: Terminal keeps it, and the server reads it
    // when a program starts there (`tilo_environment::machine_secrets`).
    WorkspaceDocument::kept("so.tilo.terminal", "terminal.machine-secrets"),
    WorkspaceDocument::kept("so.tilo.chat", "chat.skills"),
    // The workspace's agents: their names, colours, models and who added them,
    // which the workspace settings sheet, the apps and their devices all
    // read (`tilo-workspace-agent`).
    WorkspaceDocument::kept("so.tilo.workspace", "tilo.workspace.agent"),
    // The reports and decisions Chat keeps for its workspace.
    WorkspaceDocument::kept("so.tilo.chat", "tilo.moderation"),
    // Each person's place in the workspace's tilolang apps: their windows' and
    // lenses' state and what they have seen, kept once for every package of the
    // language rather than per package (`tilolang-tilo::personal`).
    WorkspaceDocument::kept("tilo.host", "tilolang.personal"),
    // The workspace's accounts (mail, calendar, messaging), which Settings keeps
    // and Mail, Calendar and Messages read; their secrets are the vault's
    // (`accounts::person`).
    WorkspaceDocument::kept("so.tilo.settings", "tilo.accounts"),
    // The people the workspace knows, with the identities each is reached at,
    // which Contacts keeps and Mail, Calendar and Messages address
    // (`tilo-contact-directory`).
    WorkspaceDocument::kept("so.tilo.contacts", "tilo.contacts.identities"),
    // What the workspace browsed and kept: its history, its bookmarks, the
    // addresses it fills in and what operated each tab's page (Browser).
    WorkspaceDocument::kept("so.tilo.browser", "browser.browsing-history"),
    WorkspaceDocument::kept("so.tilo.browser", "browser.personal-bookmarks"),
    WorkspaceDocument::kept("so.tilo.browser", "browser.address-book"),
    WorkspaceDocument::kept("so.tilo.browser", "browser.page-record"),
    // The workspace's vault: its sealed entries and who may use each, which Passwords keeps and
    // every device's vault actions write (`tilo-workspace-service`'s `vault_entries`).
    WorkspaceDocument {
        fixed: Some(tilo_actions::remote::person_vault::VAULT_RECORD),
        sealed: true,
        ..WorkspaceDocument::kept(
            "so.tilo.passwords",
            tilo_actions::remote::person_vault::VAULT_KIND,
        )
    },
    // The workspace's files: its tree's own record and each chunk of it (`fs-protocol`'s tree).
    WorkspaceDocument::kept("so.tilo.file-explorer", "files.tree"),
    // The workspace's shared desktop, each person's view of it and each window's view of what it
    // showed (`desktop-model`'s record).
    WorkspaceDocument::kept("so.tilo.browser-desktop", "tilo.desktop"),
    WorkspaceDocument::kept("so.tilo.browser-desktop", "desktop.person-view"),
    WorkspaceDocument::kept("so.tilo.browser-desktop", "desktop.window-view"),
    // What the person answered about the devices of their machines, kept once for all of them
    // (`tilo-peer-state`'s device answers).
    WorkspaceDocument::kept("so.tilo.identity", "identity.device-answers"),
];

/// The workspace document of `kind`, whichever app keeps it.
#[must_use]
pub fn workspace_document_of_kind(kind: &str) -> Option<&'static WorkspaceDocument> {
    WORKSPACE_DOCUMENTS.iter().find(|known| known.kind == kind)
}

/// The id of the workspace document of `kind` in any workspace: one per kind, the
/// same on every device, derived as the person's document of a kind is.
#[must_use]
pub fn workspace_document_id(kind: &str) -> String {
    crate::person_document_id(kind)
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
    WORKSPACE_DOCUMENTS
        .iter()
        .find(|kept| kept.fixed == Some(record))
        .map(|kept| kept.kind)
        .or_else(|| crate::person_resource_type(record))
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

/// A workspace record that even apps without a declared read of its owner may join.
#[must_use]
pub fn shared_workspace_document(app: &str, document: &str) -> Option<&'static WorkspaceDocument> {
    workspace_document(app, document)
        .filter(|_| !tilo_action_policy::roles::is_workspace_moderation(app, document))
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
            workspace_document("so.tilo.workspace", &agents).map(|document| document.kind),
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
