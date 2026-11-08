//! Starting an application from the desktop.
//!
//! A desktop surface asks for an app by name — a launch string — and this
//! crate turns that name into a process: what to run, with which arguments,
//! in which environment, and in whose workspace. A launch is one of
//!
//! - a Tilo package id (`so.tilo.notes`), which starts through
//!   `tilo-launcher` in the package's sandbox ([`plan`]);
//! - `host:<desktop-id>`, one of the machine's own applications, which runs
//!   from its `.desktop` entry the way the host runs it ([`plan`]);
//! - `window:<n>`, a window already arriving: nothing to start;
//! - `record:<app>:<record>` and `content:<app>:<hash>`, the app opening
//!   one of its records or content another app handed over;
//! - a document or a link, which goes to the app that claims its type or
//!   scheme ([`routing`]).
//!
//! An app may already be running: one that keeps a single window is raised
//! instead of started again, and one that opens another window each time is
//! asked for it through its own New Window action ([`instance`]). A launch
//! carrying a document or a link always starts the app's own command line,
//! which hands it to the process it already runs.
//!
//! Everything the session knows that the plan needs — the workspace and what was
//! granted in it, the policy the person's authority there is projected from
//! ([`Workspace`]), the displays and the person's session variables
//! ([`env`]) — is passed in as data, so a plan is assertable without a
//! process appearing. [`spawner`] is the part that does start something, and
//! reports how it ended so a crash can be told from a normal exit.
//!
//! [`downloaded`] is the other side of the same coin: a program the person
//! downloaded has no entry until they add one, so the registry follows its
//! windows, offers it, and keeps the entry Tilo writes for it — one session
//! resource the search and the app sheets read.
//!
//! `memory` is where the apps the person is not looking at go: the session
//! moves each app between the foreground and background tiers of its cgroup
//! by what it can see, and gives memory back from the background ones first.

pub mod authority;
pub mod doors;
pub mod downloaded;
pub mod env;
pub mod instance;
pub mod launch;
mod memory;
pub mod output;
pub mod plan;
pub mod routing;
pub mod spawner;
pub mod workspace;

pub use authority::Authority;
pub use doors::{Connects, Doors, HeldDoor, Routed, StartAlone, WindowConnections};
pub use env::SessionEnvironment;
pub use instance::{Answer, Instance, Shown, Windows, package_identities};
pub use launch::{Launch, Open};
pub use plan::{Kind, PlanContext, PlanError, Planned};
pub use spawner::{Cause, End, Event, Spawner, Started};
pub use workspace::Workspace;

/// Name the surface the package `planned` runs ships, before its process
/// starts: the person-record kinds it declares are what the identity service
/// this host serves admits the app's join into the person's own store against,
/// so the service knows a dev-launched app's surface the way it knows an
/// installed one's. A launch that runs no package names none, and neither does
/// one whose package ships no signed surface: such an app declares nothing of
/// the person's.
pub fn name_surface(planned: &Planned) {
    if planned.kind != Kind::Package {
        return;
    }
    match app_catalogue::apps::signed_surface_of_from_env(&planned.app) {
        Ok(Some(surface)) => collaboration::app_surface::register_package(&planned.app, &surface),
        Ok(None) => tracing::debug!(
            app = %planned.app,
            "the package ships no signed surface; it declares no person records"
        ),
        Err(error) => {
            tracing::warn!(%error, app = %planned.app, "the app's signed surface is unreadable");
        }
    }
}

/// Name every installed app's signed surface ([`name_surface`] for all of
/// them), as the session starts: a service that writes an app's person record
/// joins the person's own store before any window of that app ran — the
/// filesystem daemon writes the workspace tree as the session starts — so the
/// surface its package ships must be known without a launch.
pub fn name_installed_surfaces() {
    match app_catalogue::apps::signed_surfaces_from_env() {
        Ok(surfaces) => {
            for (app, _name, surface) in surfaces {
                collaboration::app_surface::register_package(&app, &surface);
            }
        }
        Err(error) => tracing::warn!(%error, "the installed apps' surfaces are unreadable"),
    }
}
