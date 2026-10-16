//! Process-local view of the shared spelling preferences: the per-user
//! `text_assistance` section every app's text fields follow.
//!
//! Reading the JSON document on every keypress would turn a UX feature into an
//! I/O hot path. We stat the settings file at most once every two seconds and
//! reload it when its modification stamp changes, so Settings changes propagate
//! to running apps without filesystem I/O on every draw or keypress.

use crate::time::{Duration, Instant};
use std::sync::{Mutex, OnceLock};

pub const SETTINGS_SECTION: &str = "text_assistance";

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, tilo_resource::Model,
)]
pub struct TextAssistancePrefs {
    pub check_spelling: bool,
    pub auto_correct: bool,
    /// The language words are checked in (`en_US`, `de_DE`); empty for the
    /// session's own language.
    #[serde(default)]
    pub spelling_language: String,
}

impl Default for TextAssistancePrefs {
    fn default() -> Self {
        Self {
            check_spelling: true,
            auto_correct: true,
            spelling_language: String::new(),
        }
    }
}

impl crate::storage::SettingsSchema for TextAssistancePrefs {
    const SCOPE: crate::storage::Scope = crate::storage::Scope::Shared(SETTINGS_SECTION);
}

/// The section as a record the person edits by path.
#[tilo_resource::resource(kind = "settings.text-assistance", label = "Spelling and text")]
impl TextAssistancePrefs {}

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
        && cache
            .checked_at
            .is_some_and(|checked| checked.elapsed() < Duration::from_secs(2))
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
