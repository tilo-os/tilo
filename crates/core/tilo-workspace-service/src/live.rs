//! The resources live now in the workspaces this device holds (a track playing, a timer running,
//! a call), as the shell shows them in the bar, quick settings and on the lock screen.
//!
//! Each workspace's store keeps which of its resources are live, derived from what it holds as
//! the index is ([`tilo_resource_store::Store::live`]): nothing writes a row. The service shows
//! here what its person may read of them after each change it keeps, and whoever shows them is
//! woken. A row names the device whose change made the resource live; the session tells which
//! devices it reaches now ([`LiveRows::reach`]), and a device it no longer reaches takes its
//! rows with it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

/// The resource the device's daemon publishes its live rows as, to the desktop on its shell link
/// alone ([`LiveRows::reading`]): the desktop shows them, filtered by the devices it reaches.
pub const RESOURCE: &str = "workspaces.live";

/// One live resource, as a reader is shown it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LiveRow {
    /// The workspace it is in.
    pub workspace: String,
    /// Its kind (`tl.<app>.<Type>`) and id.
    pub kind: String,
    pub id: String,
    /// The device whose change made it live, its key in hex; empty where that change named none.
    pub device: String,
    /// What that device calls itself; empty where its change named none.
    pub device_name: String,
    /// When it went live, ISO-8601 UTC; empty where its change named no time.
    pub since: String,
}

/// The live resources of this device's workspaces. Cloned, it is the same rows.
#[derive(Clone, Default)]
pub struct LiveRows(Arc<Shown>);

#[derive(Default)]
struct Shown {
    /// Per workspace, its live resources its person may read, as its store last listed them.
    rows: Mutex<BTreeMap<String, Vec<LiveRow>>>,
    /// Per workspace, the devices this one reaches in it now, by key in hex; a workspace it was
    /// told nothing of reaches only this device.
    reached: Mutex<BTreeMap<String, BTreeSet<String>>>,
    /// This device's key in hex, as its service is.
    here: Mutex<String>,
    revision: AtomicU64,
    /// Wakes whoever shows the rows when they moved.
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl LiveRows {
    /// Wake `wake` whenever the rows move.
    pub fn wake_with(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.0.wake.lock_recover() = Some(wake);
    }

    /// Grows whenever a row comes, goes, or a device is reached or lost.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.revision.load(Ordering::Acquire)
    }

    /// Every row live on this device, on a device this one reaches in its workspace, or on no
    /// device named, the one live longest first.
    #[must_use]
    pub fn rows(&self) -> Vec<LiveRow> {
        let here = self.0.here.lock_recover().clone();
        let reached = self.0.reached.lock_recover();
        let mut rows: Vec<LiveRow> = self
            .0
            .rows
            .lock_recover()
            .values()
            .flatten()
            .filter(|row| {
                row.device.is_empty()
                    || row.device == here
                    || reached
                        .get(&row.workspace)
                        .is_some_and(|devices| devices.contains(&row.device))
            })
            .cloned()
            .collect();
        drop(reached);
        rows.sort_by(|a, b| (&a.since, &a.kind, &a.id).cmp(&(&b.since, &b.kind, &b.id)));
        rows
    }

    /// Whether `device` (its key in hex) is this device.
    #[must_use]
    pub fn is_here(&self, device: &str) -> bool {
        *self.0.here.lock_recover() == device
    }

    /// The devices this one reaches in `workspace` now, replacing what it knew.
    pub fn reach(&self, workspace: &str, devices: impl IntoIterator<Item = [u8; 32]>) {
        let devices: BTreeSet<String> = devices.into_iter().map(hex::encode).collect();
        let mut reached = self.0.reached.lock_recover();
        if reached.get(workspace) == Some(&devices) {
            return;
        }
        reached.insert(workspace.to_owned(), devices);
        drop(reached);
        self.moved();
    }

    #[cfg(any(feature = "apps", feature = "agents"))]
    /// This device is `device` (its key): what is live on it is live here whoever it reaches.
    pub(crate) fn here_is(&self, device: [u8; 32]) {
        let device = hex::encode(device);
        let mut here = self.0.here.lock_recover();
        if *here != device {
            *here = device;
            drop(here);
            self.moved();
        }
    }

    /// Show `rows` as `workspace`'s live resources now.
    pub(crate) fn show(&self, workspace: &str, rows: Vec<LiveRow>) {
        let mut shown = self.0.rows.lock_recover();
        if shown
            .get(workspace)
            .map_or(rows.is_empty(), |held| *held == rows)
        {
            return;
        }
        if rows.is_empty() {
            shown.remove(workspace);
        } else {
            shown.insert(workspace.to_owned(), rows);
        }
        drop(shown);
        self.moved();
    }

    /// Every row as the stores last listed them, and this device's key, as [`RESOURCE`] reads:
    /// unfiltered, since which devices count as reached is the reader's to say.
    #[must_use]
    pub fn reading(&self) -> Vec<u8> {
        let reading = Reading {
            here: self.0.here.lock_recover().clone(),
            rows: self.0.rows.lock_recover().clone(),
        };
        serde_json::to_vec(&reading).unwrap_or_default()
    }

    /// Show what a [`Self::reading`] of another `LiveRows` says, in place of what these showed:
    /// the rows a reader follows at the device that keeps the workspaces.
    ///
    /// # Errors
    /// The reading does not read.
    pub fn take_reading(&self, reading: &[u8]) -> Result<(), String> {
        let Reading { here, rows } = serde_json::from_slice(reading)
            .map_err(|error| format!("the live rows do not read: {error}"))?;
        let mut moved = false;
        {
            let mut held = self.0.here.lock_recover();
            if *held != here {
                *held = here;
                moved = true;
            }
            drop(held);
        }
        {
            let mut shown = self.0.rows.lock_recover();
            if *shown != rows {
                *shown = rows;
                moved = true;
            }
            drop(shown);
        }
        if moved {
            self.moved();
        }
        Ok(())
    }

    fn moved(&self) {
        self.0.revision.fetch_add(1, Ordering::AcqRel);
        let wake = self.0.wake.lock_recover().clone();
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// A [`LiveRows::reading`]: this device's key and, per workspace, its rows.
#[derive(serde::Serialize, serde::Deserialize)]
struct Reading {
    here: String,
    rows: BTreeMap<String, Vec<LiveRow>>,
}
