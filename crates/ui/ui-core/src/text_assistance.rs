//! Process-local view of the shared spelling preferences: the per-user `text_assistance`
//! section every app's text fields follow. Reading the JSON document on every keypress
//! would turn a UX feature into an I/O hot path.

use crate::time::{Duration, Instant};
use settings_model::text_assistance::TextAssistancePrefs;
use std::sync::{Mutex, OnceLock};

#[derive(Default)]
struct CachedPrefs {
    stamp: Option<crate::storage::SettingsRevision>,
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
        && cache.checked_at.is_some_and(|checked| {
            crate::time::now().saturating_duration_since(checked) < Duration::from_secs(2)
        })
    {
        return cache.prefs.clone();
    }
    let settings = crate::storage::Settings::<TextAssistancePrefs>::shared();
    let stamp = Some(settings.revision());
    if !cache.loaded || cache.stamp != stamp {
        cache.prefs = settings.get();
        cache.stamp = stamp;
        cache.loaded = true;
    }
    cache.checked_at = Some(crate::time::now());
    cache.prefs.clone()
}
