//! Addressing an app's actions in a workspace: the targets a provider routes
//! to a running app or launches one for.
//!
//! Opening an app — its `open`, and the launch an app offers as an action on its
//! own resources — is declared with the window's actions in `collab_types::app_actions`,
//! which is where the arguments it takes are read.

use crate::api::ActionAddress;

use super::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, RemoteActionTarget, workspace_provider,
};

/// A file's name a launch may carry: one path component of at most 255 bytes, with no
/// control characters.
#[must_use]
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

/// The target that opens `app_id` into workspace `workspace`. An invocation may
/// carry the content hash of bytes to open read-only as its text argument.
#[must_use]
pub fn application_open_target(workspace: &str, app_id: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(workspace),
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{app_id}"),
            APPLICATION_OPEN_ACTION,
        ),
    }
}

/// Prefix of the provider token naming an application in a workspace
/// whether or not it is running; see [`application_action_target`].
pub const APPLICATION_PROVIDER: &str = "application";

/// The provider token naming `app_id`'s actions in `workspace`, whether or not the app runs:
/// the token a call reaches its process by ([`application_action_target`]).
#[must_use]
pub fn application_provider_token(workspace: &str, app_id: &str) -> String {
    format!("{APPLICATION_PROVIDER}:{workspace}:{app_id}")
}

/// The target of the action `app_id` offers on `resource` in workspace
/// `workspace`. The provider delivers it to the running app, or launches the app into
/// the workspace first and delivers it once the app offers the action; the
/// app's own published contract admits it either way.
#[must_use]
pub fn application_action_target(
    workspace: &str,
    app_id: &str,
    resource: impl Into<String>,
    action: impl Into<String>,
) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: application_provider_token(workspace, app_id),
        address: ActionAddress::new(resource, action),
    }
}

/// [`application_action_target`] in `app_id`'s record `record`: the provider
/// delivers to the app showing that record, or launches the app on it.
/// Opening a reference is the resource's `open` on this target.
#[must_use]
pub fn application_resource_target(
    workspace: &str,
    app_id: &str,
    record: &str,
    resource: impl Into<String>,
    action: impl Into<String>,
) -> RemoteActionTarget {
    let mut target = application_action_target(workspace, app_id, resource, action);
    target.provider = format!("{}:{record}", target.provider);
    target
}

/// The workspace and application an [`application_action_target`] provider
/// token names, or `None` for any other provider.
#[must_use]
pub fn application_provider(provider: &str) -> Option<(&str, &str)> {
    application_parts(provider).map(|(workspace, app_id, _)| (workspace, app_id))
}

/// The record an [`application_resource_target`] provider token names, or
/// `None` when it names none.
#[must_use]
pub fn application_provider_resource(provider: &str) -> Option<&str> {
    application_parts(provider).and_then(|(_, _, record)| record)
}

fn application_parts(provider: &str) -> Option<(&str, &str, Option<&str>)> {
    let (workspace, rest) = provider
        .strip_prefix(APPLICATION_PROVIDER)?
        .strip_prefix(':')?
        .split_once(':')?;
    let (app_id, record) = match rest.split_once(':') {
        Some((app_id, record)) => (app_id, Some(record)),
        None => (rest, None),
    };
    (!workspace.is_empty() && !app_id.is_empty() && record.is_none_or(|record| !record.is_empty()))
        .then_some((workspace, app_id, record))
}
