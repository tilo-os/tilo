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
impl From<std::borrow::Cow<'_, str>> for ArgValue {
    fn from(s: std::borrow::Cow<'_, str>) -> Self {
        ArgValue::Text(s.into_owned())
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
    /// Every Fluent source added to the source locale, kept so a fallback
    /// can be formatted under the active locale's plural rules.
    source_texts: Vec<String>,
    /// The source catalogue rebuilt under the active locale, keyed by the
    /// revision, source count and locale it was built for.
    localized_source: std::cell::RefCell<Option<LocalizedSource>>,
    /// Distinguishes this manager from any installed before it on the thread.
    id: u64,
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

struct LocalizedSource {
    revision: u64,
    texts: usize,
    locale: LanguageIdentifier,
    bundle: FluentBundle<FluentResource>,
}

/// The pseudo-locale language subtag (`qps-ploc`, `qps-plocm`): selecting it
/// renders source copy through the [`PseudoLocale::Expanded`] transform.
const PSEUDO_LANGUAGE: &str = "qps";

thread_local! {
    static NEXT_MANAGER_ID: Cell<u64> = const { Cell::new(0) };
}

fn next_manager_id() -> u64 {
    NEXT_MANAGER_ID.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1));
        id
    })
}

/// The identity of this thread's manager, if one is installed.
fn manager_id() -> Option<u64> {
    MANAGER.with(|manager| manager.borrow().as_ref().map(|manager| manager.id))
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

/// Expand `value` unless pseudo-localisation is off and the active locale is
/// not a pseudo-locale. Interpolated string arguments are the app's data (a
/// file name, a person), not copy, so they pass through unexpanded.
fn apply_pseudo_locale(value: String, args: Option<&FluentArgs>, pseudo_language: bool) -> String {
    let mode = PSEUDO_LOCALE.with(Cell::get);
    if mode == PseudoLocale::Off && !pseudo_language {
        return value;
    }
    let data: Vec<&str> = args
        .into_iter()
        .flat_map(FluentArgs::iter)
        .filter_map(|(_, value)| match value {
            FluentValue::String(text) if !text.is_empty() => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    if data.is_empty() {
        return pseudo_expand(&value);
    }
    // Private-use markers survive the expansion untouched, so each argument
    // is swapped out, the copy expanded, and the argument restored.
    let marker = |index: usize| format!("\u{e000}{index}\u{e001}");
    let mut protected = value;
    for (index, text) in data.iter().enumerate() {
        protected = protected.replace(text, &marker(index));
    }
    let mut expanded = pseudo_expand(&protected);
    for (index, text) in data.iter().enumerate() {
        expanded = expanded.replace(&marker(index), text);
    }
    expanded
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

/// Whether a POSIX locale value names the `C`/`POSIX` locale (`C`,
/// `C.UTF-8`, `POSIX`), which means untranslated source copy.
#[cfg(any(feature = "native", test))]
fn is_c_locale(value: &str) -> bool {
    let first = value.split(':').next().unwrap_or(value).trim();
    let core = first.split(['.', '@']).next().unwrap_or(first);
    core.eq_ignore_ascii_case("C") || core.eq_ignore_ascii_case("POSIX")
}

/// The user's message locale from the variables `var` reads: `LC_ALL`,
/// `LC_MESSAGES`, `LANGUAGE`, then `LANG`. The first variable with a usable
/// value decides: an explicit `C`/`POSIX` there means source copy (`None`),
/// not "ignore this and consult the next variable". Unset, empty and
/// malformed values are skipped.
#[cfg(any(feature = "native", test))]
pub(crate) fn locale_from_vars(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    for name in ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"] {
        let Some(value) = var(name) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        if is_c_locale(&value) {
            return None;
        }
        if parse_locale(&value).is_ok() {
            return Some(value);
        }
    }
    None
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

/// Format `key` from `bundle`: `None` when the bundle has no such message,
/// otherwise the text and whether it formatted without errors (a missing
/// variable, an unknown reference).
fn format_from_bundle(
    bundle: &FluentBundle<FluentResource>,
    key: &str,
    args: Option<&FluentArgs>,
) -> Option<(String, bool)> {
    let message = bundle.get_message(key)?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    let value = bundle
        .format_pattern(pattern, args, &mut errors)
        .to_string();
    Some((value, errors.is_empty()))
}

/// Formats a message through a chain of bundles. The first bundle that
/// formats it cleanly wins; a bundle that formats it only with errors is
/// remembered and returned if no later bundle does better.
struct Resolution<'k, 'a> {
    key: &'k str,
    args: Option<&'a FluentArgs<'a>>,
    partial: Option<String>,
}

impl Resolution<'_, '_> {
    fn attempt(&mut self, bundle: &FluentBundle<FluentResource>) -> Option<String> {
        match format_from_bundle(bundle, self.key, self.args)? {
            (value, true) => Some(value),
            (value, false) => {
                self.partial.get_or_insert(value);
                None
            }
        }
    }
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
                source_texts: Vec::new(),
                localized_source: std::cell::RefCell::new(None),
                id: next_manager_id(),
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
        let source_texts = catalogs
            .iter()
            .filter(|(locale, _)| parse_locale(locale).is_ok_and(|locale| locale == source_locale))
            .map(|(_, source)| (*source).to_owned())
            .collect();
        MANAGER.with(|slot| {
            let revision = slot
                .borrow()
                .as_ref()
                .map_or(1, |manager| manager.revision.wrapping_add(1));
            *slot.borrow_mut() = Some(Self {
                bundles,
                catalog_order,
                source_locale,
                source_texts,
                localized_source: std::cell::RefCell::new(None),
                id: next_manager_id(),
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
        let Ok(res) = FluentResource::try_new(ftl_source.clone()) else {
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
                // A resource that collides with existing ids still adds its
                // other messages, so the source copy is recorded either way.
                if lang_id == mgr.source_locale && !mgr.source_texts.contains(&ftl_source) {
                    mgr.source_texts.push(ftl_source);
                }
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

    /// Select `locale`, or the source locale when `locale` is `C`, `POSIX`
    /// or does not parse — where [`Self::set_locale`] would keep the current
    /// selection.
    pub fn set_locale_or_source(locale: &str) {
        if parse_locale(locale).is_ok() {
            Self::set_locale(locale);
            return;
        }
        let source = MANAGER.with(|manager| {
            manager
                .borrow()
                .as_ref()
                .map(|manager| manager.source_locale.to_string())
        });
        if let Some(source) = source {
            Self::set_locale(&source);
        }
    }

    /// Whether a catalogue installed on this thread defines `key`. The shared
    /// toolkit fallback does not count, and pseudo-localisation plays no part.
    #[must_use]
    pub fn has_message(key: &str) -> bool {
        MANAGER.with(|manager| {
            manager.borrow().as_ref().is_some_and(|manager| {
                manager
                    .bundles
                    .values()
                    .any(|bundle| bundle.get_message(key).is_some_and(|m| m.value().is_some()))
            })
        })
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

    /// Resolve `key` in the active locale.
    ///
    /// Bundles are consulted in order: the active locale's own catalogue,
    /// then a language-only catalogue of the same language, then other
    /// regions of that language in registration order, then the source
    /// catalogue — formatted under the active locale's plural rules, so a
    /// count follows the user's grammar even in fallback copy (a language
    /// without CLDR rules formats with English ones, as before) —
    /// then the shared toolkit catalogue. The first bundle that formats the
    /// message without errors wins; if every candidate reports errors the
    /// first best-effort rendering is returned, and a key no bundle defines
    /// resolves to itself.
    pub fn lookup(key: &str, args: Option<&FluentArgs>) -> String {
        let mut resolution = Resolution {
            key,
            args,
            partial: None,
        };
        let (resolved, pseudo_language) = MANAGER.with(|m| {
            let manager = m.borrow();
            let Some(manager) = manager.as_ref() else {
                return (None, false);
            };
            let pseudo_language = manager.current_locale.language.as_str() == PSEUDO_LANGUAGE;
            (manager.resolve(&mut resolution), pseudo_language)
        });
        let resolved = resolved
            .or_else(|| {
                TOOLKIT_SOURCE.with(|source| {
                    source
                        .as_ref()
                        .ok()
                        .and_then(|bundle| resolution.attempt(bundle))
                })
            })
            .or(resolution.partial)
            .unwrap_or_else(|| key.to_string());
        apply_pseudo_locale(resolved, args, pseudo_language)
    }

    fn resolve(&self, resolution: &mut Resolution<'_, '_>) -> Option<String> {
        let current = &self.current_locale;
        let same_language = |locale: &&LanguageIdentifier| locale.language == current.language;
        let generic = self
            .catalog_order
            .iter()
            .filter(same_language)
            .find(|locale| locale.region.is_none() && locale.script.is_none());
        let chain = std::iter::once(current)
            .chain(generic)
            .chain(self.catalog_order.iter().filter(same_language));
        let mut tried: Vec<&LanguageIdentifier> = Vec::new();
        for locale in chain {
            if tried.contains(&locale) {
                continue;
            }
            tried.push(locale);
            if let Some(value) = self
                .bundles
                .get(locale)
                .and_then(|bundle| resolution.attempt(bundle))
            {
                return Some(value);
            }
        }
        if tried.contains(&&self.source_locale) {
            return None;
        }
        if *current != self.source_locale && !self.source_texts.is_empty() {
            let mut cached = self.localized_source.borrow_mut();
            let stale = cached.as_ref().is_none_or(|c| {
                c.revision != self.revision
                    || c.texts != self.source_texts.len()
                    || c.locale != *current
            });
            if stale {
                let mut bundle = FluentBundle::new(vec![current.clone()]);
                bundle.set_use_isolating(false);
                for text in &self.source_texts {
                    if let Ok(resource) = FluentResource::try_new(text.clone()) {
                        let _ = bundle.add_resource(resource);
                    }
                }
                *cached = Some(LocalizedSource {
                    revision: self.revision,
                    texts: self.source_texts.len(),
                    locale: current.clone(),
                    bundle,
                });
            }
            return cached
                .as_ref()
                .and_then(|localized| resolution.attempt(&localized.bundle));
        }
        self.bundles
            .get(&self.source_locale)
            .and_then(|bundle| resolution.attempt(bundle))
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

/// Which locale a thread's first catalogue selects when no manager exists yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreshLocale {
    /// The catalogue's source locale.
    Source,
    /// The user's locale from the environment, as [`I18nManager::install_system`].
    System,
}

/// An application's Fluent catalogues, embedded with `include_str!` and
/// loaded on first use on each thread.
///
/// Apps declare one `static` and route every string through it, so a
/// `--shots` render, a unit test and a live session resolve the same copy
/// with no build step or runtime file I/O:
///
/// ```ignore
/// static CATALOG: ui::i18n::EmbeddedCatalog = ui::i18n::EmbeddedCatalog::new(
///     "en-US",
///     &[("en-US", include_str!("../locales/en-US.ftl"))],
///     "chat-mention-agent-tag",
/// );
/// pub fn t(key: &str) -> String { CATALOG.t(key) }
/// ```
///
/// The manager is thread-local, so loading is per thread and idempotent. A
/// host (the shell or the app runner) may already have installed a manager
/// with the user's locale; that one is never replaced — the catalogues are
/// added to it. A fresh manager is installed only when the thread has none.
#[derive(Debug)]
pub struct EmbeddedCatalog {
    source_locale: &'static str,
    catalogs: &'static [(&'static str, &'static str)],
    probe_key: &'static str,
    fresh_locale: FreshLocale,
    pinned_locale: Option<&'static str>,
}

thread_local! {
    /// The embedded catalogues this thread has loaded, by probe key, with the
    /// manager they were loaded into. A manager installed since then starts
    /// without them, so they load again.
    static EMBEDDED_LOADED: std::cell::RefCell<Vec<(&'static str, u64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl EmbeddedCatalog {
    /// Catalogues keyed by locale, one of which is `source_locale`.
    /// `probe_key` is a message the source catalogue always defines; it tells
    /// a host that already holds these catalogues from one that has none.
    ///
    /// A thread with no manager gets one selecting `source_locale`.
    #[must_use]
    pub const fn new(
        source_locale: &'static str,
        catalogs: &'static [(&'static str, &'static str)],
        probe_key: &'static str,
    ) -> Self {
        Self {
            source_locale,
            catalogs,
            probe_key,
            fresh_locale: FreshLocale::Source,
            pinned_locale: None,
        }
    }

    /// A thread with no manager gets one selecting the user's locale from the
    /// environment (see [`I18nManager::install_system`]).
    #[must_use]
    pub const fn with_system_locale(mut self) -> Self {
        self.fresh_locale = FreshLocale::System;
        self
    }

    /// Select `locale` whenever this catalogue loads on a thread, even over a
    /// host's choice. For test builds that assert source copy on any
    /// developer or CI machine.
    #[must_use]
    pub const fn with_pinned_locale(mut self, locale: &'static str) -> Self {
        self.pinned_locale = Some(locale);
        self
    }

    /// Load the catalogues on this thread if they are not loaded yet.
    ///
    /// A key with no catalogue resolves to the key itself rather than
    /// panicking, so a missed call degrades to something legible — but it
    /// looks wrong, which is why string sites go through [`Self::t`].
    pub fn ensure(&self) {
        let manager = manager_id();
        let loaded = EMBEDDED_LOADED.with(|loaded| {
            loaded
                .borrow()
                .iter()
                .any(|&(probe, id)| probe == self.probe_key && Some(id) == manager)
        });
        if loaded {
            return;
        }
        if !self.add_to_live_manager() {
            self.install_fresh();
        }
        if let Some(locale) = self.pinned_locale {
            I18nManager::set_locale(locale);
        }
        if let Some(id) = manager_id() {
            EMBEDDED_LOADED.with(|loaded| {
                let mut loaded = loaded.borrow_mut();
                loaded.retain(|&(probe, _)| probe != self.probe_key);
                loaded.push((self.probe_key, id));
            });
        }
    }

    /// Add every catalogue to the manager already live on this thread.
    /// Returns `false` when there is no manager to add to.
    ///
    /// Adding a new resource bumps the catalogue revision, which is the cheap
    /// tell. When it does not move — no manager, or a host that pre-installed
    /// these very catalogues — the probe key decides: only a manager holds it.
    fn add_to_live_manager(&self) -> bool {
        let before = I18nManager::revision();
        for (locale, source) in self.catalogs {
            I18nManager::add_resource(locale, (*source).to_owned());
        }
        I18nManager::revision() != before || I18nManager::has_message(self.probe_key)
    }

    fn install_fresh(&self) {
        let installed = match self.fresh_locale {
            FreshLocale::Source => {
                I18nManager::install(self.source_locale, self.source_locale, self.catalogs)
            }
            FreshLocale::System => I18nManager::install_system(self.source_locale, self.catalogs),
        };
        if installed.is_err() {
            // A catalogue with a syntax error still loads every message that
            // parsed: `add_resource` degrades per resource instead of
            // rejecting the whole set.
            I18nManager::init(self.source_locale);
            for (locale, source) in self.catalogs {
                I18nManager::add_resource(locale, (*source).to_owned());
            }
        }
    }

    /// Resolve `key` in the active locale.
    #[must_use]
    pub fn t(&self, key: &str) -> String {
        self.ensure();
        I18nManager::lookup(key, None)
    }

    /// Resolve `key`, interpolating named variables.
    ///
    /// The values are the app's own words — a label, a participant's name —
    /// so the *sentence* around them stays in the catalogue and only the
    /// nouns are substituted.
    #[must_use]
    pub fn t_args(&self, key: &str, args: &[(&str, ArgValue)]) -> String {
        self.ensure();
        I18nManager::format(key, args)
    }

    /// Select `locale` (BCP-47 or POSIX, e.g. `de_DE.UTF-8`) on this thread.
    /// An unparseable value, `C` or `POSIX` selects the source locale.
    pub fn set_locale(&self, locale: &str) {
        self.ensure();
        I18nManager::set_locale_or_source(locale);
    }

    /// The active locale's BCP-47 tag on this thread.
    #[must_use]
    pub fn current_locale(&self) -> String {
        self.ensure();
        I18nManager::current_locale()
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

    #[test]
    fn posix_locale_values_parse_like_the_environment() {
        let parsed = |raw: &str| parse_locale(raw).ok().map(|locale| locale.to_string());
        assert_eq!(parsed("C"), None);
        assert_eq!(parsed("POSIX"), None);
        assert_eq!(parsed(""), None);
        assert_eq!(parsed("de_DE.UTF-8").as_deref(), Some("de-DE"));
        assert_eq!(parsed("en_US").as_deref(), Some("en-US"));
        // A `LANGUAGE` list: the first entry wins.
        assert_eq!(parsed("de:en_US").as_deref(), Some("de"));
    }

    #[test]
    fn an_explicit_c_locale_ends_the_environment_search() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        let found = |vars| {
            locale_from_vars(env(vars))
                .and_then(|value| parse_locale(&value).ok())
                .map(|l| l.to_string())
        };
        assert_eq!(
            found(&[("LC_ALL", "C.UTF-8"), ("LANG", "de_DE.UTF-8")]),
            None
        );
        assert_eq!(
            found(&[("LC_ALL", ""), ("LANG", "de_DE.UTF-8")]).as_deref(),
            Some("de-DE")
        );
        assert_eq!(
            found(&[("LC_MESSAGES", "not a locale!"), ("LANG", "fr_FR")]).as_deref(),
            Some("fr-FR"),
            "a malformed value is skipped"
        );
        assert_eq!(found(&[]), None);
    }

    #[test]
    fn every_catalogue_of_the_active_language_is_consulted_before_source() {
        let _restore = RestoreLookupState::capture();
        I18nManager::install(
            "en-US",
            "de-DE",
            &[
                ("en-US", "greeting = Hello\ntool = Tool\napp = App"),
                ("de-DE", "tool = Werkzeug"),
                ("de", "app = Anwendung"),
            ],
        )
        .unwrap();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(I18nManager::lookup_plain("tool"), "Werkzeug");
        assert_eq!(I18nManager::lookup_plain("app"), "Anwendung");
        assert_eq!(I18nManager::lookup_plain("greeting"), "Hello");
    }

    #[test]
    fn source_fallback_follows_the_active_locales_plural_rules() {
        let _restore = RestoreLookupState::capture();
        const SOURCE: &str =
            "items = { $count ->\n    [one] { $count } item\n   *[other] { $count } items\n}";
        I18nManager::install("en-US", "en-US", &[("en-US", SOURCE)]).unwrap();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        let items = || I18nManager::format("items", &[("count", ArgValue::from(21))]);
        assert_eq!(items(), "21 items");
        // Russian puts 21 in the `one` category; the English copy follows.
        I18nManager::set_locale("ru-RU");
        assert_eq!(items(), "21 item");
        // A language without CLDR rules formats with English ones.
        I18nManager::set_locale("xx-ZZ");
        assert_eq!(items(), "21 items");
    }

    #[test]
    fn a_translation_that_fails_to_format_falls_back_to_one_that_does() {
        let _restore = RestoreLookupState::capture();
        I18nManager::install(
            "en-US",
            "de",
            &[
                ("en-US", "share = Shared with { $person }"),
                ("de", "share = Geteilt mit { $persn }"),
            ],
        )
        .unwrap();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(
            I18nManager::format("share", &[("person", ArgValue::from("Zoë"))]),
            "Shared with Zoë"
        );
        // With no clean rendering anywhere, the best effort still shows.
        assert_eq!(I18nManager::format("share", &[]), "Geteilt mit {$persn}");
    }

    #[test]
    fn pseudo_locale_selection_expands_copy_but_not_interpolated_data() {
        let _restore = RestoreLookupState::capture();
        I18nManager::install(
            "en-US",
            "en-US",
            &[("en-US", "share = Shared with { $person }")],
        )
        .unwrap();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        let initial = I18nManager::revision();
        I18nManager::set_locale("qps-ploc");
        assert_ne!(I18nManager::revision(), initial);
        let rendered = I18nManager::format("share", &[("person", ArgValue::from("Zoë <a>"))]);
        assert!(
            rendered.starts_with('⟦') && rendered.ends_with('⟧'),
            "{rendered}"
        );
        assert!(rendered.contains("Zoë <a>"), "{rendered}");
        assert!(!rendered.contains("Shared"), "{rendered}");
    }

    #[test]
    fn set_locale_or_source_treats_c_and_garbage_as_source() {
        let _restore = RestoreLookupState::capture();
        I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
        I18nManager::set_locale_or_source("C");
        assert_eq!(I18nManager::current_locale(), "en-US");
        I18nManager::set_locale_or_source("de_DE.UTF-8");
        assert_eq!(I18nManager::current_locale(), "de-DE");
        I18nManager::set_locale_or_source("not a locale!");
        assert_eq!(I18nManager::current_locale(), "en-US");
        // `set_locale` itself keeps the selection on a value it cannot parse.
        I18nManager::set_locale("de");
        I18nManager::set_locale("C");
        assert_eq!(I18nManager::current_locale(), "de");
    }

    static EMBEDDED: EmbeddedCatalog = EmbeddedCatalog::new(
        "en-US",
        &[(
            "en-US",
            "embedded-probe = Probe\nembedded-greeting = Hello { $name }",
        )],
        "embedded-probe",
    );

    #[test]
    fn embedded_catalog_installs_a_manager_on_a_bare_thread() {
        let _restore = RestoreLookupState::capture();
        assert_eq!(manager_id(), None);
        assert_eq!(EMBEDDED.t("embedded-probe"), "Probe");
        assert_eq!(EMBEDDED.current_locale(), "en-US");
        assert_eq!(
            EMBEDDED.t_args("embedded-greeting", &[("name", ArgValue::from("Ada"))]),
            "Hello Ada"
        );
    }

    #[test]
    fn embedded_catalog_joins_a_host_manager_without_replacing_its_locale() {
        let _restore = RestoreLookupState::capture();
        I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
        let host = manager_id();
        assert_eq!(EMBEDDED.t("embedded-probe"), "Probe");
        assert_eq!(manager_id(), host);
        assert_eq!(I18nManager::current_locale(), "de");
        assert_eq!(I18nManager::lookup_plain("greeting"), "Guten Tag");

        // A manager installed later starts without the catalogue; the next
        // lookup loads it again rather than trusting a stale flag.
        I18nManager::install("en-US", "en-US", &[("en-US", EN)]).unwrap();
        assert_eq!(EMBEDDED.t("embedded-probe"), "Probe");
    }

    #[test]
    fn pinned_embedded_catalog_overrides_the_host_locale() {
        static PINNED: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "pinned-probe = Pinned"),
                ("de", "pinned-probe = Festgelegt"),
            ],
            "pinned-probe",
        )
        .with_pinned_locale("en-US");
        let _restore = RestoreLookupState::capture();
        I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
        assert_eq!(PINNED.t("pinned-probe"), "Pinned");
        assert_eq!(I18nManager::current_locale(), "en-US");
    }
}
