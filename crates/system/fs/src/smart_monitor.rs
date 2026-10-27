//! Production policy inputs for predictive hydration.
//!
//! Prediction is optional, so an unreadable or ambiguous system signal denies
//! speculation rather than guessing. Foreground hydration is unaffected.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tilo_nm::{NetworkState, NmWatch};

use crate::smart::{DEFAULT_MAX_QUEUED_PREDICTIVE_BYTES, Heuristics, PredictivePolicy};

const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;
const MIN_FREE_PERCENT: u64 = 10;
const MIN_BATTERY_PERCENT: u8 = 25;
/// Power and storage are sampled on this interval while files are being
/// opened, and a missing `NetworkManager` connection is retried on it.
/// Network changes apply as they are signalled.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(15);

/// The network gate: the internet is reachable and the primary connection,
/// the one traffic actually leaves on, is known not to be metered. A second
/// unmetered device does not make a metered primary tether free, and an
/// unreadable `NetworkManager` reads as unknown, which denies.
fn network_allows(state: &NetworkState) -> bool {
    state.allows_speculative_traffic()
}

/// Wait for the followed network state to change. A follower that stopped
/// (its bus connection closed) is dropped, so the next sample denies and the
/// loop reconnects.
async fn network_changed(network: &mut Option<NmWatch>) {
    match network {
        Some(watch) => {
            if watch.changed().await.is_none() {
                *network = None;
            }
        }
        None => std::future::pending().await,
    }
}

fn battery_allows(on_battery: bool, present: bool, percent: u8) -> bool {
    !on_battery || (present && percent >= MIN_BATTERY_PERCENT)
}

fn power_allows() -> bool {
    let snapshot = tilo_power_mgmt::sysfs_battery_snapshot();
    battery_allows(
        tilo_power_mgmt::sysfs_on_battery(),
        snapshot.present,
        snapshot.percent,
    )
}

fn storage_values_allow(blocks: u64, available: u64, fragment_size: u64) -> bool {
    if blocks == 0 || fragment_size == 0 {
        return false;
    }
    let available_bytes = available.saturating_mul(fragment_size);
    available_bytes >= MIN_FREE_BYTES
        && available
            .saturating_mul(100)
            .checked_div(blocks)
            .unwrap_or(0)
            >= MIN_FREE_PERCENT
}

fn storage_allows(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stats` points to writable storage;
    // the value is read only after libc reports success.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: the successful call above initialized the complete structure.
    let stats = unsafe { stats.assume_init() };
    #[cfg(target_os = "linux")]
    let (blocks, available) = (stats.f_blocks, stats.f_bavail);
    #[cfg(not(target_os = "linux"))]
    let (blocks, available) = (u64::from(stats.f_blocks), u64::from(stats.f_bavail));
    storage_values_allow(blocks, available, stats.f_frsize)
}

fn sample(cache_root: &Path, network: Option<&NmWatch>) -> PredictivePolicy {
    PredictivePolicy {
        enabled: true,
        network_allows: network.is_some_and(|watch| network_allows(&watch.current())),
        power_allows: power_allows(),
        storage_allows: storage_allows(cache_root),
        max_queued_bytes: DEFAULT_MAX_QUEUED_PREDICTIVE_BYTES,
    }
}

/// Apply a real system snapshot before the mount becomes visible, then keep it
/// current. Missing `NetworkManager`, ambiguous metering, unreadable power state,
/// or storage pressure disables only speculative work and cancels its queue.
pub async fn start(
    heuristics: Arc<Heuristics>,
    cache_root: Option<PathBuf>,
) -> Option<tokio::task::JoinHandle<()>> {
    let Some(cache_root) = cache_root else {
        heuristics.set_policy(PredictivePolicy {
            enabled: false,
            network_allows: false,
            power_allows: false,
            storage_allows: false,
            max_queued_bytes: 0,
        });
        return None;
    };
    let mut network = tilo_nm::watch().await.ok();
    let mut applied = sample(&cache_root, network.as_ref());
    heuristics.set_policy(applied);
    Some(tokio::spawn(async move {
        loop {
            // Power and storage only matter to an open's predictions, so
            // they are sampled every interval while files are opened, and
            // at the first open after a quiet interval; an idle mount
            // sleeps. That first open is judged by the previous
            // sample, and a gate the fresh one closes cancels what it
            // queued. Network changes apply as signalled.
            let busy = heuristics.take_opened();
            tokio::select! {
                () = tokio::time::sleep(SAMPLE_INTERVAL), if busy => {}
                () = heuristics.opened(), if !busy => {}
                () = network_changed(&mut network) => {}
            }
            if network.is_none() {
                network = tilo_nm::watch().await.ok();
            }
            let policy = sample(&cache_root, network.as_ref());
            // An unchanged policy is not re-applied: applying one cancels
            // every queued and running prediction.
            if policy != applied {
                heuristics.set_policy(policy);
                applied = policy;
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_nm::{LinkKind, NmConnectivity, NmState, PrimaryLink};

    fn online(kind: LinkKind, metered: Option<bool>) -> NetworkState {
        NetworkState {
            state: NmState::ConnectedGlobal,
            connectivity: NmConnectivity::Full,
            metered,
            primary: Some(PrimaryLink {
                kind,
                name: None,
                device: None,
            }),
        }
    }

    #[test]
    fn network_allows_requires_full_and_unmetered_primary() {
        assert!(network_allows(&online(LinkKind::Wired, Some(false))));
        assert!(network_allows(&online(LinkKind::Wifi, Some(false))));
        // Ethernet (unmetered) and a phone tether (metered) are both up, and
        // NetworkManager made the tether primary: its global Metered is the
        // tether's. The idle Ethernet must not license traffic on the tether.
        assert!(
            !network_allows(&online(LinkKind::Wired, Some(true))),
            "a metered primary connection denies speculation"
        );
        assert!(
            !network_allows(&online(LinkKind::Wifi, None)),
            "unknown metering denies speculation"
        );
        assert!(
            !network_allows(&NetworkState {
                connectivity: NmConnectivity::Limited,
                ..online(LinkKind::Wired, Some(false))
            }),
            "limited connectivity denies speculation"
        );
        assert!(
            !network_allows(&NetworkState::unknown()),
            "an unreadable NetworkManager denies speculation"
        );
    }

    #[test]
    fn smart_monitor_policies_require_good_battery_and_storage() {
        {
            assert!(battery_allows(false, false, 0));
            assert!(!battery_allows(true, false, 100));
            assert!(!battery_allows(true, true, MIN_BATTERY_PERCENT - 1));
            assert!(battery_allows(true, true, MIN_BATTERY_PERCENT));
        }
        {
            let gib = 1024_u64 * 1024 * 1024;
            assert!(storage_values_allow(10 * gib, 2 * gib, 1));
            assert!(!storage_values_allow(100 * gib, gib - 1, 1));
            assert!(!storage_values_allow(100 * gib, 2 * gib, 1));
            assert!(!storage_values_allow(0, 0, 4096));
        }
    }
}
