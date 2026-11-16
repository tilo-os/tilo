//! What a package the session runs asks of the session's process: its words, and the person's
//! clock, zone and first day of the week, which the session holds as their settings.

use tilolang_tilo::background::Waker;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::Platform;
use tilolang_tilo::runtime::{NameHost, Principal};

/// How a package the session runs is put into its host.
pub(crate) trait Loads {
    /// Load the package into `host`: the later build it loaded, 0 for the installed package.
    fn load(&self, host: &Host) -> u64;
}

/// A package the session runs, put into its host by `loads`.
pub(crate) struct OnSession<L> {
    pub(crate) loads: L,
    pub(crate) waker: Waker,
}

impl<L: Loads> Platform for OnSession<L> {
    fn load(&self, host: &Host) -> u64 {
        self.loads.load(host)
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_ui_host::i18n::word(id, args, id)
    }

    /// The time of day as the person reads a clock: the package's views run in the session,
    /// which holds their settings.
    fn clock(&self, hour: u32, minute: u32) -> String {
        ui::clock::hour_minute(hour, minute)
    }

    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        if window {
            tilo_ui_host::i18n::add_catalogues(catalogues);
        } else {
            tilo_ui_host::i18n::add_shared_catalogues(catalogues);
        }
    }

    fn words_revision(&self) -> u64 {
        tilo_ui_host::i18n::revision()
    }

    fn now(&self) -> String {
        ui::clock::local_now()
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    fn zone(&self) -> i64 {
        i64::from(ui::clock::local_now().offset().local_minus_utc())
    }

    fn zone_name(&self) -> Option<String> {
        ui::time::local_zone_name()
    }

    /// The day the person's region starts its week on, as they chose it in Settings: the
    /// package's views run in the session, which holds their formats, so a day's
    /// `start_of_week()` goes back to the day their calendar starts a week on.
    fn week_start(&self) -> i64 {
        i64::from(ui::format::first_weekday().num_days_from_monday())
    }

    fn waker(&self) -> Waker {
        std::sync::Arc::clone(&self.waker)
    }
}

/// The person at the device, as the session names them.
pub(crate) struct Person(pub(crate) String);

impl NameHost for Person {
    fn name(&self, who: &Principal) -> String {
        if who.0.is_empty() {
            self.0.clone()
        } else if who.1.trim().is_empty() {
            // Named by what the actor is: an app by its own name in the person's language, an
            // agent by its name, a person by theirs. Never a principal id.
            ui::history_source::participant_name(&who.0)
        } else {
            who.1.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nothing;

    impl Loads for Nothing {
        fn load(&self, _host: &Host) -> u64 {
            0
        }
    }

    /// A package's views run in the session, which holds the person's formats: a day's
    /// `start_of_week()` goes back to the day their region starts a week on, as their shell's
    /// calendar and their app windows read it, not to the Monday a view with no formats falls
    /// back on.
    #[test]
    fn a_packages_week_starts_on_the_persons_first_day() {
        let platform = OnSession {
            loads: Nothing,
            waker: std::sync::Arc::new(|| {}),
        };
        let before = ui::format::region::current();
        let monday = ui::format::region::RegionFormats {
            first_day: ui::format::region::FirstDayOfWeek::Monday,
            ..before
        };
        ui::format::region::install(monday);
        assert_eq!(platform.week_start(), 0, "a week that starts on Monday");
        ui::format::region::install(ui::format::region::RegionFormats {
            first_day: ui::format::region::FirstDayOfWeek::Sunday,
            ..monday
        });
        assert_eq!(
            platform.week_start(),
            6,
            "the person starts their week on Sunday"
        );
        ui::format::region::install(before);
    }
}
