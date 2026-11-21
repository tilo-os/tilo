//! How each synced cloud account stands, as the cloud service reports it: the health its mount
//! last reported, and a sign-in the account status store records until the first sync. Files
//! keeps no status of its own; it shows what the service, which writes that store, says.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use tilo_cloud_files::cloud::{CloudClient, CloudMount, MountHealth};

use crate::remote_accounts::{RemoteAccount, RemoteAccountHealth};

/// How often the service is asked again while a window is open.
const EVERY: Duration = Duration::from_secs(2);

/// The cloud service's reports, read on a thread of their own while the window lives.
pub(super) struct CloudStatus {
    reports: Receiver<Vec<CloudMount>>,
    stop: Arc<AtomicBool>,
}

impl CloudStatus {
    /// Ask the cloud service how its accounts stand, now and every [`EVERY`] after, waking the
    /// window when the answer changes. `None` where no thread can run it.
    pub(super) fn follow(wake: ui::WakeHandle) -> Option<Self> {
        let (send, reports) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        ui::rt::spawn_thread("files-cloud-status", move || {
            let mut client: Option<CloudClient> = None;
            let mut last: Option<Vec<CloudMount>> = None;
            while !stopped.load(Ordering::Relaxed) {
                if client.is_none() {
                    client = tilo_cloud_files::cloud::default_files_socket()
                        .and_then(|socket| CloudClient::connect(socket).ok());
                }
                // A service that went away is asked again on the next turn.
                let report = client.as_mut().and_then(|client| client.mounts().ok());
                if report.is_none() {
                    client = None;
                }
                let report = report.unwrap_or_default();
                if last.as_ref() != Some(&report) {
                    if send.send(report.clone()).is_err() {
                        return;
                    }
                    wake.wake();
                    last = Some(report);
                }
                if ui::rt::sleep_thread(EVERY).is_err() {
                    return;
                }
            }
        })
        .map_err(|error| tracing::warn!(%error, "Files cannot follow how its cloud accounts stand"))
        .ok()?;
        Some(Self { reports, stop })
    }

    /// The newest report since the last one taken, if any arrived.
    pub(super) fn take(&self) -> Option<Vec<CloudMount>> {
        self.reports.try_iter().last()
    }
}

impl Drop for CloudStatus {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// `accounts` as the cloud service reports them in `mounts`: a mount's own health, else a sign-in
/// with nothing synced since. A disabled account stays disabled, and one the service does not
/// report is not checked yet.
pub(super) fn with_status(accounts: &mut [RemoteAccount], mounts: &[CloudMount]) {
    for account in accounts {
        if account.health == RemoteAccountHealth::Disabled {
            continue;
        }
        let Some(mount) = mounts
            .iter()
            .find(|mount| mount.account == account.id.as_str())
        else {
            continue;
        };
        account.health = match mount.health {
            Some(MountHealth::Syncing) => RemoteAccountHealth::Syncing,
            Some(MountHealth::Healthy) => RemoteAccountHealth::Healthy,
            Some(MountHealth::Offline) => RemoteAccountHealth::Offline,
            Some(MountHealth::NeedsAuthentication) => RemoteAccountHealth::NeedsAuthentication,
            Some(MountHealth::Error) => RemoteAccountHealth::Error,
            None if mount.signed_in => RemoteAccountHealth::SignedIn,
            None => RemoteAccountHealth::NeverChecked,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_cloud_files::cloud::CloudCapabilities;

    fn account(id: &str, health: RemoteAccountHealth) -> RemoteAccount {
        RemoteAccount {
            id: accounts::AccountId::from(id),
            kind: crate::remote_accounts::RemoteAccountKind::Nextcloud,
            label: id.into(),
            host: "cloud.example".into(),
            health,
            files_mode: accounts::cloud_types::FilesMode::Sync,
            mount: Some(format!("/home/ada/Remote/{id}").into()),
        }
    }

    fn mount(id: &str, health: Option<MountHealth>, signed_in: bool) -> CloudMount {
        CloudMount {
            account: id.into(),
            mount_point: format!("/home/ada/Remote/{id}").into(),
            health,
            paused: false,
            capabilities: CloudCapabilities::default(),
            signed_in,
        }
    }

    #[test]
    fn accounts_stand_as_the_cloud_service_reports_them() {
        let mut accounts = vec![
            account("just-signed-in", RemoteAccountHealth::NeverChecked),
            account("synced", RemoteAccountHealth::NeverChecked),
            account("signed-in-then-synced", RemoteAccountHealth::NeverChecked),
            account("off", RemoteAccountHealth::Disabled),
            account("unreported", RemoteAccountHealth::NeverChecked),
        ];
        with_status(
            &mut accounts,
            &[
                mount("just-signed-in", None, true),
                mount("synced", Some(MountHealth::Healthy), false),
                mount("signed-in-then-synced", Some(MountHealth::Syncing), true),
                mount("off", Some(MountHealth::Healthy), false),
            ],
        );
        let health: Vec<_> = accounts.iter().map(|account| account.health).collect();
        assert_eq!(
            health,
            [
                RemoteAccountHealth::SignedIn,
                RemoteAccountHealth::Healthy,
                RemoteAccountHealth::Syncing,
                RemoteAccountHealth::Disabled,
                RemoteAccountHealth::NeverChecked,
            ],
            "a mount's own report wins over a sign-in, and a disabled account stays so"
        );
        assert_eq!(
            RemoteAccountHealth::SignedIn.unavailable_reason(),
            "Signed in: the folder fills in as it syncs"
        );
    }
}
