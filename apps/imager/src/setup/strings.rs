//! Fluent catalogue for setup's semantic resource and action surface.

use ui::i18n::ArgValue;

pub fn t(key: &str) -> String {
    crate::resource::CATALOG.t(key)
}

pub fn t_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    crate::resource::CATALOG.t_args(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::i18n::I18nManager;

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
