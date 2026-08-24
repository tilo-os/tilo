//! Fluent-backed localization for the strings the shell paints itself.
//!
//! Chrome the compositor draws — palette rows, notification toasts — is
//! read by a person, so those strings are translation *keys* resolved
//! here rather than literals compiled into the call site.
//!
//! The catalogs are embedded with `include_str!`, so there is no build
//! script, no runtime file I/O and no global initialisation to forget:
//! a headless `--shots` render, a unit test and the live compositor all
//! resolve the same strings without any wiring. Resolution degrades
//! rather than panics — active locale, then `en-US`, then the key —
//! because a compositor that cannot find a string still has to paint
//! something legible.
//!
//! Locale comes from `LC_ALL` / `LANGUAGE` / `LANG`, read as POSIX
//! (`de_DE.UTF-8`) and reduced to its language subtag.

use std::sync::{OnceLock, RwLock};

use fluent::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

/// The source locale — always present, and the fallback for every other.
const SOURCE_FTL: &str = include_str!("../../locales/en-US.ftl");

/// Bundled translations, `(language-tag, ftl-source)`. Add a locale by
/// dropping `shell/locales/<lang>.ftl` next to `en-US.ftl` and listing
/// it here; any key it omits falls through to English.
const CATALOGS: &[(&str, &str)] = &[("de", include_str!("../../locales/de.ftl"))];

fn source_lang() -> LanguageIdentifier {
    "en-US"
        .parse()
        .unwrap_or_else(|_| "en".parse().expect("`en` is a valid language id"))
}

struct Catalog {
    lang: LanguageIdentifier,
    bundle: FluentBundle<FluentResource>,
}

/// Every bundled catalog, parsed once per thread.
///
/// `FluentBundle` is `!Sync` (its memoizer holds a `RefCell`), so the
/// parsed bundles cannot live in a process-global; only the active
/// locale, a plain `LanguageIdentifier`, is shared across threads.
struct Catalogs {
    source: Catalog,
    others: Vec<Catalog>,
}

fn make_bundle(lang: LanguageIdentifier, ftl: &str) -> FluentBundle<FluentResource> {
    let mut bundle = FluentBundle::new(vec![lang]);
    // Fluent's bidi isolation marks (U+2068/U+2069) are correct for
    // mixed-direction text and render as stray boxes in our glyph
    // pipeline for the LTR strings the shell uses.
    bundle.set_use_isolating(false);
    let resource = match FluentResource::try_new(ftl.to_string()) {
        Ok(res) => res,
        // `try_new` hands back the partially-parsed resource; keep the
        // entries that did parse rather than blanking the catalog over
        // one typo.
        Err((res, _errors)) => res,
    };
    let _ = bundle.add_resource(resource);
    bundle
}

/// Parse a POSIX locale (`de_DE.UTF-8`, `en_US`, a colon-separated
/// `LANGUAGE` list) into a BCP-47 identifier. `C`/`POSIX`/empty are not
/// locales and yield `None`.
fn parse_posix_locale(raw: &str) -> Option<LanguageIdentifier> {
    let first = raw.split(':').next().unwrap_or(raw).trim();
    if first.is_empty() || first.eq_ignore_ascii_case("C") || first.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let core = first.split(['.', '@']).next().unwrap_or(first);
    core.replace('_', "-").parse().ok()
}

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

fn build_catalogs() -> Catalogs {
    Catalogs {
        source: Catalog {
            lang: source_lang(),
            bundle: make_bundle(source_lang(), SOURCE_FTL),
        },
        others: CATALOGS
            .iter()
            .filter_map(|(tag, ftl)| {
                let lang: LanguageIdentifier = tag.parse().ok()?;
                Some(Catalog {
                    lang: lang.clone(),
                    bundle: make_bundle(lang, ftl),
                })
            })
            .collect(),
    }
}

static ACTIVE: OnceLock<RwLock<LanguageIdentifier>> = OnceLock::new();

fn active_lock() -> &'static RwLock<LanguageIdentifier> {
    ACTIVE.get_or_init(|| RwLock::new(locale_from_env().unwrap_or_else(source_lang)))
}

fn active_lang() -> LanguageIdentifier {
    active_lock()
        .read()
        .map(|l| l.clone())
        .unwrap_or_else(|_| source_lang())
}

thread_local! {
    static BUNDLES: Catalogs = build_catalogs();
}

impl Catalogs {
    fn bundle_for(&self, lang: &LanguageIdentifier) -> Option<&FluentBundle<FluentResource>> {
        if lang.language == self.source.lang.language {
            return None;
        }
        self.others
            .iter()
            .find(|c| c.lang.language == lang.language)
            .map(|c| &c.bundle)
    }

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

fn format_in(
    bundle: &FluentBundle<FluentResource>,
    key: &str,
    args: Option<&FluentArgs>,
) -> Option<String> {
    let msg = bundle.get_message(key)?;
    let pattern = msg.value()?;
    let mut errors = Vec::new();
    Some(bundle.format_pattern(pattern, args, &mut errors).into_owned())
}

/// Switch the active locale. Accepts BCP-47 or POSIX; anything
/// unparseable falls back to the source locale.
pub fn set_locale(locale: &str) {
    let active = parse_posix_locale(locale)
        .or_else(|| locale.parse().ok())
        .unwrap_or_else(source_lang);
    if let Ok(mut guard) = active_lock().write() {
        *guard = active;
    }
}

/// The active locale's BCP-47 tag.
pub fn current_locale() -> String {
    active_lang().to_string()
}

/// Resolve `key` in the active locale.
pub fn t(key: &str) -> String {
    let lang = active_lang();
    BUNDLES.with(|cats| cats.lookup(&lang, key, None))
}

/// Resolve `key`, interpolating its placeables from `args`.
pub fn t_args(key: &str, args: &[(&str, FluentValue<'_>)]) -> String {
    let mut fargs = FluentArgs::new();
    for (name, val) in args {
        fargs.set(*name, val.clone());
    }
    let lang = active_lang();
    BUNDLES.with(|cats| cats.lookup(&lang, key, Some(&fargs)))
}

/// One argument for [`t_args`].
pub fn arg<'a, V: Into<FluentValue<'a>>>(name: &'a str, value: V) -> (&'a str, FluentValue<'a>) {
    (name, value.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(locale)]
    fn palette_keys_resolve_to_english_strings() {
        set_locale("en-US");
        assert_eq!(
            t_args("palette-open-app", &[arg("app", "Sheets")]),
            "Open Sheets"
        );
        assert_eq!(
            t_args("palette-fetch-and-open-app", &[arg("app", "Sheets")]),
            "Fetch and open Sheets"
        );
        assert_eq!(t("palette-not-on-this-machine"), "not on this machine");
    }

    /// The user-facing wording must never read as an install: nobody
    /// asked to install anything, they asked to open an app.
    #[test]
    #[serial_test::serial(locale)]
    fn no_shipped_string_asks_to_install_anything() {
        for locale in ["en-US", "de"] {
            set_locale(locale);
            for key in [
                "palette-fetch-and-open-app",
                "palette-not-on-this-machine",
                "notice-fetch-connecting",
                "notice-fetch-downloading",
                "notice-fetch-unpacking",
                "notice-fetch-starting",
            ] {
                let rendered = t_args(
                    key,
                    &[
                        arg("app", "X"),
                        arg("registry", "main"),
                        arg("done", "1 MB"),
                        arg("total", "2 MB"),
                    ],
                )
                .to_lowercase();
                assert!(
                    !rendered.contains("install"),
                    "{locale}/{key} must not say install: {rendered:?}"
                );
            }
        }
        set_locale("en-US");
    }

    #[test]
    #[serial_test::serial(locale)]
    fn german_resolves_and_falls_back_to_english_per_key() {
        set_locale("de_DE.UTF-8");
        assert_eq!(
            t_args("palette-open-app", &[arg("app", "Sheets")]),
            "Sheets öffnen"
        );
        assert_eq!(t("palette-not-on-this-machine"), "nicht auf diesem Gerät");
        // A key no catalog defines still renders something debuggable
        // rather than panicking or blanking.
        assert_eq!(t("no-such-key-anywhere"), "no-such-key-anywhere");
        set_locale("en-US");
    }

    #[test]
    #[serial_test::serial(locale)]
    fn an_unknown_locale_falls_back_to_the_source_catalog() {
        set_locale("xx-ZZ");
        assert_eq!(t("palette-not-on-this-machine"), "not on this machine");
        set_locale("en-US");
    }

    #[test]
    fn posix_locale_parsing() {
        assert!(parse_posix_locale("C").is_none());
        assert!(parse_posix_locale("").is_none());
        assert_eq!(
            parse_posix_locale("de_DE.UTF-8").map(|l| l.to_string()),
            Some("de-DE".to_string())
        );
        assert_eq!(
            parse_posix_locale("de:en_US").map(|l| l.to_string()),
            Some("de".to_string())
        );
    }
}
