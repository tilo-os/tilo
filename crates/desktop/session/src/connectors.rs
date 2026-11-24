//! The outside services the session's own services keep records in step with
//! (`docs/architecture/apps.md` §1 and §8; `docs/designs/app-language/README.md`, "origins").
//!
//! The device's daemon runs every origin connector: the app's own launch with no window, in the
//! sandbox its signed manifest declares for that origin, one per app, origin and workspace,
//! shared by the app's instances and the session's services alike. The session asks the daemon
//! for a connection to the connectors its workspaces need ([`reconcile`]) on the shell link the
//! daemon started it with, and hands each to the service that reads that origin.
//!
//! Nothing here is per app: which origins an app has is its manifest's, and what a connector
//! does is the app runtime's.

use std::cell::RefCell;
use std::collections::BTreeSet;

/// Which connector: the app, its origin and the workspace.
pub type Key = (String, String, String);

/// The connectors the session's services reach now, by app, origin and workspace.
#[derive(Default)]
pub(crate) struct Reached(RefCell<BTreeSet<Key>>);

/// Start the connectors the session's services keep records in step with
/// (`docs/architecture/apps.md` §1, §8): for each origin a service names, the process of the app
/// whose package declares it, in the sandbox its signed manifest gives that origin, and the service
/// reached through it.
/// A connector is the app's own launch with no window, which the device's daemon runs: it alone
/// has the network and the ownership its sandbox gives it. A session whose connector cannot be
/// reached says why and goes on: the app's records then stand as they are, and its views show
/// what they last held.
///
/// A workspace needs an app's connector while it has a place of that app's records, or a window
/// of the app there: one workspace's connector writes that workspace's record and its media
/// scope, so a record the record service keeps per workspace is kept in step in each workspace
/// that shows it, never only in the one in front. `reconcile` runs every pass: it reaches the
/// connectors the shell's workspaces now need and forgets the ones none does, which the daemon
/// keeps running for the app's instances.
pub(crate) fn reconcile(session: &crate::Session) {
    // An origin's own crate registers how the platform reaches its connector, so a session reaches
    // one without naming the crate that keeps the service.
    tilolang_mail::register();
    tilolang_youtube::register();
    tilolang_music::register();
    tilolang_calendar::register();
    let connectors = session.shared.services.borrow().connectors();
    if connectors.is_empty() {
        return;
    }
    let wake = session.shared.wake_handle.clone();
    // The service that keeps an origin in step is told what wakes the loop: an answer from the
    // service it asked, which the next pass polls and writes.
    session.shared.services.borrow_mut().wake_origins(&wake);
    let Some(apps) = session.shared.launch.clone() else {
        return;
    };
    // The app the record `resource` belongs to, when it is one the shell holds: `tl:<app>:<id>`
    // names its app, and an app's own resources (`<app>.<Kind>`) name theirs in the catalogue.
    let app_of = |resource: &str| -> Option<String> {
        resource
            .strip_prefix("tl:")
            .and_then(|rest| rest.split_once(':'))
            .map(|(app, _)| app.to_owned())
            .or_else(|| {
                session
                    .shared
                    .services
                    .borrow()
                    .resources()
                    .into_iter()
                    .find(|spec| spec.id.as_str() == resource)
                    .and_then(|spec| spec.app().map(str::to_owned))
            })
    };
    // Every workspace the shell holds, by id, and the apps whose records stand in it: what needs a
    // connector there. A workspace with no place of an app's records needs none, however the
    // person moves between workspaces.
    let mut needed: BTreeSet<Key> = BTreeSet::new();
    for (workspace, records) in session.shared.layout.borrow().records_by_workspace() {
        for app in records.iter().filter_map(|record| app_of(record)) {
            for connector in &connectors {
                if connector.app == app {
                    needed.insert((app.clone(), connector.origin.clone(), workspace.clone()));
                }
            }
        }
    }
    // An app the session runs itself keeps its records here, with no place of it on the shell: the
    // person's actions on those records — a search, an agent's task — queue what the outside
    // service answers, so that app's origin runs in the workspace in front whether or not a place
    // shows it. Without it an app whose only surface is the record it holds could not reach its
    // service at all.
    let active = session.shared.active_workspace_id();
    if !active.is_empty() {
        let runs = |app: &str| session.shared.services.borrow().module(app).is_some();
        for connector in &connectors {
            if runs(&connector.app) {
                needed.insert((
                    connector.app.clone(),
                    connector.origin.clone(),
                    active.clone(),
                ));
            }
        }
    }
    // The connectors no workspace needs any more are forgotten: the daemon keeps each running for
    // the app's instances, and a workspace that shows the app again reaches it once more.
    session
        .origins
        .0
        .borrow_mut()
        .retain(|key| needed.contains(key));
    let processes = match apps.device_processes() {
        Ok(processes) => processes,
        Err(error) => {
            if needed
                .iter()
                .any(|key| !session.origins.0.borrow().contains(key))
            {
                tracing::warn!(%error, "no outside origin is reached: its records stand as they are");
            }
            return;
        }
    };
    for key in needed {
        // A connector already reached needs nothing: its service was reached then, and attaching
        // to it again every pass would make a connection no reader holds.
        if session.origins.0.borrow().contains(&key) {
            continue;
        }
        let (app, origin, workspace) = &key;
        let reached = processes
            .origin(app, origin, workspace)
            .map_err(|error| format!("{app}'s {origin} is not reached: {error}"))
            .and_then(|connection| {
                let mut services = session.shared.services.borrow_mut();
                tilo_service::Service::connect(&mut *services, origin, connection)
                    .map_err(|error| format!("{app}'s {origin} is not reached: {error}"))
            });
        match reached {
            Ok(()) => {
                session.origins.0.borrow_mut().insert(key);
            }
            Err(error) => tracing::warn!(
                app = %app,
                origin = %origin,
                workspace = %workspace,
                %error,
                "an outside origin's connector does not run: its records stand as they are"
            ),
        }
    }
}
