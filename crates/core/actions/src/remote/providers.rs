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

/// Files' verbs on a workspace's own folder by paths in it; naming a `device`, they change its disk.
pub const DEVICE_FILES_VERBS: [&str; 7] = [
    "new-folder",
    "rename",
    "move",
    "copy-into",
    "trash",
    "restore",
    "undo",
];

/// Files' reads of a workspace's own folder by paths in it, which the workspace answers from its
/// tree: what a folder or the trash holds, what a file says, and what changed it.
pub const WORKSPACE_FILES_READS: [&str; 3] = ["list", "read-text", "history"];

/// Action id that starts an application in a workspace without showing it:
/// an instance with no window where the provider runs one, ready for the
/// actions of the app that show nothing. It answers with that instance.
pub const APPLICATION_START_ACTION: &str = "start";

/// Action id of a resource's link: a URL that opens it for the workspace's
/// members, carrying no secret. A record's root always offers it.
pub const SHARE_LINK_ACTION: &str = "share-link";

/// Whether `provider` names an app, whose records a call of it acts on by their paths: no window
/// (a numeric peer), and no token of a workspace's or of the host's own (`workspace:<id>`,
/// `desktop:session`).
#[must_use]
pub fn names_app(provider: &str) -> bool {
    !provider.is_empty() && provider.parse::<u64>().is_err() && !provider.contains(':')
}

/// The path of one of several records of a kind, as a call names it under the record's app: the
/// kind's resource with the record's id nested under it (`tl.so.tilo.terminal.Session/<id>`).
/// A record that is its kind's only one is the kind's resource itself.
#[must_use]
pub fn record_path(resource: &str, record: &str) -> String {
    if record.is_empty() {
        resource.to_owned()
    } else {
        format!("{resource}/{record}")
    }
}

/// The record `path` names under the kind's resource `resource` ([`record_path`]): `None` when it
/// names none of that kind.
#[must_use]
pub fn record_in<'a>(path: &'a str, resource: &str) -> Option<&'a str> {
    path.strip_prefix(resource)?
        .strip_prefix('/')
        .filter(|record| !record.is_empty())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// A record's path nests its id under its kind's resource, and only that kind's path names
    /// one, whichever shape the id has.
    #[test]
    fn a_record_path_nests_its_id_under_its_kind() {
        let path = record_path("tl.so.tilo.terminal.Session", "uuid:7");
        assert_eq!(path, "tl.so.tilo.terminal.Session/uuid:7");
        assert_eq!(
            record_in(&path, "tl.so.tilo.terminal.Session"),
            Some("uuid:7")
        );
        assert_eq!(record_path("devices", ""), "devices");
        assert_eq!(record_in("devices", "devices"), None);
        assert_eq!(
            record_in(
                "tl.so.tilo.terminal.Sessions/7",
                "tl.so.tilo.terminal.Session"
            ),
            None
        );
        assert_eq!(
            record_in(
                "tl.so.tilo.terminal.Session/",
                "tl.so.tilo.terminal.Session"
            ),
            None
        );
    }
}
