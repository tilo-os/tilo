//! This device's data usage, counted where the device is read: the session
//! reads the Wi-Fi and wired interfaces' and the modems' traffic totals and
//! counts them into the person's device-held `network-usage` section
//! through its `count-data-used` verb, whatever window is open. So the
//! count is a fact of the device that agents, scripts and Settings read
//! alike, not something a page keeps up while it is shown.
//!
//! The totals are read every minute but counted only every quarter hour,
//! and at once where a counter moves to another connection, stops, or a
//! new local day begins: then the reading before the change is counted
//! first, so traffic lands on the connection and the day it moved on.

use os_actions::modem::Traffic;
use settings_sections::network_usage::{ConnectionKind, Reading};
use settings_store::InvokingAction;

/// How often the counters are read while this device has a connection up.
const READ_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
/// How often a device with nothing to count looks again.
const IDLE: std::time::Duration = std::time::Duration::from_secs(600);
/// How long a reading may wait to be counted while nothing changes.
const COUNT_EVERY: i64 = 15 * 60;

/// The section's kind, and its verb that counts traffic totals.
const SECTION: &str = "settings.network-usage";
const COUNT: &str = "count-data-used";

/// Count this device's data usage for as long as the session runs.
///
/// # Errors
/// The counting thread could not start.
pub(crate) fn spawn() -> std::io::Result<()> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_owned());
    std::thread::Builder::new()
        .name("tilo-network-usage".into())
        .spawn(move || {
            let mut counter = Counter::default();
            loop {
                let at = tilo_clock::unix_now_i64();
                let offset = chrono::Local::now().offset().local_minus_utc();
                let readings = read();
                let idle = readings.is_empty();
                for (readings, at, offset) in counter.take(Taken {
                    readings,
                    at,
                    offset,
                }) {
                    if let Err(error) = super::settings_records::change(
                        &super::settings_records::address(SECTION, COUNT),
                        &arguments(&readings, boot.as_deref(), at, offset),
                        true,
                        &InvokingAction::action("network-usage.count"),
                    ) {
                        tracing::warn!(%error, "the data used was not counted");
                    }
                }
                std::thread::park_timeout(if idle { IDLE } else { READ_EVERY });
            }
        })
        .map(drop)
}

/// Every counter there is to read now.
fn read() -> Vec<Reading> {
    let mut readings: Vec<Reading> = os_actions::read_interface_traffic()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| {
            let (kind, prefix) = if interface.wired {
                (ConnectionKind::Wired, "wired")
            } else {
                (ConnectionKind::Wifi, "wifi")
            };
            reading(
                interface.interface,
                format!("{prefix}:{}", interface.uuid),
                kind,
                interface.name,
                interface.traffic,
            )
        })
        .collect();
    let modems = os_actions::read_mobile()
        .map(|reading| reading.modems)
        .unwrap_or_default();
    readings.extend(modems.into_iter().filter_map(|modem| {
        let traffic = modem.traffic?;
        let sim = modem.active_sim();
        let id = sim
            .and_then(|sim| sim.iccid.clone())
            .unwrap_or_else(|| modem.path.clone());
        let name = sim
            .map(os_actions::modem::SimCard::label)
            .or_else(|| modem.operator.clone())
            .unwrap_or_else(|| modem.name.clone());
        Some(reading(
            format!("modem:{}", modem.path),
            format!("mobile:{id}"),
            ConnectionKind::Mobile,
            name,
            traffic,
        ))
    }));
    readings
}

fn reading(
    counter: String,
    connection: String,
    kind: ConnectionKind,
    name: String,
    traffic: Traffic,
) -> Reading {
    Reading {
        counter,
        connection,
        kind,
        name,
        received: traffic.received,
        sent: traffic.sent,
    }
}

/// The readings taken at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
struct Taken {
    readings: Vec<Reading>,
    at: i64,
    offset: i32,
}

impl Taken {
    fn day(&self) -> Option<chrono::NaiveDate> {
        settings_sections::network_usage::local_day(self.at, self.offset)
    }

    /// Whether `next` counts on another connection or day than this: a
    /// counter on another connection or gone, or another local day.
    fn changes_at(&self, next: &Self) -> bool {
        self.day() != next.day()
            || self.readings.iter().any(|before| {
                !next.readings.iter().any(|after| {
                    after.counter == before.counter && after.connection == before.connection
                })
            })
    }
}

/// What was read and what was counted, between readings.
#[derive(Default)]
struct Counter {
    /// The last reading, while it is not counted yet.
    held: Option<Taken>,
    /// When a reading was last counted.
    counted_at: Option<i64>,
}

impl Counter {
    /// Take in a reading; the readings to count now, in order, each with
    /// when it was taken and the local offset then.
    fn take(&mut self, now: Taken) -> Vec<(Vec<Reading>, i64, i32)> {
        let mut count = Vec::new();
        let changed = self.held.take().filter(|held| held.changes_at(&now));
        if let Some(before) = changed {
            count.push((before.readings, before.at, before.offset));
        }
        let due = !count.is_empty()
            || self
                .counted_at
                .is_none_or(|at| now.at.saturating_sub(at) >= COUNT_EVERY);
        if due && !now.readings.is_empty() {
            self.counted_at = Some(now.at);
            count.push((now.readings, now.at, now.offset));
        } else {
            self.held = Some(now);
        }
        count
    }
}

/// The arguments of `count-data-used` for `readings`.
fn arguments(
    readings: &[Reading],
    boot: Option<&str>,
    at: i64,
    offset: i32,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "readings".into(),
        serde_json::to_value(readings).unwrap_or_default(),
    );
    arguments.insert("boot".into(), boot.into());
    arguments.insert("at".into(), at.into());
    arguments.insert("offset".into(), offset.into());
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(connection: &str, received: u64) -> Reading {
        reading(
            "wlan0".into(),
            connection.into(),
            ConnectionKind::Wifi,
            connection.into(),
            Traffic { received, sent: 0 },
        )
    }

    fn taken(readings: Vec<Reading>, at: i64) -> Taken {
        Taken {
            readings,
            at,
            offset: 0,
        }
    }

    /// 2026-03-14 12:00 UTC.
    const NOON: i64 = 1_773_489_600;

    /// Readings wait a quarter hour to be counted, but one before a change
    /// of connection or day is counted at once, ahead of the one after it.
    #[test]
    fn readings_are_counted_on_their_connection_and_day() {
        let mut counter = Counter::default();
        assert_eq!(counter.take(taken(vec![on("wifi:a", 1)], NOON)).len(), 1);
        assert!(
            counter
                .take(taken(vec![on("wifi:a", 2)], NOON + 60))
                .is_empty()
        );
        assert!(
            counter
                .take(taken(vec![on("wifi:a", 3)], NOON + 120))
                .is_empty()
        );
        let moved = counter.take(taken(vec![on("wifi:b", 4)], NOON + 180));
        let order: Vec<(&str, u64)> = moved
            .iter()
            .map(|(readings, _, _)| (readings[0].connection.as_str(), readings[0].received))
            .collect();
        assert_eq!(order, [("wifi:a", 3), ("wifi:b", 4)]);
        assert!(
            counter
                .take(taken(vec![on("wifi:b", 5)], NOON + 240))
                .is_empty()
        );
        assert_eq!(
            counter
                .take(taken(vec![on("wifi:b", 6)], NOON + 240 + COUNT_EVERY))
                .len(),
            1,
            "a quarter hour on"
        );
        assert!(
            counter
                .take(taken(vec![on("wifi:b", 7)], NOON + 300 + COUNT_EVERY))
                .is_empty()
        );
        let next_day = counter.take(taken(vec![on("wifi:b", 8)], NOON + 86_400));
        assert_eq!(next_day.len(), 2, "the day before is counted on its day");
        assert!(counter.take(taken(Vec::new(), NOON + 86_460)).is_empty());
    }

    #[test]
    fn the_readings_are_the_verbs_arguments() {
        let arguments = arguments(&[on("wifi:a", 1_000)], Some("b"), 10, 3_600);
        assert_eq!(arguments["readings"][0]["connection"], "wifi:a");
        assert_eq!(arguments["readings"][0]["kind"], "wifi");
        assert_eq!(arguments["readings"][0]["received"], 1_000);
        assert_eq!(arguments["boot"], "b");
        assert_eq!(arguments["offset"], 3_600);
    }
}
