use std::collections::HashMap;
use fluent::{FluentBundle, FluentResource, FluentArgs};
use unic_langid::LanguageIdentifier;

pub struct I18nManager {
    bundles: HashMap<LanguageIdentifier, FluentBundle<FluentResource>>,
    pub current_locale: LanguageIdentifier,
}

thread_local! {
    static MANAGER: std::cell::RefCell<Option<I18nManager>> = const { std::cell::RefCell::new(None) };
}

impl I18nManager {
    pub fn init(locale: &str) {
        let lang_id: LanguageIdentifier = locale.parse().expect("Parsing failed");
        let mut manager = I18nManager {
            bundles: HashMap::new(),
            current_locale: lang_id.clone(),
        };
        
        let bundle = FluentBundle::new(vec![lang_id]);
        manager.bundles.insert(manager.current_locale.clone(), bundle);
        
        MANAGER.with(|m| {
            *m.borrow_mut() = Some(manager);
        });
    }

    pub fn add_resource(locale: &str, ftl_source: String) {
        let lang_id: LanguageIdentifier = locale.parse().expect("Parsing failed");
        let res = FluentResource::try_new(ftl_source).expect("Failed to parse an FTL resource.");
        
        MANAGER.with(|m| {
            if let Some(mgr) = m.borrow_mut().as_mut() {
                let bundle = mgr.bundles.entry(lang_id.clone()).or_insert_with(|| {
                    FluentBundle::new(vec![lang_id])
                });
                bundle.add_resource(res).expect("Failed to add FTL resource to the bundle.");
            }
        });
    }

    pub fn set_locale(locale: &str) {
        let lang_id: LanguageIdentifier = locale.parse().expect("Parsing failed");
        MANAGER.with(|m| {
            if let Some(mgr) = m.borrow_mut().as_mut() {
                mgr.current_locale = lang_id;
            }
        });
    }

    pub fn lookup(key: &str, args: Option<&FluentArgs>) -> String {
        MANAGER.with(|m| {
            if let Some(mgr) = m.borrow().as_ref() {
                if let Some(bundle) = mgr.bundles.get(&mgr.current_locale) {
                    if let Some(msg) = bundle.get_message(key) {
                        if let Some(pattern) = msg.value() {
                            let mut errors = vec![];
                            return bundle.format_pattern(pattern, args, &mut errors).to_string();
                        }
                    }
                }
            }
            key.to_string()
        })
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
