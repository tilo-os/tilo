//! Production policy inputs for predictive hydration.
//!
//! Prediction is optional, so an unreadable or ambiguous system signal denies
//! speculation rather than guessing. Foreground hydration is unaffected.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::smart::{DEFAULT_MAX_QUEUED_PREDICTIVE_BYTES, Heuristics, PredictivePolicy};

const MIN_FREE_BYTES: u64 = 1024 * 1024 * 1024;
const MIN_FREE_PERCENT: u64 = 10;
const MIN_BATTERY_PERCENT: u8 = 25;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(15);
const NETWORK_PROBE_LIMIT: usize = 1024 * 1024;
const NETWORK_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn connected_unmetered_device(output: &str) -> bool {
    let mut connected = false;
    let mut unmetered = false;
    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "GENERAL.STATE" => {
                if connected && unmetered {
                    return true;
                }
                connected = value.trim_start().starts_with("100");
                unmetered = false;
            }
            "GENERAL.METERED" => {
                let value = value.trim().to_ascii_lowercase();
                unmetered = value == "no" || value.starts_with("no (");
            }
            _ => {}
        }
    }
    connected && unmetered
}

fn full_connectivity(output: &str) -> bool {
    output.trim().eq_ignore_ascii_case("full")
}

async fn network_allows() -> bool {
    let Ok((connectivity, devices)) = tokio::task::spawn_blocking(|| {
        let connectivity = bounded_command::run(
            "nmcli",
            &["-t", "-f", "CONNECTIVITY", "general"],
            None,
            NETWORK_PROBE_LIMIT,
            NETWORK_PROBE_TIMEOUT,
        );
        let devices = bounded_command::run(
            "nmcli",
            &[
                "-t",
                "-f",
                "GENERAL.STATE,GENERAL.METERED",
                "device",
                "show",
            ],
            None,
            NETWORK_PROBE_LIMIT,
            NETWORK_PROBE_TIMEOUT,
        );
        (connectivity, devices)
    })
    .await
    else {
        return false;
    };
    let (Ok(connectivity), Ok(devices)) = (connectivity, devices) else {
        return false;
    };
    connectivity.status.success()
        && devices.status.success()
        && std::str::from_utf8(&connectivity.stdout).is_ok_and(full_connectivity)
        && std::str::from_utf8(&devices.stdout).is_ok_and(connected_unmetered_device)
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

async fn sample(cache_root: &Path) -> PredictivePolicy {
    PredictivePolicy {
        enabled: true,
        network_allows: network_allows().await,
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
    heuristics.set_policy(sample(&cache_root).await);
    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
        // The initial sample was applied synchronously above.
        interval.tick().await;
        loop {
            interval.tick().await;
            heuristics.set_policy(sample(&cache_root).await);
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_monitor_policies_require_good_network_battery_and_storage() {
        // Phase `network_requires_full_connectivity_and_a_connected_unmetered_device`.
        {
            assert!(full_connectivity("full\n"));
            assert!(!full_connectivity("limited\n"));
            assert!(connected_unmetered_device(
                "GENERAL.STATE:100 (connected)\nGENERAL.METERED:no (guessed)\n"
            ));
            assert!(!connected_unmetered_device(
                "GENERAL.STATE:100 (connected)\nGENERAL.METERED:yes\n"
            ));
            assert!(connected_unmetered_device(
                "GENERAL.STATE:30 (disconnected)\nGENERAL.METERED:no\n\
                 GENERAL.STATE:100 (connected)\nGENERAL.METERED:no\n"
            ));
        }
        // Phase `battery_policy_is_conservative_while_discharging`.
        {
            assert!(battery_allows(false, false, 0));
            assert!(!battery_allows(true, false, 100));
            assert!(!battery_allows(true, true, MIN_BATTERY_PERCENT - 1));
            assert!(battery_allows(true, true, MIN_BATTERY_PERCENT));
        }
        // Phase `storage_requires_both_an_absolute_and_percentage_reserve`.
        {
            let gib = 1024_u64 * 1024 * 1024;
            assert!(storage_values_allow(10 * gib, 2 * gib, 1));
            assert!(!storage_values_allow(100 * gib, gib - 1, 1));
            assert!(!storage_values_allow(100 * gib, 2 * gib, 1));
            assert!(!storage_values_allow(0, 0, 4096));
        }
    }
}
