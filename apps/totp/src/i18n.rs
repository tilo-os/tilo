//! Authenticator's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`ui::i18n`] manager. The catalogue is embedded with `include_str!`, so there
//! is no build step and no runtime file I/O — a `--shots` render and a unit test
//! resolve the same strings a live session does. Same shape as Chat's
//! `i18n.rs`; the catalogue is being filled in as surfaces are touched, so a
//! string that is still a literal in the source is simply not migrated yet.
//!
//! The manager is thread-local, so registration is per-thread and idempotent:
//! [`ensure`] is cheap to call from any string site, and the first call on a
//! thread is the one that loads the bundle.

use std::cell::Cell;

use ui::i18n::I18nManager;

const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");

thread_local! {
    static LOADED: Cell<bool> = const { Cell::new(false) };
}

/// A key every catalogue revision defines; if it does not resolve after the
/// resource was added, no manager exists on this thread yet.
const PROBE_KEY: &str = "totp-add-cancel";

/// Load the catalogue on this thread if it is not loaded yet.
///
/// The host (the shell or the app runner) may already have installed a
/// manager with the user's locale; that one is never replaced. The catalogue
/// is added to whatever locale is active, and a fresh manager is created only
/// when there is none.
pub fn ensure() {
    LOADED.with(|l| {
        if l.get() {
            return;
        }
        if !add_to_live_manager(PROBE_KEY, SOURCE_FTL) {
            I18nManager::init("en-US");
            I18nManager::add_resource("en-US", SOURCE_FTL.to_string());
        }
        l.set(true);
    });
}

/// Add `ftl` to the manager already live on this thread, under its active
/// locale. Returns `false` when there is no manager to add to.
///
/// Adding a new resource bumps the catalogue revision, which is the cheap
/// tell. When it does not move — no manager, or a host that pre-installed
/// this very catalogue — `probe_key` decides: it resolves only if a manager
/// holds it. The pseudo-locale transform also rewrites a *missing* key, so it
/// is paused for that one lookup and restored.
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

/// Resolve an Authenticator string key.
pub fn t(key: &str) -> String {
    ensure();
    I18nManager::lookup(key, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_app_uses_resolves_to_a_real_string() {
        // A key that resolves to itself is a key with no catalogue entry — the
        // failure mode that ships an untranslatable literal to a screen.
        for key in [
            "totp-add-title-add",
            "totp-add-title-edit",
            "totp-add-title-provision",
            "totp-add-subtitle-add",
            "totp-add-subtitle-edit",
            "totp-add-subtitle-provision",
            "totp-add-card-scan-title",
            "totp-add-card-scan-long",
            "totp-add-card-scan-short",
            "totp-add-card-link-title",
            "totp-add-card-link-long",
            "totp-add-card-link-short",
            "totp-add-fill",
            "totp-add-divider",
            "totp-add-label-issuer",
            "totp-add-label-account",
            "totp-add-label-secret",
            "totp-add-advanced",
            "totp-add-label-algorithm",
            "totp-add-label-digits",
            "totp-add-label-period",
            "totp-add-cancel",
            "totp-add-save-add",
            "totp-add-save-edit",
            "totp-add-save-provision",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
            assert!(!s.trim().is_empty(), "{key} resolves to nothing");
        }
    }
}
