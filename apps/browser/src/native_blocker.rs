//! Native Chromium filter-list loading, separate from WebKit JSON conversion.

use std::path::Path;
use std::sync::Arc;
use ui::webview::{SharedEngine, WebViewCommand};

// Small first-run safety net authored for qOS. Full maintained lists are
// loaded from cache and refreshed below; this is not advertised as a full list.
const BASELINE: &str = "! qOS first-run baseline\n||doubleclick.net^\n||googlesyndication.com^\n||googleadservices.com^\n||adnxs.com^\n||adsrvr.org^\n||scorecardresearch.com^\n||outbrain.com^\n||taboola.com^\n";
const MAX_LIST: u64 = 8 * 1024 * 1024;
const MAX_TOTAL: usize = 30 * 1024 * 1024;

fn append_bounded(rules: &mut String, text: &str) {
    if rules.len().saturating_add(text.len()).saturating_add(1) <= MAX_TOTAL {
        rules.push('\n');
        rules.push_str(text);
    }
}

fn cached_rules(directory: &Path) -> String {
    let mut rules = BASELINE.to_string();
    for (name, _) in crate::blocker::LISTS {
        let path = directory.join(name);
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() <= MAX_LIST) {
            if let Some(text) = crate::blocker::read_cached_list(&path) {
                append_bounded(&mut rules, &text);
            }
        }
    }
    rules
}

/// Enqueue existing protection synchronously, ahead of the initial Navigate.
/// Refreshes replace rules without changing the user's current enabled state.
pub(crate) fn start(engine: SharedEngine) {
    let initial = crate::blocker::cache_dir().map_or_else(
        || BASELINE.to_string(),
        |directory| cached_rules(&directory),
    );
    engine.send(WebViewCommand::SetNativeFilterRules {
        rules: Arc::new(initial),
        enabled: Some(true),
    });
    let engine = Arc::downgrade(&engine);
    if let Err(error) = std::thread::Builder::new()
        .name("chromium-filter-loader".into())
        .spawn(move || {
            let mut rules = BASELINE.to_string();
            let mut loaded = 0;
            for (name, url) in crate::blocker::LISTS {
                if engine.strong_count() == 0 {
                    return;
                }
                if let Some(text) = crate::blocker::load_list(name, url) {
                    append_bounded(&mut rules, &text);
                    loaded += 1;
                }
            }
            if let Some(engine) = engine.upgrade().filter(|_| loaded > 0) {
                engine.send(WebViewCommand::SetNativeFilterRules {
                    rules: Arc::new(rules),
                    enabled: None,
                });
            }
        })
    {
        tracing::warn!(%error, "filter refresh could not start; cached protection remains active");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_protection_exists_without_network_or_cache() {
        let dir = tempfile::tempdir().unwrap();
        let rules = cached_rules(dir.path());
        assert!(rules.contains("||doubleclick.net^"));
        assert!(rules.contains("||googlesyndication.com^"));
    }

    #[test]
    fn reads_known_cached_lists_and_ignores_other_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("easylist.txt"), "||ads.fixture.test^").unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), "must-not-load").unwrap();
        let rules = cached_rules(dir.path());
        assert!(rules.contains("||ads.fixture.test^"));
        assert!(!rules.contains("must-not-load"));
    }
}
