//! Tokens naming who publishes an action, and the resources a workspace's
//! own actions on an application live under.

/// Provider token naming the calling application's own connection, for an
/// app that routes one of its own actions through the provider.
pub const OWN_PROVIDER: &str = "self";

/// Prefix of the provider token for actions a workspace itself offers
/// rather than an application; see [`workspace_provider`].
pub const WORKSPACE_PROVIDER: &str = "workspace";

/// Resource prefix of a workspace's "open this application" actions.
pub const APPLICATION_RESOURCE_PREFIX: &str = "app.";

/// Resource kind of an installed Tilo application a workspace lists, which
/// a catalogue query's `resource_name` answers whole.
pub const APPLICATION_RESOURCE_KIND: &str = "applications.app";

/// Action id that opens an application into a workspace.
pub const APPLICATION_OPEN_ACTION: &str = "open";

/// The provider a device answers the workspace's own folder under: the files it holds, and
/// Files' verbs on them by paths in the workspace (`tilo_action_services::device_files`).
pub const DEVICE_FILES_PROVIDER: &str = "tilo.device-files";

/// The resource of the workspace's own folder a device's actions are on. It is the folder the
/// Files app behind [`APPLICATION_RESOURCE_PREFIX`] is about, answered where its tree is kept.
pub const DEVICE_FILES_FOLDER: &str = "files.folder";

/// Action id that starts an application in a workspace without showing it:
/// an instance with no window where the provider runs one, ready for the
/// actions of the app that show nothing. It answers with that instance.
pub const APPLICATION_START_ACTION: &str = "start";

/// Action id of a resource's link: a URL that opens it for the workspace's
/// members, carrying no secret. A record's root always offers it.
pub const SHARE_LINK_ACTION: &str = "share-link";

/// Prefix of the provider token of one record of a workspace, served by a
/// device that holds it whoever has it open: `record:<app id>:<record id>`
/// (a provider serving its workspaces' records answers under these). Such a
/// provider is the app's record, live, as a window of the app is.
pub const RESOURCE_PROVIDER: &str = "record";

/// Whether `provider` names one record of a workspace ([`RESOURCE_PROVIDER`]).
#[must_use]
pub fn is_resource_provider(provider: &str) -> bool {
    provider
        .strip_prefix(RESOURCE_PROVIDER)
        .is_some_and(|rest| rest.starts_with(':'))
}

/// The app and record ids `provider` names, when it names one record of a
/// workspace ([`RESOURCE_PROVIDER`]): `record:<app id>:<record id>`. App ids
/// hold no colon; a record id may (`uuid:…`), and an empty one is the app's
/// own first record.
#[must_use]
pub fn resource_parts(provider: &str) -> Option<(&str, &str)> {
    provider
        .strip_prefix(RESOURCE_PROVIDER)?
        .strip_prefix(':')?
        .split_once(':')
        .filter(|(app, _)| !app.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// A record provider names its app and record, whichever shape the
    /// record id has; nothing else names either.
    #[test]
    fn a_record_provider_names_its_app_and_record() {
        assert_eq!(
            resource_parts("record:so.tilo.notes:uuid:7"),
            Some(("so.tilo.notes", "uuid:7"))
        );
        assert_eq!(
            resource_parts("record:so.tilo.chat:"),
            Some(("so.tilo.chat", ""))
        );
        assert_eq!(resource_parts("record:so.tilo.chat"), None);
        assert_eq!(resource_parts("record::main"), None);
        assert_eq!(resource_parts("workspace:6b3f9a10"), None);
        assert_eq!(resource_parts("4711"), None);
        assert!(!is_resource_provider("4711"));
        assert!(is_resource_provider("record:so.tilo.chat:"));
    }
}
