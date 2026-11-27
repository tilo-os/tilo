//! The apps a workspace runs, kept as records of the store's `workspace.apps` kind
//! ([`tilo_resource_store::apps`]): installing an app writes its record, uninstalling removes
//! it, and undoing either takes it back as any change is taken back.
//!
//! The workspace's replica decides which surface a change of an app's records is decided under,
//! and whether the app may keep its own records, from the apps it last read from the store
//! ([`tilo_peer_state::WorkspaceApps`]). It reads them again whenever the store takes in a change
//! of them, once the store's lock is let go, and the link this device serves tells them to the
//! devices that keep no store of their own.

use std::collections::BTreeMap;
use std::sync::Arc;

use collab_types::store::wire::Command;
use collab_types::store::{Cell, Resource, Value};
use tilo_peer_state::workspace_membership::{AppPayload, AppRelease};
use tilo_peer_state::{InstalledApp, WorkspaceApps};
use tilo_resource_store::apps::{self, Fields};
use tilo_resource_store::{Stamp, Store};

use super::{HostedWorkspace, Workspaces};

impl HostedWorkspace {
    /// Read the apps the workspace runs from its store into its replica, and decide again every
    /// change the replica set aside for them. A store that never held a record of them leaves
    /// the replica reading the apps its governance kept before the store did.
    ///
    /// The apps the replica read now, with what reading them let in, or `None` when it read these
    /// already.
    pub(super) fn read_apps(&self) -> Option<(WorkspaceApps, tilo_peer_state::AdmissionReport)> {
        let apps = {
            let mut store = self.resources.store_mut();
            if !apps::ever_written(&store) {
                return None;
            }
            match in_store(&mut store) {
                Ok(apps) => apps,
                Err(error) => {
                    tracing::error!(workspace = %self.workspace(), %error, "the apps the workspace runs do not read");
                    return None;
                }
            }
        };
        let report = self.peer().read_apps(apps.clone(), tilo_clock::wall())?;
        Some((apps, report))
    }

    /// The apps the workspace runs as its store holds them, when it ever held a record of them.
    pub(super) fn apps_in_store(&self) -> Option<WorkspaceApps> {
        let mut store = self.resources.store_mut();
        if !apps::ever_written(&store) {
            return None;
        }
        in_store(&mut store).ok()
    }
}

impl Workspaces {
    /// Follow a change of the apps `hosted` runs its store took in since this last asked: its
    /// replica reads them again, the link this device serves tells them to the devices that keep
    /// no store, and the writes reading them let in are taken in as the writes before them were.
    pub(super) fn follow_apps(&self, hosted: &mut HostedWorkspace) {
        if !hosted.resources.take_apps_moved() {
            return;
        }
        self.read_apps(hosted);
    }

    /// [`HostedWorkspace::read_apps`], told and followed as [`Self::follow_apps`] tells them.
    pub(super) fn read_apps(&self, hosted: &HostedWorkspace) {
        let Some((apps, report)) = hosted.read_apps() else {
            return;
        };
        if let Some(link) = self.inner.direct.link(hosted.workspace_id()) {
            link.tell_apps(&apps);
        }
        if !report.released().is_empty() || report.replay_wanted {
            self.replay_soon(hosted.workspace().to_owned());
        }
    }

    /// Tell the devices on `workspace`'s link the apps it runs, as its store holds them: what a
    /// link that starts serving it tells first.
    pub(super) fn tell_apps(&self, hosted: &HostedWorkspace) {
        let (Some(apps), Some(link)) = (
            hosted.apps_in_store(),
            self.inner.direct.link(hosted.workspace_id()),
        ) else {
            return;
        };
        link.tell_apps(&apps);
    }

    /// Write each of `apps` as the record of the app it names: the version it runs, or, with
    /// `None`, no version, which removes the record. Written as this device's person, in one
    /// action, as the workspace's `install-app` or `uninstall-app`. An app with no record is
    /// uninstalled already. Answers the invocation, which undo takes back by.
    ///
    /// # Errors
    /// The workspace is not held here, or it or its store refused the write.
    pub(crate) async fn write_app_records(
        &self,
        workspace: &str,
        apps: Vec<(String, Option<InstalledApp>)>,
    ) -> Result<String, String> {
        self.keep_apps_change(workspace, |store, stamp| {
            let commands: Vec<Command> =
                apps.iter()
                    .filter_map(|(app, installed)| {
                        let id = apps::id_of(app);
                        match installed {
                            Some(installed) => Some(Command::Put {
                                kind: apps::KIND.to_owned(),
                                id,
                                resource: Resource {
                                    readers: collab_types::EVERY_MEMBER.to_owned(),
                                    parent: None,
                                    order: None,
                                    fields: fields_of(installed),
                                },
                                stamp: stamp(apps::INSTALL),
                            }),
                            None => store.index().get(apps::KIND, &id).is_some().then(|| {
                                Command::Remove {
                                    kind: apps::KIND.to_owned(),
                                    id,
                                    stamp: stamp(apps::UNINSTALL),
                                }
                            }),
                        }
                    })
                    .collect();
            match <[Command; 1]>::try_from(commands) {
                Ok([command]) => Some(command),
                Err(commands) if commands.is_empty() => None,
                Err(commands) => Some(Command::Batch(commands)),
            }
        })
        .await
    }

    /// Take back `invocation`, an install or uninstall of `workspace`'s apps, as this device's
    /// person: the apps it changed run as they did before it. Answers the undo's own invocation.
    ///
    /// # Errors
    /// The workspace is not held here, or it or its store refused the undo.
    #[cfg(test)]
    pub(crate) async fn undo_apps_change(
        &self,
        workspace: &str,
        invocation: &str,
    ) -> Result<String, String> {
        self.keep_apps_change(workspace, |_, stamp| {
            Some(Command::Undo {
                invocation: invocation.to_owned(),
                stamp: Stamp {
                    undoes: Some(invocation.to_owned()),
                    ..stamp(apps::INSTALL)
                },
            })
        })
        .await
    }

    /// Keep the change of `workspace`'s apps `command_of` drafts against its store, stamped by
    /// the stamp it is handed for a verb, as this device's person: none where it drafts none.
    /// Answers its invocation.
    async fn keep_apps_change(
        &self,
        workspace: &str,
        command_of: impl Fn(&Store, &dyn Fn(&str) -> Stamp) -> Option<Command> + Send + Sync,
    ) -> Result<String, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let device = self.signing_device();
        let invoker = hex::encode(device.cert.user_public_key);
        let invocation = uuid::Uuid::new_v4().to_string();
        let moment = tilo_clock::unix_now_ms();
        let stamp = |verb: &str| Stamp {
            verb: verb.to_owned(),
            invoker: invoker.clone(),
            invocation: invocation.clone(),
            moment,
            ..Stamp::default()
        };
        let (device, command_of, stamp) = (&device, &command_of, &stamp);
        super::wanted_keys::write_with_keys(&state, move |mut hosted| async move {
            let command = command_of(&self.store_of(&mut hosted), stamp);
            let Some(command) = command else {
                return Ok(());
            };
            let writes = tilo_resource_store::wire::draft(&mut self.store_of(&mut hosted), command)
                .map_err(|refused| refused.error.to_string())?;
            if writes.is_empty() {
                return Ok(());
            }
            self.keep_writes(hosted, Arc::clone(device), apps::APP, writes, None)
                .await
                .map(drop)
        })
        .await?;
        Ok(invocation)
    }

    /// Bring the apps `workspace`'s governance kept before its store did into the store, once:
    /// where the store never held a record of them and the governance names some. Keyed by app,
    /// so two devices bringing them over write the same records.
    ///
    /// This goes once the oldest build a workspace's devices may run is past 61782b0f16a, the
    /// last that wrote its apps to its governance: every workspace's apps are in its store then.
    pub(super) async fn bring_apps_over(&self, workspace: &str) {
        let Ok(state) = self.workspace(workspace).await else {
            return;
        };
        let hosted = state.lock().await;
        let apps = (hosted.member.is_none() && hosted.apps_in_store().is_none())
            .then(|| hosted.peer().apps_in_versions_map());
        drop(hosted);
        let Some(apps) = apps.filter(|apps| !apps.running().is_empty()) else {
            return;
        };
        let records = apps
            .running()
            .iter()
            .map(|(app, installed)| (app.clone(), Some(installed.clone())))
            .collect();
        if let Err(error) = self.write_app_records(workspace, records).await {
            tracing::error!(%workspace, %error, "the apps the workspace runs were not brought into its store");
        }
    }
}

/// The apps `store` holds the records of, at the heads it holds them at.
fn in_store(store: &mut Store) -> Result<WorkspaceApps, String> {
    let records = apps::read(store)?;
    let mut heads: Vec<[u8; 32]> = store
        .index()
        .documents()
        .filter(|doc| apps::holds_records(doc))
        .filter_map(|doc| store.index().heads(doc))
        .flatten()
        .map(|head| head.0)
        .collect();
    heads.sort_unstable();
    let mut running = BTreeMap::new();
    let mut ran: BTreeMap<String, Vec<AppRelease>> = BTreeMap::new();
    for (now, versions) in records.values() {
        // An app uninstalled since ran all the same: a change of its records is set aside, never
        // refused.
        for release in versions.iter().filter_map(installed_of) {
            let releases = ran.entry(release.release.app.clone()).or_default();
            releases.retain(|earlier| *earlier != release.release);
            releases.push(release.release);
        }
        if let Some(installed) = now.as_ref().and_then(installed_of) {
            running.insert(installed.release.app.clone(), installed);
        }
    }
    Ok(WorkspaceApps::new(running, ran).at(heads))
}

/// The app version `fields`, an app's record, names: none where it names no app.
fn installed_of(fields: &Fields) -> Option<InstalledApp> {
    let app = apps::text(fields, apps::ID);
    if app.is_empty() {
        return None;
    }
    let file = apps::text(fields, apps::FILE);
    let flatpak = apps::text(fields, apps::FLATPAK);
    Some(InstalledApp {
        release: AppRelease {
            app,
            version: apps::text(fields, apps::VERSION),
            surface: apps::text(fields, apps::SURFACE),
            package: apps::text(fields, apps::PACKAGE),
            publisher: apps::text(fields, apps::PUBLISHER),
            file: (!file.is_empty()).then_some(file),
            dev: apps::flag(fields, apps::DEV),
            pinned: apps::flag(fields, apps::PINNED),
            payload: if flatpak.is_empty() {
                AppPayload::Tapp
            } else {
                AppPayload::Flatpak { reference: flatpak }
            },
        },
        installed_by: apps::text(fields, apps::INSTALLED_BY),
        installed_at_ns: u64::try_from(apps::number(fields, apps::INSTALLED_AT))
            .unwrap_or_default(),
    })
}

/// The record of `installed`.
fn fields_of(installed: &InstalledApp) -> Fields {
    let release = &installed.release;
    let text = |value: &str| Cell::from(Value::Text(value.to_owned()));
    let mut fields = BTreeMap::from([
        (apps::ID.to_owned(), text(&release.app)),
        (apps::VERSION.to_owned(), text(&release.version)),
        (apps::INSTALLED_BY.to_owned(), text(&installed.installed_by)),
        (
            apps::INSTALLED_AT.to_owned(),
            Cell::from(Value::Int(
                i64::try_from(installed.installed_at_ns).unwrap_or(i64::MAX),
            )),
        ),
    ]);
    for (field, value) in [
        (apps::SURFACE, release.surface.as_str()),
        (apps::PACKAGE, release.package.as_str()),
        (apps::PUBLISHER, release.publisher.as_str()),
        (apps::FILE, release.file.as_deref().unwrap_or_default()),
    ] {
        if !value.is_empty() {
            fields.insert(field.to_owned(), text(value));
        }
    }
    if let AppPayload::Flatpak { reference } = &release.payload {
        fields.insert(apps::FLATPAK.to_owned(), text(reference));
    }
    for (field, set) in [(apps::DEV, release.dev), (apps::PINNED, release.pinned)] {
        if set {
            fields.insert(field.to_owned(), Cell::from(Value::Bool(true)));
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(app: &str, version: &str) -> AppRelease {
        AppRelease {
            app: app.into(),
            version: version.into(),
            surface: hex::encode(blake3::hash(version.as_bytes()).as_bytes()),
            package: "11".repeat(32),
            publisher: "22".repeat(32),
            file: Some("33".repeat(32)),
            dev: true,
            pinned: true,
            payload: AppPayload::Flatpak {
                reference: "app/org.example.App/x86_64/stable".into(),
            },
        }
    }

    /// A workspace an older build made keeps its apps in its governance's versions map: its copy
    /// admits by them from there until they are brought into its store, and by the same apps
    /// from the store after.
    #[tokio::test]
    async fn an_upgraded_devices_workspace_keeps_the_apps_it_ran() {
        use crate::workspaces::fixtures;
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = fixtures::hosting_device();
        let server = fixtures::test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let workspace = fixtures::hosted(workspaces).await;
        let app = "so.tilo.files";
        let row = InstalledApp {
            release: AppRelease {
                app: app.into(),
                version: "1.0.0".into(),
                surface: hex::encode([6; 32]),
                package: "11".repeat(32),
                publisher: "22".repeat(32),
                file: None,
                dev: false,
                pinned: false,
                payload: AppPayload::Tapp,
            },
            installed_by: hex::encode(cert.user_public_key),
            installed_at_ns: 1,
        };
        let state = workspaces.workspace(&workspace).await.unwrap();
        let hosted = state.lock().await;
        let before = hosted.apps_in_store();
        let wrote = hosted
            .peer()
            .write_versions_map_as_before(&row, &key, tilo_clock::wall());
        let from_map = hosted.peer().governance().app_version(app);
        drop(hosted);
        assert!(before.is_none(), "made before the store kept its apps");
        assert!(wrote.unwrap());
        assert_eq!(
            from_map,
            Some(row.clone()),
            "the copy admits by the map it kept them in"
        );
        workspaces.bring_apps_over(&workspace).await;
        let hosted = state.lock().await;
        let (held, read) = (hosted.apps_in_store(), hosted.peer().apps());
        drop(hosted);
        let running = BTreeMap::from([(app.to_owned(), row)]);
        assert_eq!(
            held.as_ref().map(WorkspaceApps::running),
            Some(&running),
            "the store holds the app it ran"
        );
        assert_eq!(held.as_ref(), Some(&*read));
        server.shutdown().await;
    }

    #[test]
    fn an_apps_record_reads_back_as_the_version_it_was_written_from() {
        let installed = InstalledApp {
            release: release("so.tilo.files", "2.0.0"),
            installed_by: "44".repeat(32),
            installed_at_ns: 1_770_000_000_000_000_000,
        };
        assert_eq!(installed_of(&fields_of(&installed)), Some(installed));
    }
}
