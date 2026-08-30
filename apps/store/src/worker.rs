//! One serialized package worker for catalogue reads and mutations.
//!
//! The old Store binary maintained independent refresh and action threads.
//! They could query the same backends concurrently, a refresh click was not
//! actually connected, and each path repeated snapshot construction. This
//! actor owns the backend boundary: reads are coalesced by the queue, package
//! mutations finish before the following refresh, and the UI only drains
//! immutable snapshots.

use std::sync::mpsc;
use std::time::Duration;

use crate::config::{RegistryConfig, load_registries};
use crate::model::ActionKind;
use crate::sources::{flathub_placeholder_to_item, merge, qos_to_store_item};
use crate::{FlatpakRef, StoreItem};

const REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
pub enum Request {
    Refresh,
    Act(StoreItem, ActionKind),
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub items: Vec<StoreItem>,
    pub status_line: String,
}

pub struct StoreWorker {
    requests: mpsc::Sender<Request>,
    snapshots: mpsc::Receiver<Snapshot>,
}

impl StoreWorker {
    pub fn spawn(opened_ref: Option<FlatpakRef>) -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        std::thread::spawn(move || run(request_rx, snapshot_tx, opened_ref));
        Self {
            requests: request_tx,
            snapshots: snapshot_rx,
        }
    }

    pub fn request(&self, request: Request) -> bool {
        self.requests.send(request).is_ok()
    }

    pub fn try_snapshot(&self) -> Option<Snapshot> {
        self.snapshots.try_recv().ok()
    }
}

fn run(
    requests: mpsc::Receiver<Request>,
    snapshots: mpsc::Sender<Snapshot>,
    opened_ref: Option<FlatpakRef>,
) {
    let staging = crate::actions::qpkg_staging_dir()
        .unwrap_or_else(|| std::env::temp_dir().join("qos-store-staged"));
    let _ = std::fs::create_dir_all(&staging);

    if !send_refresh(&snapshots, opened_ref.as_ref(), None) {
        return;
    }
    loop {
        match requests.recv_timeout(REFRESH_INTERVAL) {
            Ok(Request::Refresh) | Err(mpsc::RecvTimeoutError::Timeout) => {
                if !send_refresh(&snapshots, opened_ref.as_ref(), None) {
                    break;
                }
            }
            Ok(Request::Act(item, action)) => {
                let outcome = crate::actions::execute(&item, action, &staging);
                if !send_refresh(&snapshots, opened_ref.as_ref(), Some(outcome.status_line)) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn send_refresh(
    snapshots: &mpsc::Sender<Snapshot>,
    opened_ref: Option<&FlatpakRef>,
    action_status: Option<String>,
) -> bool {
    let items = collect_items(opened_ref);
    let status_line = action_status.unwrap_or_else(|| {
        if items.is_empty() {
            "No apps yet. Check your registries.toml + network.".to_string()
        } else {
            format!("Loaded {} apps.", items.len())
        }
    });
    snapshots.send(Snapshot { items, status_line }).is_ok()
}

fn collect_items(opened_ref: Option<&FlatpakRef>) -> Vec<StoreItem> {
    let registries = load_registries().registries;
    let installed_qos = crate::installed::load_installed();
    let installed_flat = crate::flathub_client::installed_flatpaks();

    let mut qos_items = Vec::new();
    for registry in registries.iter().filter(|registry| registry.enabled) {
        match crate::registry_client::fetch_entries(registry) {
            Ok(entries) => qos_items.extend(
                entries
                    .iter()
                    .map(|entry| qos_to_store_item(entry, &registry.name, &installed_qos)),
            ),
            Err(error) => tracing::warn!("registry {} refresh failed: {error}", registry.name),
        }
    }

    let mut flatpak_items = match crate::flathub_client::fetch_popular(200) {
        Ok(popular) => popular
            .iter()
            .map(|item| flathub_placeholder_to_item(item, &installed_flat))
            .collect(),
        Err(error) => {
            tracing::warn!("flathub refresh failed: {error}");
            Vec::new()
        }
    };
    if let Some(reference) = opened_ref
        && !flatpak_items
            .iter()
            .any(|item: &StoreItem| item.id == reference.app_id)
    {
        let installed_version = installed_flat
            .iter()
            .find_map(|(id, version)| (id == &reference.app_id).then(|| version.clone()));
        flatpak_items.insert(0, reference.clone().into_store_item(installed_version));
    }

    tracing::debug!("{}", registries_summary(&registries));
    merge(qos_items, flatpak_items)
}

fn registries_summary(registries: &[RegistryConfig]) -> String {
    let enabled = registries
        .iter()
        .filter(|registry| registry.enabled)
        .count();
    format!(
        "configured registries: {} total / {} enabled",
        registries.len(),
        enabled
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_is_cloneable_data_not_an_eager_side_effect() {
        let item = FlatpakRef::parse(
            "[Flatpak Ref]\nName=org.example.App\nTitle=Example\n",
            "/tmp/example.flatpakref",
        )
        .unwrap()
        .into_store_item(None);
        let request = Request::Act(item, ActionKind::FlatpakInstall);
        assert!(matches!(
            request,
            Request::Act(_, ActionKind::FlatpakInstall)
        ));
    }
}
