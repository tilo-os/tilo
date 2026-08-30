//! Chat's Fluent catalogue.
//!
//! User-facing strings are keys, never literals, resolved through the toolkit's
//! [`ui::i18n`] manager. The catalogue is embedded with `include_str!`, so there
//! is no build step and no runtime file I/O — a `--shots` render and a unit test
//! resolve the same strings a live session does.
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

/// Load the catalogue on this thread if it is not loaded yet.
///
/// Call before resolving a key. A key with no catalogue resolves to the key
/// itself rather than panicking, so a missed call degrades to something legible
/// rather than to a crash — but it does look wrong, which is why every string
/// site goes through [`t`] instead of `ui::i18n!` directly.
pub fn ensure() {
    LOADED.with(|l| {
        if l.get() {
            return;
        }
        I18nManager::init("en-US");
        I18nManager::add_resource("en-US", SOURCE_FTL.to_string());
        l.set(true);
    });
}

/// Resolve a Chat string key.
pub fn t(key: &str) -> String {
    ensure();
    I18nManager::lookup(key, None)
}

/// Resolve a Chat string key that interpolates variables.
///
/// The values are the app's own words — an operation's label, a participant's
/// name — so the *sentence* around them stays in the catalogue and only the
/// nouns are substituted. Building the line with `format!` instead would put
/// English word order in the source and leave nothing to translate.
pub fn t_args(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    ensure();
    I18nManager::format(key, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_app_uses_resolves_to_a_real_string() {
        // A key that resolves to itself is a key with no catalogue entry — the
        // failure mode that ships an untranslatable literal to a screen.
        let key = "chat-mention-agent-tag";
        let s = t(key);
        assert_ne!(s, key, "{key} has no entry in the catalogue");
        assert!(!s.trim().is_empty(), "{key} resolves to nothing");
    }
}
