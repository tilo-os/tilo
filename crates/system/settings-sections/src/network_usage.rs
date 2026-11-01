//! The data this device moved over each of its connections
//! (`network_usage`): per Wi-Fi network (by its saved profile), wired
//! connection and SIM, the bytes received and sent on each local day, kept
//! for [`HORIZON_DAYS`]. Kept on this device only: another device counts
//! its own. The session counts the interfaces' and modems' traffic into it
//! whatever window is open, nobody's undo step; the person sets the day the
//! mobile data period starts on and the plan's allowance, starts the
//! mobile count again and forgets a connection's history.

use std::collections::BTreeMap;

use chrono::{Datelike as _, NaiveDate};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema, Refusal};

/// The settings document's section holding the counts.
pub const SECTION: &str = "network_usage";

/// How many days of history are kept: enough for two periods and the last
/// 30 days whatever day a period starts on.
pub const HORIZON_DAYS: i64 = 92;

/// A day of the month a data period can start on: one every month has, the
/// 1st to the 28th.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ResetDay(u32);

impl Default for ResetDay {
    fn default() -> Self {
        Self(Self::FIRST)
    }
}

/// Only a day from the 1st to the 28th reads: a field edit with any other
/// value is refused at the field. A stored value that does not read is the
/// section's default for that key, the 1st.
impl<'de> Deserialize<'de> for ResetDay {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let day = u32::deserialize(deserializer)?;
        Self::new(day).ok_or_else(|| {
            de::Error::invalid_value(
                de::Unexpected::Unsigned(day.into()),
                &"a day of the month from 1 to 28",
            )
        })
    }
}

tilo_resource::json_model!(ResetDay);

impl ResetDay {
    const FIRST: u32 = 1;
    const LAST: u32 = 28;

    /// `day`, where every month has it.
    #[must_use]
    pub fn new(day: u32) -> Option<Self> {
        (Self::FIRST..=Self::LAST)
            .contains(&day)
            .then_some(Self(day))
    }

    /// Every day a period can start on, in order.
    pub fn all() -> impl Iterator<Item = Self> {
        (Self::FIRST..=Self::LAST).map(Self)
    }

    /// The day of the month.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The first day of the period `today` falls in.
    #[must_use]
    pub fn period_start(self, today: NaiveDate) -> NaiveDate {
        let this_month = today.with_day(self.0).unwrap_or(today);
        if today.day() >= self.0 {
            this_month
        } else {
            this_month
                .checked_sub_months(chrono::Months::new(1))
                .unwrap_or(this_month)
        }
    }

    /// The first day of the period after the one `today` falls in.
    #[must_use]
    pub fn next_period(self, today: NaiveDate) -> NaiveDate {
        let start = self.period_start(today);
        start
            .checked_add_months(chrono::Months::new(1))
            .unwrap_or(start)
    }
}

/// What a connection is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionKind {
    #[default]
    Wifi,
    Wired,
    Mobile,
}

tilo_resource::json_model!(ConnectionKind);

/// Bytes received and sent.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
#[serde(default)]
pub struct Bytes {
    pub received: u64,
    pub sent: u64,
}

impl Bytes {
    /// Both ways together.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.received.saturating_add(self.sent)
    }

    fn add(&mut self, other: Self) {
        self.received = self.received.saturating_add(other.received);
        self.sent = self.sent.saturating_add(other.sent);
    }
}

/// Where the person started a connection's count again: the last day
/// counted then, and what that day held then.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
#[serde(default)]
pub struct Restart {
    /// The day, as `YYYY-MM-DD`.
    pub day: String,
    /// The bytes that day held when the count started again.
    pub before: u64,
}

/// One connection's history.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
#[serde(default)]
pub struct ConnectionUsage {
    pub kind: ConnectionKind,
    /// Its name as last seen: the network's, the wired profile's, the SIM's.
    pub name: String,
    /// What moved on each local day, by `YYYY-MM-DD`; days with nothing
    /// are left out.
    pub days: BTreeMap<String, Bytes>,
    /// Where the person last started its count again.
    pub restart: Option<Restart>,
}

impl ConnectionUsage {
    /// What moved from `from` to `to`, both included.
    #[must_use]
    pub fn between(&self, from: NaiveDate, to: NaiveDate) -> Bytes {
        let mut sum = Bytes::default();
        for (_, bytes) in self.days.range(day_key(from)..=day_key(to)) {
            sum.add(*bytes);
        }
        sum
    }

    /// What moved on `day`.
    #[must_use]
    pub fn on(&self, day: NaiveDate) -> Bytes {
        self.days.get(&day_key(day)).copied().unwrap_or_default()
    }

    /// Bytes moved since `from` up to `today`, less what came before the
    /// person last started the count again.
    #[must_use]
    pub fn used_since(&self, from: NaiveDate, today: NaiveDate) -> u64 {
        match &self.restart {
            Some(restart) if restart.day >= day_key(from) => {
                let Some(day) = parse_day(&restart.day) else {
                    return self.between(from, today).total();
                };
                self.between(day, today)
                    .total()
                    .saturating_sub(restart.before)
            }
            _ => self.between(from, today).total(),
        }
    }
}

/// One traffic total the session read: an interface's or a modem's counter
/// and the connection on it then.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Reading {
    /// The counter read: `wlp2s0`, `modem:/org/freedesktop/ModemManager1/Modem/0`.
    pub counter: String,
    /// The connection on it: `wifi:<profile id>`, `wired:<profile id>`,
    /// `mobile:<SIM>`.
    pub connection: String,
    pub kind: ConnectionKind,
    pub name: String,
    /// The counter's totals since it started.
    pub received: u64,
    pub sent: u64,
}

/// The data this device moved over each connection.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
#[serde(default)]
pub struct NetworkUsage {
    /// The day of the month the mobile data period starts on.
    pub reset_day: ResetDay,
    /// The mobile data plan's allowance per period, in gigabytes; 0 for
    /// none.
    #[model(range = 0..=500)]
    pub limit_gb: u32,
    /// Each connection's history, by connection.
    #[model(readonly)]
    pub connections: BTreeMap<String, ConnectionUsage>,
    /// The counter totals last read, by counter, to count what came after.
    #[model(readonly)]
    pub counters: BTreeMap<String, Bytes>,
    /// The boot the totals were read in: counters start again at boot.
    #[model(readonly)]
    pub boot: Option<String>,
}

impl ui::storage::SettingsSchema for NetworkUsage {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared(SECTION);
}

#[tilo_resource::resource(kind = "settings.network-usage", label = "Data usage")]
impl NetworkUsage {
    /// Count traffic totals toward their connections: what the device
    /// reported, nobody's undo step.
    #[action(id = "count-data-used", external)]
    pub fn count_data_used(
        &mut self,
        #[param("The counters read, each with the connection on it")] readings: Vec<Reading>,
        #[param("The boot the totals were read in")] boot: Option<String>,
        #[param("When they were read, in seconds since 1970-01-01 UTC")] at: i64,
        #[param("The local time's offset from UTC then, in seconds")] offset: i32,
    ) -> Result<(), Refusal> {
        self.account(&readings, boot.as_deref(), at, offset);
        Ok(())
    }

    /// Start counting the data used this period again from now, on one
    /// connection or every mobile one.
    #[action(
        id = "reset-data-used",
        label = "Reset data used",
        keywords = "data usage, cellular data, mobile data counter, data plan"
    )]
    pub fn reset_data_used(
        &mut self,
        #[param("The connection; every mobile one when omitted")] connection: Option<String>,
    ) -> Result<(), Refusal> {
        let mut found = false;
        for (key, usage) in &mut self.connections {
            let chosen = match &connection {
                Some(wanted) => key == wanted,
                None => usage.kind == ConnectionKind::Mobile,
            };
            if !chosen {
                continue;
            }
            found = true;
            usage.restart = usage.days.iter().next_back().map(|(day, bytes)| Restart {
                day: day.clone(),
                before: bytes.total(),
            });
        }
        if connection.is_some() && !found {
            return Err(Refusal::new("no data was counted on that connection"));
        }
        Ok(())
    }

    /// Forget what was counted on one connection, or on every one.
    #[action(
        id = "forget-data-used",
        label = "Forget data usage",
        keywords = "data usage history, clear network statistics"
    )]
    pub fn forget_data_used(
        &mut self,
        #[param("The connection; every one when omitted")] connection: Option<String>,
    ) -> Result<(), Refusal> {
        match connection {
            Some(key) => {
                self.connections
                    .remove(&key)
                    .ok_or_else(|| Refusal::new("no data was counted on that connection"))?;
            }
            None => self.connections.clear(),
        }
        Ok(())
    }
}

/// `day` as the history keys it.
#[must_use]
pub fn day_key(day: NaiveDate) -> String {
    day.format("%Y-%m-%d").to_string()
}

fn parse_day(key: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(key, "%Y-%m-%d").ok()
}

/// The local day at `at` (seconds since the epoch, UTC) `offset` seconds
/// east of UTC.
#[must_use]
pub fn local_day(at: i64, offset: i32) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp(at.saturating_add(i64::from(offset)), 0)
        .map(|moment| moment.date_naive())
}

impl NetworkUsage {
    /// Take in the totals read at `at`: what moved since the totals last
    /// read counts toward the connection on each counter, on the local day.
    /// A total lower than the last, or from another boot, started again from
    /// zero; a counter never read before only starts its count. Days older
    /// than [`HORIZON_DAYS`] are let go. Whether anything changed.
    pub fn account(
        &mut self,
        readings: &[Reading],
        boot: Option<&str>,
        at: i64,
        offset: i32,
    ) -> bool {
        let before = self.clone();
        let rebooted =
            matches!((boot, self.boot.as_deref()), (Some(now), Some(then)) if now != then);
        if rebooted {
            self.counters.clear();
        }
        let today = local_day(at, offset);
        for reading in readings {
            let now = Bytes {
                received: reading.received,
                sent: reading.sent,
            };
            let moved = match self.counters.get(&reading.counter) {
                Some(seen) if now.received >= seen.received && now.sent >= seen.sent => Bytes {
                    received: now.received.saturating_sub(seen.received),
                    sent: now.sent.saturating_sub(seen.sent),
                },
                Some(_) => now,
                None if rebooted => now,
                None => Bytes::default(),
            };
            self.counters.insert(reading.counter.clone(), now);
            if moved.total() == 0 && !self.connections.contains_key(&reading.connection) {
                continue;
            }
            let usage = self
                .connections
                .entry(reading.connection.clone())
                .or_default();
            usage.kind = reading.kind;
            if usage.name != reading.name && !reading.name.is_empty() {
                usage.name.clone_from(&reading.name);
            }
            if let Some(today) = today
                && moved.total() > 0
            {
                usage.days.entry(day_key(today)).or_default().add(moved);
            }
        }
        if boot.is_some() {
            self.boot = boot.map(str::to_owned);
        }
        if let Some(oldest) = today.and_then(|today| {
            today.checked_sub_days(chrono::Days::new(u64::try_from(HORIZON_DAYS).unwrap_or(0)))
        }) {
            let oldest = day_key(oldest);
            for usage in self.connections.values_mut() {
                usage.days.retain(|day, _| *day > oldest);
                if usage
                    .restart
                    .as_ref()
                    .is_some_and(|restart| restart.day <= oldest)
                {
                    usage.restart = None;
                }
            }
            self.connections.retain(|_, usage| !usage.days.is_empty());
        }
        *self != before
    }

    /// The mobile data used in the period `today` falls in, on every SIM.
    #[must_use]
    pub fn mobile_used(&self, today: NaiveDate) -> u64 {
        let from = self.reset_day.period_start(today);
        self.connections
            .values()
            .filter(|usage| usage.kind == ConnectionKind::Mobile)
            .map(|usage| usage.used_since(from, today))
            .fold(0, u64::saturating_add)
    }

    /// The plan's allowance in bytes, when there is one.
    #[must_use]
    pub fn limit(&self) -> Option<u64> {
        (self.limit_gb > 0).then(|| u64::from(self.limit_gb).saturating_mul(1_000_000_000))
    }

    /// The allowances a plan can have, in gigabytes; 0 for none.
    #[must_use]
    pub const fn limits() -> [u32; 13] {
        [0, 1, 2, 3, 5, 10, 15, 20, 30, 50, 100, 200, 500]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    /// 2026-03-14 12:00 UTC, in seconds since the epoch.
    const MARCH_14: i64 = 1_773_489_600;
    const DAY: i64 = 86_400;

    fn reading(counter: &str, connection: &str, kind: ConnectionKind, received: u64) -> Reading {
        Reading {
            counter: counter.into(),
            connection: connection.into(),
            kind,
            name: connection.trim_start_matches("wifi:").into(),
            received,
            sent: received / 10,
        }
    }

    fn date(text: &str) -> NaiveDate {
        parse_day(text).unwrap()
    }

    #[test]
    fn traffic_counts_per_connection_and_day_through_restarts() {
        let mut usage = NetworkUsage::default();
        let home = |total| reading("wlan0", "wifi:Home", ConnectionKind::Wifi, total);
        assert!(usage.account(&[home(1_000)], Some("a"), MARCH_14, 0));
        assert!(
            usage.connections.is_empty(),
            "the first total only starts the count"
        );
        usage.account(&[home(1_500)], Some("a"), MARCH_14 + 60, 0);
        let day = usage.connections["wifi:Home"].on(date("2026-03-14"));
        assert_eq!((day.received, day.sent), (500, 50));
        assert!(
            !usage.account(&[home(1_500)], Some("a"), MARCH_14 + 120, 0),
            "nothing new"
        );

        // The same radio on another network: what came after counts there.
        let cafe = reading("wlan0", "wifi:Cafe", ConnectionKind::Wifi, 2_000);
        usage.account(&[cafe], Some("a"), MARCH_14 + 180, 0);
        assert_eq!(
            usage.connections["wifi:Cafe"]
                .on(date("2026-03-14"))
                .received,
            500
        );

        usage.account(&[home(200)], Some("a"), MARCH_14 + 240, 0);
        assert_eq!(
            usage.connections["wifi:Home"]
                .on(date("2026-03-14"))
                .received,
            700,
            "a counter that started again counts from zero"
        );
        usage.account(&[home(300)], Some("b"), MARCH_14 + DAY, 0);
        assert_eq!(
            usage.connections["wifi:Home"]
                .on(date("2026-03-15"))
                .received,
            300,
            "another boot counts from zero, on the next day"
        );
        assert_eq!(
            usage.connections["wifi:Home"]
                .between(date("2026-03-01"), date("2026-03-31"))
                .received,
            1_000
        );
    }

    #[test]
    fn the_local_day_is_the_one_counted_on() {
        let mut usage = NetworkUsage::default();
        let cable = |total| reading("eth0", "wired:Office", ConnectionKind::Wired, total);
        usage.account(&[cable(0)], None, MARCH_14, 0);
        // 23:00 UTC is the 15th two hours east.
        usage.account(&[cable(100)], None, MARCH_14 + 11 * 3_600, 7_200);
        assert_eq!(
            usage.connections["wired:Office"]
                .on(date("2026-03-15"))
                .received,
            100
        );
    }

    #[test]
    fn the_mobile_period_starts_on_its_day_and_again_on_reset() {
        let mut usage = NetworkUsage {
            reset_day: ResetDay::new(15).unwrap(),
            ..NetworkUsage::default()
        };
        let sim = |total| reading("modem:0", "mobile:8931", ConnectionKind::Mobile, total);
        usage.account(&[sim(0)], None, MARCH_14, 0);
        usage.account(&[sim(1_000)], None, MARCH_14 + 60, 0);
        assert_eq!(usage.mobile_used(date("2026-03-14")), 1_100);
        usage.account(&[sim(2_000)], None, MARCH_14 + DAY, 0);
        assert_eq!(
            usage.mobile_used(date("2026-03-15")),
            1_100,
            "the period started again on the 15th"
        );
        assert_eq!(
            usage.reset_day.period_start(date("2026-03-14")),
            date("2026-02-15")
        );
        assert_eq!(
            usage.reset_day.next_period(date("2026-03-15")),
            date("2026-04-15")
        );

        usage.reset_data_used(None).unwrap();
        assert_eq!(usage.mobile_used(date("2026-03-15")), 0);
        usage.account(&[sim(2_500)], None, MARCH_14 + DAY + 60, 0);
        assert_eq!(usage.mobile_used(date("2026-03-15")), 550);
        // A new reset day keeps the history; the period follows the day.
        usage.reset_day = ResetDay::new(1).unwrap();
        assert_eq!(
            usage.mobile_used(date("2026-03-15")),
            550,
            "the reset still holds"
        );
        usage.limit_gb = 2;
        assert_eq!(usage.limit(), Some(2_000_000_000));
    }

    #[test]
    fn history_older_than_the_horizon_is_let_go() {
        let mut usage = NetworkUsage::default();
        let home = |total| reading("wlan0", "wifi:Home", ConnectionKind::Wifi, total);
        usage.account(&[home(0)], None, MARCH_14, 0);
        usage.account(&[home(10)], None, MARCH_14 + 60, 0);
        let cafe = |total| reading("wlan1", "wifi:Cafe", ConnectionKind::Wifi, total);
        usage.account(&[cafe(0)], None, MARCH_14 + HORIZON_DAYS * DAY, 0);
        usage.account(&[cafe(5)], None, MARCH_14 + HORIZON_DAYS * DAY + 60, 0);
        assert!(!usage.connections.contains_key("wifi:Home"));
        assert!(usage.connections.contains_key("wifi:Cafe"));
    }

    /// Only a day every month has is a reset day: any other is refused at
    /// the field.
    #[test]
    fn only_a_day_every_month_has_is_a_reset_day() {
        for day in [1, 15, 28] {
            let read: ResetDay = serde_json::from_value(serde_json::json!(day)).unwrap();
            assert_eq!(read.get(), day);
        }
        for value in [
            serde_json::json!(0),
            serde_json::json!(29),
            serde_json::json!(-3),
            serde_json::json!("15"),
            serde_json::json!(null),
        ] {
            assert!(
                serde_json::from_value::<ResetDay>(value.clone()).is_err(),
                "{value}"
            );
        }
        assert_eq!(ResetDay::all().count(), 28);
    }

    /// The device's count changes the section without being anybody's step;
    /// resetting and forgetting are the person's.
    #[test]
    fn the_count_is_counted_reset_and_forgotten_through_the_verbs() {
        let mut usage = Replica::<NetworkUsage>::new();
        for (total, at) in [(1_000, MARCH_14), (1_250, MARCH_14 + 60)] {
            usage
                .apply(NetworkUsage::COUNT_DATA_USED.with((
                    vec![reading(
                        "modem:0",
                        "mobile:8931",
                        ConnectionKind::Mobile,
                        total,
                    )],
                    Some("a".to_owned()),
                    at,
                    0,
                )))
                .unwrap();
        }
        let today = date("2026-03-14");
        assert_eq!(usage.state().unwrap().mobile_used(today), 275);
        usage
            .apply(NetworkUsage::RESET_DATA_USED.with(None))
            .unwrap();
        assert_eq!(usage.state().unwrap().mobile_used(today), 0);
        assert!(
            usage
                .apply(NetworkUsage::FORGET_DATA_USED.with(Some("wifi:Nowhere".to_owned())))
                .is_err()
        );
        usage
            .apply(NetworkUsage::FORGET_DATA_USED.with(Some("mobile:8931".to_owned())))
            .unwrap();
        assert!(usage.state().unwrap().connections.is_empty());
    }
}
