use fluent::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use std::cell::Cell;
use std::collections::HashMap;
use thiserror::Error;
use unic_langid::LanguageIdentifier;

/// An owned value to interpolate into a localized message. Owned (rather than a
/// borrowed `FluentValue`) so callers — most importantly the retained-mode
/// `Text` widget — can stash arguments alongside a translation key and re-format
/// every frame without keeping the original references alive.
///
/// `From` is implemented for the common scalar types, so `5`, `1.5`, `"x"`, and
/// `String` all coerce directly at the call site.
#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    Number(f64),
    Text(String),
}

impl ArgValue {
    fn to_fluent(&self) -> FluentValue<'_> {
        match self {
            ArgValue::Number(n) => FluentValue::from(*n),
            ArgValue::Text(s) => FluentValue::from(s.as_str()),
        }
    }
}

impl From<&str> for ArgValue {
    fn from(s: &str) -> Self {
        ArgValue::Text(s.to_string())
    }
}
impl From<String> for ArgValue {
    fn from(s: String) -> Self {
        ArgValue::Text(s)
    }
}
impl From<&String> for ArgValue {
    fn from(s: &String) -> Self {
        ArgValue::Text(s.clone())
    }
}

/// Widening conversions into `f64` — exact for every value of `$t`.
macro_rules! arg_from_number_lossless {
    ($($t:ty),*) => {$(
        impl From<$t> for ArgValue {
            fn from(n: $t) -> Self { ArgValue::Number(f64::from(n)) }
        }
    )*};
}
arg_from_number_lossless!(i8, i16, i32, u8, u16, u32, f32);

/// Narrowing conversions into `f64`, for display/interpolation only: a
/// fluent message shows a *formatted* number, not an exact one, so losing
/// precision above 2^52 — a magnitude no UI counter or index realistically
/// reaches — is a deliberate, harmless approximation.
macro_rules! arg_from_number_lossy {
    ($($t:ty),*) => {$(
        impl From<$t> for ArgValue {
            #[allow(
                clippy::cast_precision_loss,
                reason = "display-only formatting; precision above 2^52 is never meaningful in a UI string"
            )]
            fn from(n: $t) -> Self { ArgValue::Number(n as f64) }
        }
    )*};
}
arg_from_number_lossy!(i64, isize, u64, usize);

impl From<f64> for ArgValue {
    fn from(n: f64) -> Self {
        ArgValue::Number(n)
    }
}

pub struct I18nManager {
    bundles: HashMap<LanguageIdentifier, FluentBundle<FluentResource>>,
    catalog_order: Vec<LanguageIdentifier>,
    source_locale: LanguageIdentifier,
    pub current_locale: LanguageIdentifier,
    revision: u64,
}

/// Layout-stress translation mode used by screenshots and acceptance tests.
/// It deliberately preserves digits and punctuation while expanding letters,
/// making clipped labels and fixed-width controls visible before translators
/// have completed a catalog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PseudoLocale {
    #[default]
    Off,
    Expanded,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum I18nError {
    #[error("invalid locale {0:?}")]
    InvalidLocale(String),
    #[error("locale {0:?} occurs more than once in the catalog")]
    DuplicateLocale(String),
    #[error("the source locale {0:?} has no catalog")]
    MissingSource(String),
    #[error("invalid Fluent resource for {locale:?}: {details}")]
    InvalidResource { locale: String, details: String },
}

thread_local! {
    static MANAGER: std::cell::RefCell<Option<I18nManager>> = const { std::cell::RefCell::new(None) };
    static PSEUDO_LOCALE: Cell<PseudoLocale> = const { Cell::new(PseudoLocale::Off) };
    static PSEUDO_REVISION: Cell<u64> = const { Cell::new(0) };
    // Shared controls remain readable even when an app installs only its own
    // catalog. App translations always take precedence over this source.
    static TOOLKIT_SOURCE: Result<FluentBundle<FluentResource>, I18nError> =
        make_bundle(&parse_locale_or_default("en-US"), include_str!("../locales/en-US.ftl"));
}

fn pseudo_expand(value: &str) -> String {
    let mut expanded = String::with_capacity(value.len().saturating_mul(2).saturating_add(4));
    expanded.push('⟦');
    for ch in value.chars() {
        let replacement = match ch {
            'a' => "áa",
            'A' => "ÁA",
            'e' => "ée",
            'E' => "ÉE",
            'i' => "íi",
            'I' => "ÍI",
            'o' => "óo",
            'O' => "ÓO",
            'u' => "úu",
            'U' => "ÚU",
            'y' => "ýy",
            'Y' => "ÝY",
            _ => {
                expanded.push(ch);
                continue;
            }
        };
        expanded.push_str(replacement);
    }
    expanded.push('⟧');
    expanded
}

fn apply_pseudo_locale(value: String) -> String {
    PSEUDO_LOCALE.with(|mode| match mode.get() {
        PseudoLocale::Off => value,
        PseudoLocale::Expanded => pseudo_expand(&value),
    })
}

/// Parse a BCP-47 locale tag, falling back to `en-US` on a malformed value
/// (e.g. a garbled system locale) rather than panicking the whole app over
/// a string it didn't choose. `en-US` is a compile-time-known-valid literal,
/// so the final `unwrap_or_default` (BCP-47 "und", undefined) is a belt and
/// braces path that's never actually reached.
fn parse_locale_or_default(locale: &str) -> LanguageIdentifier {
    parse_locale(locale).unwrap_or_else(|_| "en-US".parse().unwrap_or_default())
}

fn parse_locale(locale: &str) -> Result<LanguageIdentifier, I18nError> {
    let first = locale.split(':').next().unwrap_or(locale).trim();
    if first.is_empty() || first.eq_ignore_ascii_case("C") || first.eq_ignore_ascii_case("POSIX") {
        return Err(I18nError::InvalidLocale(locale.to_owned()));
    }
    let core = first.split(['.', '@']).next().unwrap_or(first);
    core.replace('_', "-")
        .parse()
        .map_err(|_| I18nError::InvalidLocale(locale.to_owned()))
}

/// Whether `locale` names a language this module can load — not empty,
/// `C` or `POSIX`.
pub(crate) fn is_usable_locale(locale: &str) -> bool {
    parse_locale(locale).is_ok()
}

/// The host's preferred locale; see [`crate::host::HostEnvironment::locale`].
fn preferred_locale() -> Option<LanguageIdentifier> {
    parse_locale(crate::host::environment().locale.as_deref()?).ok()
}

fn make_bundle(
    locale: &LanguageIdentifier,
    source: &str,
) -> Result<FluentBundle<FluentResource>, I18nError> {
    let resource = FluentResource::try_new(source.to_owned()).map_err(|(_, errors)| {
        I18nError::InvalidResource {
            locale: locale.to_string(),
            details: format!("{errors:?}"),
        }
    })?;
    let mut bundle = FluentBundle::new(vec![locale.clone()]);
    bundle.set_use_isolating(false);
    bundle
        .add_resource(resource)
        .map_err(|errors| I18nError::InvalidResource {
            locale: locale.to_string(),
            details: format!("{errors:?}"),
        })?;
    Ok(bundle)
}

fn format_from_bundle(
    bundle: &FluentBundle<FluentResource>,
    key: &str,
    args: Option<&FluentArgs>,
) -> Option<String> {
    let message = bundle.get_message(key)?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    Some(
        bundle
            .format_pattern(pattern, args, &mut errors)
            .to_string(),
    )
}

impl I18nManager {
    /// Register shared widget labels without replacing an application's
    /// catalogs or its selected language. Standalone widgets get the same
    /// source-locale fallback as application-hosted widgets.
    pub fn register_widget_messages() {
        const ENGLISH: &str = include_str!("../locales/en-US.ftl");
        const GERMAN: &str = include_str!("../locales/de-DE.ftl");
        if Self::revision() == 0 {
            let _ = Self::install_system("en-US", &[("en-US", ENGLISH), ("de-DE", GERMAN)]);
        } else {
            Self::add_resource("en-US", ENGLISH.to_owned());
            Self::add_resource("de-DE", GERMAN.to_owned());
        }
    }

    pub fn init(locale: &str) {
        let lang_id: LanguageIdentifier = parse_locale_or_default(locale);
        let locale = lang_id.clone();
        let mut bundle = FluentBundle::new(vec![lang_id]);
        // Don't wrap interpolated variables in Unicode bidi isolation marks
        // (FSI/PDI) — we render to a single run, so "Count is { $n }" should
        // read "Count is 5", not "Count is \u{2068}5\u{2069}".
        bundle.set_use_isolating(false);
        MANAGER.with(|m| {
            let revision = m
                .borrow()
                .as_ref()
                .map_or(1, |manager| manager.revision.wrapping_add(1));
            *m.borrow_mut() = Some(I18nManager {
                bundles: HashMap::from([(locale.clone(), bundle)]),
                catalog_order: vec![locale.clone()],
                source_locale: locale.clone(),
                current_locale: locale,
                revision,
            });
        });
    }

    /// Atomically install every bundled Fluent catalog and select an explicit
    /// active locale. Lookup falls back from the active language to
    /// `source_locale`, then to the key itself.
    ///
    /// # Errors
    ///
    /// Returns a locale/resource/duplicate error without replacing the live
    /// catalog.
    pub fn install(
        source_locale: &str,
        active_locale: &str,
        catalogs: &[(&str, &str)],
    ) -> Result<(), I18nError> {
        let source_locale = parse_locale(source_locale)?;
        let active_locale = parse_locale(active_locale)?;
        let mut bundles = HashMap::new();
        let mut catalog_order = Vec::new();
        for (locale, source) in catalogs {
            let locale = parse_locale(locale)?;
            let bundle = make_bundle(&locale, source)?;
            if bundles.insert(locale.clone(), bundle).is_some() {
                return Err(I18nError::DuplicateLocale(locale.to_string()));
            }
            catalog_order.push(locale);
        }
        if !bundles.contains_key(&source_locale) {
            return Err(I18nError::MissingSource(source_locale.to_string()));
        }
        MANAGER.with(|slot| {
            let revision = slot
                .borrow()
                .as_ref()
                .map_or(1, |manager| manager.revision.wrapping_add(1));
            *slot.borrow_mut() = Some(Self {
                bundles,
                catalog_order,
                source_locale,
                current_locale: active_locale,
                revision,
            });
        });
        Ok(())
    }

    /// Install catalogs using the process/user locale (`LC_ALL`,
    /// `LC_MESSAGES`, `LANGUAGE`, then `LANG`) with source-locale fallback.
    ///
    /// # Errors
    ///
    /// Returns any error documented by [`Self::install`].
    pub fn install_system(source_locale: &str, catalogs: &[(&str, &str)]) -> Result<(), I18nError> {
        let active = preferred_locale()
            .map_or_else(|| source_locale.to_owned(), |locale| locale.to_string());
        Self::install(source_locale, &active, catalogs)
    }

    pub fn add_resource(locale: &str, ftl_source: String) {
        let lang_id: LanguageIdentifier = parse_locale_or_default(locale);
        // A malformed `.ftl` resource degrades to "not loaded" — `lookup`
        // already falls back to the raw key when no message is found,
        // which is a safer failure mode than panicking the whole app over
        // a translator's typo.
        let Ok(res) = FluentResource::try_new(ftl_source) else {
            return;
        };

        MANAGER.with(|m| {
            if let Some(mgr) = m.borrow_mut().as_mut() {
                if !mgr.bundles.contains_key(&lang_id) {
                    let mut b = FluentBundle::new(vec![lang_id.clone()]);
                    b.set_use_isolating(false);
                    mgr.bundles.insert(lang_id.clone(), b);
                    mgr.catalog_order.push(lang_id.clone());
                }
                // Same degrade: a message-id collision leaves the existing
                // message in place rather than crashing the caller.
                if mgr
                    .bundles
                    .get_mut(&lang_id)
                    .is_some_and(|bundle| bundle.add_resource(res).is_ok())
                {
                    mgr.revision = mgr.revision.wrapping_add(1);
                }
            }
        });
    }

    pub fn set_locale(locale: &str) {
        let lang_id = parse_locale(locale).ok();
        MANAGER.with(|m| {
            if let (Some(mgr), Some(lang_id)) = (m.borrow_mut().as_mut(), lang_id)
                && mgr.current_locale != lang_id
            {
                mgr.current_locale = lang_id;
                mgr.revision = mgr.revision.wrapping_add(1);
            }
        });
    }

    #[must_use]
    pub fn current_locale() -> String {
        MANAGER.with(|manager| {
            manager.borrow().as_ref().map_or_else(
                || "en-US".to_owned(),
                |manager| manager.current_locale.to_string(),
            )
        })
    }

    /// Monotonic retained-widget invalidation token. It changes only when a
    /// catalog or active locale changes.
    #[must_use]
    pub fn revision() -> u64 {
        let catalog = MANAGER.with(|manager| {
            manager
                .borrow()
                .as_ref()
                .map_or(0, |manager| manager.revision)
        });
        catalog.wrapping_add(PSEUDO_REVISION.with(Cell::get))
    }

    /// Enable or disable pseudo-localized output. The retained-widget revision
    /// changes on an effective transition so text is reshaped and layout can
    /// be recomputed without restarting the application.
    pub fn set_pseudo_locale(mode: PseudoLocale) {
        PSEUDO_LOCALE.with(|current| {
            if current.replace(mode) != mode {
                PSEUDO_REVISION.with(|revision| {
                    revision.set(revision.get().wrapping_add(1));
                });
            }
        });
    }

    #[must_use]
    pub fn pseudo_locale() -> PseudoLocale {
        PSEUDO_LOCALE.with(Cell::get)
    }

    pub fn lookup(key: &str, args: Option<&FluentArgs>) -> String {
        let resolved = MANAGER.with(|m| {
            if let Some(manager) = m.borrow().as_ref() {
                let active = manager.bundles.get(&manager.current_locale).or_else(|| {
                    let same_language = |locale: &&LanguageIdentifier| {
                        locale.language == manager.current_locale.language
                    };
                    manager
                        .catalog_order
                        .iter()
                        .filter(same_language)
                        .find(|locale| locale.region.is_none() && locale.script.is_none())
                        .or_else(|| manager.catalog_order.iter().find(same_language))
                        .and_then(|locale| manager.bundles.get(locale))
                });
                if let Some(value) = active.and_then(|bundle| format_from_bundle(bundle, key, args))
                {
                    return value;
                }
                if let Some(value) = manager
                    .bundles
                    .get(&manager.source_locale)
                    .and_then(|bundle| format_from_bundle(bundle, key, args))
                {
                    return value;
                }
            }
            TOOLKIT_SOURCE.with(|source| {
                source
                    .as_ref()
                    .ok()
                    .and_then(|bundle| format_from_bundle(bundle, key, args))
                    .unwrap_or_else(|| key.to_string())
            })
        });
        apply_pseudo_locale(resolved)
    }

    /// Resolve a message without variables. This function-shaped form is
    /// convenient in iterator adapters such as `Option::map`.
    #[must_use]
    pub fn lookup_plain(key: &str) -> String {
        Self::lookup(key, None)
    }

    /// Resolve `key`, interpolating named variables into the message's
    /// placeables (e.g. `counter = Count is { $count }` with `count=5` yields
    /// `Count is 5`).
    ///
    /// Degrades gracefully: a key with no loaded resource falls back to the key
    /// itself, and a placeable whose variable is missing is left as the
    /// formatter's best-effort rendering rather than panicking. Passing an empty
    /// slice is equivalent to a plain lookup.
    pub fn format(key: &str, args: &[(&str, ArgValue)]) -> String {
        if args.is_empty() {
            return Self::lookup(key, None);
        }
        let mut fluent_args = FluentArgs::new();
        for (name, value) in args {
            fluent_args.set(*name, value.to_fluent());
        }
        Self::lookup(key, Some(&fluent_args))
    }
}

#[macro_export]
macro_rules! i18n {
    ($key:expr) => {
        $crate::i18n::I18nManager::lookup($key, None)
    };
    ($key:expr, $($name:expr => $val:expr),*) => {
        {
            let mut args = fluent::FluentArgs::new();
            $(
                args.set($name, $val);
            )*
            $crate::i18n::I18nManager::lookup($key, Some(&args))
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lookup uses thread-local state; worker threads are reused across tests.
    struct RestoreLookupState {
        manager: Option<I18nManager>,
        pseudo: PseudoLocale,
        pseudo_revision: u64,
    }

    impl RestoreLookupState {
        fn capture() -> Self {
            Self {
                manager: MANAGER.with(|manager| manager.borrow_mut().take()),
                pseudo: PSEUDO_LOCALE.with(Cell::get),
                pseudo_revision: PSEUDO_REVISION.with(Cell::get),
            }
        }
    }

    impl Drop for RestoreLookupState {
        fn drop(&mut self) {
            MANAGER.with(|manager| *manager.borrow_mut() = self.manager.take());
            PSEUDO_LOCALE.with(|mode| mode.set(self.pseudo));
            PSEUDO_REVISION.with(|revision| revision.set(self.pseudo_revision));
        }
    }

    const EN: &str = "greeting = Hello\nonly-source = Source fallback";
    const DE: &str = "greeting = Guten Tag";

    #[test]
    fn shared_toolkit_history_is_readable_without_app_catalogs() {
        let _restore = RestoreLookupState::capture();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        TOOLKIT_SOURCE.with(|source| assert!(source.is_ok(), "bundled toolkit catalog must parse"));
        assert_eq!(
            I18nManager::lookup_plain("standalone-history-title"),
            "History"
        );
        assert_eq!(
            I18nManager::lookup_plain("standalone-history-close"),
            "Close history"
        );
    }

    #[test]
    fn app_locale_and_source_override_shared_toolkit_fallback() {
        let _restore = RestoreLookupState::capture();
        I18nManager::install(
            "en-US",
            "de-DE",
            &[
                (
                    "en-US",
                    "standalone-history-title = App history\nstandalone-history-close = App close",
                ),
                ("de", "standalone-history-title = Verlauf"),
            ],
        )
        .unwrap();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(
            I18nManager::lookup_plain("standalone-history-title"),
            "Verlauf"
        );
        assert_eq!(
            I18nManager::lookup_plain("standalone-history-close"),
            "App close"
        );
        assert_eq!(
            I18nManager::lookup_plain("standalone-history-selected-details"),
            "Selected changes"
        );
    }

    #[test]
    fn installed_catalog_uses_language_match_then_source_fallback() {
        I18nManager::install("en-US", "de-DE", &[("en-US", EN), ("de", DE)]).unwrap();
        assert_eq!(I18nManager::current_locale(), "de-DE");
        assert_eq!(I18nManager::lookup("greeting", None), "Guten Tag");
        assert_eq!(I18nManager::lookup("only-source", None), "Source fallback");
        assert_eq!(I18nManager::lookup("missing", None), "missing");
    }

    #[test]
    fn language_fallback_prefers_generic_then_registration_order() {
        I18nManager::install(
            "en-US",
            "de-AT",
            &[("en-US", EN), ("de-CH", "greeting = Grüezi"), ("de", DE)],
        )
        .unwrap();
        assert_eq!(I18nManager::lookup("greeting", None), "Guten Tag");

        I18nManager::install(
            "en-US",
            "de-AT",
            &[("en-US", EN), ("de-CH", "greeting = Grüezi"), ("de-DE", DE)],
        )
        .unwrap();
        assert_eq!(I18nManager::lookup("greeting", None), "Grüezi");
    }

    #[test]
    fn locale_revision_changes_only_for_effective_changes() {
        I18nManager::install("en-US", "en-US", &[("en-US", EN), ("de", DE)]).unwrap();
        let initial = I18nManager::revision();
        I18nManager::set_locale("en_US.UTF-8");
        assert_eq!(I18nManager::revision(), initial);
        I18nManager::set_locale("de_DE.UTF-8");
        assert_ne!(I18nManager::revision(), initial);
        assert_eq!(I18nManager::lookup("greeting", None), "Guten Tag");
    }

    #[test]
    fn invalid_install_does_not_replace_the_live_catalog() {
        I18nManager::install("en-US", "en-US", &[("en-US", EN)]).unwrap();
        let revision = I18nManager::revision();
        let error = I18nManager::install("en-US", "de", &[("en-US", "broken = {"), ("de", DE)])
            .unwrap_err();
        assert!(matches!(error, I18nError::InvalidResource { .. }));
        assert_eq!(I18nManager::revision(), revision);
        assert_eq!(I18nManager::lookup("greeting", None), "Hello");
    }

    #[test]
    fn expanded_pseudo_locale_lengthens_text_and_invalidates_retained_widgets() {
        I18nManager::install("en-US", "en-US", &[("en-US", EN)]).unwrap();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        let initial = I18nManager::revision();

        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        let expanded = I18nManager::lookup("greeting", None);
        assert!(expanded.starts_with('⟦') && expanded.ends_with('⟧'));
        assert!(expanded.chars().count() > "Hello".chars().count());
        assert_ne!(I18nManager::revision(), initial);

        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(I18nManager::lookup("greeting", None), "Hello");
    }
}
