//! Every seam between this session and the app broker's [`HostServices`],
//! decided.
//!
//! The broker answers an app's call from the services its host hands it, and
//! silently refuses what the host left empty. A session that builds its
//! services with a default leaves that refusal to chance: a seam nobody
//! decided is a feature nobody serves, and an app finds out by being told
//! "no such action is offered".
//!
//! So this session names every seam: served ([`Seam::served`]), served where
//! this run has what it carries ([`Seam::when`] — no device fabric, no
//! workspace engine, no content), or not served at all and why
//! ([`Seam::unserved`]). The conversion to [`HostServices`] names every field
//! of it without a default, so a seam added to the broker does not compile
//! here until the session decides about it.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_action_broker::channel_encryption::ChannelKeyProvider;
use tilo_action_broker::services::{
    AppActionGrants, ApplicationServices, AudioOutputServices, BrowserServiceSessionServices,
    ContentStoreServices, ContinuationServices, DisplayControlServices, DriveServices,
    FileClipboardServices, HostServices, HostStepServices, KeyboardShortcutServices,
    LauncherServices, LegacyWorkspaceProjection, LocalPersonServices, NearbyTransferServices,
    OwnDeviceActions, PublishedResources, RealtimeServices, RecordChangeServices,
    RecordIndexChanges, SearchIndexServices, ShortcutLibraryServices, SpeakerServices, TrackerRuns,
    WifiContinuityServices, WorkspaceControlServices, WorkspaceGovernanceServices,
    WorkspaceMemberships, WorkspaceNodeActions,
};

/// What this session serves of one seam.
pub(crate) enum Seam<T> {
    /// Served for every app this session admits.
    Served(T),
    /// Served where this run has what it carries: a session on no device fabric carries no speaker
    /// groups, and one that keeps no state keeps no shortcuts on disk.
    When(Option<T>),
    /// Not served, and why: said at session start, so a seam an app found missing can be found here
    /// with its reason.
    NotServed(&'static str),
}

impl<T> Seam<T> {
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

    /// Why this seam is not served; `None` when it is.
    fn decision(&self) -> Option<&'static str> {
        match self {
            Self::Served(_) | Self::When(_) => None,
            Self::NotServed(reason) => Some(reason),
        }
    }
}

/// This session's decision for each of the broker's seams: what it serves,
/// and for what it serves nothing, the reason it was left out.
pub(crate) struct Seams {
    pub(crate) approved_checks:
        Seam<Arc<dyn tilo_action_broker::collab_app_broker::ApprovedCheckServices>>,
    pub(crate) resource_views: Seam<Arc<dyn tilo_action_broker::services::NativeResourceViews>>,
    pub(crate) committed_documents:
        Seam<Arc<dyn tilo_action_broker::services::NativeDocumentReads>>,
    pub(crate) native_media: Seam<Arc<dyn tilo_action_broker::services::NativeMediaServices>>,
    pub(crate) audio_output: Seam<Arc<dyn AudioOutputServices>>,
    pub(crate) workspace_memberships: Seam<Arc<dyn WorkspaceMemberships>>,
    pub(crate) tracker_runs: Seam<Arc<dyn TrackerRuns>>,
    pub(crate) agents: Seam<Arc<dyn tilo_action_broker::services::AgentRoster>>,
    pub(crate) native_certificates:
        Seam<Arc<dyn tilo_action_broker::certificate_authority::CertificateAuthority>>,
    pub(crate) local_person: Seam<Arc<dyn LocalPersonServices>>,
    pub(crate) record_changes: Seam<Arc<dyn RecordChangeServices>>,
    pub(crate) legacy_workspace: Seam<Arc<dyn LegacyWorkspaceProjection>>,
    pub(crate) launcher: Seam<Arc<dyn LauncherServices>>,
    pub(crate) published: Seam<Arc<dyn PublishedResources>>,
    pub(crate) workspace_governance: Seam<Arc<dyn WorkspaceGovernanceServices>>,
    pub(crate) shortcut_library: Seam<Arc<dyn ShortcutLibraryServices>>,
    pub(crate) keyboard_shortcuts: Seam<Arc<dyn KeyboardShortcutServices>>,
    pub(crate) host_steps: Seam<Vec<Arc<dyn HostStepServices>>>,
    pub(crate) continuations: Seam<Arc<dyn ContinuationServices>>,
    pub(crate) search_index: Seam<Arc<dyn SearchIndexServices>>,
    pub(crate) record_index_changes: Seam<Arc<dyn RecordIndexChanges>>,
    pub(crate) content_store: Seam<Arc<dyn ContentStoreServices>>,
    pub(crate) workspace_control: Seam<Arc<dyn WorkspaceControlServices>>,
    pub(crate) realtime: Seam<Arc<dyn RealtimeServices>>,
    pub(crate) speakers: Seam<Arc<dyn SpeakerServices>>,
    pub(crate) drives: Seam<Arc<dyn DriveServices>>,
    pub(crate) applications: Seam<Arc<dyn ApplicationServices>>,
    pub(crate) settings: Seam<Arc<dyn settings_store::SettingsSource>>,
    pub(crate) display_control: Seam<Arc<dyn DisplayControlServices>>,
    pub(crate) live_activities: Seam<Arc<runtime_collab::LiveActivities>>,
    pub(crate) wifi_continuity: Seam<Arc<dyn WifiContinuityServices>>,
    pub(crate) browser_service_sessions: Seam<Arc<dyn BrowserServiceSessionServices>>,
    pub(crate) nearby_transfer: Seam<Arc<dyn NearbyTransferServices>>,
    pub(crate) file_clipboard: Seam<Arc<dyn FileClipboardServices>>,
    pub(crate) app_action_access: Seam<tilo_action_broker::app_actions::AppActionAccessResolver>,
    pub(crate) app_action_grants: Seam<Arc<dyn AppActionGrants>>,
    pub(crate) launchable_catalogue: Seam<PathBuf>,
    pub(crate) record_content: Seam<Arc<tilo_action_broker::record_content::RouteContent>>,
    pub(crate) channel_keys: Seam<ChannelKeyProvider>,
    pub(crate) action_offload: Seam<Arc<dyn tilo_actions::offload::ActionOffload>>,
    pub(crate) own_device: Seam<Arc<dyn OwnDeviceActions>>,
    pub(crate) workspace_nodes: Seam<Arc<dyn WorkspaceNodeActions>>,
    pub(crate) personal: Seam<tilo_peer_state::PersonalSync>,
}

impl Seams {
    /// What the broker's doors read: every seam, as this session decided it.
    pub(crate) fn into_host_services(self) -> HostServices {
        HostServices {
            approved_checks: self.approved_checks.into_option(),
            resource_views: self.resource_views.into_option(),
            committed_documents: self.committed_documents.into_option(),
            native_media: self.native_media.into_option(),
            audio_output: self.audio_output.into_option(),
            workspace_memberships: self.workspace_memberships.into_option(),
            tracker_runs: self.tracker_runs.into_option(),
            agents: self.agents.into_option(),
            native_certificates: self.native_certificates.into_option(),
            local_person: self.local_person.into_option(),
            record_changes: self.record_changes.into_option(),
            legacy_workspace: self.legacy_workspace.into_option(),
            launcher: self.launcher.into_option(),
            published: self.published.into_option(),
            workspace_governance: self.workspace_governance.into_option(),
            shortcut_library: self.shortcut_library.into_option(),
            keyboard_shortcuts: self.keyboard_shortcuts.into_option(),
            host_steps: self.host_steps.into_option().unwrap_or_default(),
            continuations: self.continuations.into_option(),
            search_index: self.search_index.into_option(),
            record_index_changes: self.record_index_changes.into_option(),
            content_store: self.content_store.into_option(),
            workspace_control: self.workspace_control.into_option(),
            realtime: self.realtime.into_option(),
            speakers: self.speakers.into_option(),
            drives: self.drives.into_option(),
            applications: self.applications.into_option(),
            settings: self.settings.into_option(),
            display_control: self.display_control.into_option(),
            live_activities: self.live_activities.into_option(),
            wifi_continuity: self.wifi_continuity.into_option(),
            browser_service_sessions: self.browser_service_sessions.into_option(),
            nearby_transfer: self.nearby_transfer.into_option(),
            file_clipboard: self.file_clipboard.into_option(),
            app_action_access: self.app_action_access.into_option(),
            app_action_grants: self.app_action_grants.into_option(),
            launchable_catalogue: self.launchable_catalogue.into_option(),
            record_content: self.record_content.into_option(),
            channel_keys: self.channel_keys.into_option(),
            action_offload: self.action_offload.into_option(),
            own_device: self.own_device.into_option(),
            workspace_nodes: self.workspace_nodes.into_option(),
            personal: self.personal.into_option(),
        }
    }

    /// Each seam this session decides, in the order the broker's [`HostServices`] names them: its
    /// name, and why it is not served (`None` when it is).
    pub(crate) fn decisions(&self) -> Vec<(&'static str, Option<&'static str>)> {
        vec![
            ("approved_checks", self.approved_checks.decision()),
            ("resource_views", self.resource_views.decision()),
            ("committed_documents", self.committed_documents.decision()),
            ("native_media", self.native_media.decision()),
            ("audio_output", self.audio_output.decision()),
            (
                "workspace_memberships",
                self.workspace_memberships.decision(),
            ),
            ("tracker_runs", self.tracker_runs.decision()),
            ("agents", self.agents.decision()),
            ("native_certificates", self.native_certificates.decision()),
            ("local_person", self.local_person.decision()),
            ("record_changes", self.record_changes.decision()),
            ("legacy_workspace", self.legacy_workspace.decision()),
            ("launcher", self.launcher.decision()),
            ("published", self.published.decision()),
            ("workspace_governance", self.workspace_governance.decision()),
            ("shortcut_library", self.shortcut_library.decision()),
            ("keyboard_shortcuts", self.keyboard_shortcuts.decision()),
            ("host_steps", self.host_steps.decision()),
            ("continuations", self.continuations.decision()),
            ("search_index", self.search_index.decision()),
            ("record_index_changes", self.record_index_changes.decision()),
            ("content_store", self.content_store.decision()),
            ("workspace_control", self.workspace_control.decision()),
            ("realtime", self.realtime.decision()),
            ("speakers", self.speakers.decision()),
            ("drives", self.drives.decision()),
            ("applications", self.applications.decision()),
            ("settings", self.settings.decision()),
            ("display_control", self.display_control.decision()),
            ("live_activities", self.live_activities.decision()),
            ("wifi_continuity", self.wifi_continuity.decision()),
            (
                "browser_service_sessions",
                self.browser_service_sessions.decision(),
            ),
            ("nearby_transfer", self.nearby_transfer.decision()),
            ("file_clipboard", self.file_clipboard.decision()),
            ("app_action_access", self.app_action_access.decision()),
            ("app_action_grants", self.app_action_grants.decision()),
            ("launchable_catalogue", self.launchable_catalogue.decision()),
            ("record_content", self.record_content.decision()),
            ("channel_keys", self.channel_keys.decision()),
            ("action_offload", self.action_offload.decision()),
            ("own_device", self.own_device.decision()),
            ("workspace_nodes", self.workspace_nodes.decision()),
            ("personal", self.personal.decision()),
        ]
    }
}

/// What each installed app declared it may do on the action door, read from the lists search keeps:
/// a ceiling the broker decides an app's calls against, never a grant.
pub(crate) fn declared_access(
    lists: crate::lists::Lists,
) -> tilo_action_broker::app_actions::AppActionAccessResolver {
    Arc::new(move |app_id: &str| {
        lists
            .latest()
            .declared
            .get(app_id)
            .copied()
            .unwrap_or_default()
    })
}

/// What every app that connects to this session calls, whatever it is: the resources it reads and
/// invokes, the settings it keeps, and the launch its connection is admitted by. Serving none of
/// these refuses every app at its first call.
#[cfg(test)]
pub(crate) const EVERY_APP: &[&str] = &["published", "settings", "launcher"];

/// How a shipped app's source reaches a seam of the action door: a marker in its code, and the seam
/// that answers the call. A marker here is a call, so an app that starts calling a seam it did not
/// is found by the guard.
#[cfg(test)]
pub(crate) const CALLED_SEAMS: &[(&str, &str)] = &[
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
    ("ui::collaboration::record_content", "record_content"),
    (
        "ui::collaboration::channel_live_activity",
        "live_activities",
    ),
    ("ui::collaboration::open_selected", "file_clipboard"),
    ("ui::collaboration::file_clipboard", "file_clipboard"),
    ("ui::collaboration::write_attribution", "file_clipboard"),
    ("NearbyTransferCommand", "nearby_transfer"),
];

/// The seams shipped apps still call that this session serves nothing of: a list that only shrinks,
/// each entry a call that does not work yet. Serving one, or moving the app off it, removes its
/// line; a call to a seam this session leaves unserved and this list does not name fails the guard.
#[cfg(test)]
pub(crate) const UNSERVED_BUT_CALLED: &[&str] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    /// A seam serves what it holds, and says why when it holds nothing.
    #[test]
    fn a_seam_decides_what_it_serves_and_says_why_when_it_serves_nothing() {
        assert_eq!(Seam::served(7u8).into_option(), Some(7));
        assert_eq!(Seam::when(Some(7u8)).into_option(), Some(7));
        assert_eq!(Seam::when(None::<u8>).into_option(), None);
        assert_eq!(Seam::<u8>::unserved("no store here").into_option(), None);

        assert_eq!(Seam::served(7u8).decision(), None);
        assert_eq!(
            Seam::when(None::<u8>).decision(),
            None,
            "a seam this run has nothing for is still served"
        );
        assert_eq!(
            Seam::<u8>::unserved("no store here").decision(),
            Some("no store here")
        );
    }

    /// The action door's ceiling comes from the installed apps' signed manifests, read with the
    /// lists search keeps: an app Tilo cannot find declared nothing, and never more than its
    /// manifest says.
    #[test]
    fn an_apps_declared_access_is_what_its_manifest_says() {
        use tilo_action_broker::app_actions::AppActionAccess;

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
