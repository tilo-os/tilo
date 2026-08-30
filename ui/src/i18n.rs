use fluent::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use std::collections::HashMap;
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

macro_rules! arg_from_number {
    ($($t:ty),*) => {$(
        impl From<$t> for ArgValue {
            fn from(n: $t) -> Self { ArgValue::Number(n as f64) }
        }
    )*};
}
arg_from_number!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, f32, f64);

pub struct I18nManager {
    bundles: HashMap<LanguageIdentifier, FluentBundle<FluentResource>>,
    pub current_locale: LanguageIdentifier,
}

thread_local! {
    static MANAGER: std::cell::RefCell<Option<I18nManager>> = const { std::cell::RefCell::new(None) };
}

/// Parse a BCP-47 locale tag, falling back to `en-US` on a malformed value
/// (e.g. a garbled system locale) rather than panicking the whole app over
/// a string it didn't choose. The fallback literal is a compile-time
/// constant known to parse, so this can't recurse into failure.
fn parse_locale_or_default(locale: &str) -> LanguageIdentifier {
    locale
        .parse()
        .unwrap_or_else(|_| "en-US".parse().expect("\"en-US\" is a valid BCP-47 tag"))
}

impl I18nManager {
    pub fn init(locale: &str) {
        let lang_id: LanguageIdentifier = parse_locale_or_default(locale);
        let mut manager = I18nManager {
            bundles: HashMap::new(),
            current_locale: lang_id.clone(),
        };

        let mut bundle = FluentBundle::new(vec![lang_id]);
        // Don't wrap interpolated variables in Unicode bidi isolation marks
        // (FSI/PDI) — we render to a single run, so "Count is { $n }" should
        // read "Count is 5", not "Count is \u{2068}5\u{2069}".
        bundle.set_use_isolating(false);
        manager
            .bundles
            .insert(manager.current_locale.clone(), bundle);

        MANAGER.with(|m| {
            *m.borrow_mut() = Some(manager);
        });
    }

    pub fn add_resource(locale: &str, ftl_source: String) {
        let lang_id: LanguageIdentifier = parse_locale_or_default(locale);
        let res = FluentResource::try_new(ftl_source).expect("Failed to parse an FTL resource.");

        MANAGER.with(|m| {
            if let Some(mgr) = m.borrow_mut().as_mut() {
                let bundle = mgr.bundles.entry(lang_id.clone()).or_insert_with(|| {
                    let mut b = FluentBundle::new(vec![lang_id]);
                    b.set_use_isolating(false);
                    b
                });
                bundle
                    .add_resource(res)
                    .expect("Failed to add FTL resource to the bundle.");
            }
        });
    }

    pub fn set_locale(locale: &str) {
        let lang_id: LanguageIdentifier = parse_locale_or_default(locale);
        MANAGER.with(|m| {
            if let Some(mgr) = m.borrow_mut().as_mut() {
                mgr.current_locale = lang_id;
            }
        });
    }

    pub fn lookup(key: &str, args: Option<&FluentArgs>) -> String {
        MANAGER.with(|m| {
            if let Some(mgr) = m.borrow().as_ref()
                && let Some(bundle) = mgr.bundles.get(&mgr.current_locale)
                && let Some(msg) = bundle.get_message(key)
                && let Some(pattern) = msg.value()
            {
                let mut errors = vec![];
                return bundle
                    .format_pattern(pattern, args, &mut errors)
                    .to_string();
            }
            key.to_string()
        })
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
