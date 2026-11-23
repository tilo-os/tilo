//! Every service between this session and the app broker's [`ProcessServices`],
//! decided.
//!
//! The broker answers an app's call from the services its host hands it, and
//! silently refuses what the host left empty. A session that builds its
//! services with a default leaves that refusal to chance: a service nobody
//! decided is a feature nobody serves, and an app finds out by being told
//! "no such action is offered".
//!
//! So this session names every service: served ([`BrokerService::served`]), served where
//! this run has what it carries ([`BrokerService::when`] — no device fabric, no
//! workspace engine, no content), or not served at all and why
//! ([`BrokerService::unserved`]). The conversion to [`ProcessServices`] names every field
//! of it without a default, so a service added to the broker does not compile
//! here until the session decides about it.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_broker_core::services::{
    ContentStoreServices, DocumentChangeServices, DocumentIndexChanges, KeyboardShortcutServices,
    LocalPersonServices, OwnDeviceActions, SearchIndexServices, WorkspaceGovernanceServices,
    WorkspaceNodeActions,
};
use tilo_broker_core::steps::ContinuationServices;
use tilo_workspace_keys::KeyProvider;
use tilo_workspace_service::apps::services::{
    ApplicationServices, AudioOutputServices, BrowserServiceSessionServices,
    DisplayControlServices, DriveServices, FileClipboardServices, LauncherServices,
    NearbyTransferServices, ProcessServices, PublishedResources, RealtimeServices,
    ShortcutLibraryServices, SpeakerServices, WifiContinuityServices, WorkspaceControlServices,
    WorkspaceGrants, WorkspaceMemberships,
};

/// What this session serves of one service.
pub(crate) enum BrokerService<T> {
    /// Served for every app this session admits.
    Served(T),
    /// Served where this run has what it carries: a session on no device fabric carries no speaker
    /// groups, and one with no workspace engine serves no memberships.
    When(Option<T>),
    /// Not served, and why: said at session start, so a service an app found missing can be found here
    /// with its reason.
    NotServed(&'static str),
}

impl<T> BrokerService<T> {
    pub(crate) fn served(value: T) -> Self {
        Self::Served(value)
    }

    pub(crate) fn when(value: Option<T>) -> Self {
        Self::When(value)
    }

    pub(crate) fn unserved(reason: &'static str) -> Self {
        Self::NotServed(reason)
    }

    fn into_option(self) -> Option<T> {
        match self {
            Self::Served(value) => Some(value),
            Self::When(value) => value,
            Self::NotServed(_) => None,
        }
    }

    /// Why this service is not served; `None` when it is.
    fn decision(&self) -> Option<&'static str> {
        match self {
            Self::Served(_) | Self::When(_) => None,
            Self::NotServed(reason) => Some(reason),
        }
    }
}

/// What one service holds for the broker: the service, the option it may be, or nothing and why.
macro_rules! service_field {
    ($this:ident.$field:ident) => {
        $this.$field.into_option()
    };
    ($this:ident.$field:ident, default) => {
        $this.$field.into_option().unwrap_or_default()
    };
}

/// This session's decision for each of the broker's services, in the order the broker's
/// [`ProcessServices`] names them: what it serves, and for what it serves nothing, the reason it was
/// left out. One list, because the decision, the conversion and the reasons are the same service.
macro_rules! broker_services {
    ($( $(#[$meta:meta])* $field:ident: $ty:ty $(=> $default:tt)? ),* $(,)?) => {
        pub(crate) struct BrokerServices {
            $( $(#[$meta])* pub(crate) $field: BrokerService<$ty> ),*
        }

        impl BrokerServices {
            /// What the broker's APIs read: every service, as this session decided it.
            pub(crate) fn into_host_services(self) -> ProcessServices {
                ProcessServices { $( $field: service_field!(self.$field $(, $default)?) ),* }
            }

            /// Each service's name, and why it is not served (`None` when it is).
            pub(crate) fn decisions(&self) -> Vec<(&'static str, Option<&'static str>)> {
                vec![ $( (stringify!($field), self.$field.decision()) ),* ]
            }
        }
    };
}

broker_services! {
    approved_checks: Arc<dyn tilo_workspace_service::apps::collab_app_broker::ApprovedCheckServices>,
    committed_documents: Arc<dyn tilo_workspace_service::apps::services::NativeDocumentReads>,
    native_media: Arc<dyn tilo_workspace_service::apps::services::NativeMediaServices>,
    audio_output: Arc<dyn AudioOutputServices>,
    workspace_memberships: Arc<dyn WorkspaceMemberships>,
    agents: Arc<dyn tilo_broker_core::services::AgentRoster>,
    agent_sessions: Arc<dyn tilo_broker_core::services::AgentSessions>,
    native_certificates: Arc<dyn tilo_workspace_service::apps::certificate_authority::CertificateAuthority>,
    local_person: Arc<dyn LocalPersonServices>,
    resource_changes: Arc<dyn DocumentChangeServices>,
    journal_root: Option<std::path::PathBuf> => default,
    launcher: Arc<dyn LauncherServices>,
    published: Arc<dyn PublishedResources>,
    workspace_governance: Arc<dyn WorkspaceGovernanceServices>,
    shortcut_library: Arc<dyn ShortcutLibraryServices>,
    keyboard_shortcuts: Arc<dyn KeyboardShortcutServices>,
    providers: tilo_workspace_service::workspaces::providers::Providers,
    continuations: Arc<dyn ContinuationServices>,
    search_index: Arc<dyn SearchIndexServices>,
    document_index_changes: Arc<dyn DocumentIndexChanges>,
    content_store: Arc<dyn ContentStoreServices>,
    workspace_control: Arc<dyn WorkspaceControlServices>,
    realtime: Arc<dyn RealtimeServices>,
    speakers: Arc<dyn SpeakerServices>,
    drives: Arc<dyn DriveServices>,
    applications: Arc<dyn ApplicationServices>,
    settings: Arc<dyn settings_store::SettingsSource>,
    display_control: Arc<dyn DisplayControlServices>,
    live_activities: Arc<runtime_collab::LiveActivities>,
    wifi_continuity: Arc<dyn WifiContinuityServices>,
    browser_service_sessions: Arc<dyn BrowserServiceSessionServices>,
    nearby_transfer: Arc<dyn NearbyTransferServices>,
    file_clipboard: Arc<dyn FileClipboardServices>,
    app_action_access: tilo_broker_core::access::AppActionAccessResolver,
    app_resource_kinds: tilo_broker_core::access::AppResourceKindsResolver,
    workspace_grants: Arc<dyn WorkspaceGrants>,
    launchable_catalogue: PathBuf,
    document_content: Arc<tilo_workspace_service::content::RouteContent>,
    group_keys: KeyProvider,
    action_offload: Arc<dyn tilo_actions::offload::ActionOffload>,
    own_device: Arc<dyn OwnDeviceActions>,
    workspace_nodes: Arc<dyn WorkspaceNodeActions>,
    hosted_workspaces: Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>,
    resources: Arc<dyn tilo_workspace_service::apps::services::ResourceServices>,
    shell: Arc<tilo_workspace_service::apps::shell::ShellServices>,
    device_daemon: tilo_workspace_service::apps::services::DeviceDaemon,
}

/// What the session's own roles read and write of the person's workspaces: the device's daemon
/// keeps them, and answers on the shell `link` it handed this session; and each write the daemon
/// takes, which the desktop reads its layouts again by.
pub(crate) fn session_documents(
    link: tilo_workspace_service::apps::services::DeviceDaemon,
    runtime: tokio::runtime::Handle,
) -> (
    Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
    Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>,
    tilo_session_client::daemon_documents::DaemonWritesFeed,
) {
    let (documents, writes, shell) =
        tilo_session_client::daemon_documents::on_daemon(link, runtime);
    let feed = tilo_session_client::daemon_documents::DaemonWritesFeed::start(&shell);
    (documents, writes, feed)
}

/// The apps' resources in the device's one store, which the daemon keeps, read over the shell's
/// link: what an agent this session admits reads them with. An app writes its own on its
/// connection at the daemon, and an agent writes none here.
pub(crate) struct DaemonResources {
    pub(crate) shell: tilo_session_client::daemon_documents::DaemonShell,
    /// The daemon's writes, which a query held open is asked again on.
    pub(crate) writes: tilo_session_client::daemon_documents::DaemonWritesFeed,
}

/// `command` of `app`'s resources in `workspace`, answered by the daemon as the device's person.
async fn daemon_resource_command(
    shell: &tilo_session_client::daemon_documents::DaemonShell,
    workspace: String,
    app: String,
    command: &tilo_resource_store::wire::Command,
) -> Result<tilo_resource_store::wire::Reply, String> {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    let command = serde_json::to_string(command)
        .map_err(|error| format!("a resource command does not encode: {error}"))?;
    match shell
        .ask(ShellCall::Resources {
            workspace,
            app,
            command,
        })
        .await?
    {
        ShellReply::Resources(answer) => serde_json::from_str(&answer?)
            .map_err(|error| format!("the daemon's answer does not read: {error}")),
        other => Err(format!("the daemon answered something else: {other:?}")),
    }
}

impl tilo_workspace_service::apps::services::ResourceServices for DaemonResources {
    fn command(
        &self,
        context: &tilo_workspace_service::apps::services::ResourceRequestContext,
        command: tilo_resource_store::wire::Command,
    ) -> tilo_workspace_service::apps::services::ResourceAnswer {
        if command.writes() {
            return Box::pin(std::future::ready(Err(
                "an app writes its resources on its own connection at the device's daemon"
                    .to_owned(),
            )));
        }
        let shell = self.shell.clone();
        let (workspace, app) = (
            context.workspace_id().to_owned(),
            context.app_id().to_owned(),
        );
        Box::pin(async move { daemon_resource_command(&shell, workspace, app, &command).await })
    }

    fn open(
        &self,
        context: &tilo_workspace_service::apps::services::ResourceRequestContext,
        query: tilo_resource_store::Query,
    ) -> tilo_workspace_service::apps::services::ResourcePages {
        let shell = self.shell.clone();
        let mut written = self.writes.follow();
        let (workspace, app) = (
            context.workspace_id().to_owned(),
            context.app_id().to_owned(),
        );
        Box::pin(async move {
            let command = tilo_resource_store::wire::Command::Query(query);
            let page = |reply| match reply {
                tilo_resource_store::wire::Reply::Page(page) => Ok(page),
                other => Err(format!("the daemon answered a query with {other:?}")),
            };
            let first = page(
                daemon_resource_command(&shell, workspace.clone(), app.clone(), &command).await?,
            )?;
            let (pages, received) = tokio::sync::mpsc::unbounded_channel();
            let mut shown = serde_json::to_vec(&first).unwrap_or_default();
            pages
                .send(first)
                .map_err(|_| "the query was let go before its first page".to_owned())?;
            // Each write to the workspace (or writes missed, told with no workspace) may change
            // what the query shows: it is asked again then, and a different page is sent.
            tokio::spawn(async move {
                loop {
                    let heard = tokio::select! {
                        heard = written.recv() => heard,
                        () = pages.closed() => return,
                    };
                    match heard {
                        Ok(write)
                            if !write.workspace.is_empty() && write.workspace != workspace =>
                        {
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    }
                    let asked =
                        daemon_resource_command(&shell, workspace.clone(), app.clone(), &command)
                            .await
                            .and_then(page);
                    match asked {
                        Ok(next) => {
                            let bytes = serde_json::to_vec(&next).unwrap_or_default();
                            if bytes != shown {
                                shown = bytes;
                                if pages.send(next).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "a held query was not asked again");
                            return;
                        }
                    }
                }
            });
            Ok(received)
        })
    }
}

/// The workspaces' records as the device's daemon keeps them, read over the shell's link: what an
/// agent this session admits reads a record outside its app with. The session keeps no copy;
/// the agent's read is decided here, at the record's node, before the daemon is asked.
pub(crate) struct DaemonRecordReads(
    pub(crate) tilo_session_client::daemon_documents::DaemonDocuments,
);

impl tilo_workspace_service::apps::collab_app_broker::NativeDocumentReads for DaemonRecordReads {
    fn resource_read(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> tilo_workspace_service::apps::collab_app_broker::ResourceRead {
        use tilo_peer_state::workspace_documents::DeviceDocuments as _;
        if self.0.holds(workspace, app, record) {
            tilo_workspace_service::apps::collab_app_broker::ResourceRead::Sealed
        } else {
            tilo_workspace_service::apps::collab_app_broker::ResourceRead::Own
        }
    }

    fn authorize(
        &self,
        context: &tilo_workspace_service::apps::collab_app_broker::NativeDocumentReadContext,
    ) -> Result<(), String> {
        context.revalidate()?;
        let route = context.route();
        self.0.authorizes(
            &route.cluster_id,
            &route.app_id,
            &route.document_id,
            context.certificate(),
        )?;
        context.revalidate()
    }

    fn capture(
        &self,
        context: &tilo_workspace_service::apps::collab_app_broker::NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<tilo_workspace_service::apps::collab_app_broker::NativeDocumentRead, String> {
        use tilo_peer_state::workspace_documents::{DeviceDocuments as _, DocumentRead};
        use tilo_workspace_service::apps::collab_app_broker::NativeDocumentRead;
        context.revalidate()?;
        let route = context.route();
        let read = self
            .0
            .read(&route.cluster_id, &route.app_id, &route.document_id, None)?;
        let answer = match read {
            DocumentRead::Whole { revision, .. } | DocumentRead::Unchanged(revision)
                if Some(revision) == known_revision =>
            {
                NativeDocumentRead::Unchanged(revision)
            }
            DocumentRead::Whole {
                revision,
                heads,
                bytes,
            } => {
                if bytes.len() > maximum {
                    return Err(format!(
                        "the record is {} bytes, more than the {maximum} one read takes",
                        bytes.len()
                    ));
                }
                NativeDocumentRead::Captured {
                    revision,
                    heads,
                    body: Box::new(Captured(bytes)),
                }
            }
            // A read asked for nothing since: the daemon answers the record whole or not at all.
            DocumentRead::Changes { .. } | DocumentRead::Unchanged(_) => {
                return Err("the daemon answered a whole read with a part".to_owned());
            }
            DocumentRead::Absent => NativeDocumentRead::Absent,
            DocumentRead::Pending => NativeDocumentRead::Pending,
        };
        context.revalidate()?;
        Ok(answer)
    }
}

/// A record's bytes as the daemon answered them.
struct Captured(Vec<u8>);

impl tilo_workspace_service::apps::collab_app_broker::DocumentBytes for Captured {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// What each installed app declared it may do on the action API, read from the lists search keeps:
/// a ceiling the broker decides an app's calls against, never a grant.
pub(crate) fn declared_access(
    lists: crate::lists::Lists,
) -> tilo_broker_core::access::AppActionAccessResolver {
    Arc::new(move |app_id: &str| {
        lists
            .latest()
            .declared
            .get(app_id)
            .copied()
            .unwrap_or_default()
    })
}

/// The resource kinds each installed app declares, by package id: what it may reach in its own node.
pub(crate) fn declared_resource_kinds(
    lists: crate::lists::Lists,
) -> tilo_broker_core::access::AppResourceKindsResolver {
    std::sync::Arc::new(move |app_id: &str| {
        lists
            .latest()
            .resource_kinds
            .get(app_id)
            .cloned()
            .unwrap_or_default()
    })
}

/// What every app that connects to this session calls, whatever it is: the resources it reads and
/// invokes, the settings it keeps, and the launch its connection is admitted by. Serving none of
/// these refuses every app at its first call.
#[cfg(test)]
pub(crate) const EVERY_APP: &[&str] = &["published", "settings", "launcher"];

/// How a shipped app's source reaches a service of the action API: a marker in its code, and the service
/// that answers the call. A marker here is a call, so an app that starts calling a service it did not
/// is found by the guard.
#[cfg(test)]
pub(crate) const CALLED_SERVICES: &[(&str, &str)] = &[
    (
        "ui::collaboration::wifi_continuity_command",
        "wifi_continuity",
    ),
    ("ui::collaboration::speaker_command", "speakers"),
    ("ui::collaboration::display_control", "display_control"),
    (
        "ui::collaboration::browser_service_sessions",
        "browser_service_sessions",
    ),
    ("ui::collaboration::document_content", "document_content"),
    (
        "ui::collaboration::channel_live_activity",
        "live_activities",
    ),
    ("ui::collaboration::file_clipboard", "file_clipboard"),
    ("NearbyTransferCommand", "nearby_transfer"),
];

/// The services shipped apps still call that this session serves nothing of: a list that only shrinks,
/// each entry a call that does not work yet. Serving one, or moving the app off it, removes its
/// line; a call to a service this session leaves unserved and this list does not name fails the guard.
#[cfg(test)]
pub(crate) const UNSERVED_BUT_CALLED: &[&str] = &[];

#[cfg(test)]
#[path = "broker_services/daemon_resources_tests.rs"]
mod daemon_resources_tests;

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_service_says_what_it_serves_and_what_an_app_declared;

        phase a_service_decides_what_it_serves_and_says_why_when_it_serves_nothing;
        // A service serves what it holds, and says why when it holds nothing.
        {
            assert_eq!(BrokerService::served(7u8).into_option(), Some(7));
            assert_eq!(BrokerService::when(Some(7u8)).into_option(), Some(7));
            assert_eq!(BrokerService::when(None::<u8>).into_option(), None);
            assert_eq!(BrokerService::<u8>::unserved("no store here").into_option(), None);

            assert_eq!(BrokerService::served(7u8).decision(), None);
            assert_eq!(
                BrokerService::when(None::<u8>).decision(),
                None,
                "a service this run has nothing for is still served"
            );
            assert_eq!(
                BrokerService::<u8>::unserved("no store here").decision(),
                Some("no store here")
            );
        }

        phase an_apps_declared_access_is_what_its_manifest_says;
        // The action API's ceiling comes from the installed apps' signed manifests, read with the
        // lists search keeps: an app Tilo cannot find declared nothing, and never more than its
        // manifest says.
        {
            use tilo_broker_core::access::AppActionAccess;

            let lists = crate::lists::Lists::fixed(crate::lists::AppLists {
                declared: [(
                    "so.tilo.agent".to_owned(),
                    AppActionAccess {
                        query: true,
                        invoke: true,
                    },
                )]
                .into_iter()
                .collect(),
                ..crate::lists::AppLists::default()
            });
            let access = declared_access(lists);
            assert_eq!(
                access("so.tilo.agent"),
                AppActionAccess {
                    query: true,
                    invoke: true
                }
            );
            assert_eq!(
                access("so.tilo.notes"),
                AppActionAccess::default(),
                "an app that declared nothing reaches nothing beyond its own actions"
            );
        }
    }
}
