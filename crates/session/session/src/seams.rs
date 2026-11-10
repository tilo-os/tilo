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

use tilo_action_broker::services::{
    AppActionGrants, ApplicationServices, AudioOutputServices, BrowserServiceSessionServices,
    DisplayControlServices, DriveServices, FileClipboardServices, HostServices, LauncherServices,
    NearbyTransferServices, PublishedResources, RealtimeServices, ShortcutLibraryServices,
    SpeakerServices, WifiContinuityServices, WorkspaceControlServices, WorkspaceMemberships,
};
use tilo_broker_core::services::{
    ContentStoreServices, KeyboardShortcutServices, LegacyWorkspaceProjection, LocalPersonServices,
    OwnDeviceActions, RecordChangeServices, RecordIndexChanges, SearchIndexServices, TrackerRuns,
    WorkspaceGovernanceServices, WorkspaceNodeActions,
};
use tilo_broker_core::steps::{ContinuationServices, HostStepServices};
use tilo_channels::ChannelKeyProvider;

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

/// What one seam holds for the broker: the service, the option it may be, or nothing and why.
macro_rules! seam_field {
    ($this:ident.$field:ident) => {
        $this.$field.into_option()
    };
    ($this:ident.$field:ident, default) => {
        $this.$field.into_option().unwrap_or_default()
    };
}

/// This session's decision for each of the broker's seams, in the order the broker's
/// [`HostServices`] names them: what it serves, and for what it serves nothing, the reason it was
/// left out. One list, because the decision, the conversion and the reasons are the same seam.
macro_rules! seams {
    ($( $(#[$meta:meta])* $field:ident: $ty:ty $(=> $default:tt)? ),* $(,)?) => {
        pub(crate) struct Seams {
            $( $(#[$meta])* pub(crate) $field: Seam<$ty> ),*
        }

        impl Seams {
            /// What the broker's doors read: every seam, as this session decided it.
            pub(crate) fn into_host_services(self) -> HostServices {
                HostServices { $( $field: seam_field!(self.$field $(, $default)?) ),* }
            }

            /// Each seam's name, and why it is not served (`None` when it is).
            pub(crate) fn decisions(&self) -> Vec<(&'static str, Option<&'static str>)> {
                vec![ $( (stringify!($field), self.$field.decision()) ),* ]
            }
        }
    };
}

seams! {
    approved_checks: Arc<dyn tilo_action_broker::collab_app_broker::ApprovedCheckServices>,
    committed_documents: Arc<dyn tilo_action_broker::services::NativeDocumentReads>,
    native_media: Arc<dyn tilo_action_broker::services::NativeMediaServices>,
    audio_output: Arc<dyn AudioOutputServices>,
    workspace_memberships: Arc<dyn WorkspaceMemberships>,
    tracker_runs: Arc<dyn TrackerRuns>,
    agents: Arc<dyn tilo_broker_core::services::AgentRoster>,
    native_certificates: Arc<dyn tilo_action_broker::certificate_authority::CertificateAuthority>,
    local_person: Arc<dyn LocalPersonServices>,
    record_changes: Arc<dyn RecordChangeServices>,
    legacy_workspace: Arc<dyn LegacyWorkspaceProjection>,
    launcher: Arc<dyn LauncherServices>,
    published: Arc<dyn PublishedResources>,
    workspace_governance: Arc<dyn WorkspaceGovernanceServices>,
    shortcut_library: Arc<dyn ShortcutLibraryServices>,
    keyboard_shortcuts: Arc<dyn KeyboardShortcutServices>,
    host_steps: Vec<Arc<dyn HostStepServices>> => default,
    continuations: Arc<dyn ContinuationServices>,
    search_index: Arc<dyn SearchIndexServices>,
    record_index_changes: Arc<dyn RecordIndexChanges>,
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
    app_action_grants: Arc<dyn AppActionGrants>,
    launchable_catalogue: PathBuf,
    record_content: Arc<tilo_action_broker::record_content::RouteContent>,
    channel_keys: ChannelKeyProvider,
    action_offload: Arc<dyn tilo_actions::offload::ActionOffload>,
    own_device: Arc<dyn OwnDeviceActions>,
    workspace_nodes: Arc<dyn WorkspaceNodeActions>,
    personal: tilo_peer_state::PersonalSync,
}

/// The records this session reads outside their apps, from the engine that holds its workspaces.
pub(crate) fn document_reads(
    engine: Option<&Arc<crate::engine::Engine>>,
) -> Seam<Arc<dyn tilo_action_broker::services::NativeDocumentReads>> {
    Seam::when(engine.map(|engine| {
        Arc::new(crate::record_reads::Records::new(Arc::clone(engine)))
            as Arc<dyn tilo_action_broker::services::NativeDocumentReads>
    }))
}

/// What each installed app declared it may do on the action door, read from the lists search keeps:
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
