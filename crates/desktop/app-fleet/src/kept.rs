//! Records the system keeps for an app's whole workspace, beside the app's other records, as the
//! kind's owner declares them: one document per kind and workspace, reached on its app's route.
//!
//! Other apps can join a shared workspace record from their own windows; the workspace's grants on
//! its app still decide every read and change.

/// Who reads and changes a record an app keeps for the whole workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readers {
    /// The workspace's members, as its grants on the app decide.
    Members,
    /// It is decided as its app's own record (`""`): it moved out of that record, and reading or
    /// changing it takes what reading or changing the app's own record takes, so no other app
    /// joins it.
    AppRecord,
    /// No one: an earlier build kept several people's private records in it with no owner to say
    /// whose, so neither a role nor a grant may disclose it.
    NoOne,
    /// Only the workspace's owner reads or changes anything its app keeps.
    Owner,
}

/// A record an app keeps for the whole workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceDocument {
    /// The app that keeps it: whose surface and grants decide its changes.
    pub app: &'static str,
    /// Its resource kind.
    pub kind: &'static str,
    /// The id it is kept under, where that is not the one its kind derives
    /// ([`derived_record`]). Either way the id is bound to the kind.
    pub fixed: Option<&'static str>,
    /// Whether it is kept under keys of its own from its first write.
    pub sealed: bool,
    /// Who reads and changes it, beyond what the workspace's grants on its app decide.
    pub readers: Readers,
    /// The prefix each member's own record of the kind is kept under, before their id
    /// ([`WorkspaceDocument::record_of`]): every member reads each, and only its person changes
    /// theirs.
    pub each_person: Option<&'static str>,
}

impl WorkspaceDocument {
    const fn kept(app: &'static str, kind: &'static str) -> Self {
        Self {
            app,
            kind,
            fixed: None,
            sealed: false,
            readers: Readers::Members,
            each_person: None,
        }
    }

    /// The id of `person`'s own record of the kind, when each member keeps one.
    #[must_use]
    pub fn record_of(&self, person: &str) -> Option<String> {
        self.each_person.map(|prefix| format!("{prefix}{person}"))
    }

    /// The person whose own record `record` is, when each member keeps one.
    #[must_use]
    pub fn person_of<'a>(&self, record: &'a str) -> Option<&'a str> {
        record
            .strip_prefix(self.each_person?)
            .filter(|person| !person.is_empty())
    }

    /// Whether `record` is its id: its fixed one, or the one its kind derives.
    fn is(&self, record: &str) -> bool {
        match self.fixed {
            Some(fixed) => fixed == record,
            None => derived_kind(record) == Some(self.kind),
        }
    }
}

/// What each member's own view of a workspace's layout is kept under, before their id.
pub const PERSON_VIEW_PREFIX: &str = "view-of-";

pub use tilo_app_id::passwords::{VAULT_KIND, VAULT_RECORD};

/// The records apps keep for their whole workspace.
pub const WORKSPACE_DOCUMENTS: &[WorkspaceDocument] = &[
    // What a workspace's VM session keeps for its next one, its settings by name: the one
    // record its device writes besides its window's (`tilo-workspace-service::environment`).
    WorkspaceDocument::kept("so.tilo.browser", "browser.environment"),
    // The workspace's own emoji, which any view draws for `:name:`
    // (`tilo-workspace-emoji`).
    WorkspaceDocument::kept("so.tilo.chat", "tilo.workspace.emoji"),
    WorkspaceDocument::kept("so.tilo.chat", "chat.skills"),
    // The workspace's agents: their names, colours, models and who added them,
    // which the workspace settings sheet, the apps and their devices all
    // read (`tilo-workspace-agent`).
    WorkspaceDocument::kept("tilo.workspace", "tilo.workspace.agent"),
    // The reports and decisions Chat keeps for its workspace, decided as Chat's own record.
    WorkspaceDocument {
        readers: Readers::AppRecord,
        ..WorkspaceDocument::kept("so.tilo.chat", "tilo.moderation")
    },
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
        fixed: Some(VAULT_RECORD),
        sealed: true,
        ..WorkspaceDocument::kept("so.tilo.passwords", VAULT_KIND)
    },
    // The workspace's files: its tree's own record and each chunk of it (`fs-protocol`'s tree).
    WorkspaceDocument::kept("so.tilo.file-explorer", "files.tree"),
    // The workspace's shared desktop, each person's view of it and each window's view of what it
    // showed (`desktop-model`'s record).
    WorkspaceDocument::kept("so.tilo.browser-desktop", "tilo.desktop"),
    WorkspaceDocument {
        each_person: Some(PERSON_VIEW_PREFIX),
        ..WorkspaceDocument::kept("so.tilo.browser-desktop", "desktop.person-view")
    },
    WorkspaceDocument::kept("so.tilo.browser-desktop", "desktop.window-view"),
    // What the person answered about the devices of their machines, kept once for all of them
    // (`tilo-peer-state`'s device answers).
    WorkspaceDocument::kept("so.tilo.identity", "identity.device-answers"),
    // What a workspace's owner alone keeps of it: the labels they gave it and whether keys
    // reaching it alone run commands in its machine unasked (`tilo-server-protocol`'s
    // `workspaces::OWNERSHIP`).
    WorkspaceDocument {
        fixed: Some("ownership"),
        readers: Readers::Owner,
        ..WorkspaceDocument::kept("so.tilo.ownership", "tilo.ownership")
    },
];

/// Records earlier builds kept for a whole workspace and no build writes now: each person's unsent
/// Messages drafts and Tube watch history, all in one record. They stay bound to their kind and
/// unread ([`Readers::NoOne`]) wherever a device still keeps one, and are no workspace
/// record an app keeps.
pub const EARLIER_RECORDS: &[WorkspaceDocument] = &[
    WorkspaceDocument {
        fixed: Some("uuid:fe64cba3-45c6-4e34-a3ac-95b0038e889a"),
        readers: Readers::NoOne,
        ..WorkspaceDocument::kept("so.tilo.messages", "messages.workspace-drafts")
    },
    WorkspaceDocument {
        fixed: Some("uuid:3bd2daf5-120b-469b-be38-d5203c58efb2"),
        readers: Readers::NoOne,
        ..WorkspaceDocument::kept("so.tilo.tube", "tube.workspace-history")
    },
];

/// The id the workspace document of `kind` is kept under in any workspace, when it has no fixed
/// one: the same on every device, derived as a person's document of a kind is.
#[must_use]
pub fn derived_record(kind: &str) -> String {
    format!("derived:{kind}@{DERIVED_BASE}")
}

const DERIVED_BASE: &str = "tab:0000000000000000";

/// The kind the id `record` derives from ([`derived_record`]); `None` for any other id.
#[must_use]
pub fn derived_kind(record: &str) -> Option<&str> {
    record
        .strip_prefix("derived:")?
        .strip_suffix(DERIVED_BASE)?
        .strip_suffix('@')
        .filter(|kind| !kind.is_empty() && !kind.contains('@'))
}

/// The record `app` keeps under `record`, whether a workspace record or an earlier one, as
/// declared ([`WORKSPACE_DOCUMENTS`], [`EARLIER_RECORDS`]).
#[must_use]
pub fn declared_record(app: &str, record: &str) -> Option<&'static WorkspaceDocument> {
    let app = tilo_app_id::fleet_id(app);
    WORKSPACE_DOCUMENTS
        .iter()
        .chain(EARLIER_RECORDS)
        .find(|kept| kept.app == app.as_ref() && kept.is(record))
}

/// The member's own record `record` of `app`, and whose it is, when it is one
/// ([`WorkspaceDocument::each_person`]).
#[must_use]
pub fn person_record<'a>(
    app: &str,
    record: &'a str,
) -> Option<(&'static WorkspaceDocument, &'a str)> {
    let app = tilo_app_id::fleet_id(app);
    WORKSPACE_DOCUMENTS
        .iter()
        .filter(|kept| kept.app == app.as_ref())
        .find_map(|kept| Some((kept, kept.person_of(record)?)))
}

/// The kind a fixed id is declared for, whichever app keeps it.
#[must_use]
pub fn fixed_kind(record: &str) -> Option<&'static str> {
    WORKSPACE_DOCUMENTS
        .iter()
        .chain(EARLIER_RECORDS)
        .find(|kept| kept.fixed == Some(record))
        .map(|kept| kept.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_record_is_its_apps_alone_and_named_by_its_own_id() {
        let moderation = derived_record("tilo.moderation");
        assert!(
            declared_record("so.tilo.chat", &moderation)
                .is_some_and(|kept| kept.readers == Readers::AppRecord)
        );
        assert!(
            declared_record("dev.tilo.chat", &moderation)
                .is_some_and(|kept| kept.readers == Readers::AppRecord),
            "a development build's Chat is Chat"
        );
        assert!(declared_record("so.tilo.notes", &moderation).is_none());
        assert!(
            declared_record("so.tilo.chat", &format!("{moderation}x")).is_none(),
            "another id"
        );
        assert!(
            declared_record("so.tilo.passwords", &derived_record(VAULT_KIND)).is_none(),
            "a record with a fixed id is not also kept under the id its kind derives"
        );
        let (view, person) = person_record("so.tilo.browser-desktop", "view-of-ada").unwrap();
        assert_eq!((view.kind, person), ("desktop.person-view", "ada"));
        assert_eq!(view.record_of("ada").as_deref(), Some("view-of-ada"));
        assert!(
            person_record("so.tilo.browser-desktop", "view-of-").is_none(),
            "no one's"
        );
        assert!(person_record("so.tilo.notes", "view-of-ada").is_none());
        for earlier in EARLIER_RECORDS {
            let id = earlier.fixed.unwrap();
            assert!(
                declared_record(earlier.app, id).is_some_and(|kept| kept.readers == Readers::NoOne)
            );
            assert_eq!(fixed_kind(id), Some(earlier.kind));
            assert!(declared_record("so.tilo.notes", id).is_none());
        }
    }
}
