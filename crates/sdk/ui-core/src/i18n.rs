use fluent::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;
use unic_langid::LanguageIdentifier;

/// An owned value to interpolate into a localized message.
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

impl From<&tilo_resource::label::Argument> for ArgValue {
    fn from(argument: &tilo_resource::label::Argument) -> Self {
        match argument {
            tilo_resource::label::Argument::Text(text) => ArgValue::Text(text.clone()),
            tilo_resource::label::Argument::Number(number) => ArgValue::Number(*number),
        }
    }
}

/// The Fluent message a label, placeholder or button shows: looked up in the active locale when drawn,
/// shown verbatim when no catalog has it.
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

macro_rules! arg_from_number_lossless {
    ($($t:ty),*) => {$(
        impl From<$t> for ArgValue {
            fn from(n: $t) -> Self { ArgValue::Number(f64::from(n)) }
        }
    )*};
}
arg_from_number_lossless!(i8, i16, i32, u8, u16, u32, f32);

/// Narrowing conversions into `f64`, for display/interpolation only: a fluent message shows a
/// *formatted* number, not an exact one, so losing precision above 2^52 — a magnitude no UI counter or
/// index realistically reaches — is a deliberate, harmless approximation.
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

/// A bundle of catalogues whose parsed resources are shared with every thread that loaded the same text
/// ([`shared_resource`]).
type Bundle = FluentBundle<Arc<FluentResource>>;

pub struct I18nManager {
    /// Every catalogue added on this thread, by locale, the locales in the order they were first added.
    catalogs: Vec<(LanguageIdentifier, Vec<CatalogText>)>,
    /// The parsed catalogues of the active language and the source locale.
    bundles: HashMap<LanguageIdentifier, LoadedBundle>,
    /// The languages a window catalogue added on this thread is in: the languages this window speaks
    /// (see [`Self::speaking_locale`]).
    spoken: Vec<LanguageIdentifier>,
    source_locale: LanguageIdentifier,
    /// The source catalogue rebuilt under the active locale, keyed by the revision, source count and
    /// locale it was built for.
    localized_source: std::cell::RefCell<Option<LocalizedSource>>,
    id: u64,
    pub current_locale: LanguageIdentifier,
    revision: u64,
}

/// Where one catalogue's Fluent text comes from. Text is parsed only while its language is shown, so a
/// catalogue in a language nobody on the desktop reads costs its embedded bytes and nothing else.
#[derive(Clone)]
enum CatalogText {
    Embedded(&'static str),
    Pack(LanguagePack, &'static str),
    Parsed(Arc<FluentResource>),
}

impl CatalogText {
    fn is(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Embedded(a), Self::Embedded(b)) => std::ptr::eq(*a, *b) || a == b,
            (Self::Pack(a, la), Self::Pack(b, lb)) => a.id == b.id && la == lb,
            (Self::Parsed(a), Self::Parsed(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    fn is_widget_label(&self) -> bool {
        matches!(self, Self::Embedded(text) if WIDGET_CATALOG
            .catalogs
            .iter()
            .any(|(_, source)| std::ptr::eq(*text, *source)))
    }

    /// The parsed resource: `None` for text that does not parse (a translator's typo degrades to "not
    /// loaded") or a pack no longer installed.
    fn resource(&self) -> Option<Arc<FluentResource>> {
        match self {
            Self::Embedded(text) => shared_resource(std::borrow::Cow::Borrowed(text)).ok(),
            Self::Pack(pack, locale) => {
                shared_resource(std::borrow::Cow::Owned(pack.read(locale)?)).ok()
            }
            Self::Parsed(resource) => Some(Arc::clone(resource)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Window,
    /// Words every window shows: they ride along in its language and never choose it.
    Shared,
}

struct LoadedBundle {
    bundle: Bundle,
    /// The resources added to `bundle`, in order.
    resources: Vec<Arc<FluentResource>>,
    read: usize,
}

impl LoadedBundle {
    fn new(locale: &LanguageIdentifier) -> Self {
        Self {
            bundle: new_bundle(locale),
            resources: Vec::new(),
            read: 0,
        }
    }
}

/// Every parsed catalogue in the process, by a hash of its text.
static PARSED: std::sync::Mutex<Vec<(u64, std::sync::Weak<FluentResource>)>> =
    std::sync::Mutex::new(Vec::new());

fn shared_resource(text: std::borrow::Cow<'_, str>) -> Result<Arc<FluentResource>, String> {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();
    let mut parsed = PARSED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    parsed.retain(|(_, resource)| resource.strong_count() > 0);
    if let Some(resource) = parsed
        .iter()
        .filter(|(held, _)| *held == hash)
        .filter_map(|(_, resource)| resource.upgrade())
        .find(|resource| resource.source() == text.as_ref())
    {
        return Ok(resource);
    }
    let resource =
        FluentResource::try_new(text.into_owned()).map_err(|(_, errors)| format!("{errors:?}"))?;
    let resource = Arc::new(resource);
    parsed.push((hash, Arc::downgrade(&resource)));
    Ok(resource)
}

/// Layout-stress translation mode used by screenshots and acceptance tests.
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
    /// Messages resolved without variables, under the manager and revision they were resolved at
    /// ([`I18nManager::lookup`]).
    static PLAIN_LOOKUPS: std::cell::RefCell<PlainLookups> = std::cell::RefCell::default();
    /// The words resolved from a catalogue in another language than the window's, `(key, locale)`, each
    /// key once ([`I18nManager::foreign_words`]).
    static FOREIGN_WORDS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    // Shared controls remain readable even when an app installs only its own
    // catalog. App translations always take precedence over this source.
    static TOOLKIT_SOURCE: Result<Bundle, I18nError> =
        make_bundle(&parse_locale_or_default("en-US"), include_str!("../locales/en-US.ftl"));
}

/// What [`I18nManager::lookup`] resolved without variables since the catalogues, the locale,
/// pseudo-localisation or the region formats last changed: the same key resolves to the same text until
/// one of them does.
#[derive(Default)]
struct PlainLookups {
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
    bundle: Bundle,
}

/// The pseudo-locale language subtag (`qps-ploc`, `qps-plocm`): selecting it renders source copy
/// through the [`PseudoLocale::Expanded`] transform.
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

fn manager_id() -> Option<u64> {
    MANAGER.with(|manager| manager.borrow().as_ref().map(|manager| manager.id))
}

/// The markers a pseudo-localised string is wrapped in. They are what makes the copy findable in a
/// frame at a glance, so exactly one pair may surround any string.
const PSEUDO_OPEN: char = '⟦';
const PSEUDO_CLOSE: char = '⟧';

fn pseudo_expand(value: &str) -> String {
    // Copy reaches the screen localised more than once in places: a section
    // header composed from two already-localised fragments, a title set from a
    // string that was translated on its way in. Expanding that again renders
    // `⟦⟦Wíí-Fíí⟧⟧`, and the review then counts a doubling that is an artefact
    // of the transform rather than a string that is too long. Expansion is
    // therefore idempotent: one pair of markers, wherever the copy came from.
    if value.starts_with(PSEUDO_OPEN) && value.ends_with(PSEUDO_CLOSE) && value.len() > 2 {
        return value.to_owned();
    }
    let mut expanded = String::with_capacity(value.len().saturating_mul(2).saturating_add(4));
    expanded.push(PSEUDO_OPEN);
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
    expanded.push(PSEUDO_CLOSE);
    expanded
}

/// Expand `value` unless pseudo-localisation is off and the active locale is not a pseudo-locale.
fn apply_pseudo_locale(value: String, args: Option<&FluentArgs>, pseudo_language: bool) -> String {
    let mode = PSEUDO_LOCALE.with(Cell::get);
    if mode == PseudoLocale::Off && !pseudo_language {
        return value;
    }
    // Nothing says nothing: a bound text with no words yet — a save status
    // that has nothing to report — must not become a pair of markers, which
    // is how a review finds copy that is too long rather than copy that is
    // absent.
    if value.is_empty() {
        return value;
    }
    // Copy composed from copy already localised — a date heading, "⟦Tue⟧,
    // ⟦Jun⟧ 30", a section header of two translated fragments — carries its
    // marks inside it. Wrapping it again draws a doubled pair, which a
    // review reads as a defect in the string rather than in the transform.
    if value.contains(PSEUDO_OPEN) || value.contains(PSEUDO_CLOSE) {
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

/// Parse a BCP-47 locale tag, falling back to `en-US` on a malformed value (e.g. `en-US` is a
/// compile-time-known-valid literal, so the final `unwrap_or_default` (BCP-47 "und", undefined) is a
/// belt and braces path that's never actually reached.
fn parse_locale_or_default(locale: &str) -> LanguageIdentifier {
    parse_locale(locale).unwrap_or_else(|_| "en-US".parse().unwrap_or_default())
}

fn parse_locale(locale: &str) -> Result<LanguageIdentifier, I18nError> {
    crate::host::parse_posix_locale(locale)
        .ok_or_else(|| I18nError::InvalidLocale(locale.to_owned()))
}

/// The language this desktop speaks, as a BCP 47 tag: the one the person chose in Settings, or else the
/// locale the device runs in ([`settings_store::appearance::preferred_language`]).
#[must_use]
pub fn preferred_language() -> Option<String> {
    settings_store::appearance::preferred_language()
}

fn preferred_locale() -> Option<LanguageIdentifier> {
    preferred_language().and_then(|language| parse_locale(&language).ok())
}

/// Select the language the person chose on this thread, when it is not the one selected: a choice made
/// in Settings while the app runs relabels it (the [`I18nManager::revision`] moves).
pub fn follow_configured_language() {
    match preferred_locale() {
        Some(locale) => {
            let loaded = MANAGER.with(|manager| {
                manager
                    .borrow()
                    .as_ref()
                    .is_none_or(|manager| manager.speaks(&locale))
            });
            if !loaded {
                refresh_language_packs();
            }
            I18nManager::set_locale(&locale.to_string());
        }
        None => I18nManager::set_locale_or_source("C"),
    }
}

/// The language a window speaks, chosen once from the person's preference and the languages what the
/// window shows ships (`shipped`, the locales of its catalogues): the language they prefer when it is
/// among them, the same language in another region when that is, the source language when it ships
/// that, else the first it ships.
#[must_use]
pub fn window_language(shipped: &[&str]) -> String {
    let preference = preferred_locale().map(|locale| locale.to_string());
    tilo_history::window_language(preference.as_deref(), shipped)
}

/// Install a package's catalogues (`(locale, Fluent text)`) and make this window speak one language:
/// the person's when the package ships it, the package's source language otherwise.
pub fn speak_window_language(catalogues: &[(String, String)]) -> String {
    // The toolkit's own labels first: on a thread with no catalogue yet they install it, in
    // the desktop's language, so the window's language is chosen over a catalogue that exists
    // rather than one built after it and left speaking the desktop's.
    I18nManager::register_widget_messages();
    for (locale, text) in catalogues {
        I18nManager::add_resource(locale, text.clone());
    }
    let shipped: Vec<&str> = catalogues
        .iter()
        .map(|(locale, _)| locale.as_str())
        .collect();
    let language = window_language(&shipped);
    I18nManager::set_locale(&language);
    language
}

fn new_bundle(locale: &LanguageIdentifier) -> Bundle {
    let mut bundle = FluentBundle::new(vec![locale.clone()]);
    // Don't wrap interpolated variables in Unicode bidi isolation marks
    // (FSI/PDI) — we render to a single run, so "Count is { $n }" should
    // read "Count is 5", not "Count is \u{2068}5\u{2069}".
    bundle.set_use_isolating(false);
    bundle
}

fn make_bundle(locale: &LanguageIdentifier, source: &str) -> Result<Bundle, I18nError> {
    let invalid = |details: String| I18nError::InvalidResource {
        locale: locale.to_string(),
        details,
    };
    let resource = shared_resource(std::borrow::Cow::Borrowed(source)).map_err(invalid)?;
    let mut bundle = new_bundle(locale);
    bundle
        .add_resource(resource)
        .map_err(|errors| invalid(format!("{errors:?}")))?;
    Ok(bundle)
}

/// Log, once per message and thread, that `key` formatted only with errors in every catalogue: its
/// caller left a variable out, or the message references one that does not exist.
fn report_unformatted(key: &str) {
    thread_local! {
        static REPORTED: std::cell::RefCell<std::collections::HashSet<String>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }
    if REPORTED.with(|reported| reported.borrow_mut().insert(key.to_owned())) {
        tracing::error!(
            message_key = key,
            "a message was shown with a placeable unfilled: its caller passed no value for a variable it names"
        );
    }
}

/// Note that `key` showed in `locale`, when that is a language other than the window's (`speaking`):
/// one window speaks one language, and every other is a word it should not say
/// ([`I18nManager::foreign_words`]).
fn record_foreign(key: &str, locale: &LanguageIdentifier, speaking: &LanguageIdentifier) {
    if locale.language == speaking.language {
        return;
    }
    FOREIGN_WORDS.with(|seen| {
        let mut seen = seen.borrow_mut();
        if !seen.iter().any(|(held, _)| held == key) {
            seen.push((key.to_owned(), locale.to_string()));
        }
    });
}

/// Formats a message through a chain of bundles.
struct Resolution<'k, 'a> {
    key: &'k str,
    args: Option<&'a FluentArgs<'a>>,
    partial: Option<String>,
}

impl Resolution<'_, '_> {
    fn attempt(&mut self, bundle: &Bundle) -> Option<String> {
        match tilo_history::format_message(bundle, self.key, self.args)? {
            (value, true) => Some(value),
            (value, false) => {
                self.partial.get_or_insert(value);
                None
            }
        }
    }
}

/// The toolkit's own widget labels, in every language it ships, part of every manager from its start
/// ([`I18nManager::replace`]).
static WIDGET_CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[
        ("en-US", include_str!("../locales/en-US.ftl")),
        ("de-DE", include_str!("../locales/de-DE.ftl")),
    ],
)
.with_system_locale()
.as_shared();

/// Resolve a toolkit message in the host's locale.
pub(crate) fn widget_format(key: &str, args: &[(&str, ArgValue)]) -> String {
    if manager_id().is_some() {
        WIDGET_CATALOG.ensure();
    }
    I18nManager::format(key, args)
}

/// Resolve one of the toolkit's own messages in the host's locale.
#[must_use]
pub fn widget_text(key: &str) -> String {
    widget_text_with(key, None)
}

/// [`widget_text`] with the message's arguments.
#[must_use]
pub fn widget_text_with(key: &str, args: Option<&fluent::FluentArgs<'_>>) -> String {
    if manager_id().is_some() {
        WIDGET_CATALOG.ensure();
    }
    I18nManager::lookup(key, args)
}

/// What a window says about an edit an app declined ([`tilo_actions::Declined`]) in the language
/// this thread reads, for a window that draws it itself.
#[must_use]
pub fn declined_text(reason: tilo_actions::Declined) -> String {
    let (key, args) = reason.words();
    if args.is_empty() {
        return widget_text(&key);
    }
    let mut fluent = FluentArgs::new();
    for (name, value) in &args {
        fluent.set(name.as_str(), value.as_str());
    }
    widget_text_with(&key, Some(&fluent))
}

/// The name a control is announced by: `name` taken as text when the caller resolved it (`resolved`,
/// the row's caption an app localized itself), else as the message id [`widget_text`] resolves.
#[must_use]
pub fn spoken_name(name: &str, resolved: bool) -> Option<String> {
    let said = if resolved {
        name.to_owned()
    } else {
        widget_text(name)
    };
    (!said.is_empty()).then_some(said)
}

/// A resource's name in the language this thread reads: the catalogue entry for its kind
/// ([`tilo_resource::label::message_key`]) when one is loaded, and nothing when none is, so the
/// resource keeps the name it declares.
fn resource_label(
    catalogue: Option<&'static tilo_resource::label::Catalogue>,
    key: &str,
    arguments: &[(&str, tilo_resource::label::Argument)],
) -> Option<String> {
    if let Some(catalogue) = catalogue {
        declared_catalogue(catalogue).ensure();
    }
    manager_id()?;
    let arguments: Vec<(&str, ArgValue)> = arguments
        .iter()
        .map(|(name, value)| (*name, ArgValue::from(value)))
        .collect();
    let said = I18nManager::format(key, &arguments);
    (said != key).then_some(said)
}

/// The catalogue a declaration names its words in, loaded as any crate's embedded one: its words ride
/// along in the window's language ([`EmbeddedCatalog::as_shared`]), its other languages read from the
/// language packs.
fn declared_catalogue(
    catalogue: &'static tilo_resource::label::Catalogue,
) -> &'static EmbeddedCatalog {
    static LOADED: std::sync::Mutex<Vec<(&'static str, &'static EmbeddedCatalog)>> =
        std::sync::Mutex::new(Vec::new());
    let mut loaded = LOADED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, held)) = loaded.iter().find(|(id, _)| *id == catalogue.id) {
        return held;
    }
    let sources: &'static [(&'static str, &'static str)] =
        Box::leak(Box::new([("en-US", catalogue.source)]));
    let held: &'static EmbeddedCatalog = Box::leak(Box::new(
        EmbeddedCatalog::new("en-US", sources)
            .with_language_pack(LanguagePack {
                id: catalogue.id,
                locales: catalogue.locales,
                source_dir: catalogue.dir,
            })
            .with_system_locale()
            .as_shared(),
    ));
    loaded.push((catalogue.id, held));
    held
}

/// [`resource_label`] of a kind's noun, formatted with the count `1` (one of it) or `2` (several): the
/// catalogue's Fluent selector picks the singular or the plural in the reader's language.
fn resource_noun(key: &str, count: u32) -> Option<String> {
    manager_id()?;
    let said = I18nManager::format(key, &[("count", ArgValue::from(count))]);
    (said != key).then_some(said)
}

/// What one of the collection `key` names is, in this window's language, with its article ("an issue",
/// "eine Aufgabe"): the noun the app's catalogue writes for it ([`tilo_resource::label::noun_key`]).
#[must_use]
pub fn noun(key: &str) -> Option<String> {
    resource_noun(key, 1)
}

impl I18nManager {
    /// Register shared widget labels in every bundled language without replacing an application's
    /// catalogs or its selected language.
    pub fn register_widget_messages() {
        WIDGET_CATALOG.ensure();
    }

    pub fn init(locale: &str) {
        let locale = parse_locale_or_default(locale);
        Self::replace(
            locale.clone(),
            locale.clone(),
            vec![(locale.clone(), Vec::new())],
            vec![locale],
        );
    }

    /// Install a manager with `catalogs`, selecting `active`, in place of the thread's current one.
    fn replace(
        source_locale: LanguageIdentifier,
        active: LanguageIdentifier,
        catalogs: Vec<(LanguageIdentifier, Vec<CatalogText>)>,
        spoken: Vec<LanguageIdentifier>,
    ) {
        MANAGER.with(|slot| {
            let revision = slot
                .borrow()
                .as_ref()
                .map_or(1, |manager| manager.revision.wrapping_add(1));
            let mut manager = Self {
                catalogs,
                bundles: HashMap::new(),
                spoken,
                source_locale,
                localized_source: std::cell::RefCell::new(None),
                id: next_manager_id(),
                current_locale: active,
                revision,
            };
            manager.parse_shown();
            *slot.borrow_mut() = Some(manager);
        });
        WIDGET_CATALOG.ensure();
        // From here on every resource this thread publishes is named in the
        // language it just selected, and named again when that moves; so are
        // the nouns search names it by.
        tilo_resource::label::install(resource_label, Self::revision);
        tilo_resource::label::install_noun(resource_noun);
    }

    /// Whether the thread holds words of a window — a package's, or chrome a host draws around it —
    /// rather than only the toolkit's own labels, which say nothing about its language.
    fn has_window_words(&self) -> bool {
        self.catalogs
            .iter()
            .any(|(_, texts)| texts.iter().any(|text| !text.is_widget_label()))
    }

    /// Whether this window speaks `locale`'s language: a catalogue of its own is in it (see
    /// [`Self::speaking_locale`]).
    fn speaks(&self, locale: &LanguageIdentifier) -> bool {
        self.spoken
            .iter()
            .any(|spoken| spoken.language == locale.language)
    }

    /// Parse the catalogues of the locales shown that are not parsed yet, and drop those of locales no
    /// longer shown.
    fn parse_shown(&mut self) {
        let Self {
            catalogs,
            bundles,
            source_locale,
            current_locale,
            ..
        } = self;
        let shows = |locale: &LanguageIdentifier| {
            locale.language == current_locale.language || locale == source_locale
        };
        bundles.retain(|locale, _| shows(locale));
        for (locale, texts) in catalogs.iter() {
            if !shows(locale) {
                continue;
            }
            let loaded = bundles
                .entry(locale.clone())
                .or_insert_with(|| LoadedBundle::new(locale));
            for text in texts.iter().skip(loaded.read) {
                if let Some(resource) = text.resource() {
                    let _ = loaded.bundle.add_resource(Arc::clone(&resource));
                    loaded.resources.push(resource);
                }
            }
            loaded.read = texts.len();
        }
    }

    /// Add `text` to `locale`'s catalogues, parsing it if the locale is shown. A [`Role::Window`]
    /// catalogue makes `locale`'s language one this window speaks ([`Self::speaking_locale`]); a
    /// [`Role::Shared`] one rides along in it and never does.
    fn add_catalog(&mut self, locale: LanguageIdentifier, text: CatalogText, role: Role) {
        if role == Role::Window
            && !self
                .spoken
                .iter()
                .any(|spoken| spoken.language == locale.language)
        {
            self.spoken.push(locale.clone());
        }
        match self.catalogs.iter_mut().find(|(held, _)| *held == locale) {
            Some((_, texts)) if texts.iter().any(|held| held.is(&text)) => return,
            Some((_, texts)) => {
                let at = if text.is_widget_label() {
                    texts.len()
                } else {
                    texts
                        .iter()
                        .position(CatalogText::is_widget_label)
                        .unwrap_or(texts.len())
                };
                texts.insert(at, text);
                if self
                    .bundles
                    .get(&locale)
                    .is_some_and(|loaded| at < loaded.read)
                {
                    self.bundles.remove(&locale);
                }
            }
            None => self.catalogs.push((locale, vec![text])),
        }
        self.parse_shown();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Atomically install every bundled Fluent catalog and select an explicit active locale.
    pub fn install(
        source_locale: &str,
        active_locale: &str,
        catalogs: &[(&str, &str)],
    ) -> Result<(), I18nError> {
        let source_locale = parse_locale(source_locale)?;
        let active_locale = parse_locale(active_locale)?;
        let mut parsed: Vec<(LanguageIdentifier, Vec<CatalogText>)> = Vec::new();
        for (locale, source) in catalogs {
            let locale = parse_locale(locale)?;
            if parsed.iter().any(|(held, _)| *held == locale) {
                return Err(I18nError::DuplicateLocale(locale.to_string()));
            }
            let invalid = |details: String| I18nError::InvalidResource {
                locale: locale.to_string(),
                details,
            };
            let resource = shared_resource(std::borrow::Cow::Borrowed(source)).map_err(invalid)?;
            // Built only to report messages that collide.
            new_bundle(&locale)
                .add_resource(Arc::clone(&resource))
                .map_err(|errors| invalid(format!("{errors:?}")))?;
            parsed.push((locale, vec![CatalogText::Parsed(resource)]));
        }
        if !parsed.iter().any(|(locale, _)| *locale == source_locale) {
            return Err(I18nError::MissingSource(source_locale.to_string()));
        }
        let spoken: Vec<LanguageIdentifier> =
            parsed.iter().map(|(locale, _)| locale.clone()).collect();
        Self::replace(source_locale, active_locale, parsed, spoken);
        Ok(())
    }

    /// Install catalogs in the interface language ([`preferred_language`]: the person's choice in
    /// Settings, else the system locale) with source-locale fallback.
    pub fn install_system(source_locale: &str, catalogs: &[(&str, &str)]) -> Result<(), I18nError> {
        let active = preferred_locale()
            .map_or_else(|| source_locale.to_owned(), |locale| locale.to_string());
        Self::install(source_locale, &active, catalogs)
    }

    /// The words of the window this process draws, in `locale`: a language it is written in becomes one
    /// the window speaks.
    pub fn add_resource(locale: &str, ftl_source: String) {
        Self::add_resource_as(locale, ftl_source, Role::Window);
    }

    /// The words of another window's package, one this process reads or runs for someone else: they
    /// ride along like the toolkit's labels and say nothing about the language this process draws in.
    pub fn add_shared_resource(locale: &str, ftl_source: String) {
        Self::add_resource_as(locale, ftl_source, Role::Shared);
    }

    fn add_resource_as(locale: &str, ftl_source: String, role: Role) {
        let locale = parse_locale_or_default(locale);
        // A malformed `.ftl` resource degrades to "not loaded" — `lookup`
        // already falls back to the raw key when no message is found,
        // which is a safer failure mode than panicking the whole app over
        // a translator's typo.
        let Ok(resource) = shared_resource(std::borrow::Cow::Owned(ftl_source)) else {
            return;
        };
        MANAGER.with(|m| {
            if let Some(manager) = m.borrow_mut().as_mut() {
                manager.add_catalog(locale, CatalogText::Parsed(resource), role);
            }
        });
    }

    pub fn set_locale(locale: &str) {
        let lang_id = parse_locale(locale).ok();
        MANAGER.with(|m| {
            if let (Some(mgr), Some(lang_id)) = (m.borrow_mut().as_mut(), lang_id)
                && mgr.current_locale != lang_id
            {
                mgr.current_locale = lang_id.clone();
                mgr.parse_shown();
                mgr.revision = mgr.revision.wrapping_add(1);
            }
        });
    }

    /// Select `locale`, or the source locale when `locale` is `C`, `POSIX` or does not parse — where
    /// [`Self::set_locale`] would keep the current selection.
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

    /// Whether a catalogue installed on this thread, in the active language or the source, defines
    /// `key`.
    #[must_use]
    pub fn has_message(key: &str) -> bool {
        MANAGER.with(|manager| {
            manager.borrow().as_ref().is_some_and(|manager| {
                manager.bundles.values().any(|loaded| {
                    let bundle = &loaded.bundle;
                    bundle.get_message(key).is_some_and(|m| m.value().is_some())
                        || key.rsplit_once('.').is_some_and(|(name, attribute)| {
                            bundle
                                .get_message(name)
                                .is_some_and(|m| m.get_attribute(attribute).is_some())
                        })
                })
            })
        })
    }

    /// The locale the window selected: the person's choice when Settings or the device named one,
    /// whatever the window's own catalogues carry.
    #[must_use]
    pub fn current_locale() -> String {
        MANAGER.with(|manager| {
            manager.borrow().as_ref().map_or_else(
                || "en-US".to_owned(),
                |manager| manager.current_locale.to_string(),
            )
        })
    }

    /// The words this thread resolved from a catalogue in a language other than the window's
    /// ([`Self::window_locale`]), `(message key, catalogue locale)` in the order they were first shown,
    /// and each key once.
    #[must_use]
    pub fn foreign_words() -> Vec<(String, String)> {
        FOREIGN_WORDS.with(|seen| seen.borrow().clone())
    }

    /// Forget the words [`Self::foreign_words`] holds, for a check that starts a new window.
    pub fn clear_foreign_words() {
        FOREIGN_WORDS.with(|seen| seen.borrow_mut().clear());
    }

    /// The language this thread's window resolves in: the selected locale when the window's own
    /// catalogues are in it, the source language otherwise (the rule of
    /// [`I18nManager::speaking_locale`]).
    #[must_use]
    pub fn window_locale() -> String {
        MANAGER.with(|manager| {
            manager.borrow().as_ref().map_or_else(
                || "en-US".to_owned(),
                |manager| manager.speaking_locale().to_string(),
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

    /// Enable or disable pseudo-localized output.
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

    /// Resolve `key` in the window's language ([`Self::speaking_locale`]: the selected locale when its
    /// catalogues are in it, the source language otherwise).
    pub(crate) fn lookup(key: &str, args: Option<&FluentArgs>) -> String {
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
            .or_else(|| {
                // Every catalogue formats it only with errors: a variable
                // the caller did not pass, or a reference to no message.
                // Shown as best it can be, and said once per message.
                let partial = resolution.partial.take()?;
                report_unformatted(key);
                Some(partial)
            })
            .unwrap_or_else(|| key.to_string());
        apply_pseudo_locale(resolved, args, pseudo_language)
    }

    /// The language this window speaks: the selected one when the window's own words are in it, else
    /// the source.
    fn speaking_locale(&self) -> &LanguageIdentifier {
        if self.spoken.is_empty() && !self.has_window_words() {
            return &self.current_locale;
        }
        if self.speaks(&self.current_locale) {
            &self.current_locale
        } else {
            &self.source_locale
        }
    }

    fn resolve(&self, resolution: &mut Resolution<'_, '_>) -> Option<String> {
        let current = self.speaking_locale();
        let same_language = |locale: &&LanguageIdentifier| locale.language == current.language;
        let order = || self.catalogs.iter().map(|(locale, _)| locale);
        let generic = order()
            .filter(same_language)
            .find(|locale| locale.region.is_none() && locale.script.is_none());
        let chain = std::iter::once(current)
            .chain(generic)
            .chain(order().filter(same_language));
        let mut tried: Vec<&LanguageIdentifier> = Vec::new();
        for locale in chain {
            if tried.contains(&locale) {
                continue;
            }
            tried.push(locale);
            if let Some(value) = self
                .bundles
                .get(locale)
                .and_then(|loaded| resolution.attempt(&loaded.bundle))
            {
                record_foreign(resolution.key, locale, current);
                return Some(value);
            }
        }
        if tried.contains(&&self.source_locale) {
            return None;
        }
        let source = self.bundles.get(&self.source_locale)?;
        if *current != self.source_locale && !source.resources.is_empty() {
            let mut cached = self.localized_source.borrow_mut();
            let stale = cached.as_ref().is_none_or(|c| {
                c.revision != self.revision
                    || c.texts != source.resources.len()
                    || c.locale != *current
            });
            if stale {
                let mut bundle = new_bundle(current);
                for resource in &source.resources {
                    let _ = bundle.add_resource(Arc::clone(resource));
                }
                *cached = Some(LocalizedSource {
                    revision: self.revision,
                    texts: source.resources.len(),
                    locale: current.clone(),
                    bundle,
                });
            }
            let said = cached
                .as_ref()
                .and_then(|localized| resolution.attempt(&localized.bundle));
            if said.is_some() {
                record_foreign(resolution.key, &self.source_locale, current);
            }
            return said;
        }
        let said = resolution.attempt(&source.bundle);
        if said.is_some() {
            record_foreign(resolution.key, &self.source_locale, current);
        }
        said
    }

    /// Resolve a message without variables.
    #[must_use]
    pub(crate) fn lookup_plain(key: &str) -> String {
        Self::lookup(key, None)
    }

    /// Resolve `key`, interpolating named variables into the message's placeables (e.g.
    #[must_use]
    pub fn lookup_complete(key: &str, args: Option<&FluentArgs>) -> Option<String> {
        let mut resolution = Resolution {
            key,
            args,
            partial: None,
        };
        let (resolved, pseudo_language) = MANAGER.with(|m| {
            let manager = m.borrow();
            match manager.as_ref() {
                Some(manager) => {
                    let pseudo_language =
                        manager.current_locale.language.as_str() == PSEUDO_LANGUAGE;
                    (manager.resolve(&mut resolution), pseudo_language)
                }
                None => (None, false),
            }
        });
        let resolved = resolved.or_else(|| {
            TOOLKIT_SOURCE.with(|source| {
                source
                    .as_ref()
                    .ok()
                    .and_then(|bundle| resolution.attempt(bundle))
            })
        })?;
        Some(apply_pseudo_locale(resolved, args, pseudo_language))
    }

    #[must_use]
    pub(crate) fn format(key: &str, args: &[(&str, ArgValue)]) -> String {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreshLocale {
    Source,
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
    shared: bool,
}

/// Where a catalogue's other languages come from: the language packs. Each other language is a shared
/// language pack, installed for the languages the person uses and the same bytes for everyone:
/// `<root>/<locale>/<id>.ftl` for every catalogue that has that language, one directory per locale.
#[derive(Debug, Clone, Copy)]
pub struct LanguagePack {
    /// The catalogue's name in a pack: its crate's package name.
    pub id: &'static str,
    /// Every locale the catalogue is translated into, its source among them.
    pub locales: &'static [&'static str],
    /// The crate's `locales/` directory where it was built, read when it exists (`<dir>/<locale>.ftl`).
    pub source_dir: &'static str,
}

/// Where a sandboxed app sees the person's language packs: the launcher binds them here, read-only.
pub const SANDBOX_LANGUAGE_PACKS: &str = "/var/lib/tilo/language-packs";

/// Where the OS image's language packs are: the language it was installed in.
pub const IMAGE_LANGUAGE_PACKS: &str = "/usr/share/tilo/language-packs";

/// Where installed language packs are, in the order they are read: the ones the person installed
/// ([`tilo_paths::language_packs_dir`], or where a sandbox sees them, [`SANDBOX_LANGUAGE_PACKS`]), then
/// the image's.
#[must_use]
pub fn language_pack_roots() -> Vec<std::path::PathBuf> {
    tilo_paths::language_packs_dir()
        .into_iter()
        .chain([SANDBOX_LANGUAGE_PACKS, IMAGE_LANGUAGE_PACKS].map(std::path::PathBuf::from))
        .collect()
}

/// Moves when a language pack is installed while the process runs: every catalogue reads its languages
/// again the next time it is used.
static PACKS_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Read the installed language packs again: one installed since the catalogues loaded (the language a
/// person just chose) is read the next time each catalogue is used.
pub fn refresh_language_packs() {
    PACKS_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn interned(locale: &str) -> &'static str {
    static LOCALES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut locales = LOCALES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(known) = locales.iter().find(|known| **known == locale) {
        return known;
    }
    let leaked: &'static str = Box::leak(locale.to_owned().into_boxed_str());
    locales.push(leaked);
    leaked
}

impl LanguagePack {
    /// Where `locale`'s catalogue would be, in the order they are read.
    fn candidates(&self, locale: &str) -> impl Iterator<Item = std::path::PathBuf> {
        let file = format!("{}.ftl", self.id);
        language_pack_roots()
            .into_iter()
            .map(move |root| root.join(locale).join(&file))
            .chain(std::iter::once(
                std::path::Path::new(self.source_dir).join(format!("{locale}.ftl")),
            ))
    }

    /// The Fluent source of `locale`'s catalogue, from the first root that has it.
    #[must_use]
    pub fn read(&self, locale: &str) -> Option<String> {
        self.candidates(locale).find_map(|path| {
            let bytes =
                tilo_files::native::bounded::read(&path, MAX_LANGUAGE_PACK_BYTES).ok()??;
            String::from_utf8(bytes).ok()
        })
    }

    /// Whether `locale`'s catalogue is installed: in a pack, or beside the crate where it was built.
    #[must_use]
    pub fn has(&self, locale: &str) -> bool {
        self.candidates(locale).any(|path| path.is_file())
    }

    /// Every locale this catalogue has installed: the ones it was built with and every pack installed
    /// since that carries it (a language the registry offers later reads as any other).
    #[must_use]
    pub fn installed_locales(&self) -> Vec<&'static str> {
        let file = format!("{}.ftl", self.id);
        let mut locales: Vec<&'static str> = self
            .locales
            .iter()
            .copied()
            .filter(|locale| self.has(locale))
            .collect();
        for root in language_pack_roots() {
            let Ok(names) = tilo_files::native::entry_names(&root) else {
                continue;
            };
            for name in names {
                let Some(name) = name.to_str() else {
                    continue;
                };
                if parse_locale(name).is_ok()
                    && !locales.contains(&name)
                    && root.join(name).join(&file).is_file()
                {
                    locales.push(interned(name));
                }
            }
        }
        locales
    }
}

/// The language `tag` names, as its language pack is named ([`tilo_paths::language_of`]): `de` of
/// `de-DE`.
pub use tilo_paths::language_of;

/// `tag`'s language in its own words, as a language picker offers it: a person looking for their
/// language reads it in that language, once, whatever region the tag adds.
#[must_use]
pub fn language_name(tag: &str) -> String {
    match language_of(tag).as_str() {
        "en" => "English".to_owned(),
        "de" => "Deutsch".to_owned(),
        _ => tag.to_owned(),
    }
}

const MAX_LANGUAGE_PACK_BYTES: u64 = 4 << 20;

thread_local! {
    /// The embedded catalogues this thread has loaded, by the address of their sources, with the
    /// manager they were loaded into.
    static EMBEDDED_LOADED: std::cell::RefCell<Vec<(usize, u64, u64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl EmbeddedCatalog {
    /// Catalogues keyed by locale, one of which is `source_locale`.
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
            shared: false,
        }
    }

    /// The words ride along in the window's language instead of deciding it: a catalogue every window
    /// shows (the toolkit's widget labels, the Settings index) is written in languages the window
    /// itself may not speak ([`I18nManager::speaking_locale`]).
    #[must_use]
    pub const fn as_shared(mut self) -> Self {
        self.shared = true;
        self
    }

    /// Read the catalogue's other languages from the language packs ([`LanguagePack`]) when it loads.
    #[must_use]
    pub const fn with_language_pack(mut self, pack: LanguagePack) -> Self {
        self.pack = Some(pack);
        self
    }

    /// Every language's Fluent source, by tag: the embedded ones and those the installed language packs
    /// hold. For content that must be read in every language at once, such as the words search matches
    /// whatever language the desktop speaks.
    #[must_use]
    pub fn every_source(&self) -> Vec<(&'static str, std::borrow::Cow<'static, str>)> {
        let mut sources: Vec<(&'static str, std::borrow::Cow<'static, str>)> = self
            .catalogs
            .iter()
            .map(|(locale, source)| (*locale, std::borrow::Cow::Borrowed(*source)))
            .collect();
        if let Some(pack) = &self.pack {
            for locale in pack.installed_locales() {
                let language = language_of(locale);
                if sources
                    .iter()
                    .any(|(held, _)| language_of(held) == language)
                {
                    continue;
                }
                if let Some(source) = pack.read(locale) {
                    sources.push((locale, std::borrow::Cow::Owned(source)));
                }
            }
        }
        sources
    }

    /// Every language's words for `key`, each once, in the order of [`Self::every_source`]: what a
    /// stored name the app itself wrote can be recognised by, whichever language wrote it (a built-in's
    /// name kept in a record before the reader's language was known).
    #[must_use]
    pub fn every_value(&self, key: &str) -> Vec<String> {
        // Keys are few and the answer never depends on the selected language,
        // so it is read once per catalogue until the language packs move.
        thread_local! {
            static VALUES: std::cell::RefCell<
                std::collections::HashMap<
                    (usize, Option<&'static str>, String),
                    (u64, Vec<String>),
                >,
            > = std::cell::RefCell::new(std::collections::HashMap::new());
        }
        let catalogue = self.catalogs.as_ptr().addr();
        let generation = PACKS_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
        let slot = (catalogue, self.pack.map(|pack| pack.id), key.to_owned());
        if let Some(held) = VALUES.with(|values| {
            values
                .borrow()
                .get(&slot)
                .filter(|(held, _)| *held == generation)
                .map(|(_, words)| words.clone())
        }) {
            return held;
        }
        let words: Vec<String> = self
            .every_source()
            .into_iter()
            .filter_map(|(locale, source)| {
                let resource = shared_resource(source).ok()?;
                let mut bundle = new_bundle(&parse_locale_or_default(locale));
                bundle.add_resource(resource).ok()?;
                let message = bundle.get_message(key)?;
                let mut errors = Vec::new();
                Some(
                    bundle
                        .format_pattern(message.value()?, None, &mut errors)
                        .into_owned(),
                )
            })
            .collect();
        VALUES.with(|values| {
            values
                .borrow_mut()
                .insert(slot, (generation, words.clone()));
        });
        words
    }

    /// The languages this catalogue can show, each tag once: the embedded ones and those with an
    /// installed pack, in the order the catalogue names them.
    #[must_use]
    pub fn languages(&self) -> Vec<&'static str> {
        let mut languages: Vec<&'static str> =
            self.catalogs.iter().map(|(locale, _)| *locale).collect();
        if let Some(pack) = &self.pack {
            for locale in pack.installed_locales() {
                if !languages.contains(&locale) {
                    languages.push(locale);
                }
            }
        }
        languages
    }

    /// The Fluent source of the source locale's catalogue.
    #[must_use]
    pub fn source(&self) -> Option<&'static str> {
        self.catalogs
            .iter()
            .find(|(locale, _)| *locale == self.source_locale)
            .map(|(_, source)| *source)
    }

    /// A thread with no manager gets one selecting the interface language from the person's settings,
    /// the system locale as its default (see [`I18nManager::install_system`]).
    #[must_use]
    pub const fn with_system_locale(mut self) -> Self {
        self.fresh_locale = FreshLocale::System;
        self
    }

    /// Select `locale` when this catalogue first loads into a thread's manager, even over a host's
    /// choice.
    #[must_use]
    pub const fn with_pinned_locale(mut self, locale: &'static str) -> Self {
        self.pinned_locale = Some(locale);
        self
    }

    /// Load the catalogues on this thread if they are not loaded yet.
    pub fn ensure(&self) {
        let manager = manager_id();
        let key = self.catalogs.as_ptr().addr();
        let packs = PACKS_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
        let (loaded, in_this_manager) = EMBEDDED_LOADED.with(|loaded| {
            let loaded = loaded.borrow();
            let entry = loaded
                .iter()
                .find(|&&(loaded, id, _)| loaded == key && Some(id) == manager);
            (
                entry.is_some_and(|&(_, _, generation)| generation == packs),
                entry.is_some(),
            )
        });
        if loaded {
            return;
        }
        let texts = self.catalog_texts();
        let role = if self.shared {
            Role::Shared
        } else {
            Role::Window
        };
        if manager.is_some() {
            MANAGER.with(|slot| {
                if let Some(manager) = slot.borrow_mut().as_mut() {
                    for (locale, text) in texts {
                        manager.add_catalog(locale, text, role);
                    }
                }
            });
        } else {
            self.install_fresh(texts);
        }
        // Only a first load pins: reading the packs again (any thread may ask
        // for that) must not undo a locale chosen since.
        if let Some(locale) = self.pinned_locale
            && !in_this_manager
        {
            I18nManager::set_locale(locale);
        }
        if let Some(id) = manager_id() {
            EMBEDDED_LOADED.with(|loaded| {
                let mut loaded = loaded.borrow_mut();
                loaded.retain(|&(loaded, _, _)| loaded != key);
                loaded.push((key, id, packs));
            });
        }
    }

    /// Every catalogue, unparsed: the embedded ones and those the installed language packs hold, read
    /// when their language is shown.
    fn catalog_texts(&self) -> Vec<(LanguageIdentifier, CatalogText)> {
        let mut texts: Vec<(&'static str, CatalogText)> = self
            .catalogs
            .iter()
            .map(|(locale, source)| (*locale, CatalogText::Embedded(source)))
            .collect();
        if let Some(pack) = self.pack {
            for locale in pack.installed_locales() {
                if !texts.iter().any(|(held, _)| *held == locale) {
                    texts.push((locale, CatalogText::Pack(pack, locale)));
                }
            }
        }
        texts
            .into_iter()
            .filter_map(|(locale, text)| Some((parse_locale(locale).ok()?, text)))
            .collect()
    }

    fn install_fresh(&self, texts: Vec<(LanguageIdentifier, CatalogText)>) {
        let source = parse_locale_or_default(self.source_locale);
        let active = match self.fresh_locale {
            FreshLocale::Source => source.clone(),
            FreshLocale::System => preferred_locale().unwrap_or_else(|| source.clone()),
        };
        let mut catalogs: Vec<(LanguageIdentifier, Vec<CatalogText>)> = Vec::new();
        for (locale, text) in texts {
            match catalogs.iter_mut().find(|(held, _)| *held == locale) {
                Some((_, held)) => held.push(text),
                None => catalogs.push((locale, vec![text])),
            }
        }
        // A catalogue with a syntax error leaves the others loaded: each
        // degrades on its own. A shared catalogue speaks for no window: it is
        // the only one here, and the window's own words are not.
        let spoken = if self.shared {
            Vec::new()
        } else {
            catalogs.iter().map(|(locale, _)| locale.clone()).collect()
        };
        I18nManager::replace(source, active, catalogs, spoken);
    }

    /// Resolve `key` in the active locale.
    #[must_use]
    pub fn t(&self, key: &str) -> String {
        self.ensure();
        I18nManager::lookup(key, None)
    }

    /// Resolve `key`, interpolating named variables.
    #[must_use]
    pub fn t_args(&self, key: &str, args: &[(&str, ArgValue)]) -> String {
        self.ensure();
        I18nManager::format(key, args)
    }

    /// Select `locale` (BCP-47 or POSIX, e.g.
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

/// Define the calling crate's [`EmbeddedCatalog`]: `locales/en-US.ftl` in the crate embedded as the
/// source, the crate's other `locales/<locale>.ftl` read from the language packs ([`LanguagePack`],
/// named by the crate's package name), the user's locale when no host chose one, and source copy pinned
/// in the crate's own tests.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A catalogue embeds only its source; its other languages come from the language packs, here the
    /// crate's own `locales/` directory.
    #[test]
    fn the_interface_language_is_the_one_the_settings_name() {
        let _restore = RestoreLookupState::capture();
        let document = serde_json::json!({"appearance": {"language": "en-US"}})
            .as_object()
            .cloned()
            .unwrap_or_default();
        let _settings = settings_store::scoped_source(Some(std::sync::Arc::new(
            settings_store::HostDocument::new(document, |_| Ok(())),
        )));
        assert_eq!(preferred_language().as_deref(), Some("en-US"));
        I18nManager::install_system(
            "en-US",
            &[
                ("en-US", "greeting = Hello"),
                ("de-DE", "greeting = Guten Tag"),
            ],
        )
        .unwrap();
        assert_eq!(
            I18nManager::current_locale(),
            "en-US",
            "the settings' language, not the machine's"
        );
        assert_eq!(I18nManager::lookup_plain("greeting"), "Hello");
        let day = chrono::NaiveDate::from_ymd_opt(2025, 11, 5).unwrap();
        assert_eq!(
            crate::format::day_heading(day, false),
            "Wednesday, November 5"
        );
        I18nManager::set_locale("de-DE");
        assert_eq!(
            crate::format::day_heading(day, false),
            "Mittwoch, 5. November"
        );
    }

    /// A window speaks one language: an app that ships English only leaves the toolkit's words and the
    /// platform's formats English too, whatever the person's preference.
    #[test]
    fn an_english_only_app_leaves_the_window_english() {
        let _restore = RestoreLookupState::capture();
        I18nManager::install(
            "en-US",
            "de-DE",
            &[
                ("en-US", "en-only = English"),
                ("de-DE", "de-only = Deutsch"),
            ],
        )
        .unwrap();
        assert_eq!(I18nManager::lookup_plain("de-only"), "Deutsch");
        let language = speak_window_language(&[("en-US".to_owned(), "d-new = New".to_owned())]);
        assert_eq!(
            language, "en-US",
            "the app ships English only, so its window does"
        );
        assert_eq!(I18nManager::current_locale(), "en-US");
        assert_eq!(I18nManager::lookup_plain("d-new"), "New");
        assert_eq!(
            I18nManager::lookup_plain("de-only"),
            "de-only",
            "the window stopped speaking German"
        );
    }

    #[test]
    fn a_window_speaks_its_language_over_the_toolkits_own_words() {
        let _restore = RestoreLookupState::capture();
        // A thread that has drawn nothing yet: its first catalogue is the toolkit's, installed
        // in the desktop's language, and the window's language must win over it.
        let language = speak_window_language(&[("en-US".to_owned(), "d-new = New".to_owned())]);
        assert_eq!(language, "en-US");
        assert_eq!(I18nManager::current_locale(), "en-US");
        assert_eq!(I18nManager::lookup_plain("d-new"), "New");
        assert_eq!(
            I18nManager::lookup_plain("date-field-add"),
            "Add date",
            "the toolkit's own words follow the window's language"
        );
    }

    /// An edit an app declined is said in the person's own language, with the limit it is held
    /// to: the one word set a native and a hosted window both read.
    #[test]
    fn a_refused_edit_is_said_in_the_windows_language() {
        let _restore = RestoreLookupState::capture();
        I18nManager::register_widget_messages();
        I18nManager::set_locale("en-US");
        assert_eq!(
            declined_text(tilo_actions::Declined::TooLong),
            "This paste is too long to keep (limit 8 MB). Paste it in smaller parts."
        );
        assert_eq!(
            declined_text(tilo_actions::Declined::Changed),
            "This paragraph changed while you were typing. Your edit wasn't kept."
        );
        I18nManager::set_locale("de-DE");
        assert_eq!(
            declined_text(tilo_actions::Declined::TooLong),
            "Der eingefügte Text ist zu lang (höchstens 8 MB). Füge ihn in kleineren Stücken ein."
        );
        assert_eq!(
            declined_text(tilo_actions::Declined::Changed),
            "Dieser Absatz hat sich beim Tippen geändert. Deine Änderung wurde nicht übernommen."
        );
    }

    /// A window with no catalogue of its own — a system menu, a sheet of frames — has no language of
    /// its own to hold: the toolkit's labels read in the language selected for it, which is the
    /// person's.
    #[test]
    fn a_window_with_no_catalogue_speaks_its_selected_language() {
        let _restore = RestoreLookupState::capture();
        I18nManager::register_widget_messages();
        I18nManager::set_locale("de-DE");
        assert_eq!(
            I18nManager::lookup_plain("menu-query-placeholder"),
            "Tippen zum Filtern",
            "the filter field's hint is in the person's language"
        );
        assert_eq!(
            I18nManager::lookup_plain("date-field-add"),
            "Datum hinzufügen"
        );
        I18nManager::add_shared_resource("de-DE", "host-sort = Sortieren".to_owned());
        assert_eq!(I18nManager::lookup_plain("date-field-add"), "Add date");
    }

    /// A word a window shows from a catalogue in another language is recorded, so a check can hold a
    /// window to one language: a translation the app lacks (its source language standing in), or a
    /// catalogue of used package read in the wrong language.
    #[test]
    fn a_word_from_another_languages_catalogue_is_recorded() {
        let _restore = RestoreLookupState::capture();
        install_plain(
            "en-US",
            "de-DE",
            &[
                (
                    "en-US",
                    "probe = Probe\nprobe-only-english = English only\n",
                ),
                ("de-DE", "probe = Sonde\n"),
            ],
        );
        I18nManager::clear_foreign_words();
        assert_eq!(I18nManager::window_locale(), "de-DE");
        assert_eq!(I18nManager::lookup_plain("probe"), "Sonde");
        assert_eq!(
            I18nManager::foreign_words(),
            Vec::<(String, String)>::new(),
            "a word the window's own language holds is no foreign word"
        );
        assert_eq!(
            I18nManager::lookup_plain("probe-only-english"),
            "English only"
        );
        assert_eq!(
            I18nManager::foreign_words(),
            vec![("probe-only-english".to_owned(), "en-US".to_owned())],
            "the source language stood in for the missing translation"
        );
        assert_eq!(
            I18nManager::lookup_plain("probe-only-english"),
            "English only"
        );
        assert_eq!(
            I18nManager::foreign_words().len(),
            1,
            "each word is recorded once"
        );
        I18nManager::clear_foreign_words();
        assert!(I18nManager::foreign_words().is_empty());
    }

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
        assert!(!missing.has("xx-XX"));
        assert_eq!(PACKED.languages(), ["en-US", "de-DE"]);
    }

    #[test]
    fn a_messages_words_come_back_in_every_language_the_catalogue_ships() {
        static SPELLED: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "spelled-name = Default\nspelled-only = English\n"),
                ("de-DE", "spelled-name = Standard\n"),
            ],
        );
        assert_eq!(SPELLED.every_value("spelled-name"), ["Default", "Standard"]);
        assert_eq!(SPELLED.every_value("spelled-only"), ["English"]);
        assert!(SPELLED.every_value("spelled-missing").is_empty());
    }

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

    fn install_plain(source: &str, locale: &str, catalogs: &[(&str, &str)]) {
        I18nManager::install(source, locale, catalogs).unwrap();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
    }

    /// A message a value completes is said with it, and is not said at all where the caller has none:
    /// the words a step title can stand on.
    #[test]
    fn a_message_only_a_value_completes_is_not_said_without_one() {
        let _restore = RestoreLookupState::capture();
        install_plain(
            "en-US",
            "en-US",
            &[(
                "en-US",
                "sheets-workbook-title = Named the workbook { $new }\nbare = Post a message\n",
            )],
        );
        assert_eq!(
            I18nManager::lookup_complete("sheets-workbook-title", None),
            None,
            "a placeable left unfilled is no sentence"
        );
        let mut args = FluentArgs::new();
        args.set("new", "Q3");
        assert_eq!(
            I18nManager::lookup_complete("sheets-workbook-title", Some(&args)).as_deref(),
            Some("Named the workbook Q3")
        );
        assert_eq!(
            I18nManager::lookup_complete("bare", None).as_deref(),
            Some("Post a message"),
            "a message that names no variable is said as it stands"
        );
        assert_eq!(I18nManager::lookup_complete("no-such-message", None), None);
    }

    /// A plain lookup is resolved once per catalogue, locale and pseudo-localisation, and resolved
    /// again as soon as any of them changes.
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
        install_plain("en-US", "en-US", &[("en-US", "kept-probe = Other\n")]);
        assert_eq!(I18nManager::lookup_plain("kept-probe"), "Other");
    }

    #[test]
    fn lookup_falls_back_through_language_catalogues_then_source() {
        let _restore = RestoreLookupState::capture();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        TOOLKIT_SOURCE.with(|source| assert!(source.is_ok(), "bundled toolkit catalog must parse"));
        assert_eq!(I18nManager::lookup_plain("history-mode-title"), "History");
        assert_eq!(
            I18nManager::lookup_plain("standalone-search-title"),
            "Search"
        );

        type Catalogs = &'static [(&'static str, &'static str)];
        let cases: &[(&str, &str, Catalogs, Catalogs)] = &[
            // The active language answers before the source does, the
            // toolkit's own words included: its German catalogue is part of
            // every manager, so a key the host only has in its source reads
            // in the toolkit's German rather than in English.
            (
                "the active language answers before the source, toolkit words included",
                "de-DE",
                &[
                    (
                        "en-US",
                        "history-mode-title = App history\nstandalone-search-title = App search",
                    ),
                    ("de", "history-mode-title = Verlauf"),
                ],
                &[
                    ("history-mode-title", "Verlauf"),
                    ("standalone-search-title", "Suchen"),
                    ("proposal-accept", "Übernehmen"),
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
        I18nManager::install("en-US", "en-US", &[("en-US", EN)]).unwrap();
        let revision = I18nManager::revision();
        let error = I18nManager::install("en-US", "de", &[("en-US", "broken = {"), ("de", DE)])
            .unwrap_err();
        assert!(matches!(error, I18nError::InvalidResource { .. }));
        assert_eq!(I18nManager::revision(), revision);
        assert_eq!(I18nManager::lookup("greeting", None), "Hello");

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
        assert_eq!(I18nManager::format("share", &[]), "Geteilt mit {$persn}");

        const SOURCE: &str =
            "items = { $count ->\n    [one] { $count } item\n   *[other] { $count } items\n}";
        install_plain("en-US", "en-US", &[("en-US", SOURCE)]);
        let items = || I18nManager::format("items", &[("count", ArgValue::from(21))]);
        assert_eq!(items(), "21 items");
        // A window that speaks Russian reads its English fallback under
        // Russian's rules, where 21 is `one`.
        install_plain(
            "en-US",
            "ru-RU",
            &[("en-US", SOURCE), ("ru-RU", "greeting = Здравствуйте")],
        );
        assert_eq!(items(), "21 item");
        I18nManager::set_locale("xx-ZZ");
        assert_eq!(items(), "21 items");
    }

    #[test]
    fn pseudo_locale_expansion_is_idempotent() {
        // Copy that is localised twice must still carry one pair of markers:
        // the doubling a reviewer sees is the string, not the transform
        // (`T3` in the UX audit).
        let once = pseudo_expand("Wi-Fi");
        assert_eq!(pseudo_expand(&once), once);
        assert_eq!(once.matches(PSEUDO_OPEN).count(), 1);
        assert_eq!(once.matches(PSEUDO_CLOSE).count(), 1);
        // A string that merely starts and ends with the markers is left alone
        // rather than wrapped a second time.
        assert_eq!(pseudo_expand("⟦⟧"), "⟦⟧");
    }

    /// Copy made of copy already localised keeps the one pair of marks it has: a date heading reads
    /// `⟦Tue⟧, ⟦Jun⟧ 30`, never `⟦⟦Tue⟧, ⟦Jun⟧ 30⟧`.
    #[test]
    fn a_message_of_localised_fragments_is_not_marked_twice() {
        let _restore = RestoreLookupState::capture();
        install_plain("en-US", "en-US", &[("en-US", EN)]);
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        let weekday = I18nManager::lookup("greeting", None);
        let composed =
            I18nManager::format("share", &[("person", ArgValue::from(weekday.as_str()))]);
        assert_eq!(
            composed.matches(PSEUDO_OPEN).count(),
            1,
            "one pair of markers: {composed}"
        );
    }

    /// Nothing says nothing: an empty message is not two markers, which a review would read as copy
    /// that is too long.
    #[test]
    fn an_empty_message_stays_empty_under_the_pseudo_locale() {
        let _restore = RestoreLookupState::capture();
        install_plain("en-US", "en-US", &[("en-US", EN)]);
        I18nManager::set_pseudo_locale(PseudoLocale::Expanded);
        assert_eq!(I18nManager::lookup("", None), "");
        assert_eq!(I18nManager::format("", &[]), "");
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

        {
            let _restore = RestoreLookupState::capture();
            I18nManager::install("en-US", "de", &[("en-US", EN), ("de", DE)]).unwrap();
            assert_eq!(PINNED.t("pinned-probe"), "Pinned");
            assert_eq!(I18nManager::current_locale(), "en-US");
        }
    }

    /// A window speaks the language of its own words.
    #[test]
    fn the_window_speaks_its_own_language_not_the_toolkits() {
        static GERMAN_WINDOW: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "german-window-probe = a window"),
                ("de", "german-window-probe = ein Fenster"),
            ],
        );
        let show_all = || widget_format("search-show-all", &[("count", ArgValue::from(13))]);
        let _restore = RestoreLookupState::capture();

        // A German device and a window that ships English only.
        I18nManager::install("en-US", "de", &[("en-US", EN)]).unwrap();
        assert_eq!(I18nManager::current_locale(), "de");
        assert_eq!(show_all(), "Show all 13");

        GERMAN_WINDOW.ensure();
        assert_eq!(show_all(), "Alle 13 zeigen");
    }

    fn parsed_locales() -> Vec<String> {
        MANAGER.with(|manager| {
            let mut locales: Vec<String> = manager
                .borrow()
                .as_ref()
                .map(|manager| manager.bundles.keys().map(ToString::to_string).collect())
                .unwrap_or_default();
            locales.sort();
            locales
        })
    }

    /// Only the language shown and the source are parsed; choosing another language parses it then, and
    /// lets go of the one it replaced.
    #[test]
    fn only_the_shown_language_and_the_source_are_parsed() {
        static SWITCHED: EmbeddedCatalog = EmbeddedCatalog::new(
            "en-US",
            &[
                ("en-US", "switch-probe = Hello\nswitch-only-source = Source"),
                ("de-DE", "switch-probe = Hallo"),
                ("fr", "switch-probe = Bonjour"),
            ],
        );
        let _restore = RestoreLookupState::capture();
        I18nManager::set_pseudo_locale(PseudoLocale::Off);
        assert_eq!(SWITCHED.t("switch-probe"), "Hello");
        assert_eq!(parsed_locales(), ["en-US"]);

        let revision = I18nManager::revision();
        SWITCHED.set_locale("de-DE");
        assert_ne!(I18nManager::revision(), revision);
        assert_eq!(SWITCHED.t("switch-probe"), "Hallo");
        assert_eq!(SWITCHED.t("switch-only-source"), "Source");
        assert_eq!(SWITCHED.t("switch-missing"), "switch-missing");
        assert_eq!(parsed_locales(), ["de-DE", "en-US"]);

        SWITCHED.set_locale("fr-CA");
        assert_eq!(SWITCHED.t("switch-probe"), "Bonjour");
        assert_eq!(parsed_locales(), ["en-US", "fr"]);

        SWITCHED.set_locale("en-US");
        assert_eq!(SWITCHED.t("switch-probe"), "Hello");
        assert_eq!(parsed_locales(), ["en-US"]);
    }

    #[test]
    fn threads_share_one_parse_of_a_catalogue() {
        static SHARED: EmbeddedCatalog =
            EmbeddedCatalog::new("en-US", &[("en-US", "shared-probe = Shared\n")]);
        let source = || {
            SHARED.ensure();
            MANAGER.with(|manager| {
                let manager = manager.borrow();
                let loaded = manager.as_ref()?.bundles.values().next()?;
                Some(Arc::as_ptr(loaded.resources.first()?).addr())
            })
        };
        let _restore = RestoreLookupState::capture();
        let here = source();
        assert!(here.is_some());
        let there = std::thread::spawn(source).join().unwrap();
        assert_eq!(here, there);
    }

    #[test]
    fn relative_times_follow_the_host_locale_and_its_plural_rules() {
        use crate::format::relative;
        use crate::history_source::participant_display_label;
        use crate::time::{Duration, SystemTime};
        use crate::widget::format_relative_time;
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ago = |secs| format_relative_time(base, base + Duration::from_secs(secs));

        {
            let _restore = RestoreLookupState::capture();
            assert_eq!(ago(10), "just now");
            assert_eq!(ago(120), "2 min ago");
            assert_eq!(manager_id(), None);
        }

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
            assert_eq!(participant_display_label("Ada"), "Ada");
        }
    }

    /// A toolkit key only English defines would show English on a German desktop; every relative-time
    /// message is translated with the same variables.
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
        ] {
            let mut args = FluentArgs::new();
            args.set("count", FluentValue::from(2));
            args.set("name", FluentValue::from("scribe"));
            args.set("person", FluentValue::from("Ada"));
            for (locale, bundle) in [("en-US", &en), ("de-DE", &de)] {
                let (value, clean) = tilo_history::format_message(bundle, key, Some(&args))
                    .unwrap_or_else(|| panic!("{locale}.ftl lacks {key}"));
                assert!(clean, "{locale}.ftl formats {key} with errors: {value}");
            }
        }
    }
}
