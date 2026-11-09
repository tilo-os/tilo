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
