//! Documents that every app of a workspace opens beside its own: one per kind
//! and workspace, kept by one app (its owner), read and changed from any
//! app's window of the workspace.
//!
//! An app's launch otherwise reaches only its own routes. These documents are
//! the exception, named here one by one: every app of the workspace may join
//! and write the route `(owner, workspace, workspace_document_id(kind))` of its
//! launch workspace, and nothing else of the owner's. The workspace's grants
//! on the owner's resource decide every change, wherever it is made, as they
//! decide the owner's own.

/// A document every app of a workspace opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceDocument {
    /// The app that keeps it: whose surface and grants decide its changes.
    pub app: &'static str,
    /// Its resource kind.
    pub kind: &'static str,
}

/// Every document every app of a workspace opens.
pub const WORKSPACE_DOCUMENTS: &[WorkspaceDocument] = &[
    // The workspace's own emoji, which any view draws for `:name:`
    // (`tilo-workspace-emoji`).
    WorkspaceDocument {
        app: "so.tilo.chat",
        kind: "tilo.workspace.emoji",
    },
    // Which vault items the workspace's Linux machine hands the programs it
    // starts, and in which variable: Terminal keeps it, and the server reads it
    // when a program starts there (`tilo_environment::machine_secrets`).
    WorkspaceDocument {
        app: "so.tilo.terminal",
        kind: "terminal.machine-secrets",
    },
    WorkspaceDocument {
        app: "so.tilo.chat",
        kind: "chat.skills",
    },
    // The workspace's agents: their names, colours, keepers and models,
    // which the workspace settings sheet, the apps and their devices all
    // read (`tilo-workspace-agent`).
    WorkspaceDocument {
        app: "so.tilo.assistant",
        kind: "tilo.workspace.agent",
    },
    // Each person's place in the workspace's tilolang apps: their windows' and
    // lenses' state and what they have seen, kept once for every package of the
    // language rather than per package (`tilolang-tilo::personal`).
    WorkspaceDocument {
        app: "tilo.host",
        kind: "tilolang.personal",
    },
    // The workspace's accounts (mail, calendar, messaging), which Settings keeps
    // and Mail, Calendar and Messages read; their secrets are the vault's
    // (`accounts::person`).
    WorkspaceDocument {
        app: "so.tilo.settings",
        kind: "tilo.accounts",
    },
    // The people the workspace knows, with the identities each is reached at,
    // which Contacts keeps and Mail, Calendar and Messages address
    // (`tilo-contact-directory`).
    WorkspaceDocument {
        app: "so.tilo.contacts",
        kind: "tilo.contacts.identities",
    },
    // What the workspace browsed and kept: its history, its bookmarks, the
    // addresses it fills in and what operated each tab's page (Browser).
    WorkspaceDocument {
        app: "so.tilo.browser",
        kind: "browser.browsing-history",
    },
    WorkspaceDocument {
        app: "so.tilo.browser",
        kind: "browser.personal-bookmarks",
    },
    WorkspaceDocument {
        app: "so.tilo.browser",
        kind: "browser.address-book",
    },
    WorkspaceDocument {
        app: "so.tilo.browser",
        kind: "browser.page-record",
    },
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
            workspace_document("so.tilo.assistant", &agents).map(|document| document.kind),
            Some("tilo.workspace.agent"),
            "Assistant keeps the workspace's agents for every app of the workspace"
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
            workspace_document("so.tilo.tasks", &personal).is_none(),
            "one document for every package of the language, not one per package"
        );
    }
}
