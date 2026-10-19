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

/// The Fluent message a label, placeholder or button shows: looked up in
/// the active locale when drawn, shown verbatim when no catalog has it.
///
/// `tilo-app check` fails on a literal key that the app's
/// `locales/en-US.ftl` lacks.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LabelKey(std::borrow::Cow<'static, str>);

impl LabelKey {
    /// The message `key`.
    #[must_use]
    pub const fn new(key: &'static str) -> Self {
        Self(std::borrow::Cow::Borrowed(key))
    }

    /// The message id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LabelKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for LabelKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&LabelKey> for LabelKey {
    fn from(key: &LabelKey) -> Self {
        key.clone()
    }
}

impl From<&str> for LabelKey {
    fn from(key: &str) -> Self {
        Self(std::borrow::Cow::Owned(key.to_owned()))
    }
}

impl From<String> for LabelKey {
    fn from(key: String) -> Self {
        Self(std::borrow::Cow::Owned(key))
    }
}

impl From<&String> for LabelKey {
    fn from(key: &String) -> Self {
        Self(std::borrow::Cow::Owned(key.clone()))
    }
}

impl From<LabelKey> for String {
    fn from(key: LabelKey) -> Self {
        key.0.into_owned()
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
    /// Messages resolved without variables, under the manager and revision
    /// they were resolved at ([`I18nManager::lookup`]).
    static PLAIN_LOOKUPS: std::cell::RefCell<PlainLookups> = std::cell::RefCell::default();
    // Shared controls remain readable even when an app installs only its own
    // catalog. App translations always take precedence over this source.
    static TOOLKIT_SOURCE: Result<FluentBundle<FluentResource>, I18nError> =
        make_bundle(&parse_locale_or_default("en-US"), include_str!("../locales/en-US.ftl"));
}

/// What [`I18nManager::lookup`] resolved without variables since the
/// catalogues, the locale, pseudo-localisation or the region formats last
/// changed: the same key resolves to the same text until one of them does.
/// A list publishing a hundred rows asked Fluent for the same dozen labels
/// on each of them.
#[derive(Default)]
struct PlainLookups {
    /// The manager and its [`I18nManager::revision`] they were resolved at.
    stamp: Option<(Option<u64>, u64)>,
    texts: HashMap<String, String>,
}

impl PlainLookups {
    /// At most this many texts are kept; past it they are resolved again.
    const MAX: usize = 4096;
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
    crate::host::parse_posix_locale(locale)
        .ok_or_else(|| I18nError::InvalidLocale(locale.to_owned()))
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

/// The toolkit's own widget labels, in every language it ships, joined to a
/// host's manager on first use.
static WIDGET_CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[
        ("en-US", include_str!("../locales/en-US.ftl")),
        ("de-DE", include_str!("../locales/de-DE.ftl")),
    ],
)
.with_system_locale();

/// Resolve a toolkit message in the host's locale.
///
/// A thread with a manager gets the toolkit's translations added to it once;
/// a thread without one resolves the source copy, as every toolkit string
/// does, and no manager is installed behind the host's back.
pub(crate) fn widget_format(key: &str, args: &[(&str, ArgValue)]) -> String {
    if manager_id().is_some() {
        WIDGET_CATALOG.ensure();
    }
    I18nManager::format(key, args)
}

impl I18nManager {
    /// Register shared widget labels in every bundled language without
    /// replacing an application's catalogs or its selected language. A host
    /// (the shell, an app) calls this once per thread so every toolkit widget
    /// it draws follows the desktop's language; calling it again is free.
    pub fn register_widget_messages() {
        WIDGET_CATALOG.ensure();
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
    /// catalog, the active locale or the person's region formats
    /// ([`crate::format::region`]) change, so text that shows dates and
    /// numbers re-renders with the language.
    #[must_use]
    pub fn revision() -> u64 {
        let catalog = MANAGER.with(|manager| {
            manager
                .borrow()
                .as_ref()
                .map_or(0, |manager| manager.revision)
        });
        catalog
            .wrapping_add(PSEUDO_REVISION.with(Cell::get))
            .wrapping_add(crate::format::region::revision())
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
        if args.is_some() {
            return Self::resolve_lookup(key, args);
        }
        let stamp = (manager_id(), Self::revision());
        let kept = PLAIN_LOOKUPS.with(|kept| {
            let kept = kept.borrow();
            (kept.stamp == Some(stamp))
                .then(|| kept.texts.get(key).cloned())
                .flatten()
        });
        if let Some(text) = kept {
            return text;
        }
        let text = Self::resolve_lookup(key, None);
        PLAIN_LOOKUPS.with(|kept| {
            let mut kept = kept.borrow_mut();
            if kept.stamp != Some(stamp) || kept.texts.len() >= PlainLookups::MAX {
                kept.stamp = Some(stamp);
                kept.texts.clear();
            }
            kept.texts.insert(key.to_owned(), text.clone());
        });
        text
    }

    /// [`Self::lookup`], resolved by Fluent.
    fn resolve_lookup(key: &str, args: Option<&FluentArgs>) -> String {
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
    fresh_locale: FreshLocale,
    pinned_locale: Option<&'static str>,
    pack: Option<LanguagePack>,
}

/// Where a catalogue's other languages come from: the language packs.
///
/// An app's package carries only its source catalogue, the fallback every
/// message resolves to. Each other language is a shared language pack,
/// installed for the languages the person uses and the same bytes for
/// everyone: `<root>/<locale>/<id>.ftl` for every catalogue that has that
/// language, one directory per locale. The roots are [`LANGUAGE_PACK_ROOTS`],
/// which the sandbox lets every app read, and, for a build run from its
/// source tree (a developer's, a test's, a screenshot's), the crate's own
/// `locales/` directory.
#[derive(Debug, Clone, Copy)]
pub struct LanguagePack {
    /// The catalogue's name in a pack: its crate's package name.
    pub id: &'static str,
    /// Every locale the catalogue is translated into, its source among them.
    pub locales: &'static [&'static str],
    /// The crate's `locales/` directory where it was built, read when it
    /// exists (`<dir>/<locale>.ftl`).
    pub source_dir: &'static str,
}

/// Where installed language packs are, in the order they are read: the ones
/// the person's languages fetched, then the ones the OS image carries.
pub const LANGUAGE_PACK_ROOTS: &[&str] = &[
    "/var/lib/tilo/language-packs",
    "/usr/share/tilo/language-packs",
];

impl LanguagePack {
    /// The Fluent source of `locale`'s catalogue, from the first root that
    /// has it.
    #[must_use]
    pub fn read(&self, locale: &str) -> Option<String> {
        let file = format!("{}.ftl", self.id);
        LANGUAGE_PACK_ROOTS
            .iter()
            .map(|root| std::path::Path::new(root).join(locale).join(&file))
            .chain(std::iter::once(
                std::path::Path::new(self.source_dir).join(format!("{locale}.ftl")),
            ))
            .find_map(|path| std::fs::read_to_string(path).ok())
    }
}

thread_local! {
    /// The embedded catalogues this thread has loaded, by the address of
    /// their sources, with the manager they were loaded into. A manager
    /// installed since then starts without them, so they load again.
    static EMBEDDED_LOADED: std::cell::RefCell<Vec<(usize, u64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl EmbeddedCatalog {
    /// Catalogues keyed by locale, one of which is `source_locale`.
    ///
    /// A thread with no manager gets one selecting `source_locale`.
    #[must_use]
    pub const fn new(
        source_locale: &'static str,
        catalogs: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            source_locale,
            catalogs,
            fresh_locale: FreshLocale::Source,
            pinned_locale: None,
            pack: None,
        }
    }

    /// Read the catalogue's other languages from the language packs
    /// ([`LanguagePack`]) when it loads.
    #[must_use]
    pub const fn with_language_pack(mut self, pack: LanguagePack) -> Self {
        self.pack = Some(pack);
        self
    }

    /// Every language's Fluent source, by tag: the embedded ones and those
    /// the installed language packs hold. For content that must be read in
    /// every language at once, such as the words search matches whatever
    /// language the desktop speaks.
    #[must_use]
    pub fn every_source(&self) -> Vec<(&'static str, std::borrow::Cow<'static, str>)> {
        let mut sources: Vec<(&'static str, std::borrow::Cow<'static, str>)> = self
            .catalogs
            .iter()
            .map(|(locale, source)| (*locale, std::borrow::Cow::Borrowed(*source)))
            .collect();
        if let Some(pack) = &self.pack {
            for locale in pack.locales {
                if sources.iter().any(|(held, _)| held == locale) {
                    continue;
                }
                if let Some(source) = pack.read(locale) {
                    sources.push((*locale, std::borrow::Cow::Owned(source)));
                }
            }
        }
        sources
    }

    /// The Fluent source of the source locale's catalogue.
    #[must_use]
    pub fn source(&self) -> Option<&'static str> {
        self.catalogs
            .iter()
            .find(|(locale, _)| *locale == self.source_locale)
            .map(|(_, source)| *source)
    }

    /// The embedded Fluent sources, by tag; [`Self::every_source`] adds the
    /// languages of the installed language packs.
    #[must_use]
    pub const fn sources(&self) -> &'static [(&'static str, &'static str)] {
        self.catalogs
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
        let key = self.catalogs.as_ptr().addr();
        let loaded = EMBEDDED_LOADED.with(|loaded| {
            loaded
                .borrow()
                .iter()
                .any(|&(loaded, id)| loaded == key && Some(id) == manager)
        });
        if loaded {
            return;
        }
        let sources = self.every_source();
        if manager.is_some() {
            for (locale, source) in &sources {
                I18nManager::add_resource(locale, source.clone().into_owned());
            }
        } else {
            self.install_fresh(&sources);
        }
        if let Some(locale) = self.pinned_locale {
            I18nManager::set_locale(locale);
        }
        if let Some(id) = manager_id() {
            EMBEDDED_LOADED.with(|loaded| {
                let mut loaded = loaded.borrow_mut();
                loaded.retain(|&(loaded, _)| loaded != key);
                loaded.push((key, id));
            });
        }
    }

    fn install_fresh(&self, sources: &[(&'static str, std::borrow::Cow<'static, str>)]) {
        let catalogs: Vec<(&str, &str)> = sources
            .iter()
            .map(|(locale, source)| (*locale, source.as_ref()))
            .collect();
        let installed = match self.fresh_locale {
            FreshLocale::Source => {
                I18nManager::install(self.source_locale, self.source_locale, &catalogs)
            }
            FreshLocale::System => I18nManager::install_system(self.source_locale, &catalogs),
        };
        if installed.is_err() {
            // A catalogue with a syntax error still loads every message that
            // parsed: `add_resource` degrades per resource instead of
            // rejecting the whole set.
            I18nManager::init(self.source_locale);
            for (locale, source) in sources {
                I18nManager::add_resource(locale, source.clone().into_owned());
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

/// Define the calling crate's [`EmbeddedCatalog`]: `locales/en-US.ftl` in the
/// crate embedded as the source, the crate's other `locales/<locale>.ftl`
/// read from the language packs ([`LanguagePack`], named by the crate's
/// package name), the user's locale when no host chose one, and source copy
/// pinned in the crate's own tests.
///
/// `embedded_catalog!(pub(crate) static STORE_CATALOG, ["en-US", "de-DE"])`
/// defines just the static; `embedded_catalog!(pub fn, ["en-US"])`
/// defines `CATALOG` and the `t` / `t_args` lookups the crate's copy
/// goes through.
#[macro_export]
macro_rules! embedded_catalog {
    ($vis:vis static $name:ident, [$($locale:literal),+ $(,)?]) => {
        $vis static $name: $crate::i18n::EmbeddedCatalog = {
            // Only the source catalogue is embedded; the other languages
            // come from the language packs.
            let catalog = $crate::i18n::EmbeddedCatalog::new(
                "en-US",
                &[(
                    "en-US",
                    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/locales/en-US.ftl")),
                )],
            )
            .with_language_pack($crate::i18n::LanguagePack {
                id: env!("CARGO_PKG_NAME"),
                locales: &[$($locale),+],
                source_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/locales"),
            })
            .with_system_locale();
            #[cfg(test)]
            let catalog = catalog.with_pinned_locale("en-US");
            catalog
        };
    };
    ($vis:vis fn, [$($locale:literal),+ $(,)?]) => {
        $crate::embedded_catalog!($vis static CATALOG, [$($locale),+]);

        #[allow(dead_code, reason = "a crate may resolve only plain or only interpolated copy")]
        $vis fn t(key: &str) -> String {
            CATALOG.t(key)
        }

        #[allow(dead_code, reason = "a crate may resolve only plain or only interpolated copy")]
        $vis fn t_args(key: &str, args: &[(&str, $crate::i18n::ArgValue)]) -> String {
            CATALOG.t_args(key, args)
        }
    };
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

    /// A catalogue embeds only its source; its other languages come from the
    /// language packs, here the crate's own `locales/` directory.
    #[test]
    fn a_catalogue_reads_its_other_languages_from_the_language_packs() {
        static PACKED: EmbeddedCatalog =
            EmbeddedCatalog::new("en-US", &[("en-US", "packed-greeting = Hello\n")])
                .with_language_pack(LanguagePack {
                    id: "tilo-ui-pack-test",
                    locales: &["en-US", "de-DE"],
                    source_dir: concat!(env!("CARGO_MANIFEST_DIR"), "/locales"),
                });
        let sources = PACKED.every_source();
        assert_eq!(sources.first().map(|(locale, _)| *locale), Some("en-US"));
        let german = sources
            .iter()
            .find(|(locale, _)| *locale == "de-DE")
            .expect("the German catalogue is read from the crate's locales");
        assert_eq!(
            german.1,
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/locales/de-DE.ftl"))
                .unwrap()
        );
        let missing = LanguagePack {
            id: "tilo-ui-pack-test",
            locales: &["xx-XX"],
            source_dir: "/nonexistent",
        };
        assert_eq!(missing.read("xx-XX"), None);
    }

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

    /// Installs `catalogs` with pseudo-localisation off.
    fn install_plain(source: &str, locale: &str, catalogs: &[(&str, &str)]) {
        I18nManager::install(source, locale, catalogs).unwrap();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
    }

    /// A plain lookup is resolved once per catalogue, locale and
    /// pseudo-localisation, and resolved again as soon as any of them changes.
    #[test]
    fn a_kept_plain_lookup_follows_catalogue_locale_and_pseudo_locale() {
        let _restore = RestoreLookupState::capture();
        install_plain(
            "en-US",
            "en-US",
            &[
                ("en-US", "kept-probe = One\n"),
                ("de-DE", "kept-probe = Eins\n"),
            ],
        );
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "One");
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "One");
        I18nManager::set_locale("de-DE");
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "Eins");
        I18nManager::add_resource("de-DE", "kept-other = Zwei\n".to_owned());
        assert_eq!(I18nManager::lookup_plain("kept-other"), "Zwei");
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        assert_ne!(I18nManager::lookup_plain("kept-probe"), "Eins");
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "Eins");
        // Another manager on the thread starts from its own catalogues.
        install_plain("en-US", "en-US", &[("en-US", "kept-probe = Other\n")]);
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "Other");
    }

    #[test]
    fn lookup_falls_back_through_language_catalogues_then_source() {
        let _restore = RestoreLookupState::capture();
        // The shared toolkit history is readable without app catalogs.
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

        type Catalogs = &'static [(&'static str, &'static str)];
        let cases: &[(&str, &str, Catalogs, Catalogs)] = &[
            (
                "app locale and source override the shared toolkit fallback",
                "de-DE",
                &[
                    (
                        "en-US",
                        "standalone-history-title = App history\nstandalone-history-close = App close",
                    ),
                    ("de", "standalone-history-title = Verlauf"),
                ],
                &[
                    ("standalone-history-title", "Verlauf"),
                    ("standalone-history-close", "App close"),
                    ("standalone-history-selected-details", "Selected changes"),
                ],
            ),
            (
                "language match, then source fallback, then the key itself",
                "de-DE",
                &[("en-US", EN), ("de", DE)],
                &[
                    ("greeting", "Guten Tag"),
                    ("only-source", "Source fallback"),
                    ("missing", "missing"),
                ],
            ),
            (
                "the generic language wins over another region",
                "de-AT",
                &[("en-US", EN), ("de-CH", "greeting = Grüezi"), ("de", DE)],
                &[("greeting", "Guten Tag")],
            ),
            (
                "without a generic language, registration order decides",
                "de-AT",
                &[("en-US", EN), ("de-CH", "greeting = Grüezi"), ("de-DE", DE)],
                &[("greeting", "Grüezi")],
            ),
            (
                "every catalogue of the active language is consulted before source",
                "de-DE",
                &[
                    ("en-US", "greeting = Hello\ntool = Tool\napp = App"),
                    ("de-DE", "tool = Werkzeug"),
                    ("de", "app = Anwendung"),
                ],
                &[
                    ("tool", "Werkzeug"),
                    ("app", "Anwendung"),
                    ("greeting", "Hello"),
                ],
            ),
        ];
        for (case, locale, catalogs, expected) in cases {
            install_plain("en-US", locale, catalogs);
            assert_eq!(I18nManager::current_locale(), *locale, "case {case}");
            for (key, want) in *expected {
                assert_eq!(
                    I18nManager::lookup_plain(key),
                    *want,
                    "case {case}: key {key}"
                );
            }
        }
    }

    #[test]
    fn locale_selection_parses_posix_values_and_revises_only_on_change() {
        let _restore = RestoreLookupState::capture();
        // POSIX values parse like the environment; a `LANGUAGE` list's first
        // entry wins.
        let parsed = |raw: &str| parse_locale(raw).ok().map(|locale| locale.to_string());
        for (raw, want) in [
            ("C", None),
            ("POSIX", None),
            ("", None),
            ("de_DE.UTF-8", Some("de-DE")),
            ("en_US", Some("en-US")),
            ("de:en_US", Some("de")),
        ] {
            assert_eq!(parsed(raw).as_deref(), want, "parse {raw:?}");
        }

        // The revision changes only for an effective locale change.
        I18nManager::install("en-US", "en-US", &[("en-US", EN), ("de", DE)]).unwrap();
        let initial = I18nManager::revision();
        I18nManager::set_locale("en_US.UTF-8");
        assert_eq!(I18nManager::revision(), initial);
        I18nManager::set_locale("de_DE.UTF-8");
        assert_ne!(I18nManager::revision(), initial);
        assert_eq!(I18nManager::lookup("greeting", None), "Guten Tag");

        // `set_locale_or_source` treats C and garbage as the source locale.
        I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
        for (raw, want) in [
            ("C", "en-US"),
            ("de_DE.UTF-8", "de-DE"),
            ("not a locale!", "en-US"),
        ] {
            I18nManager::set_locale_or_source(raw);
            assert_eq!(
                I18nManager::current_locale(),
                want,
                "set_locale_or_source({raw:?})"
            );
        }
        // `set_locale` itself keeps the selection on a value it cannot parse.
        I18nManager::set_locale("de");
        I18nManager::set_locale("C");
        assert_eq!(I18nManager::current_locale(), "de");
    }

    #[test]
    fn broken_resources_and_messages_degrade_to_one_that_works() {
        let _restore = RestoreLookupState::capture();
        // An invalid install does not replace the live catalog.
        I18nManager::install("en-US", "en-US", &[("en-US", EN)]).unwrap();
        let revision = I18nManager::revision();
        let error = I18nManager::install("en-US", "de", &[("en-US", "broken = {"), ("de", DE)])
            .unwrap_err();
        assert!(matches!(error, I18nError::InvalidResource { .. }));
        assert_eq!(I18nManager::revision(), revision);
        assert_eq!(I18nManager::lookup("greeting", None), "Hello");

        // A translation that fails to format falls back to one that does.
        install_plain(
            "en-US",
            "de",
            &[
                ("en-US", "share = Shared with { $person }"),
                ("de", "share = Geteilt mit { $persn }"),
            ],
        );
        assert_eq!(
            I18nManager::format("share", &[("person", ArgValue::from("Zoë"))]),
            "Shared with Zoë"
        );
        // With no clean rendering anywhere, the best effort still shows.
        assert_eq!(I18nManager::format("share", &[]), "Geteilt mit {$persn}");

        // The source fallback follows the active locale's plural rules.
        const SOURCE: &str =
            "items = { $count ->\n    [one] { $count } item\n   *[other] { $count } items\n}";
        install_plain("en-US", "en-US", &[("en-US", SOURCE)]);
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
    fn pseudo_locales_expand_copy_but_not_data_and_invalidate_widgets() {
        let _restore = RestoreLookupState::capture();
        // Expanded pseudo-localisation lengthens text and bumps the revision
        // so retained widgets re-resolve.
        install_plain("en-US", "en-US", &[("en-US", EN)]);
        let initial = I18nManager::revision();
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        let expanded = I18nManager::lookup("greeting", None);
        assert!(expanded.starts_with('⟦') && expanded.ends_with('⟧'));
        assert!(expanded.chars().count() > "Hello".chars().count());
        assert_ne!(I18nManager::revision(), initial);
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(I18nManager::lookup("greeting", None), "Hello");

        // Selecting the pseudo locale expands copy but not interpolated data.
        install_plain(
            "en-US",
            "en-US",
            &[("en-US", "share = Shared with { $person }")],
        );
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

    static EMBEDDED: EmbeddedCatalog = EmbeddedCatalog::new(
        "en-US",
        &[(
            "en-US",
            "embedded-probe = Probe\nembedded-greeting = Hello { $name }",
        )],
    );

    #[test]
    fn embedded_catalogs_install_join_or_pin_their_manager() {
        static PINNED: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "pinned-probe = Pinned"),
                ("de", "pinned-probe = Festgelegt"),
            ],
        )
        .with_pinned_locale("en-US");

        // On a bare thread the embedded catalog installs its own manager.
        {
            let _restore = RestoreLookupState::capture();
            assert_eq!(manager_id(), None);
            assert_eq!(EMBEDDED.t("embedded-probe"), "Probe");
            assert_eq!(EMBEDDED.current_locale(), "en-US");
            assert_eq!(
                EMBEDDED.t_args("embedded-greeting", &[("name", ArgValue::from("Ada"))]),
                "Hello Ada"
            );
        }

        // It joins a host manager without replacing its locale.
        {
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

        // A pinned catalog overrides the host locale.
        {
            let _restore = RestoreLookupState::capture();
            I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
            assert_eq!(PINNED.t("pinned-probe"), "Pinned");
            assert_eq!(I18nManager::current_locale(), "en-US");
        }
    }

    #[test]
    fn relative_times_follow_the_host_locale_and_its_plural_rules() {
        use crate::format::relative;
        use crate::history_source::participant_display_label;
        use crate::time::{Duration, SystemTime};
        use crate::widget::format_relative_time;
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ago = |secs| format_relative_time(base, base + Duration::from_secs(secs));

        // With no manager the toolkit's source copy is used and none is installed.
        {
            let _restore = RestoreLookupState::capture();
            assert_eq!(ago(10), "just now");
            assert_eq!(ago(120), "2 min ago");
            assert_eq!(manager_id(), None);
        }

        // A German host gets German copy, with the count choosing the form.
        {
            let _restore = RestoreLookupState::capture();
            install_plain("en-US", "de-DE", &[("en-US", EN), ("de-DE", DE)]);
            assert_eq!(ago(10), "gerade eben");
            assert_eq!(ago(120), "vor 2 Min.");
            assert_eq!(ago(2 * 3600 + 5), "vor 2 Std.");
            assert_eq!(ago(86_400), "gestern");
            assert_eq!(ago(259_200), "vor 3 Tagen");

            assert_eq!(relative(0), "gerade eben");
            assert_eq!(relative(90_000), "gestern");
            assert_eq!(relative(3 * 86_400), "vor 3 Tagen");
            assert_eq!(relative(8 * 86_400), "vor 1 Woche");
            assert_eq!(relative(90 * 86_400), "vor 3 Monaten");
            assert_eq!(relative(400 * 86_400), "vor 1 Jahr");

            assert_eq!(
                participant_display_label("agent:"),
                "Unbekannter Agent · Agent"
            );
            assert_eq!(participant_display_label("Quinten"), "Quinten");
        }
    }

    /// A toolkit key only English defines would show English on a German
    /// desktop; every relative-time message is translated with the same
    /// variables.
    #[test]
    fn relative_time_messages_are_translated() {
        const EN_FTL: &str = include_str!("../locales/en-US.ftl");
        const DE_FTL: &str = include_str!("../locales/de-DE.ftl");
        let en = make_bundle(&parse_locale_or_default("en-US"), EN_FTL).unwrap();
        let de = make_bundle(&parse_locale_or_default("de-DE"), DE_FTL).unwrap();
        for key in [
            "relative-ago-just-now",
            "relative-ago-minutes",
            "relative-ago-hours",
            "relative-ago-yesterday",
            "relative-ago-days",
            "relative-ago-weeks",
            "relative-ago-months",
            "relative-ago-years",
            "participant-agent",
            "participant-unknown-agent",
            "participant-shortcut",
            "participant-app",
            "participant-acting-for",
        ] {
            let mut args = FluentArgs::new();
            args.set("count", FluentValue::from(2));
            args.set("name", FluentValue::from("scribe"));
            args.set("person", FluentValue::from("Ada"));
            for (locale, bundle) in [("en-US", &en), ("de-DE", &de)] {
                let (value, clean) = format_from_bundle(bundle, key, Some(&args))
                    .unwrap_or_else(|| panic!("{locale}.ftl lacks {key}"));
                assert!(clean, "{locale}.ftl formats {key} with errors: {value}");
            }
        }
    }
}
