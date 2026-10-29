//! The mobile data this device used (`mobile_data_usage`): the count of the
//! modem's traffic in the current period and the day of the month the
//! period starts on. Kept on this device only: another device's modem
//! counts its own. The person sets the day and starts the count again; the
//! count itself follows what the modem reports, nobody's undo step: the
//! session counts the modem's traffic into it where the modem is read,
//! whether or not a page shows it.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Refusal};

/// The settings document's section holding the count.
pub const SECTION: &str = "mobile_data_usage";

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

    /// The nearest day every month has to `day`.
    #[must_use]
    pub fn nearest(day: u32) -> Self {
        Self(day.clamp(Self::FIRST, Self::LAST))
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
}

/// The mobile data this device used, counted from the modem's traffic
/// totals.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[model(defaults)]
#[serde(default)]
pub struct MobileDataUsage {
    /// The day of the month the data used starts counting again.
    pub reset_day: ResetDay,
    /// The month the period being counted started in, as `year * 12 +
    /// month - 1`; 0 before the first count.
    #[model(readonly)]
    pub period: u32,
    /// The day of the month the period being counted started on: the reset
    /// day when it was last counted. A reset day changed since moves the
    /// count, as it is, to the period the new day makes.
    #[model(readonly)]
    pub period_day: u32,
    /// Bytes used in the period.
    #[model(readonly)]
    pub used: u64,
    /// The traffic total last read, to count what came after it.
    #[model(readonly)]
    pub counter: Option<u64>,
    /// The boot the total was read in: totals start again at boot.
    #[model(readonly)]
    pub boot: Option<String>,
}

impl ui::storage::SettingsSchema for MobileDataUsage {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared(SECTION);
}

#[tilo_resource::resource(kind = "settings.mobile-data-usage", label = "Mobile data used")]
impl MobileDataUsage {
    /// Count the modem's traffic total toward the period: what the modem
    /// reported, nobody's undo step.
    #[action(id = "count-data-used", external)]
    pub fn count_data_used(
        &mut self,
        #[param("The modems' traffic total in bytes since they started; none when unread")]
        total: Option<u64>,
        #[param("The boot the total was read in")] boot: Option<String>,
        #[param("When it was read, in seconds since 1970-01-01 UTC")] at: i64,
    ) -> Result<(), Refusal> {
        self.account(total, boot.as_deref(), at);
        Ok(())
    }

    /// Start counting the data used again from now.
    #[action(
        id = "reset-data-used",
        label = "Reset data used",
        keywords = "data usage, cellular data, mobile data counter, data plan"
    )]
    pub fn reset_data_used(&mut self) -> Result<(), Refusal> {
        self.used = 0;
        Ok(())
    }
}

/// The period `now` (seconds since the epoch, UTC) falls in when periods
/// start on `day`, as `year * 12 + month - 1` of its first day.
fn period_on(now: i64, day: ResetDay) -> u32 {
    use chrono::Datelike as _;
    let Some(date) = chrono::DateTime::from_timestamp(now, 0).map(|at| at.date_naive()) else {
        return 0;
    };
    let month = date
        .year()
        .checked_mul(12)
        .and_then(|months| months.checked_add(i32::try_from(date.month0()).ok()?));
    let index = if date.day() < day.get() {
        month.and_then(|month| month.checked_sub(1))
    } else {
        month
    };
    index
        .and_then(|index| u32::try_from(index).ok())
        .unwrap_or(0)
}

impl MobileDataUsage {
    /// The first day of the period the count belongs to at `now`, as
    /// (month, day).
    #[must_use]
    pub fn period_start(&self, now: i64) -> (u32, u32) {
        let period = if self.period_day == self.reset_day.get() {
            self.period
        } else {
            period_on(now, self.reset_day)
        };
        (
            period.rem_euclid(12).saturating_add(1),
            self.reset_day.get(),
        )
    }

    /// Take in a traffic total read at `now`: what was used since the last
    /// total counts toward the period, which starts again on its day. A
    /// total lower than the last, or from another boot, started again from
    /// zero. The first total ever read only starts the count. Whether
    /// anything changed.
    pub fn account(&mut self, total: Option<u64>, boot: Option<&str>, now: i64) -> bool {
        let before = self.clone();
        // A new period began only where one did on the day the count was
        // last counted on: the count so far stays with a new reset day.
        if period_on(now, ResetDay::nearest(self.period_day)) != self.period {
            self.used = 0;
        }
        self.period = period_on(now, self.reset_day);
        self.period_day = self.reset_day.get();
        if let Some(total) = total {
            let rebooted =
                matches!((boot, self.boot.as_deref()), (Some(now), Some(then)) if now != then);
            let used = match self.counter {
                Some(seen) if !rebooted && total >= seen => total.saturating_sub(seen),
                Some(_) => total,
                None => 0,
            };
            self.used = self.used.saturating_add(used);
            self.counter = Some(total);
            if boot.is_some() {
                self.boot = boot.map(str::to_owned);
            }
        }
        *self != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    /// 2026-03-14 12:00 UTC, in seconds since the epoch.
    const MARCH_14: i64 = 1_773_489_600;
    const DAY: i64 = 86_400;

    #[test]
    fn data_used_counts_traffic_deltas_through_restarts_and_starts_again_on_its_day() {
        let mut usage = MobileDataUsage {
            reset_day: ResetDay::new(15).unwrap(),
            ..MobileDataUsage::default()
        };
        assert!(usage.account(Some(1_000), Some("a"), MARCH_14));
        assert_eq!(usage.used, 0, "the first total only starts the count");
        assert_eq!(
            usage.period_start(MARCH_14),
            (2, 15),
            "March 14 is in the period from February 15"
        );

        usage.account(Some(1_500), Some("a"), MARCH_14 + 60);
        assert_eq!(usage.used, 500);
        assert!(
            !usage.account(Some(1_500), Some("a"), MARCH_14 + 120),
            "nothing new"
        );

        usage.account(Some(200), Some("a"), MARCH_14 + 180);
        assert_eq!(
            usage.used, 700,
            "a counter that started again counts from zero"
        );

        usage.account(Some(300), Some("b"), MARCH_14 + 240);
        assert_eq!(usage.used, 1_000, "another boot counts from zero");

        usage.account(Some(350), Some("b"), MARCH_14 + DAY);
        assert_eq!(usage.used, 50, "the period started again on the 15th");
        assert_eq!(usage.period_start(MARCH_14 + DAY), (3, 15));

        usage.account(None, None, MARCH_14 + 2 * DAY);
        assert_eq!(usage.used, 50, "no traffic read changes nothing");
    }

    /// A new reset day keeps the count so far: the period it belongs to
    /// follows the new day, at once as shown and with the next count.
    #[test]
    fn a_new_reset_day_keeps_the_count() {
        let mut usage = MobileDataUsage {
            reset_day: ResetDay::new(15).unwrap(),
            ..MobileDataUsage::default()
        };
        usage.account(Some(1_000), None, MARCH_14);
        usage.account(Some(1_400), None, MARCH_14 + 60);
        usage.reset_day = ResetDay::new(10).unwrap();
        assert_eq!(usage.period_start(MARCH_14 + 120), (3, 10));
        usage.account(Some(1_500), None, MARCH_14 + 120);
        assert_eq!(usage.used, 500, "the count stays with the new day");
        assert_eq!(usage.period_start(MARCH_14 + 120), (3, 10));
        usage.account(Some(1_600), None, MARCH_14 + 27 * DAY);
        assert_eq!(
            usage.used, 100,
            "and starts again on the 10th of April, with what came after"
        );
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
        assert_eq!(ResetDay::nearest(0), ResetDay::default());
        assert_eq!(ResetDay::nearest(31).get(), 28);
    }

    /// The modem's count changes the section without being anybody's step;
    /// resetting is the person's.
    #[test]
    fn the_count_is_counted_and_reset_through_the_verbs() {
        let mut usage = Replica::<MobileDataUsage>::new();
        usage
            .apply(MobileDataUsage::COUNT_DATA_USED.with((
                Some(1_000),
                Some("a".to_owned()),
                MARCH_14,
            )))
            .unwrap();
        usage
            .apply(MobileDataUsage::COUNT_DATA_USED.with((
                Some(1_250),
                Some("a".to_owned()),
                MARCH_14 + 60,
            )))
            .unwrap();
        assert_eq!(usage.state().unwrap().used, 250);
        usage
            .apply(MobileDataUsage::RESET_DATA_USED.with(()))
            .unwrap();
        let state = usage.state().unwrap();
        assert_eq!((state.used, state.counter), (0, Some(1_250)));
    }
}
