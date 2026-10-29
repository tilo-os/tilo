//! The device status the top bar and Quick Settings both draw: battery,
//! link, volume and capture streams, as last read. The shell reads them
//! (`shell::status`); this is the reading and what it says.

use media::audio::default_node::VolumeState;
use os_actions::{LinkKind, LinkStatus};
use tilo_power_mgmt::{BatterySnapshot, BatteryState};

use crate::i18n::{t, t_args};

/// Every field is `Option` because "not read yet" and "this machine
/// has none" are both real and both drawn as absence, not a zero.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemStatus {
    /// `Some` with `present == false` on a machine with no battery.
    pub battery: Option<BatterySnapshot>,
    /// Time to empty or to full, when the battery reports its rate.
    pub battery_time: Option<tilo_power_mgmt::BatteryTime>,
    pub link: Option<LinkStatus>,
    pub volume: Option<VolumeState>,
    /// Active capture streams, attributed from `PipeWire`'s server security
    /// PID to the kernel executable. Read again only when the number of
    /// running capture streams changes, and without a process while none
    /// runs.
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
        self.reading().unwrap_or_else(|| t("status-summary-empty"))
    }

    /// The readings the machine has, joined, or `None` while it has none:
    /// for a surface that would rather show nothing than a stand-in line.
    pub fn reading(&self) -> Option<String> {
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
        (!parts.is_empty()).then(|| parts.join(", "))
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
            assert_eq!(s.reading(), None, "nothing read means nothing to show");
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
                battery_time: None,
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
                "Wi-Fi: Cafe WiFi, Volume 35%, Battery 64%, charging"
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
}
