//! Shared ad-filter service backing the per-WebView `set_ad_blocking`
//! toggle.
//!
//! Fetches the canonical WebKit-clean filter lists, caches them on disk,
//! compiles them (chunked) to WebKit content-blocker JSON via
//! [`crate::webview::adblock`], and exposes the compiled chunks plus the
//! scriptlet-style user scripts the declarative filter can't express.
//!
//! The service is **process-wide and lazily started** ([`service`]): the
//! first WebView to enable blocking kicks a background fetch+compile
//! thread, and every WebView shares the single compiled result — the heavy
//! WebKit compile happens once regardless of how many WebViews block. The
//! engine polls [`Shared::filters`] and installs when it turns `Some`.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Filter lists to fetch, as `(cache file name, URL)`.
///
/// The canonical uBlock-Origin-default blocking set, restricted to lists
/// whose rules survive adblock-rust's conversion to WebKit's *declarative*
/// content-blocker format cleanly:
///
/// - **EasyList** / **EasyPrivacy** — the network + tracker backbone.
/// - **EasyList Cookie List** — cookie/consent banner annoyances.
/// - **uBlock Origin** `filters`, `privacy`, `badware`, `resource-abuse` —
///   uBO's own maintained supplements.
///
/// Deliberately excluded: uBO **quick-fixes** and **AdGuard Base**. Both
/// lean on rule types WebKit content blocking cannot express — `$replace`
/// (response-body rewriting), `$redirect`, and raw-regex `$frame` filters
/// spanning a huge URL range. adblock-rust's lossy conversion turns some of
/// those into over-broad `block` rules that catch a page's *own*
/// first-party document/scripts during load, blanking it. They also churn
/// daily, so a fresh compile can regress unpredictably. EasyList + uBO
/// already cover the same targets through WebKit-safe rules.
pub const LISTS: &[(&str, &str)] = &[
    ("easylist.txt", "https://easylist.to/easylist/easylist.txt"),
    (
        "easyprivacy.txt",
        "https://easylist.to/easylist/easyprivacy.txt",
    ),
    (
        "easylist-cookie.txt",
        "https://secure.fanboy.co.nz/fanboy-cookiemonster.txt",
    ),
    (
        "ublock-filters.txt",
        "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/filters.txt",
    ),
    (
        "ublock-privacy.txt",
        "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/privacy.txt",
    ),
    (
        "ublock-badware.txt",
        "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/badware.txt",
    ),
    (
        "ublock-resource-abuse.txt",
        "https://raw.githubusercontent.com/uBlockOrigin/uAssets/master/filters/resource-abuse.txt",
    ),
];

/// Re-download a cached list after this age.
const CACHE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// A compiled, ready-to-install filter set. Cheap to clone (Arc-shared).
#[derive(Clone)]
pub struct AdFilters {
    /// Stable id of the *source* lists, so the engine reuses previously
    /// compiled WebKit filters when the lists are unchanged.
    pub version: u64,
    /// WebKit content-blocker JSON, split under the per-filter rule cap.
    pub chunks: Arc<Vec<Arc<String>>>,
    /// Document-start user scripts for scriptlet-style blocking (YouTube).
    pub user_scripts: Arc<Vec<String>>,
}

/// Shared service handle. Obtain via [`service`].
pub struct Shared {
    ready: Mutex<Option<AdFilters>>,
}

impl Shared {
    /// The compiled filters if the background load has finished, else
    /// `None` (still loading, or the fetch failed and blocking is
    /// unavailable this run).
    pub fn filters(&self) -> Option<AdFilters> {
        self.ready.lock().unwrap().clone()
    }
}

static SERVICE: OnceLock<Arc<Shared>> = OnceLock::new();

/// Start the shared loader (idempotent) and return its handle. The first
/// call spawns the fetch+compile thread; later calls return the same
/// handle without doing more work.
pub fn service() -> Arc<Shared> {
    SERVICE
        .get_or_init(|| {
            let shared = Arc::new(Shared {
                ready: Mutex::new(None),
            });
            let sink = shared.clone();
            let spawned = std::thread::Builder::new()
                .name("qos-adblock".into())
                .spawn(move || match load_and_compile() {
                    Ok(filters) => {
                        tracing::info!(
                            rules = filters.chunks.len(),
                            "ad filters compiled and ready"
                        );
                        *sink.ready.lock().unwrap() = Some(filters);
                    }
                    Err(err) => tracing::warn!("ad blocking unavailable: {err}"),
                });
            if let Err(e) = spawned {
                tracing::warn!("could not spawn ad-filter loader: {e}");
            }
            shared
        })
        .clone()
}

/// JavaScript user scripts for scriptlet-style blocking that the
/// declarative content filter cannot express. Each entry is standalone,
/// injected at document-start into all frames, and defensive (feature
/// detection, no throwing) so injection into an unexpected page is
/// harmless.
pub fn user_scripts() -> Vec<String> {
    vec![
        youtube_ad_skip_script().to_string(),
        generic_ad_cosmetic_script().to_string(),
    ]
}

/// Robust YouTube video-ad skipper. YouTube stitches video ads into the
/// same origin as the player, so no network rule can drop them — only
/// page-side JS can mute, fast-forward, and click past them. Resilient to
/// YouTube's frequent markup churn (several selector variants, re-run on
/// every DOM mutation plus an interval so it survives SPA navigation).
fn youtube_ad_skip_script() -> &'static str {
    r#"(function () {
  'use strict';
  var host = location.hostname;
  if (!/(^|\.)youtube(-nocookie)?\.com$/.test(host) &&
      !/(^|\.)youtube\.googleapis\.com$/.test(host)) {
    return;
  }

  // Hide ad overlays/banners the declarative filter can't reach on
  // YouTube's same-origin markup.
  var css =
    '.ad-showing .video-ads, .ytp-ad-module, .ytp-ad-overlay-slot,' +
    '.ytp-ad-image-overlay, .ytp-ad-overlay-container,' +
    'ytd-ad-slot-renderer, ytd-in-feed-ad-layout-renderer,' +
    'ytd-banner-promo-renderer-background, ytd-display-ad-renderer,' +
    'ytd-promoted-video-renderer, ytd-companion-slot-renderer,' +
    '#masthead-ad, #player-ads { display: none !important; }';
  function injectStyle() {
    if (document.getElementById('qos-yt-adblock')) return;
    var el = document.createElement('style');
    el.id = 'qos-yt-adblock';
    el.textContent = css;
    (document.head || document.documentElement).appendChild(el);
  }

  function skip() {
    injectStyle();
    var player = document.getElementById('movie_player');
    var video = document.querySelector('video');
    var adShowing = !!(player && player.classList.contains('ad-showing'));

    // Click any skip-button variant as soon as it is present.
    var skipBtn = document.querySelector(
      '.ytp-ad-skip-button, .ytp-ad-skip-button-modern,' +
      '.ytp-skip-ad-button, .ytp-ad-skip-button-modern .ytp-button,' +
      '.ytp-ad-skip-button-container button'
    );
    if (skipBtn) {
      try { skipBtn.click(); } catch (e) {}
    }

    // Dismiss overlay/interstitial close buttons.
    var close = document.querySelector(
      '.ytp-ad-overlay-close-button, .ytp-ad-overlay-close-container'
    );
    if (close) {
      try { close.click(); } catch (e) {}
    }

    if (adShowing && video) {
      // Only mute if the user hadn't already muted the video.
      if (!video.muted) {
        video.muted = true;
        video.dataset.qosAdMuted = '1';
      }
      try { video.playbackRate = 16; } catch (e) {}
      if (isFinite(video.duration) && video.duration > 0) {
        // Seek to the end: fast-forwards the unskippable portion away.
        video.currentTime = video.duration;
      }
    } else if (video && video.dataset.qosAdMuted === '1') {
      // Ad cleared — restore only what we changed.
      video.muted = false;
      try { video.playbackRate = 1; } catch (e) {}
      delete video.dataset.qosAdMuted;
    }
  }

  function start() {
    skip();
    if (document.body) {
      new MutationObserver(skip).observe(document.body, {
        childList: true,
        subtree: true,
      });
    }
  }

  if (document.body) {
    start();
  } else {
    document.addEventListener('DOMContentLoaded', start, { once: true });
  }
  // Interval backstop for SPA navigations that don't mutate <body>.
  setInterval(skip, 500);
})();
"#
}

/// Generic cosmetic belt-and-suspenders: hide the most common ad-slot
/// containers at document-start, before the content filter's CSS lands.
/// Conservative — only unambiguous ad-slot attribute/id shapes, never
/// generic layout, so it can't hide real content.
fn generic_ad_cosmetic_script() -> &'static str {
    r#"(function () {
  'use strict';
  var css =
    'ins.adsbygoogle, .adsbygoogle,' +
    'iframe[id^="google_ads_iframe"], iframe[id^="aswift_"],' +
    'div[id^="div-gpt-ad"], div[id^="google_ads_"],' +
    'div[data-ad-slot], div[data-ad-client],' +
    '[aria-label="Advertisement"], [aria-label="advertisement"]' +
    ' { display: none !important; }';
  var el = document.createElement('style');
  el.textContent = css;
  (document.head || document.documentElement).appendChild(el);
})();
"#
}

fn cache_dir() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|d| d.join("qos-webview").join("filters"))
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

fn load_and_compile() -> Result<AdFilters, String> {
    let texts: Vec<String> = LISTS
        .iter()
        .filter_map(|(name, url)| load_list(name, url))
        .collect();
    if texts.is_empty() {
        return Err("no filter lists available (offline with an empty cache)".into());
    }
    let version = source_version(&texts);
    let filter = super::adblock::compile_filter_lists_chunked(
        texts.iter().map(String::as_str),
        super::adblock::MAX_RULES_PER_FILTER,
    )
    .map_err(|e| e.to_string())?;
    Ok(AdFilters {
        version,
        chunks: Arc::new(filter.chunks),
        user_scripts: Arc::new(user_scripts()),
    })
}

/// FNV-1a over the concatenated source lists — the cache version tag.
fn source_version(texts: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for t in texts {
        for &b in t.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_set_is_the_webkit_clean_seven() {
        let names: Vec<&str> = LISTS.iter().map(|(n, _)| *n).collect();
        for expected in [
            "easylist.txt",
            "easyprivacy.txt",
            "easylist-cookie.txt",
            "ublock-filters.txt",
            "ublock-privacy.txt",
            "ublock-badware.txt",
            "ublock-resource-abuse.txt",
        ] {
            assert!(names.contains(&expected), "missing filter list {expected}");
        }
        // The two page-blanking lists must stay excluded.
        assert!(!names.contains(&"ublock-quick-fixes.txt"));
        assert!(!names.contains(&"adguard-base.txt"));
        for (_, url) in LISTS {
            assert!(url.starts_with("https://"), "non-https list URL: {url}");
        }
    }

    #[test]
    fn user_scripts_present_and_youtube_functional() {
        let scripts = user_scripts();
        assert!(!scripts.is_empty());
        let yt = scripts
            .iter()
            .find(|s| s.contains("ytp-ad"))
            .expect("a YouTube ad-skip script");
        assert!(yt.contains("ytp-ad-skip-button"), "no skip-button click");
        assert!(yt.contains("ad-showing"), "no ad-in-progress detection");
        assert!(yt.contains("currentTime"), "no fast-forward seek");
        assert!(
            yt.contains("MutationObserver") && yt.contains("setInterval"),
            "no SPA-navigation resilience"
        );
    }

    #[test]
    fn source_version_is_stable_and_order_sensitive() {
        let a = source_version(&["hello".into(), "world".into()]);
        let b = source_version(&["hello".into(), "world".into()]);
        let c = source_version(&["world".into(), "hello".into()]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
