//! Live system status for the top bar: battery, network, volume.
//!
//! Two of the three readings cost a subprocess (`nmcli`, `wpctl`), and
//! the frame loop must never wait on one — a 200ms `fork`/`exec` is a
//! dropped frame. So reads happen on a plain OS thread (not a tokio
//! task, which would stall the notification service behind the
//! blocking `Command::output`), and the frame loop only copies a
//! mutex-guarded snapshot.
//!
//! [`StatusBridge::spawn`] is called from real-session constructors
//! only — a `--shots` render must produce the same pixels everywhere,
//! so with no poller the readings stay `None`, rendered as *nothing*.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use os_actions::{LinkKind, LinkStatus, VolumeState};
use qos_power_mgmt::{BatterySnapshot, BatteryState};

/// Fast enough that plugging in a charger shows up while the user is
/// still thinking about it, slow enough that two `nmcli` calls a
/// minute is the whole cost.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Every field is `Option` because "not read yet" and "this machine
/// has none" are both real and both drawn as absence, not a zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStatus {
    /// `Some` with `present == false` on a machine with no battery.
    pub battery: Option<BatterySnapshot>,
    pub link: Option<LinkStatus>,
    pub volume: Option<VolumeState>,
}

/// `level` is nominally `0.0..=1.0`, but reads come from `wpctl` output and
/// aren't validated against that range, so clamp defensively: the cast
/// below is only exact because the value is pinned to `0.0..=100.0` first.
fn volume_percent(level: f32) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0.0..=100.0 immediately above, so the cast neither truncates nor loses sign"
    )]
    let pct = (level * 100.0).round().clamp(0.0, 100.0) as u32;
    pct
}

impl SystemStatus {
    /// Matches `PowerPolicy`'s default low threshold; a charging
    /// battery is never "low" however empty, because it's filling.
    pub fn battery_is_low(&self) -> bool {
        self.battery
            .is_some_and(|b| b.present && b.state.is_discharging() && b.percent <= 20)
    }

    pub fn battery_is_critical(&self) -> bool {
        self.battery
            .is_some_and(|b| b.present && b.state.is_discharging() && b.percent <= 5)
    }

    pub fn battery_is_charging(&self) -> bool {
        self.battery
            .is_some_and(|b| b.present && matches!(b.state, BatteryState::Charging))
    }

    pub fn battery_percent(&self) -> Option<u8> {
        self.battery.filter(|b| b.present).map(|b| b.percent)
    }

    /// Muted and zero-volume both draw the crossed-out speaker.
    pub fn audio_is_silent(&self) -> bool {
        self.volume
            .is_some_and(|v| v.muted || v.level <= f32::EPSILON)
    }

    /// For the tray's accessible name and tests asserting on meaning.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        match &self.link {
            Some(l) if l.kind == LinkKind::Wired => parts.push(
                l.name
                    .clone()
                    .map_or_else(|| "Wired".to_string(), |n| format!("Wired: {n}")),
            ),
            Some(l) if l.kind == LinkKind::Wifi => parts.push(
                l.name
                    .clone()
                    .map_or_else(|| "Wi-Fi".to_string(), |n| format!("Wi-Fi: {n}")),
            ),
            Some(_) => parts.push("Offline".to_string()),
            None => {}
        }
        if let Some(v) = self.volume {
            parts.push(if v.muted {
                "Muted".to_string()
            } else {
                format!("Volume {}%", volume_percent(v.level))
            });
        }
        if let Some(pct) = self.battery_percent() {
            parts.push(if self.battery_is_charging() {
                format!("Battery {pct}%, charging")
            } else {
                format!("Battery {pct}%")
            });
        }
        if parts.is_empty() {
            "System status".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// Cloning shares the same reading (`Arc` bump), so the media-key
/// handler can push a fresh volume in without waiting for the next
/// poll — a volume key must move the indicator *now*.
#[derive(Clone, Debug, Default)]
pub struct StatusBridge {
    inner: Arc<Mutex<SystemStatus>>,
    /// So a second `spawn` is a no-op, not a second thread racing the first.
    polling: Arc<AtomicBool>,
}

impl StatusBridge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> SystemStatus {
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn set(&self, status: SystemStatus) {
        if let Ok(mut g) = self.inner.lock() {
            *g = status;
        }
    }

    /// Out of band, so a media key moves the indicator the same frame it was pressed.
    pub fn set_volume(&self, volume: VolumeState) {
        if let Ok(mut g) = self.inner.lock() {
            g.volume = Some(volume);
        }
    }

    pub fn is_polling(&self) -> bool {
        self.polling.load(Ordering::Relaxed)
    }

    pub fn spawn(&self) {
        if self.polling.swap(true, Ordering::SeqCst) {
            return;
        }
        let bridge = self.clone();
        let spawned = std::thread::Builder::new()
            .name("qos-status".to_string())
            .spawn(move || {
                loop {
                    bridge.set(read_once());
                    std::thread::sleep(POLL_INTERVAL);
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(target: "qos::status", error = %e, "status poller not started");
            self.polling.store(false, Ordering::SeqCst);
        }
    }
}

/// Public so a diagnostic or a test can take a reading without owning a poller.
pub fn read_once() -> SystemStatus {
    let battery = Some(qos_power_mgmt::sysfs_battery_snapshot());
    // Allowed to fail — a machine without NetworkManager or PipeWire
    // omits the glyph rather than claiming a state.
    let link = os_actions::get_link_status().ok();
    let volume = os_actions::get_volume_state().ok();
    SystemStatus {
        battery,
        link,
        volume,
    }
}

/// `0` when offline or wired (a cable has no bars), `1..=4` on Wi-Fi.
pub fn link_bars(link: &LinkStatus) -> u8 {
    match link.kind {
        LinkKind::Wifi => link.signal.clamp(1, 4),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn battery(percent: u8, state: BatteryState) -> BatterySnapshot {
        BatterySnapshot {
            present: true,
            percent,
            state,
        }
    }

    #[test]
    fn an_unread_status_reports_nothing_rather_than_a_default() {
        let s = SystemStatus::default();
        assert_eq!(s.battery_percent(), None);
        assert!(!s.battery_is_low());
        assert!(!s.audio_is_silent(), "unknown volume is not silence");
        assert_eq!(s.summary(), "System status");
    }

    #[test]
    fn a_machine_with_no_battery_draws_no_battery() {
        let s = SystemStatus {
            battery: Some(BatterySnapshot::absent()),
            ..Default::default()
        };
        assert_eq!(
            s.battery_percent(),
            None,
            "absent is not 100% — a desktop shows no gauge at all"
        );
    }

    #[test]
    fn low_is_only_low_while_discharging() {
        let draining = SystemStatus {
            battery: Some(battery(12, BatteryState::Discharging)),
            ..Default::default()
        };
        assert!(draining.battery_is_low());
        assert!(!draining.battery_is_critical());

        let filling = SystemStatus {
            battery: Some(battery(12, BatteryState::Charging)),
            ..Default::default()
        };
        assert!(
            !filling.battery_is_low(),
            "a battery that is filling is not a warning, however empty"
        );
        assert!(filling.battery_is_charging());
    }

    #[test]
    fn critical_is_also_low() {
        let s = SystemStatus {
            battery: Some(battery(3, BatteryState::Discharging)),
            ..Default::default()
        };
        assert!(s.battery_is_critical());
        assert!(s.battery_is_low(), "critical never skips the low styling");
    }

    #[test]
    fn zero_volume_reads_as_silent_just_like_mute() {
        let muted = SystemStatus {
            volume: Some(VolumeState {
                level: 0.6,
                muted: true,
            }),
            ..Default::default()
        };
        let zeroed = SystemStatus {
            volume: Some(VolumeState {
                level: 0.0,
                muted: false,
            }),
            ..Default::default()
        };
        assert!(muted.audio_is_silent());
        assert!(zeroed.audio_is_silent());
    }

    #[test]
    fn summary_names_the_connection_and_the_charge() {
        let s = SystemStatus {
            battery: Some(battery(64, BatteryState::Charging)),
            link: Some(LinkStatus {
                kind: LinkKind::Wifi,
                name: Some("Cafe WiFi".into()),
                signal: 3,
            }),
            volume: Some(VolumeState {
                level: 0.35,
                muted: false,
            }),
        };
        assert_eq!(
            s.summary(),
            "Wi-Fi: Cafe WiFi · Volume 35% · Battery 64%, charging"
        );
    }

    #[test]
    fn a_wired_link_has_no_bars() {
        let wired = LinkStatus {
            kind: LinkKind::Wired,
            name: Some("eth0".into()),
            signal: 4,
        };
        assert_eq!(link_bars(&wired), 0);
        assert_eq!(link_bars(&LinkStatus::offline()), 0);
    }

    #[test]
    fn a_connected_radio_always_shows_at_least_one_bar() {
        let weak = LinkStatus {
            kind: LinkKind::Wifi,
            name: Some("Far".into()),
            signal: 0,
        };
        assert_eq!(
            link_bars(&weak),
            1,
            "connected-but-weak must not render identically to offline"
        );
    }

    #[test]
    fn the_bridge_publishes_a_volume_without_waiting_for_a_poll() {
        let b = StatusBridge::new();
        assert_eq!(b.snapshot().volume, None);
        b.set_volume(VolumeState {
            level: 0.8,
            muted: false,
        });
        assert!((b.snapshot().volume.unwrap().level - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn a_bridge_starts_no_poller_until_asked() {
        let b = StatusBridge::new();
        assert!(
            !b.is_polling(),
            "a screenshot render must not depend on the host's hardware"
        );
    }
}
