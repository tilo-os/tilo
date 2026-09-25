//! Fluent catalogue for setup's semantic resource and action surface.

use std::cell::Cell;

use ui::i18n::{ArgValue, I18nManager};

const SOURCE_FTL: &str = include_str!("../../locales/en-US.ftl");
const PROBE_KEY: &str = "imager-setup-action-next";

thread_local! {
    static LOADED: Cell<bool> = const { Cell::new(false) };
}

pub fn ensure() {
    LOADED.with(|loaded| {
        if loaded.get() {
            return;
        }
        if !add_to_live_manager(PROBE_KEY, SOURCE_FTL) {
            I18nManager::init("en-US");
            I18nManager::add_resource("en-US", SOURCE_FTL.to_string());
        }
        loaded.set(true);
    });
}

fn add_to_live_manager(probe_key: &str, ftl: &str) -> bool {
    let before = I18nManager::revision();
    I18nManager::add_resource(&I18nManager::current_locale(), ftl.to_string());
    if I18nManager::revision() != before {
        return true;
    }
    let mode = I18nManager::pseudo_locale();
    let pseudo_on = !matches!(mode, ui::i18n::PseudoLocale::Off);
    if pseudo_on {
        I18nManager::set_pseudo_locale(ui::i18n::PseudoLocale::Off);
    }
    let resolved = I18nManager::lookup_plain(probe_key) != probe_key;
    if pseudo_on {
        I18nManager::set_pseudo_locale(mode);
    }
    resolved
}

pub fn t(key: &str) -> String {
    ensure();
    I18nManager::lookup_plain(key)
}

pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    ensure();
    I18nManager::format(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_catalog_resolves_and_preserves_the_host_locale() {
        I18nManager::install(
            "en-US",
            "de-DE",
            &[
                ("en-US", "host-probe = Host"),
                ("de-DE", "host-probe = Rechner"),
            ],
        )
        .unwrap();

        assert_eq!(t("imager-setup-action-next"), "Continue setup");
        assert_eq!(I18nManager::current_locale(), "de-DE");
        assert_eq!(I18nManager::lookup_plain("host-probe"), "Rechner");
    }
}
