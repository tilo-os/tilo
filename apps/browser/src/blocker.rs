//! Built-in ad blocking: fetch Brave-compatible filter lists (EasyList +
//! EasyPrivacy), cache them on disk, compile them to WebKit
//! content-blocker JSON via [`ui::webview::adblock`], and hand the
//! result to the engine. Blocking then runs natively inside the
//! engine's network process — no per-request work in the app.
//!
//! Everything happens on a spawned thread so a cold cache / slow
//! network never delays first paint; pages loaded before the filter
//! compiles simply go unfiltered once.

use std::path::PathBuf;
use std::time::Duration;

use ui::webview::{SharedEngine, WebViewCommand, WebViewEngine};

/// Filter lists to fetch, as `(cache file name, URL)`. EasyList +
/// EasyPrivacy are the backbone of Brave's default blocking.
const LISTS: &[(&str, &str)] = &[
    ("easylist.txt", "https://easylist.to/easylist/easylist.txt"),
    ("easyprivacy.txt", "https://easylist.to/easylist/easyprivacy.txt"),
];

/// Re-download a cached list after this age.
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// Load + compile the filter lists in the background and install the
/// compiled content filter into `engine` when ready.
pub fn spawn_filter_loader(engine: SharedEngine) {
    std::thread::Builder::new()
        .name("filter-loader".into())
        .spawn(move || match load_and_compile() {
            Ok(filter) => {
                tracing::info!(
                    rules = filter.rule_count,
                    skipped = filter.skipped,
                    "content filter compiled"
                );
                engine.send(WebViewCommand::SetContentFilter(Some(filter.json)));
            }
            Err(err) => tracing::warn!("ad blocking unavailable: {err}"),
        })
        .expect("spawn filter-loader thread");
}

fn cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("qos-browser").join("filters"))
}

/// Return the list text: fresh cache if young enough, otherwise fetch
/// (updating the cache), otherwise a stale cache beats nothing.
fn load_list(name: &str, url: &str) -> Option<String> {
    let path = cache_dir().map(|d| d.join(name));

    if let Some(p) = &path {
        if let Ok(meta) = std::fs::metadata(p) {
            let fresh = meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age < CACHE_MAX_AGE);
            if fresh {
                if let Ok(text) = std::fs::read_to_string(p) {
                    return Some(text);
                }
            }
        }
    }

    match reqwest::blocking::get(url)
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
    {
        Ok(text) => {
            if let Some(p) = &path {
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir).ok();
                }
                std::fs::write(p, &text).ok();
            }
            Some(text)
        }
        Err(err) => {
            tracing::warn!("fetching {url} failed: {err}");
            path.and_then(|p| std::fs::read_to_string(p).ok())
        }
    }
}

fn load_and_compile() -> Result<ui::webview::adblock::CompiledFilter, String> {
    let texts: Vec<String> = LISTS
        .iter()
        .filter_map(|(name, url)| load_list(name, url))
        .collect();
    if texts.is_empty() {
        return Err("no filter lists available (offline with an empty cache)".into());
    }
    ui::webview::adblock::compile_filter_lists(texts.iter().map(String::as_str))
        .map_err(|e| e.to_string())
}
