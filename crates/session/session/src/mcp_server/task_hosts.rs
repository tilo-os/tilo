//! The apps a desktop session's task programs compile against: every installed app's package or
//! signed surface, and the packages the session runs itself.

use std::collections::BTreeSet;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;
use tilo_runtime::device::tasks::{Catalogue, TaskCatalogue};

use super::{ApiAsk, SessionServices, WAIT};

/// The apps a task's programs may name here, as the session knows them: each tilolang app's own
/// package, verified as a launch verifies it, which declares the names a program calls its
/// actions and roots by, and the signed surface of every other app.
/// One catalogue, whoever reads it: `tilolang check` reads the same packages and surfaces from
/// the apps' directories.
fn installed_apps(kept: &BTreeSet<String>) -> Vec<runtime_portal::process_link::App> {
    use runtime_portal::process_link::App;
    let mut apps = Vec::new();
    let mut named: BTreeSet<String> = kept.clone();
    for package in tilo_runtime::device::package_runs::task_packages() {
        if !named.insert(package.app.clone()) {
            continue;
        }
        apps.push(App::Module(package.module));
    }
    let installed = match app_catalogue::apps::signed_surfaces_from_env() {
        Ok(installed) => installed,
        Err(error) => {
            tracing::warn!(%error, "MCP server: the installed apps' surfaces are not read");
            Vec::new()
        }
    };
    for (app, title, surface) in installed {
        if named.contains(app.as_str()) {
            continue;
        }
        let Ok(surface) = String::from_utf8(surface) else {
            tracing::warn!(%app, "MCP server: an app's surface is not text");
            continue;
        };
        named.insert(app.to_string());
        apps.push(App::Surface {
            app: app.to_string(),
            title,
            surface,
        });
    }
    apps
}

/// Where a session's task catalogue is read: what its own APIs answer of the packages it runs
/// itself ([`ApiAsk::Apps`]), and the apps this device has installed.
pub(crate) struct SessionCatalogue {
    session: Arc<SessionServices>,
}

impl SessionCatalogue {
    pub(crate) const fn new(session: Arc<SessionServices>) -> Self {
        Self { session }
    }
}

impl TaskCatalogue for SessionCatalogue {
    fn catalogue(&self, _workspace: &str) -> Catalogue {
        // The apps the task's programs are compiled against: every package the session runs
        // itself (the shell), whose records a program reads by the app, every installed tilolang
        // app by its package, every native app this device has, from its signed surface, and the
        // platform's own (the device and its layout, Settings).
        let (done, whole) = tilo_actions::remote::pending();
        self.session.ask(ApiAsk::Apps(done));
        let mut apps = whole.wait_timeout(WAIT).ok().flatten().unwrap_or_default();
        let kept = self.session.kept.lock_recover().clone();
        apps.extend(installed_apps(&kept));
        Catalogue {
            apps,
            changed_at_document: kept,
            ..Catalogue::default()
        }
    }
}
