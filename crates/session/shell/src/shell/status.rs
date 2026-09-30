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

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::shell::i18n::{t, t_args};

use os_actions::{LinkKind, LinkStatus, VolumeState};
use tilo_power_mgmt::{BatterySnapshot, BatteryState};

/// When `nmcli monitor` or `pw-dump --monitor` is silent, still refresh
/// battery and missed events. Thirty seconds is slow enough that idle
/// does not fork `nmcli`/`wpctl` twice a second, and fast enough that
/// a charger still shows up while the user is looking at the tray.
const FALLBACK_INTERVAL: Duration = Duration::from_secs(30);

/// Every field is `Option` because "not read yet" and "this machine
/// has none" are both real and both drawn as absence, not a zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStatus {
    /// `Some` with `present == false` on a machine with no battery.
    pub battery: Option<BatterySnapshot>,
    pub link: Option<LinkStatus>,
    pub volume: Option<VolumeState>,
    /// Active capture streams, attributed from `PipeWire`'s server security
    /// PID to the kernel executable. Polled independently at one-second
    /// cadence so a privacy indicator appears promptly without running
    /// `NetworkManager` queries every second.
    pub media: Vec<os_actions::ActiveMediaUse>,
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
            Some(l) if l.kind == LinkKind::Wired => parts.push(l.name.clone().map_or_else(
                || t("status-wired"),
                |n| t_args("status-wired-named", &[("name", n.into())]),
            )),
            Some(l) if l.kind == LinkKind::Wifi => parts.push(l.name.clone().map_or_else(
                || t("status-wifi"),
                |n| t_args("status-wifi-named", &[("name", n.into())]),
            )),
            Some(_) => parts.push(t("status-offline")),
            None => {}
        }
        if let Some(v) = self.volume {
            parts.push(if v.muted {
                t("status-muted")
            } else {
                t_args(
                    "status-volume",
                    &[("percent", volume_percent(v.level).into())],
                )
            });
        }
        if let Some(pct) = self.battery_percent() {
            parts.push(if self.battery_is_charging() {
                t_args("status-battery-charging", &[("percent", pct.into())])
            } else {
                t_args("status-battery", &[("percent", pct.into())])
            });
        }
        if parts.is_empty() {
            t("status-summary-empty")
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

    pub fn set(&self, mut status: SystemStatus) {
        if let Ok(mut g) = self.inner.lock() {
            // The slow poll owns battery/link/volume; the one-second media
            // poll owns this field. Never let one cadence erase the other.
            status.media.clone_from(&g.media);
            *g = status;
        }
    }

    /// Out of band, so a media key moves the indicator the same frame it was pressed.
    pub fn set_volume(&self, volume: VolumeState) {
        if let Ok(mut g) = self.inner.lock() {
            g.volume = Some(volume);
        }
    }

    pub fn set_media(&self, media: Vec<os_actions::ActiveMediaUse>) {
        if let Ok(mut g) = self.inner.lock() {
            g.media = media;
        }
    }

    pub fn is_polling(&self) -> bool {
        self.polling.load(Ordering::Relaxed)
    }

    pub fn spawn(&self) {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("status worker");
        if self.polling.swap(true, Ordering::SeqCst) {
            return;
        }
        let (wake_tx, wake_rx) = mpsc::channel();
        spawn_change_monitor("tilo-nm-watch", &["nmcli", "monitor"], wake_tx.clone());
        spawn_change_monitor(
            "tilo-pw-watch",
            &["pw-dump", "--monitor", "--no-colors"],
            wake_tx,
        );
        let bridge = self.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-status".to_string())
            .spawn(move || {
                loop {
                    bridge.set(read_once());
                    if wake_rx.recv_timeout(FALLBACK_INTERVAL).is_ok() {
                        while wake_rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(target: "tilo::status", error = %e, "status poller not started");
            self.polling.store(false, Ordering::SeqCst);
            return;
        }
        let media_bridge = self.clone();
        let _ = std::thread::Builder::new()
            .name("tilo-media-privacy".to_string())
            .spawn(move || {
                loop {
                    let bridge = media_bridge.clone();
                    let _ = os_actions::monitor_media_usage(|media| bridge.set_media(media));
                    // PipeWire may restart during login or an update. Rebind
                    // without spinning and without ending the shell session.
                    std::thread::sleep(Duration::from_secs(2));
                }
            });
    }
}

fn spawn_change_monitor(name: &'static str, argv: &'static [&'static str], tx: Sender<()>) {
    let Some((program, rest)) = argv.split_first() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            loop {
                if monitor_until_exit(program, rest, &tx).is_err() {
                    std::thread::sleep(FALLBACK_INTERVAL);
                }
            }
        });
}

fn monitor_until_exit(program: &str, args: &[&str], tx: &Sender<()>) -> Result<(), ()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| ())?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Err(());
    };
    for line in BufReader::new(stdout).lines() {
        if line.is_err() {
            break;
        }
        if tx.send(()).is_err() {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(())
}

/// Public so a diagnostic or a test can take a reading without owning a poller.
pub fn read_once() -> SystemStatus {
    let battery = Some(tilo_power_mgmt::sysfs_battery_snapshot());
    // Allowed to fail — a machine without NetworkManager or PipeWire
    // omits the glyph rather than claiming a state.
    let link = os_actions::get_link_status().ok();
    let volume = os_actions::get_volume_state().ok();
    SystemStatus {
        battery,
        link,
        volume,
        media: Vec::new(),
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
    fn battery_status_reports_only_real_low_or_critical_charge() {
        // Phase `an_unread_status_reports_nothing_rather_than_a_default`.
        {
            let s = SystemStatus::default();
            assert_eq!(s.battery_percent(), None);
            assert!(!s.battery_is_low());
            assert!(!s.audio_is_silent(), "unknown volume is not silence");
            assert_eq!(s.summary(), "System status");
        }

        // Phase `a_machine_with_no_battery_draws_no_battery`.
        {
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

        // Phase `low_is_only_low_while_discharging`.
        {
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

        // Phase `critical_is_also_low`.
        {
            let s = SystemStatus {
                battery: Some(battery(3, BatteryState::Discharging)),
                ..Default::default()
            };
            assert!(s.battery_is_critical());
            assert!(s.battery_is_low(), "critical never skips the low styling");
        }
    }

    #[test]
    fn volume_and_network_summaries_read_truthfully() {
        // Phase `zero_volume_reads_as_silent_just_like_mute`.
        {
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

        // Phase `summary_names_the_connection_and_the_charge`.
        {
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
                media: Vec::new(),
            };
            assert_eq!(
                s.summary(),
                "Wi-Fi: Cafe WiFi · Volume 35% · Battery 64%, charging"
            );
        }

        // Phase `a_wired_link_has_no_bars`.
        {
            let wired = LinkStatus {
                kind: LinkKind::Wired,
                name: Some("eth0".into()),
                signal: 4,
            };
            assert_eq!(link_bars(&wired), 0);
            assert_eq!(link_bars(&LinkStatus::offline()), 0);
        }

        // Phase `a_connected_radio_always_shows_at_least_one_bar`.
        {
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
    }

    #[test]
    fn status_bridge_publishes_volume_immediately_and_polls_only_when_asked() {
        // Phase `the_bridge_publishes_a_volume_without_waiting_for_a_poll`.
        {
            let b = StatusBridge::new();
            assert_eq!(b.snapshot().volume, None);
            b.set_volume(VolumeState {
                level: 0.8,
                muted: false,
            });
            assert!((b.snapshot().volume.unwrap().level - 0.8).abs() < f32::EPSILON);
        }

        // Phase `a_bridge_starts_no_poller_until_asked`.
        {
            let b = StatusBridge::new();
            assert!(
                !b.is_polling(),
                "a screenshot render must not depend on the host's hardware"
            );
        }
    }

    #[test]
    fn idle_status_thread_sleeps_between_reads() {
        let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
        let (_bridge, tid) = crate::idle_thread_probe::spawned_thread("tilo-stat-wait", || {
            let bridge = StatusBridge::new();
            let poller = bridge.clone();
            std::thread::Builder::new()
                .name("tilo-stat-wait".into())
                .spawn(move || {
                    poller.set(SystemStatus::default());
                    let _ = wake_rx.recv_timeout(FALLBACK_INTERVAL);
                })
                .expect("status idle thread");
            bridge
        });
        crate::idle_thread_probe::assert_stays_asleep(&tid, "status fallback wait");
        drop(wake_tx);
    }
}
