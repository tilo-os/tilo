//! What a package the session runs asks of the session's process: its words, and the person's
//! clock, zone and first day of the week, which the session holds as their settings.

use tilolang_tilo::background::Waker;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{GlobalLocale, Locale, Platform};

/// The session hosts several tilolang packages on one thread and owns the desktop's active
/// locale, so it shares the toolkit's catalogues, language, formats and live language-pack
/// revision and overrides only `add_words`: loading one package adds its catalogues without
/// narrowing the desktop's active locale via `speak_window_language`.
static SESSION_LOCALE: GlobalLocale = {
    let mut locale = tilo_app_window::platform_locale!(GlobalLocale);
    locale.add_words = tilo_app_window::i18n::add_session_words;
    locale
};

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

    fn locale(&self) -> Option<&dyn Locale> {
        Some(&SESSION_LOCALE)
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_app_window::i18n::phrase(id, args)
    }

    /// The time of day as the person reads a clock: the package's views run in the session,
    /// which holds their settings.
    fn clock(&self, hour: u32, minute: u32) -> String {
        ui::clock::hour_minute(hour, minute)
    }

    /// The families this device sets text in: the toolkit's catalog, which a session running a
    /// window has.
    fn fonts(&self) -> Vec<String> {
        ui::font_catalog::installed_families()
    }

    /// How a day or a time of day named in a sentence reads: the person's formats, which the
    /// session holds.
    fn sentence(&self, kind: &str, value: &str) -> String {
        tilo_app_window::date::sentence_text(kind, value)
    }

    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        if window {
            tilo_app_window::i18n::add_catalogues(catalogues);
        } else {
            tilo_app_window::i18n::add_shared_catalogues(catalogues);
        }
    }

    fn words_revision(&self) -> u64 {
        tilo_app_window::i18n::revision()
    }

    fn now(&self) -> String {
        tilolang_tilo::instance::wall_now()
    }

    fn moment(&self) -> tilolang_tilo::instance::Zoned {
        tilolang_tilo::instance::wall_moment()
    }

    fn waker(&self) -> Waker {
        std::sync::Arc::clone(&self.waker)
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

    /// Restores thread-local region formats, clock zone and i18n state (including an unset
    /// `person_locale` override) even if an assertion fails.
    struct ThreadLocaleGuard {
        prev_locale: String,
        prev_person: Option<String>,
    }

    impl ThreadLocaleGuard {
        fn enter(person: Option<&str>) -> Self {
            tilo_app_window::i18n::ensure();
            Self {
                prev_locale: ui::i18n::I18nManager::current_locale(),
                prev_person: ui::i18n::set_person_locale(person),
            }
        }
    }

    impl Drop for ThreadLocaleGuard {
        fn drop(&mut self) {
            let _ = ui::i18n::set_person_locale(self.prev_person.as_deref());
            ui::i18n::I18nManager::set_locale(&self.prev_locale);
            ui::format::region::install_here(None, None);
        }
    }

    /// A package's views run in the session, which holds the person's formats: a day's
    /// `start_of_week()` goes back to the day their region starts a week on, as their shell's
    /// calendar and their app windows read it, not to the Monday a view with no formats falls
    /// back on.
    #[test]
    fn a_packages_week_starts_on_the_persons_first_day() {
        let _guard = ThreadLocaleGuard::enter(None);
        let platform = OnSession {
            loads: Nothing,
            waker: std::sync::Arc::new(|| {}),
        };
        let monday = ui::format::region::RegionFormats {
            first_day: ui::format::region::FirstDayOfWeek::Monday,
            ..ui::format::region::RegionFormats::default()
        };
        ui::format::region::install_here(Some(monday), None);
        assert_eq!(platform.week_start(), 0, "a week that starts on Monday");
        ui::format::region::install_here(
            Some(ui::format::region::RegionFormats {
                first_day: ui::format::region::FirstDayOfWeek::Sunday,
                ..monday
            }),
            None,
        );
        assert_eq!(
            platform.week_start(),
            6,
            "the person starts their week on Sunday"
        );
        ui::format::region::install_here(
            Some(ui::format::region::RegionFormats {
                first_day: ui::format::region::FirstDayOfWeek::Saturday,
                ..monday
            }),
            None,
        );
        assert_eq!(
            platform.week_start(),
            5,
            "the person starts their week on Saturday"
        );
    }

    struct CompiledProbe(tilolang_tilo::tilolang::Compiled);

    impl Loads for CompiledProbe {
        fn load(&self, host: &Host) -> u64 {
            host.load_compiled(&self.0)
                .expect("the probe package loads");
            0
        }
    }

    /// The session host agrees with every other host on the shared table of dates, times,
    /// clocks, week starts and German typed dates, and moves `words_revision` when display
    /// locale changes (without moving it when only the person's language changes).
    #[test]
    fn a_session_packages_dates_week_and_language_agree_with_every_other_host() {
        use std::rc::Rc;
        use tilolang_tilo::instance::testing::{
            LocalAgreementHost, assert_platform_agrees, compile_probe,
        };

        let _guard = ThreadLocaleGuard::enter(Some("en-US"));
        ui::i18n::I18nManager::set_locale("en-US");

        let (compiled, actions) = compile_probe();
        let platform = Rc::new(OnSession {
            loads: CompiledProbe(compiled),
            waker: std::sync::Arc::new(|| {}),
        });
        let utc = ui::clock::Zone::of(ui::format::TimeZone::UTC);
        let mut harness = LocalAgreementHost::open(
            "OnSession",
            Rc::clone(&platform) as Rc<dyn Platform>,
            move |formats| {
                ui::format::region::install_here(Some(formats), Some(utc.clone()));
            },
            |locale| {
                let _ = ui::i18n::set_person_locale(Some(locale));
            },
        );
        assert_platform_agrees(&mut harness, &actions);

        // Subtract the process-wide `region::revision()` counter so another test thread calling
        // `region::install_here` cannot disturb this thread's catalogue revision check.
        let catalog_rev = || {
            platform
                .words_revision()
                .wrapping_sub(ui::format::region::revision())
        };
        let rev_before = catalog_rev();
        let _ = ui::i18n::set_person_locale(Some("en-US"));
        assert_eq!(catalog_rev(), rev_before);
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_ne!(
            catalog_rev(),
            rev_before,
            "switching display locale moves words_revision"
        );
    }
}
