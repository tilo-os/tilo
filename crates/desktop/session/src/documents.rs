//! Which app opens a file or a link here.
//!
//! Files' Open, a search row and a link all ask the same question, and it is answered once
//! ([`tilo_app_launch::routing`]): the app this workspace's rules or the person's own defaults
//! name for the type, else a Tilo app that claims it, else the app this machine opens the type
//! with, else nothing and the person is asked which app they want. What this module adds is
//! where those defaults are read from — the workspace's own record, the person's settings, and
//! this machine's `mimeapps.list` — so no caller assembles them itself.

use std::path::Path;
use std::sync::Arc;

use tilo_app_launch::Launch;
use tilo_app_launch::routing;
use tilo_desktop_search::providers::host_apps::HostAppKind;
use tilo_session_core::desktop::state::DesktopHandle;

use crate::lists::AppLists;
use runtime_portal::tools::{AppKind, DocumentApp};

/// What this session decides an open with, read once: this device's apps, the defaults that
/// decide among them, and the machine's own lists.
struct Deciding {
    lists: Arc<AppLists>,
    personal: desktop_policy::PersonalDefaults,
    host: desktop_policy::MimeAppsChain,
    workspace: Option<desktop_policy::WorkspaceDefaults>,
}

impl Deciding {
    /// Read what decides an open in `workspace`: its own record when the shell shows it, the
    /// person's defaults as their settings hold them, and the machine's default applications.
    fn read(shell: Option<&DesktopHandle>, workspace: &str) -> Self {
        Self {
            // The lists search reads, kept current by their own worker: the same apps a row, a
            // card and this answer name.
            lists: crate::app_icons::app_lists().unwrap_or_default(),
            personal: desktop_policy::PersonalDefaults::load(),
            host: desktop_policy::mimeapps::session_chain(),
            workspace: shell.and_then(|shell| {
                shell.read(|tree| {
                    tree.workspaces
                        .iter()
                        .find(|shown| shown.id().to_string() == workspace)
                        .map(|shown| shown.default_apps.clone())
                })
            }),
        }
    }

    /// The defaults that decide among `self`'s apps, for `person`.
    fn defaults<'a>(&'a self, person: &'a str) -> routing::Defaults<'a> {
        routing::Defaults {
            person,
            personal: &self.personal,
            workspace: self.workspace.as_ref(),
            host: Some(&self.host),
        }
    }
}

/// Every app that opens the document at `path` in `workspace`, the one it opens with today first:
/// what Files' "Open with" lists, and what opening the file starts.
pub(crate) fn openers(
    shell: Option<&DesktopHandle>,
    workspace: &str,
    person: &str,
    path: &Path,
) -> Vec<Launch> {
    let deciding = Deciding::read(shell, workspace);
    tracing::debug!(
        path = %path.display(),
        candidates = deciding.lists.candidates.len(),
        handlers = deciding.personal.handlers.len(),
        workspace_defaults = deciding.workspace.as_ref().map_or(0, |ws| ws.policy.len()),
        "which apps open a file"
    );
    routing::documents(path, &deciding.lists.candidates, &deciding.defaults(person))
}

/// The app that opens `uri` in `workspace`; `None` when nothing here takes it.
pub(crate) fn opener(
    shell: Option<&DesktopHandle>,
    workspace: &str,
    person: &str,
    uri: &str,
) -> Option<Launch> {
    let deciding = Deciding::read(shell, workspace);
    routing::link(uri, &deciding.lists.candidates, &deciding.defaults(person))
}

/// Where a link `launch` opens reaches its app: the app's `open-link` on the resource its signed
/// manifest offers it on, which starts the app when no window of it runs. `None` for a launch that
/// is no Tilo app opening a link, or an app that offers no `open-link`.
pub(crate) fn link_delivery(
    workspace: &str,
    launch: &Launch,
) -> Option<(
    tilo_actions::remote::RemoteActionTarget,
    serde_json::Map<String, serde_json::Value>,
)> {
    let Launch::Package {
        app,
        open: Some(tilo_app_launch::Open::Uri(link)),
    } = launch
    else {
        return None;
    };
    let lists = crate::app_icons::app_lists().unwrap_or_default();
    link_offer(&lists, app).map(|resource| {
        (
            tilo_actions::remote::application_action_target(
                workspace,
                app,
                resource,
                tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION,
            ),
            tilo_actions::remote::open_link_arguments(link),
        )
    })
}

/// The resource `app`'s signed manifest offers `open-link` on.
fn link_offer(lists: &AppLists, app: &str) -> Option<String> {
    lists
        .launchable
        .iter()
        .find(|launchable| launchable.package_id == app)?
        .offers
        .iter()
        .find(|offer| offer.action == tilo_actions::remote::APPLICATION_OPEN_LINK_ACTION)
        .map(|offer| offer.resource.clone())
}

/// The app `launch` starts, as a launch names it: a Tilo package's id, or a desktop-file id of one
/// of the machine's own apps.
pub(crate) fn app_of(launch: &Launch) -> Option<&str> {
    match launch {
        Launch::Package { app, .. } => Some(app),
        Launch::Host { desktop_id, .. } => Some(desktop_id),
        _ => None,
    }
}

/// The apps that open a document, as Files' "Open with" rows read them, the one it opens with today
/// first: what each is called, the glyph its app rows wear, and where this device got it, which
/// tells two rows of one name apart.
pub(crate) fn candidates(launches: &[Launch], lists: &AppLists) -> Vec<DocumentApp> {
    let mut rows: Vec<DocumentApp> = Vec::with_capacity(launches.len());
    for launch in launches {
        let Some(app) = app_of(launch) else {
            continue;
        };
        // A Tilo app that keeps a desktop entry of its own is listed once, as itself: both launches
        // name the same app.
        if rows.iter().any(|row| row.id == app) {
            continue;
        }
        rows.push(candidate(app, lists));
    }
    for (index, row) in rows.iter_mut().enumerate() {
        row.is_default = index == 0;
    }
    rows
}

/// One candidate as a row names it: the name its entry gives it (a Tilo app's name in the person's
/// language, or the localized `Name` of the machine's own desktop entry, which search lists it by),
/// and the glyph its own app rows wear.
fn candidate(app: &str, lists: &AppLists) -> DocumentApp {
    let name = lists
        .candidates
        .iter()
        .find(|listed| listed.package_id == app || listed.host_desktop_id() == Some(app))
        .map_or_else(|| app.to_owned(), |listed| listed.display_name.clone());
    let category = lists
        .applications
        .iter()
        .find(|listed| listed.id == app)
        .and_then(|listed| listed.category.clone())
        .or_else(|| {
            lists
                .host_apps
                .iter()
                .find(|listed| listed.desktop_id == app)
                .and_then(|listed| listed.category.clone())
        });
    DocumentApp {
        id: app.to_owned(),
        name,
        icon: Some(
            ui::icons::app_glyph(app, category.as_deref())
                .name()
                .to_owned(),
        ),
        // Where this device got it, always: two rows for apps one name names apart by it, and a
        // window handing a file over reads it to know a Tilo app from the machine's own.
        source: source_of(app, lists),
        artwork: None,
        is_default: false,
    }
}

/// Where this device got `app`: a Flatpak or the system's own package for one of the machine's apps,
/// its own package for a Tilo app.
fn source_of(app: &str, lists: &AppLists) -> Option<AppKind> {
    if let Some(host) = lists
        .host_apps
        .iter()
        .find(|listed| listed.desktop_id == app)
    {
        return Some(match host.kind {
            HostAppKind::Flatpak => AppKind::Flatpak,
            HostAppKind::System => AppKind::System,
        });
    }
    lists
        .applications
        .iter()
        .any(|listed| listed.id == app)
        .then_some(AppKind::Tilo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_search::providers::AppEntry;
    use tilo_desktop_search::providers::host_apps::{HostAppEntry, HostAppKind};

    /// A machine with a Flatpak editor whose entry names it in words, and the same app installed as
    /// a Tilo package, which the two rows then tell apart by where each came from.
    fn lists() -> AppLists {
        AppLists {
            applications: vec![AppEntry {
                id: "so.tilo.editor".into(),
                title: "Editor".into(),
                category: Some("Development".into()),
            }],
            host_apps: vec![HostAppEntry {
                desktop_id: "org.gnome.TextEditor".into(),
                name: "Editor".into(),
                kind: HostAppKind::Flatpak,
                category: Some("Utility".into()),
                ..HostAppEntry::default()
            }],
            candidates: vec![
                desktop_policy::AppCandidate::package(
                    "so.tilo.editor".into(),
                    "Editor".into(),
                    &[],
                    &["text/plain".into()],
                    &[],
                    false,
                ),
                desktop_policy::AppCandidate::host(
                    "org.gnome.TextEditor",
                    "Editor".into(),
                    &["text/plain".into()],
                )
                .expect("an entry claiming a type is a candidate"),
            ],
            ..AppLists::default()
        }
    }

    fn host(text: &str) -> Launch {
        Launch::Host {
            desktop_id: text.into(),
            open: None,
        }
    }

    /// A host candidate is named by its entry, wears its category's glyph, and says where the
    /// machine got it.
    #[test]
    fn a_host_candidate_is_named_by_its_entry() {
        let rows = candidates(&[host("org.gnome.TextEditor")], &lists());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Editor", "not its desktop id");
        assert_eq!(rows[0].icon.as_deref(), Some("box"), "a utility's glyph");
        assert!(rows[0].is_default, "the app it opens with today");
        assert_eq!(
            rows[0].source,
            Some(AppKind::Flatpak),
            "where the machine got it, which a window handing a file over reads"
        );

        let both = candidates(
            &[
                Launch::Package {
                    app: "so.tilo.editor".into(),
                    open: None,
                },
                host("org.gnome.TextEditor"),
            ],
            &lists(),
        );
        assert_eq!(both[0].source, Some(AppKind::Tilo));
        assert_eq!(both[1].source, Some(AppKind::Flatpak));
        assert_eq!(both[1].name, "Editor");
        // A Tilo app that keeps a desktop entry of its own is one row, not two.
        let repeated = candidates(
            &[host("org.gnome.TextEditor"), host("org.gnome.TextEditor")],
            &lists(),
        );
        assert_eq!(repeated.len(), 1);
    }

    /// A link reaches a Tilo app through the `open-link` its manifest offers, on the resource it
    /// names; an app offering none, or a launch that opens no link, is started as before.
    #[test]
    fn a_link_reaches_the_app_through_its_offered_open_link() {
        use tilo_app_package::manifest::{OfferKind, OfferOperation, OfferedAction};
        let offer = |action: &str| OfferedAction {
            resource: "tl.so.tilo.maps.Atlas".into(),
            resource_kind: None,
            action: action.into(),
            label: "Open link".into(),
            description: None,
            kind: OfferKind::default(),
            operation: OfferOperation::View,
            confirm: false,
            asks: None,
            keywords: Vec::new(),
            icon: None,
        };
        let app = |id: &str, offers| tilo_broker_core::services::LaunchableApplication {
            package_id: id.into(),
            display_name: id.into(),
            unconfined: false,
            offers,
            capabilities: Vec::new(),
            surface: None,
        };
        let lists = AppLists {
            launchable: vec![
                app(
                    "so.tilo.maps",
                    vec![offer("show-recent"), offer("open-link")],
                ),
                app("so.tilo.notes", vec![offer("show-recent")]),
            ],
            ..AppLists::default()
        };
        assert_eq!(
            link_offer(&lists, "so.tilo.maps").as_deref(),
            Some("tl.so.tilo.maps.Atlas")
        );
        assert_eq!(link_offer(&lists, "so.tilo.notes"), None);
        assert_eq!(link_offer(&lists, "so.tilo.absent"), None);
    }
}
