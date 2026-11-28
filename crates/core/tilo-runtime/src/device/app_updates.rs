//! A workspace's apps follow the store, as a computer's updater keeps its programs current.
//!
//! The version of an app a device runs is the one its workspace records, and an update is the
//! workspace's change. So the daemon that keeps workspaces for its person keeps each of them
//! current: it moves every app a workspace runs to the newest release the workspace's
//! repositories offer this device, on start, every hour after, and at once when an app here fails
//! at start every time or the runtime its release runs on is offered by no repository any more.
//! This is the one updater of a device's apps, for every workspace it keeps alike.
//!
//! The move is the person's own standing choice, made through the workspace's install action as
//! `app update` makes it ([`super::dev_builds::ask_as_person`]): the workspace decides whether
//! they may change its apps, every device fetches the release and checks its signature, package
//! root and publisher against the workspace's trust as for any install, and the workspace's
//! history keeps the release it ran before. An agent changes nothing here: it never acts as the
//! person's device.
//!
//! What a person chose stays: a pinned app is left at its version, a developer's own build is
//! left alone, and an app whose workspace went back to an earlier release (`app rollback`) stays
//! there until it no longer runs here.

use std::sync::Arc;
use std::time::Duration;

use tilo_actions::remote::{ActionCallError, ReceiptStatus};
use tilo_peer_state::workspace_membership::{AppPayload, AppRelease};

use super::dev_builds::{Unasked, ask_as_person};
use super::workspaces::ServiceWorkspaces;

/// How often the store is asked for newer releases.
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);

/// Whether the release a workspace runs of an app does not run on this device.
pub(crate) type Stuck = dyn Fn(&str, &AppRelease) -> bool + Send + Sync;

/// Move the apps of every workspace this device keeps to the store's newest releases, from now
/// on: at once, every [`CHECK_EVERY`], and whenever `failures` stops an app here or `cache` finds
/// a release it cannot run.
pub(crate) fn follow_store(
    workspaces: Arc<ServiceWorkspaces>,
    cache: Arc<super::packages::PackageCache>,
    failures: Arc<crate::apps::Failures>,
) {
    tilo_rt::app().spawn(async move {
        let mut stopped = failures.watch();
        let mut cannot_run = cache.watch_cannot_run();
        let mut holds = workspaces.copies().holds_changed();
        let stuck: Arc<Stuck> = Arc::new(move |workspace, release| {
            failures.stopped(workspace, &release.app) || cache.cannot_run(&release.package)
        });
        loop {
            for summary in workspaces.copies().list().await {
                follow_store_in(&workspaces, &summary.workspace, &stuck).await;
            }
            tokio::select! {
                () = tilo_rt::sleep(CHECK_EVERY) => {}
                moved = stopped.changed() => if moved.is_err() { return; },
                moved = cannot_run.changed() => if moved.is_err() { return; },
                held = holds.changed() => if held.is_err() { return; },
            }
        }
    });
}

/// Move each app `workspace` runs to the newest release its repositories offer this device, as
/// its person, where [`next_release`] says it moves.
pub(crate) async fn follow_store_in(
    workspaces: &Arc<ServiceWorkspaces>,
    workspace: &str,
    stuck: &Arc<Stuck>,
) {
    let Ok(installed) = workspaces.copies().app_versions(workspace).await else {
        return;
    };
    let running: Vec<AppRelease> = installed
        .into_iter()
        .map(|installed| installed.release)
        .filter(|release| {
            !release.pinned && !release.dev && matches!(release.payload, AppPayload::Tapp)
        })
        .collect();
    if running.is_empty() {
        return;
    }
    let registries = app_cli::config::registries_from_rows(&serde_json::json!({
        "registries": workspaces.copies().registries(workspace).await.unwrap_or_default(),
    }))
    .unwrap_or_default();
    let Ok(listing) = tokio::task::spawn_blocking(move || {
        app_cli::config::with_registries(registries, app_cli::all_offers)
    })
    .await
    else {
        return;
    };
    for release in running {
        let earlier = workspaces
            .copies()
            .earlier_app_versions_of(workspace, &release.app)
            .await
            .unwrap_or_default();
        let Some(newest) = next_release(
            &release,
            &listing.apps,
            &earlier,
            stuck(workspace, &release),
        ) else {
            continue;
        };
        let from = release.version.clone();
        let to = newest.version.clone();
        let asked = {
            let workspaces = Arc::clone(workspaces);
            let workspace = workspace.to_owned();
            tokio::task::spawn_blocking(move || {
                ask_as_person(
                    &workspaces,
                    &workspace,
                    tilo_action_policy::roles::governance::INSTALL_APP,
                    app_cli::device::release_of(&newest),
                )
            })
            .await
        };
        let app = &release.app;
        match asked {
            Ok(Ok(ReceiptStatus::Completed { .. })) => {
                tracing::info!(%workspace, app, %from, %to, "a workspace's app moved to the store's newest release");
            }
            // A workspace that does not let this person change its apps is its owners' to update.
            Ok(Err(Unasked::Refused(
                ActionCallError::UnknownAction | ActionCallError::NotPermitted,
            ))) => {
                tracing::debug!(%workspace, app, %to, "this person may not change the workspace's apps");
            }
            Ok(Err(Unasked::Refused(refused))) => {
                tracing::warn!(%workspace, app, %to, error = %refused.said(), "the workspace refused moving an app to the store's newest release");
            }
            Ok(Ok(status)) => {
                tracing::warn!(%workspace, app, %to, ?status, "moving a workspace's app to the store's newest release did not complete");
            }
            Ok(Err(Unasked::NoWorkspace | Unasked::Unanswered)) | Err(_) => {
                tracing::warn!(%workspace, app, %to, "the workspace did not answer moving an app to the store's newest release");
            }
        }
    }
}

/// The release `running` moves to from `offers`: the newest one of its app, when it is newer.
/// A workspace that went back from a newer release (`earlier` holds one) stays where it went,
/// unless the release it went to does not run here (`stuck`).
fn next_release(
    running: &AppRelease,
    offers: &[app_cli::CatalogueApp],
    earlier: &[AppRelease],
    stuck: bool,
) -> Option<app_cli::CatalogueApp> {
    let newer =
        |version: &str| tilo_app_package::version::compare(version, &running.version).is_gt();
    let newest = offers
        .iter()
        .filter(|offer| offer.package_id == running.app)
        .max_by(|left, right| tilo_app_package::version::compare(&left.version, &right.version))
        .filter(|offer| newer(&offer.version))?;
    let went_back = earlier.iter().any(|before| newer(&before.version));
    (stuck || !went_back).then(|| newest.clone())
}
