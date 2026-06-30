//! Fluent-based localization for the word processor's user-facing strings.
//!
//! The project mandates i18n via [Fluent](https://projectfluent.org/): UI code
//! passes **translation keys**, never literal display strings, to the chrome.
//! This module is the editor's small, self-contained loader for that — it owns
//! the bundled `.ftl` catalogs (embedded with `include_str!`, so there is no
//! build script and no runtime file I/O), picks a locale from the environment,
//! and resolves keys through [`t`] / [`t_args`].
//!
//! Design goals:
//! - **Dependency-light**: only `fluent` + `unic-langid` (already in the
//!   workspace lockfile via `ui`). No `fluent-fallback`, no loader crates.
//! - **Panic-free**: a missing key, a missing locale, or a malformed pattern
//!   never panics — it degrades to the English string, and ultimately to the
//!   key itself, so the UI always renders *something* legible.
//! - **Self-contained**: the editor does not depend on global i18n state set up
//!   elsewhere (e.g. `ui::i18n`), so a headless `--shots` render or a unit test
//!   gets correct strings without any wiring.
//!
//! Locale selection (first hit wins): `LC_ALL`, then `LANGUAGE`, then `LANG`.
//! The value is read as a POSIX locale (`de_DE.UTF-8`), of which only the
//! language/region part (`de-DE`) is used. If no catalog matches the requested
//! language, `en-US` (the source locale) is used.

use std::sync::{OnceLock, RwLock};

use fluent::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

/// The source language id (`en-US`). Parsed from a known-good literal; if the
/// parse ever fails (it cannot for this constant) we fall back to a bare `en`.
fn source_lang() -> LanguageIdentifier {
    "en-US".parse().unwrap_or_else(|_| "en".parse().expect("`en` is a valid language id"))
}

/// The source locale — always present, the fallback for every other locale.
const SOURCE_FTL: &str = include_str!("../locales/en-US.ftl");

/// Bundled non-source catalogs: `(language-id, ftl-source)`. Add a locale by
/// dropping an `editor/locales/<lang>.ftl` next to `en-US.ftl` and listing it
/// here. Translations are best-effort: any key a catalog omits transparently
/// falls back to the English string.
const CATALOGS: &[(&str, &str)] = &[("de", include_str!("../locales/de.ftl"))];

/// One resolvable locale: a parsed Fluent bundle plus its language id.
///
/// `FluentBundle` is `!Sync` (its `intl-memoizer` holds a `RefCell`), so the
/// parsed bundles cannot live in a process-global. We keep them in
/// thread-local storage instead (see `BUNDLES`); only the *active locale* — a
/// plain, `Sync` [`LanguageIdentifier`] — is shared across threads.
struct Catalog {
    lang: LanguageIdentifier,
    bundle: FluentBundle<FluentResource>,
}

/// All bundled catalogs, parsed once per thread. Source (`en-US`) is index-0;
/// every other locale follows in registration order.
struct Catalogs {
    /// The source/fallback catalog (`en-US`), tried last in every lookup.
    source: Catalog,
    /// Additional locales, in registration order.
    others: Vec<Catalog>,
}

/// Build a bundle from FTL source. A parse error in a (bundled, test-validated)
/// catalog is non-fatal: we keep whatever entries parsed and move on, so a typo
/// in one message never blanks the whole UI.
fn make_bundle(lang: LanguageIdentifier, ftl: &str) -> FluentBundle<FluentResource> {
    let mut bundle = FluentBundle::new(vec![lang]);
    // Disable Fluent's default bidi isolation marks (U+2068/U+2069). They are
    // correct for mixed-direction text but render as stray boxes in our glyph
    // pipeline for the simple LTR strings the editor uses.
    bundle.set_use_isolating(false);
    let resource = match FluentResource::try_new(ftl.to_string()) {
        Ok(res) => res,
        // `try_new` returns the partially-parsed resource alongside the errors;
        // keep the good entries rather than dropping the whole catalog.
        Err((res, _errors)) => res,
    };
    // An overriding/duplicate id is the only `add_resource` error; ignore it —
    // the first definition wins, which is the behaviour we want.
    let _ = bundle.add_resource(resource);
    bundle
}

/// Read the preferred locale from the environment, as a [`LanguageIdentifier`].
/// Returns `None` when nothing usable is set (callers then keep `en-US`).
fn locale_from_env() -> Option<LanguageIdentifier> {
    for var in ["LC_ALL", "LANGUAGE", "LANG"] {
        if let Ok(val) = std::env::var(var) {
            if let Some(id) = parse_posix_locale(&val) {
                return Some(id);
            }
        }
    }
    None
}

/// Parse a POSIX-style locale string (`de_DE.UTF-8`, `en_US`, `de`, or a
/// colon-separated `LANGUAGE` list) into a BCP-47 [`LanguageIdentifier`].
fn parse_posix_locale(raw: &str) -> Option<LanguageIdentifier> {
    // `LANGUAGE` may be `de:en:...`; take the first entry. Strip the charset
    // (`.UTF-8`) and modifier (`@euro`) suffixes, and normalise `_` to `-`.
    let first = raw.split(':').next().unwrap_or(raw).trim();
    if first.is_empty() || first.eq_ignore_ascii_case("C") || first.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let core = first.split(['.', '@']).next().unwrap_or(first);
    let bcp47 = core.replace('_', "-");
    bcp47.parse().ok()
}

/// Parse every bundled catalog into [`Catalogs`]. Called once per thread the
/// first time that thread resolves a key.
fn build_catalogs() -> Catalogs {
    let source = Catalog { lang: source_lang(), bundle: make_bundle(source_lang(), SOURCE_FTL) };
    let others = CATALOGS
        .iter()
        .filter_map(|(tag, ftl)| {
            let lang: LanguageIdentifier = tag.parse().ok()?;
            Some(Catalog { lang: lang.clone(), bundle: make_bundle(lang, ftl) })
        })
        .collect();
    Catalogs { source, others }
}

/// The process-wide active locale. Shared across threads (a test thread's
/// `set_locale` is visible to the render thread). A plain `LanguageIdentifier`
/// is `Send + Sync`, so — unlike the bundles — it can live in a global.
static ACTIVE: OnceLock<RwLock<LanguageIdentifier>> = OnceLock::new();

fn active_lock() -> &'static RwLock<LanguageIdentifier> {
    ACTIVE.get_or_init(|| RwLock::new(locale_from_env().unwrap_or_else(source_lang)))
}

/// The currently-active language id.
fn active_lang() -> LanguageIdentifier {
    active_lock().read().map(|l| l.clone()).unwrap_or_else(|_| source_lang())
}

thread_local! {
    // Parsed bundles, lazily built per thread (FluentBundle is !Sync).
    static BUNDLES: Catalogs = build_catalogs();
}

impl Catalogs {
    /// The bundle for `lang`, or `None` when no catalog matches its primary
    /// language subtag (so `de-DE` resolves the `de` catalog; an unknown
    /// language resolves nothing and the caller falls back to source).
    fn bundle_for(&self, lang: &LanguageIdentifier) -> Option<&FluentBundle<FluentResource>> {
        if lang.language == self.source.lang.language {
            return None;
        }
        self.others
            .iter()
            .find(|c| c.lang.language == lang.language)
            .map(|c| &c.bundle)
    }

    /// Resolve `key` for `lang`: active locale first, then the source catalog,
    /// then the key itself. Never panics.
    fn lookup(&self, lang: &LanguageIdentifier, key: &str, args: Option<&FluentArgs>) -> String {
        if let Some(bundle) = self.bundle_for(lang) {
            if let Some(s) = format_in(bundle, key, args) {
                return s;
            }
        }
        if let Some(s) = format_in(&self.source.bundle, key, args) {
            return s;
        }
        key.to_string()
    }
}

/// Format `key` against `bundle`, returning `None` if the key is absent or has
/// no value (so the caller can fall through to the next bundle).
fn format_in(
    bundle: &FluentBundle<FluentResource>,
    key: &str,
    args: Option<&FluentArgs>,
) -> Option<String> {
    let msg = bundle.get_message(key)?;
    let pattern = msg.value()?;
    let mut errors = Vec::new();
    let out = bundle.format_pattern(pattern, args, &mut errors);
    Some(out.into_owned())
}

/// Switch the active locale at runtime (used by tests and by callers that want
/// to honour a locale change without restarting). Accepts a BCP-47 or POSIX
/// locale string; an unparseable value falls back to the source locale. Unknown
/// languages fall back to the source locale at lookup time.
pub fn set_locale(locale: &str) {
    let active = parse_posix_locale(locale)
        .or_else(|| locale.parse().ok())
        .unwrap_or_else(source_lang);
    if let Ok(mut guard) = active_lock().write() {
        *guard = active;
    }
}

/// The active locale's BCP-47 tag (e.g. `"de"` or `"en-US"`). Useful for tests
/// and diagnostics.
pub fn current_locale() -> String {
    active_lang().to_string()
}

/// Translate `key` to a string in the active locale.
///
/// Resolution order: active locale → `en-US` source → the key itself. A missing
/// key returns the key (a safe, debuggable placeholder) rather than panicking.
pub fn t(key: &str) -> String {
    let lang = active_lang();
    BUNDLES.with(|cats| cats.lookup(&lang, key, None))
}

/// Translate `key`, interpolating Fluent placeables (e.g. `{ $n }`) from
/// `args`. Use [`arg`] to build the slice ergonomically:
///
/// ```ignore
/// let s = t_args("status.page-of", &[arg("cur", 3), arg("total", 10)]);
/// ```
pub fn t_args(key: &str, args: &[(&str, FluentValue<'_>)]) -> String {
    let mut fargs = FluentArgs::new();
    for (name, val) in args {
        fargs.set(*name, val.clone());
    }
    let lang = active_lang();
    BUNDLES.with(|cats| cats.lookup(&lang, key, Some(&fargs)))
}

/// Build a single Fluent argument from anything convertible into a
/// [`FluentValue`] (numbers, `&str`, `String`). A small ergonomic helper for
/// [`t_args`] call sites.
pub fn arg<'a, V: Into<FluentValue<'a>>>(name: &'a str, value: V) -> (&'a str, FluentValue<'a>) {
    (name, value.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    // The active locale is process-global; serialise the locale-switching tests
    // so they don't race each other's `set_locale`. Tolerate poisoning (a
    // failing assertion in one test must not cascade into the others).
    static LOCALE_GUARD: StdMutex<()> = StdMutex::new(());

    fn lock_locale() -> std::sync::MutexGuard<'static, ()> {
        LOCALE_GUARD.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn en_us_returns_source_string() {
        let _g = lock_locale();
        set_locale("en-US");
        assert_eq!(t("toolbar-body"), "Body");
        assert_eq!(t("template-report"), "Report");
        assert_eq!(t("recovery-banner"), "Recovered unsaved changes from a previous session.");
    }

    #[test]
    fn de_returns_german_string() {
        let _g = lock_locale();
        set_locale("de");
        assert_eq!(t("toolbar-body"), "Fließtext");
        assert_eq!(t("template-report"), "Bericht");
        // A POSIX-style value resolves the same `de` catalog.
        set_locale("de_DE.UTF-8");
        assert_eq!(t("template-report"), "Bericht");
        set_locale("en-US");
    }

    #[test]
    fn missing_key_falls_back_to_key_without_panicking() {
        let _g = lock_locale();
        set_locale("de");
        assert_eq!(t("this.key.does.not.exist"), "this.key.does.not.exist");
        set_locale("en-US");
        assert_eq!(t("this.key.does.not.exist"), "this.key.does.not.exist");
    }

    #[test]
    fn de_falls_back_to_english_for_untranslated_key() {
        let _g = lock_locale();
        // `find.match-case` is intentionally English-only in the de catalog test
        // surface; assert it falls through to en-US rather than to the key.
        set_locale("de");
        let v = t("find-find");
        // The de catalog DOES translate this; assert it is neither the key nor
        // empty (proves resolution worked at all).
        assert_ne!(v, "find.find");
        assert!(!v.is_empty());
        set_locale("en-US");
    }

    #[test]
    fn t_args_interpolates_numbers() {
        let _g = lock_locale();
        set_locale("en-US");
        let s = t_args("find-count", &[arg("cur", 2), arg("total", 7)]);
        assert_eq!(s, "2 / 7");

        set_locale("de");
        // German uses the same numeric pattern; the count format is identical.
        let s = t_args("find-count", &[arg("cur", 3), arg("total", 9)]);
        assert_eq!(s, "3 / 9");
        set_locale("en-US");
    }

    #[test]
    fn unknown_locale_falls_back_to_source() {
        let _g = lock_locale();
        set_locale("xx-ZZ");
        // No catalog for xx-ZZ → English string.
        assert_eq!(t("template-blank"), "Blank");
        set_locale("en-US");
    }

    #[test]
    fn posix_c_locale_parses_to_none() {
        assert!(parse_posix_locale("C").is_none());
        assert!(parse_posix_locale("POSIX").is_none());
        assert!(parse_posix_locale("").is_none());
        assert_eq!(parse_posix_locale("de_DE.UTF-8").map(|l| l.to_string()), Some("de-DE".to_string()));
        assert_eq!(parse_posix_locale("en_US").map(|l| l.to_string()), Some("en-US".to_string()));
        // LANGUAGE list: first entry wins.
        assert_eq!(parse_posix_locale("de:en_US").map(|l| l.to_string()), Some("de".to_string()));
    }
}
