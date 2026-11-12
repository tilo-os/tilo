//! The page's palette is the session's search: the one role a native desktop runs stands here too,
//! over what this page publishes.
//!
//! [`tilo_session::search::SessionSearch`] is the desktop's search beyond its own windows and
//! commands — what stands over the desk as published actions, Settings' pages, the apps this page
//! has, the records its workspaces hold, the question the person types, and the sources they
//! switched off — and the rows a host alone can carry out. The desk draws the palette and the row
//! the person chose ([`tilo_desktop_surface::SearchHost`]); this page hands over what only it
//! knows and takes the choices back to the desk it arranges.

use std::rc::Rc;
use std::sync::Arc;

use tilo_actions::DoorError;
use tilo_actions::remote::{RemoteActionTarget, application_provider};
use tilo_desktop_search::SearchPreferences;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_search::{LaunchPlace, Target};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_desktop_surface::session::Reveal;
use tilo_session::platform::apps::Apps;
use tilo_session::search::{AppActions, SearchOptions, SessionSearch};

/// The page's action host: a row that reaches an app's own window, which the page opens the app
/// for.
///
/// A native desktop's action host starts an app and delivers its actions into the process it
/// started. The page draws that app's window itself, so it asks the desk to bring the app's window
/// forward — opening it when the app is closed — and asks for the action to run in it once it is up,
/// as a window asking another app to show something does ([`super::session_asks`]).
struct PageActions {
    handle: DesktopHandle,
    reveals: crate::browser::Reveals,
}

impl AppActions for PageActions {
    /// The page arranges its own windows: every other row is the desk's, which launches, focuses
    /// and opens addresses where this page does.
    fn performs(&self, target: &Target) -> bool {
        matches!(target, Target::Invoke(_))
    }

    /// What apps publish through an action host: on the page each app publishes into the desk it
    /// draws, and the desk's own door runs those rows.
    fn published(&self, _: &str) -> Vec<PublishedActions> {
        Vec::new()
    }

    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    fn invoke(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), DoorError> {
        let app = application_provider(&target.provider)
            .map(|(_, app)| app.to_owned())
            .ok_or_else(|| DoorError::Unavailable {
                code: "unknown_application".into(),
                reason: "That row names no app this page opens".into(),
            })?;
        if !self.showing(&app) {
            self.handle
                .dispatch(ChromeEffect::OpenApp {
                    app: app.clone(),
                    beside: None,
                    background: None,
                    float: None,
                })
                .map_err(|reason| DoorError::Unavailable {
                    code: "application_unavailable".into(),
                    reason,
                })?;
        }
        self.reveals.ask(Reveal {
            app,
            // The app's own record: the window it has of this workspace.
            record: String::new(),
            resource: target.address.resource.clone(),
            action: target.address.action,
            arguments,
        });
        Ok(())
    }

    /// Runs are the session's, and this page runs none.
    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), DoorError> {
        Err(DoorError::Unavailable {
            code: "unknown_invocation".into(),
            reason: "This page runs no invocations".into(),
        })
    }

    /// A row this page alone reaches is answered by the desk, which opens addresses
    /// ([`Target::Open`]).
    fn open_uri(&self, _: &str, _: &str) -> Result<(), DoorError> {
        Err(DoorError::Unavailable {
            code: "no_uri_opener".into(),
            reason: "The desk opens addresses here".into(),
        })
    }

    /// The page runs none of the machine's programs.
    fn start_host_app(&self, _: &str) -> Result<(), DoorError> {
        Err(DoorError::Unavailable {
            code: "no_host_apps".into(),
            reason: "This page runs no programs of the machine's".into(),
        })
    }

    /// A launch is the desk's to arrange ([`Self::performs`]).
    fn open_application(&self, _: &str, _: &str, _: LaunchPlace) -> Result<(), DoorError> {
        Err(DoorError::Unavailable {
            code: "desk_arranges_launches".into(),
            reason: "The desk opens applications here".into(),
        })
    }
}

impl PageActions {
    /// Whether a window of `app` stands on the desk.
    fn showing(&self, app: &str) -> bool {
        self.handle
            .read(|tree| tilo_desktop_surface::navigation::window_of_app(tree, app).is_some())
    }
}

/// The session's search over this page's desk, mounted as a native desktop mounts it: the bundle's
/// apps as the apps installed here, the records the workspace's indexes name as `store` holds them
/// (what the page's replication took in, noted in its one stream of record changes), the page's
/// own model service as the gateway its questions run on, and the page as the host of the rows that
/// reach an app's window.
pub(crate) fn mount(
    handle: &DesktopHandle,
    person: &str,
    store: tilo_session::platform::Store,
    wake: Arc<dyn Fn() + Send + Sync>,
    reveals: crate::browser::Reveals,
) -> Rc<SessionSearch> {
    let search = Rc::new(SessionSearch::new(
        SearchOptions {
            preferences: SearchPreferences::default(),
            records: Some(tilo_session::records::Records::over(
                store,
                crate::replicated::changes(),
            )),
            ..SearchOptions::default()
        },
        Some(Arc::new(crate::page_apps::PageApps) as Arc<dyn Apps>),
        person,
        Some(Rc::new(PageActions {
            handle: handle.clone(),
            reveals,
        }) as Rc<dyn AppActions>),
        Some(wake),
        Some(Arc::new(PageGateway) as Arc<dyn tilo_session::platform::Models>),
    ));
    handle.set_search_host(Rc::clone(&search) as Rc<dyn tilo_desktop_surface::SearchHost>);
    search
}

/// The model service this page runs, as the session's ask reaches it: its models hold the page's
/// own handles, so each question builds the capability where it is asked.
struct PageGateway;

impl tilo_session::platform::Models for PageGateway {
    fn models(&self) -> Option<ui::models::Models> {
        Some(crate::host_services::page_models())
    }
}
