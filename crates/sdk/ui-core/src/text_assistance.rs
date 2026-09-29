//! Process-local view of the shared spelling preferences.
//!
//! Reading the JSON document on every keypress would turn a UX feature into an
//! I/O hot path. We stat the settings file at most once every two seconds and
//! reload it when its modification stamp changes, so Settings changes propagate
//! to running apps without filesystem I/O on every draw or keypress.

use crate::time::{Duration, Instant};
use std::sync::{Mutex, OnceLock};

pub const SETTINGS_SECTION: &str = "text_assistance";
pub const CHECK_SPELLING_KEY: &str = "check_spelling";
pub const AUTO_CORRECT_KEY: &str = "auto_correct";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextAssistancePrefs {
    pub check_spelling: bool,
    pub auto_correct: bool,
}

impl Default for TextAssistancePrefs {
    fn default() -> Self {
        Self {
            check_spelling: true,
            auto_correct: true,
        }
    }
}

#[derive(Default)]
struct CachedPrefs {
    stamp: Option<settings_store::SettingsStamp>,
    checked_at: Option<Instant>,
    loaded: bool,
    prefs: TextAssistancePrefs,
}

/// Current user preference, cached until `settings.json` changes.
pub fn preferences() -> TextAssistancePrefs {
    static CACHE: OnceLock<Mutex<CachedPrefs>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(CachedPrefs::default()));
    let Ok(mut cache) = cache.lock() else {
        return TextAssistancePrefs::default();
    };
    if cache.loaded
        && cache
            .checked_at
            .is_some_and(|checked| checked.elapsed() < Duration::from_secs(2))
    {
        return cache.prefs;
    }
    let stamp = settings_store::stamp();
    if !cache.loaded || cache.stamp != stamp {
        let doc = settings_store::load();
        cache.prefs = TextAssistancePrefs {
            check_spelling: settings_store::bool_at(&doc, SETTINGS_SECTION, CHECK_SPELLING_KEY)
                .unwrap_or(true),
            auto_correct: settings_store::bool_at(&doc, SETTINGS_SECTION, AUTO_CORRECT_KEY)
                .unwrap_or(true),
        };
        cache.stamp = stamp;
        cache.loaded = true;
    }
    cache.checked_at = Some(crate::time::now());
    cache.prefs
}
