//! Fluent strings for the Windows compatibility manager.
use std::cell::Cell;
use ui::i18n::I18nManager;

pub(crate) const EN_US: &str = include_str!("../locales/en-US.ftl");

thread_local! { static LOADED: Cell<bool> = const { Cell::new(false) }; }

/// Resolve a Windows Apps Fluent key, registering the bundled English fallback
/// per render thread before lookup.
pub fn t(key: &str) -> String {
    load();
    I18nManager::lookup(key, None)
}

/// Resolve `key` with one named variable (`{ $name }` in the message).
pub fn format(key: &str, name: &str, value: &str) -> String {
    load();
    I18nManager::format(key, &[(name, value.into())])
}

fn load() {
    LOADED.with(|loaded| {
        if !loaded.replace(true) {
            I18nManager::init("en-US");
            I18nManager::add_resource("en-US", EN_US.to_string());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::t;
    #[test]
    fn controls_resolve_from_the_bundle() {
        for key in [
            "windows-route-wine",
            "windows-route-proton",
            "windows-route-vm",
            "windows-use-recommendation",
            "windows-choose-windows-iso",
            "windows-open-recovery-desktop",
            "windows-start-appliance",
            "windows-escalate-install-to-vm",
            "windows-restart-appliance",
            "windows-suspend-appliance",
            "windows-resume-appliance",
            "windows-shutdown-appliance",
            "windows-connect-for-activation",
        ] {
            assert_ne!(t(key), key, "missing Fluent key: {key}");
        }
    }
}
