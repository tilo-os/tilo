//! Built-in ad blocking. Two complementary mechanisms:
//!
//! 1. **Content filter** — fetch Brave/EasyList-syntax filter lists,
//!    cache them on disk, compile them to `WebKit` content-blocker JSON via
//!    [`ui::webview::adblock`], and hand the result to the engine
//!    ([`spawn_filter_loader`]). Blocking then runs natively inside the
//!    engine's network process — no per-request work in the app. This
//!    covers network blocking + cosmetic (`css-display-none`) hiding for
//!    ordinary sites.
//!
//! 2. **User scripts** — [`user_scripts`] returns JavaScript injected at
//!    document-start into every frame. `WebKit` content filters are purely
//!    *declarative* (block + hide only); they cannot run scriptlets. Sites
//!    that serve their ads same-origin and need JS to defeat — `YouTube`
//!    being the canonical case — are handled here instead. `user_scripts`
//!    is consumed separately by the app: the orchestrator sends each
//!    string to the engine as a document-start, all-frames user script.
//!
//! The content-filter path happens on a spawned thread so a cold cache /
//! slow network never delays first paint; pages loaded before the filter
//! compiles simply go unfiltered once.

use std::path::PathBuf;
use std::time::Duration;

use ui::webview::{SharedEngine, WebViewCommand};

/// Filter lists to fetch, as `(cache file name, URL)`.
///
/// The canonical uBlock-Origin-default blocking set, restricted to lists
/// whose rules survive adblock-rust's conversion to `WebKit`'s *declarative*
/// content-blocker format cleanly:
///
/// - **`EasyList`** / **`EasyPrivacy`** — the network + tracker backbone.
/// - **`EasyList` Cookie List** — cookie/consent banner annoyances.
/// - **uBlock Origin** `filters`, `privacy`, `badware`, `resource-abuse` —
///   uBO's own maintained supplements.
///
/// Deliberately excluded: uBO **quick-fixes** and **`AdGuard` Base**. Both
/// lean on rule types `WebKit` content blocking cannot express — `$replace`
/// (response-body rewriting), `$redirect`, and raw-regex `$frame` filters
/// spanning a huge URL range. adblock-rust's lossy conversion turns some
/// of those into over-broad `block` rules that catch a page's *own*
/// first-party document/scripts during load, blanking it. They also churn
/// daily, so a fresh compile can regress unpredictably. `EasyList` + uBO
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

/// Lists that must stay out of [`LISTS`], by cache-file name.
///
/// Adding either one back reintroduces the blank-page-on-load regression
/// described above; `apps/browser/examples/adblock_blank_repro.rs`
/// reproduces it. Named here so the invariant is testable rather than
/// only stated in prose.
#[cfg(test)]
const EXCLUDED_LISTS: &[&str] = &["ublock-quick-fixes.txt", "adguard-base.txt"];

/// Re-download a cached list after this age.
const CACHE_MAX_AGE: Duration = Duration::from_secs(604_800);

/// Load + compile the filter lists in the background and install the
/// compiled content filter into `engine` when ready.
///
/// This is only the declarative content-filter path. The scriptlet path
/// ([`user_scripts`]) is installed separately by the app, since it maps
/// to a different engine capability (user scripts, not content filters).
///
/// If the OS refuses to spawn the background loader thread (e.g. the
/// process is out of resources), ad blocking is simply left disabled for
/// this session rather than taking the whole app down with it.
pub fn spawn_filter_loader(engine: SharedEngine) {
    if engine.supports_native_filtering() {
        crate::native_blocker::start(engine);
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("filter-loader".into())
        .spawn(move || {
            match load_and_compile() {
                Ok((version, filter)) => {
                    tracing::info!(
                        rules = filter.rule_count,
                        chunks = filter.chunks.len(),
                        skipped = filter.skipped,
                        "content filter compiled"
                    );
                    // Installed as several filters — the full rule set exceeds
                    // WebKit's per-filter cap (see `MAX_RULES_PER_FILTER`). The
                    // `version` (hash of the source lists) lets the engine reuse
                    // previously compiled filters when the lists are unchanged.
                    engine.send(WebViewCommand::SetContentFilters {
                        version,
                        chunks: std::sync::Arc::new(filter.chunks),
                    });
                }
                Err(err) => tracing::warn!("ad blocking unavailable: {err}"),
            }
            #[cfg(test)]
            if let Some(path) = std::env::var_os("TILO_TEST_FILTER_LOADER_DONE") {
                let _ = std::fs::write(path, b"done\n");
            }
        });
    if let Err(err) = spawned {
        tracing::warn!("ad blocking unavailable: could not spawn filter-loader thread: {err}");
    }
}

/// JavaScript user scripts for scriptlet-style blocking that the
/// declarative content filter cannot express.
///
/// Each entry is a standalone, self-contained script meant to be injected
/// at **document-start** into **all frames**. They are defensive (feature
/// detection, no throwing) so an injection into an unexpected page is
/// harmless. The app/orchestrator owns delivery to the engine — this
/// function is a pure catalogue with no engine dependency, which keeps it
/// trivially unit-testable.
///
/// The `YouTube` ad-skip script is the priority: `YouTube` stitches video ads
/// into the same origin as the player, so no network rule can drop them —
/// only page-side JS can mute, fast-forward, and click past them.
pub fn user_scripts() -> Vec<String> {
    vec![
        youtube_ad_skip_script().to_string(),
        generic_ad_cosmetic_script().to_string(),
    ]
}

/// Robust `YouTube` video-ad skipper.
///
/// Detection + skip strategy (kept resilient to `YouTube`'s frequent markup
/// churn by matching on several class/selector variants and re-running on
/// every DOM mutation plus a short interval, so it survives SPA
/// navigation between videos without a full reload):
///
/// - **Ad in progress?** `#movie_player` carries the `ad-showing` class
///   while a video ad plays.
/// - **Skip button** — click any of the known skip-button variants the
///   moment it appears (`.ytp-ad-skip-button`,
///   `.ytp-ad-skip-button-modern`, `.ytp-skip-ad-button`, …).
/// - **Unskippable portion** — mute the `<video>` (recording that *we*
///   muted it so the user's own mute is never clobbered), crank
///   `playbackRate`, and seek `currentTime` to `duration` to fast-forward
///   the ad to its end; restore audio + rate once the ad clears.
/// - **Overlays/banners** — inject CSS hiding the in-player ad modules,
///   overlay slots, and feed/masthead ad renderers that the declarative
///   filter can't target on this same-origin markup, and click the
///   overlay close button.
fn youtube_ad_skip_script() -> &'static str {
    r"(function () {
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
    if (document.getElementById('tilo-yt-adblock')) return;
    var el = document.createElement('style');
    el.id = 'tilo-yt-adblock';
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
        video.dataset.tiloAdMuted = '1';
      }
      try { video.playbackRate = 16; } catch (e) {}
      if (isFinite(video.duration) && video.duration > 0) {
        // Seek to the end: fast-forwards the unskippable portion away.
        video.currentTime = video.duration;
      }
    } else if (video && video.dataset.tiloAdMuted === '1') {
      // Ad cleared — restore only what we changed.
      video.muted = false;
      try { video.playbackRate = 1; } catch (e) {}
      delete video.dataset.tiloAdMuted;
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
"
}

/// Generic cosmetic belt-and-suspenders: hide the most common ad-slot
/// containers at document-start, before the content filter's CSS lands.
///
/// Conservative on purpose — it only matches attribute/id shapes that are
/// unambiguously ad slots (`AdSense`, Google Publisher Tag, generic
/// `data-ad-*` slots), never generic layout, so it can't hide real
/// content. Injecting the rule from JS lets it apply inside frames the
/// declarative filter may reach later.
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

pub fn cache_dir() -> Option<PathBuf> {
    crate::storage::cache_dir().map(|d| d.join("filters"))
}

pub fn read_cached_list(path: &std::path::Path) -> Option<String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(8 * 1024 * 1024 + 1)
        .read_to_string(&mut text)
        .ok()?;
    (text.len() <= 8 * 1024 * 1024).then_some(text)
}

/// Return the list text: fresh cache if young enough, otherwise fetch
/// (updating the cache), otherwise a stale cache beats nothing.
pub fn load_list(name: &str, url: &str) -> Option<String> {
    let path = cache_dir().map(|d| d.join(name));

    if let Some(p) = &path
        && let Ok(meta) = std::fs::metadata(p)
    {
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age < CACHE_MAX_AGE);
        if fresh && let Some(text) = read_cached_list(p) {
            return Some(text);
        }
    }

    let fetch = || -> Result<String, Box<dyn std::error::Error>> {
        use std::io::Read as _;
        let client = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(15))
            .build()?;
        let response = client.get(url).send()?.error_for_status()?;
        let mut text = String::new();
        response
            .take(8 * 1024 * 1024 + 1)
            .read_to_string(&mut text)?;
        if text.len() > 8 * 1024 * 1024 {
            return Err("filter list exceeds 8 MiB".into());
        }
        Ok(text)
    };
    match fetch() {
        Ok(text) => {
            if let Some(p) = &path
                && let Some(dir) = p.parent()
            {
                match crate::storage::private_root_write_guard(dir) {
                    Ok(_private_write) => {
                        let _ =
                            tilo_files::native::atomic::write_creating_parent(p, text.as_bytes());
                    }
                    Err(error) => tracing::warn!(%error, "filter cache is unavailable"),
                }
            }
            Some(text)
        }
        Err(err) => {
            tracing::warn!("fetching {url} failed: {err}");
            path.and_then(|p| read_cached_list(&p))
        }
    }
}

fn load_and_compile() -> Result<(u64, ui::webview::adblock::ChunkedFilter), String> {
    #[cfg(test)]
    let override_list = std::env::var("TILO_TEST_FILTER_LIST_URL").ok();
    #[cfg(test)]
    let lists: Vec<(&str, &str)> = override_list
        .as_deref()
        .map_or_else(|| LISTS.to_vec(), |url| vec![("fixture.txt", url)]);
    #[cfg(not(test))]
    let lists = LISTS;
    let texts: Vec<String> = lists
        .iter()
        .filter_map(|(name, url)| load_list(name, url))
        .collect();
    if texts.is_empty() {
        return Err("no filter lists available (offline with an empty cache)".into());
    }
    // A stable id of the *source* rule set, so the engine can reuse the
    // previously compiled WebKit filters when the lists haven't changed.
    let version = source_version(&texts);
    let filter = ui::webview::adblock::compile_filter_lists_chunked(
        texts.iter().map(String::as_str),
        ui::webview::adblock::MAX_RULES_PER_FILTER,
    )
    .map_err(|e| e.to_string())?;
    Ok((version, filter))
}

/// FNV-1a over the concatenated source lists — the cache version tag.
fn source_version(texts: &[String]) -> u64 {
    let mut hasher = fnv::FnvHasher::default();
    for t in texts {
        std::hash::Hasher::write(&mut hasher, t.as_bytes());
    }
    std::hash::Hasher::finish(&hasher)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bundled user scripts.
    #[test]
    fn bundled_user_scripts_are_present_and_skip_youtube_ads() {
        // Phase `user_scripts_are_present_and_non_empty`.
        {
            let scripts = user_scripts();
            assert!(!scripts.is_empty(), "expected at least one user script");
            for (i, s) in scripts.iter().enumerate() {
                assert!(!s.trim().is_empty(), "user script {i} is empty");
            }
        }

        // Phase `youtube_ad_skip_script_is_included_and_functional`.
        {
            let scripts = user_scripts();
            let yt = scripts
                .iter()
                .find(|s| s.contains("ytp-ad"))
                .expect("a YouTube ad-skip script referencing ytp-ad");

            assert!(!yt.trim().is_empty(), "YouTube script is empty");
            // References the skip logic the content filter can't express.
            assert!(yt.contains("ytp-ad-skip-button"), "no skip-button click");
            assert!(
                yt.to_lowercase().contains("skip"),
                "no skip logic in YouTube script"
            );
            // Detects an ad in progress and fast-forwards past it.
            assert!(yt.contains("ad-showing"), "no ad-in-progress detection");
            assert!(yt.contains("currentTime"), "no fast-forward seek");
            // Survives SPA navigation.
            assert!(
                yt.contains("MutationObserver") && yt.contains("setInterval"),
                "no SPA-navigation resilience",
            );
        }
    }

    /// Filter list selection.
    #[test]
    fn filter_list_set_covers_sources_and_excludes_page_blanking_lists() {
        // Phase `filter_list_set_covers_the_expected_sources`.
        {
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
            // Every entry points at an https raw source.
            for (_, url) in LISTS {
                assert!(url.starts_with("https://"), "non-https list URL: {url}");
            }
        }

        // Phase `page_blanking_filter_lists_stay_excluded`.
        {
            // uBO quick-fixes and AdGuard Base convert to over-broad WebKit
            // `block` rules that catch a page's own first-party document, so a
            // reload or fresh navigation goes permanently blank. They were
            // removed once and silently reinstated; assert their absence so the
            // next reinstatement fails here instead of in the browser.
            let names: Vec<&str> = LISTS.iter().map(|(n, _)| *n).collect();
            for banned in EXCLUDED_LISTS {
                assert!(
                    !names.contains(banned),
                    "{banned} blanks pages under WebKit content blocking — it must stay out of LISTS",
                );
            }
        }
    }
}
