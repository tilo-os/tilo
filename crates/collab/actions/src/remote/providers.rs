//! Tokens naming who publishes an action, and the resources a workspace's
//! own actions on an application live under.

/// Provider token naming the calling application's own connection, for an
/// app that routes one of its own actions through the host.
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

/// Action id that starts an application in a workspace without showing it:
/// an instance with no window where the host runs one, ready for the
/// actions of the app that show nothing. It answers with that instance.
pub const APPLICATION_START_ACTION: &str = "start";

/// Action id of a resource's link: a URL that opens it for the workspace's
/// members, carrying no secret. A record's root always offers it.
pub const SHARE_LINK_ACTION: &str = "share-link";

/// Prefix of the provider token of one record of a workspace, served by a
/// device that holds it whoever has it open: `record:<app id>:<record id>`
/// (a host serving its workspaces' records answers under these). Such a
/// provider is the app's record, live, as a window of the app is.
pub const RECORD_PROVIDER: &str = "record";

/// Whether `provider` names one record of a workspace ([`RECORD_PROVIDER`]).
#[must_use]
pub fn is_record_provider(provider: &str) -> bool {
    provider
        .strip_prefix(RECORD_PROVIDER)
        .is_some_and(|rest| rest.starts_with(':'))
}

/// The app and record ids `provider` names, when it names one record of a
/// workspace ([`RECORD_PROVIDER`]): `record:<app id>:<record id>`. App ids
/// hold no colon; a record id may (`uuid:…`), and an empty one is the app's
/// own first record.
#[must_use]
pub fn record_parts(provider: &str) -> Option<(&str, &str)> {
    provider
        .strip_prefix(RECORD_PROVIDER)?
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
            record_parts("record:so.tilo.notes:uuid:7"),
            Some(("so.tilo.notes", "uuid:7"))
        );
        assert_eq!(
            record_parts("record:so.tilo.chat:"),
            Some(("so.tilo.chat", ""))
        );
        assert_eq!(record_parts("record:so.tilo.chat"), None);
        assert_eq!(record_parts("record::main"), None);
        assert_eq!(record_parts("workspace:6b3f9a10"), None);
        assert_eq!(record_parts("4711"), None);
        assert!(!is_record_provider("4711"));
        assert!(is_record_provider("record:so.tilo.chat:"));
    }
}
