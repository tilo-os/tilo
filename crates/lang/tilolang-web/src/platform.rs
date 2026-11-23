//! The platform a window with nothing on screen stands on: the packages it loads, the words its
//! window speaks and the clock and zone it reads. `tilolang layout-check` lays its windows out on
//! it, and a browser page runs its apps on it.

use std::sync::Arc;

use std::rc::Rc;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{GlobalLocale, Locale, Platform};
use tilolang_tilo::tilolang::Compiled;

/// Browser and headless layout-check windows share the toolkit's catalogues, language and region
/// formats on their thread. Neither runs a desktop settings watcher or reads on-disk language
/// packs, so `words_revision` reflects only in-process `I18nManager` or region-format changes and
/// cannot notice an external catalogue changing while running.
static WEB_LOCALE: GlobalLocale = tilo_app_window::platform_locale!(GlobalLocale);

/// The compiled packages an instance loads: the app and what it uses, the app last; and the
/// packages it uses as metadata: the native apps' it calls (`use so.tilo.session as session`),
/// whose actions run at their own action APIs, and the platform's (the device's, Settings') whose
/// published resources it reads.
pub struct Packages(
    pub Rc<Vec<Compiled>>,
    pub Vec<tilolang_tilo::tilolang::PackageMeta>,
);

impl Platform for Packages {
    fn load(&self, host: &Host) -> u64 {
        // The platform's resources stand as a device that has none of them,
        // read as each declares when nothing is published: a view over the
        // sound or the battery lays out as it does on a computer without
        // them.
        for meta in &self.1 {
            if let Err(error) = host.runtime().load_foreign(meta.clone()) {
                eprintln!("tilolang: a platform package did not load: {error}");
            }
        }
        for (published, ty) in tilolang_tilo::platform_apps::platform_roots() {
            if !self
                .1
                .iter()
                .any(|meta| ty.starts_with(&format!("{}#", meta.app)))
            {
                continue;
            }
            match tilolang_tilo::runtime::ResourceRef::parse_id(published, ty) {
                Some(target) => {
                    if let Err(error) = host.runtime().set_reading(&target, "{}") {
                        eprintln!("tilolang: `{published}` reads nothing: {error}");
                    }
                }
                None => eprintln!("tilolang: `{published}` names no resource"),
            }
        }
        for compiled in self.0.iter() {
            if let Err(error) = host.load_compiled(compiled) {
                eprintln!("tilolang: a package did not load: {error}");
            }
        }
        0
    }

    fn locale(&self) -> Option<&dyn Locale> {
        Some(&WEB_LOCALE)
    }

    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_app_window::i18n::phrase(id, args)
    }

    fn now(&self) -> String {
        tilolang_tilo::instance::wall_now()
    }

    fn moment(&self) -> tilolang_tilo::instance::Zoned {
        tilolang_tilo::instance::wall_moment()
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        Arc::new(|| {})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The web / layout-check host agrees with every other host on the shared table of dates,
    /// times, clocks, week starts and German typed dates, and moves `words_revision` on
    /// in-process display locale changes.
    #[test]
    fn a_web_packages_dates_week_and_language_agree_with_every_other_host() {
        use tilolang_tilo::instance::testing::{
            LocalAgreementHost, assert_platform_agrees, compile_probe,
        };

        let _guard = ThreadLocaleGuard::enter(Some("en-US"));
        ui::i18n::I18nManager::set_locale("en-US");

        let (compiled, actions) = compile_probe();
        let platform = Rc::new(Packages(Rc::new(vec![compiled]), Vec::new()));
        let utc = ui::clock::Zone::of(ui::format::TimeZone::UTC);
        let mut harness = LocalAgreementHost::open(
            "Packages",
            Rc::clone(&platform) as Rc<dyn Platform>,
            move |formats| {
                ui::format::region::install_here(Some(formats), Some(utc.clone()));
            },
            |locale| {
                let _ = ui::i18n::set_person_locale(Some(locale));
            },
        );
        assert_platform_agrees(&mut harness, &actions);

        let catalog_rev = || {
            platform
                .words_revision()
                .wrapping_sub(ui::format::region::revision())
        };
        let rev_before = catalog_rev();
        ui::i18n::I18nManager::set_locale("de-DE");
        assert_ne!(
            catalog_rev(),
            rev_before,
            "switching display locale moves words_revision"
        );
    }
}
