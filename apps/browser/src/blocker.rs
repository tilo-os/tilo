//! Ad blocking, two ways: filter lists compiled to `WebKit` content-blocker JSON
//! and run natively in the engine's network process, and user scripts for what
//! declarative filters cannot do, such as scriptlets that skip `YouTube`'s ads.
//! Lists compile on a spawned thread, so a cold cache never delays first paint.

use std::path::PathBuf;
use std::time::Duration;

use ui::webview::{SharedEngine, WebViewCommand};

/// Filter lists to fetch, as `(cache file name, URL)`: uBlock Origin's default
/// set, restricted to the lists whose rules survive conversion to `WebKit`'s
/// declarative format.
///
/// `EasyList` and `EasyPrivacy` are the backbone, with the cookie list and uBO's
/// own supplements. Left out: uBO quick fixes and `AdGuard` Base, whose
/// `$replace`, `$redirect` and regex `$frame` rules become over-broad blocks
/// that blank a page's own document, and which churn daily.
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

/// Lists that stay out of [`LISTS`], by cache file name:
/// `apps/browser/examples/adblock_blank_repro.rs` reproduces what adding one
/// back does.
#[cfg(test)]
const EXCLUDED_LISTS: &[&str] = &["ublock-quick-fixes.txt", "adguard-base.txt"];

const CACHE_MAX_AGE: Duration = Duration::from_secs(604_800);

/// Load and compile the filter lists in the background and install them when
/// ready, sending `then` (the window's first navigation) once the engine has the
/// rules it can have at once. This is the declarative filter only; the scriptlet
/// path ([`user_scripts`]) is installed separately as user scripts.
pub fn spawn_filter_loader(engine: SharedEngine, then: Option<WebViewCommand>) {
    if engine.supports_native_filtering() {
        crate::native_blocker::start(&engine, then);
        return;
    }
    if let Some(then) = then {
        engine.send(then);
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
                    // Installed as several filters, since the full rule set
                    // exceeds WebKit's per-filter cap. The `version` (a hash of
                    // the source lists) lets the engine reuse compiled filters.
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

/// JavaScript user scripts for scriptlet-style blocking that the declarative
/// content filter cannot express.
///
/// Each is standalone and injected at document-start into every frame, and
/// defensive, so an injection into an unexpected page is harmless. The app sends
/// them to the engine; this is a pure catalogue, so it is testable alone.
pub fn user_scripts() -> Vec<String> {
    vec![
        youtube_ad_skip_script().to_string(),
        generic_ad_cosmetic_script().to_string(),
    ]
}

/// A `YouTube` ad skipper: the ads are stitched into the player's own origin, so no
/// network rule can drop them and only page-side script can.
///
/// While `#movie_player` carries `ad-showing`: click any known skip button; mute
/// the video (recording that we did, so the person's own mute survives), raise
/// `playbackRate` and seek to the end; then restore audio and rate once the ad
/// clears, and hide the in-player ad slots the content filter cannot reach.
fn youtube_ad_skip_script() -> &'static str {
    r"(function () {
  'use strict';
  var host = location.hostname;
  if (!/(^|\.)youtube(-nocookie)?\.com$/.test(host) &&
      !/(^|\.)youtube\.googleapis\.com$/.test(host)) {
    return;
  }

  // Hide ad overlays and banners on YouTube's same-origin markup.
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

    // Dismiss overlay and interstitial close buttons.
    var close = document.querySelector(
      '.ytp-ad-overlay-close-button, .ytp-ad-overlay-close-container'
    );
    if (close) {
      try { close.click(); } catch (e) {}
    }

    if (adShowing && video) {
      // Only mute if the person had not muted it.
      if (!video.muted) {
        video.muted = true;
        video.dataset.tiloAdMuted = '1';
      }
      try { video.playbackRate = 16; } catch (e) {}
      if (isFinite(video.duration) && video.duration > 0) {
        // Seek to the end, to fast-forward the rest of the ad.
        video.currentTime = video.duration;
      }
    } else if (video && video.dataset.tiloAdMuted === '1') {
      // Ad cleared: restore only what we changed.
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
  // Backstop for SPA navigations that never mutate <body>.
  setInterval(skip, 500);
})();
"
}

/// Hide the commonest ad-slot containers at document-start, before the content
/// filter's CSS lands.
///
/// Conservative on purpose: only unambiguously ad-shaped attributes and ids
/// (`AdSense`, Google Publisher Tag, `data-ad-*`), so real content is never hidden.
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

/// The list text: a fresh cache, else a fetch that updates the cache, else a
/// stale cache.
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
    // A stable id of the source rule set, so the engine can reuse the compiled
    // filters while the lists are unchanged.
    let version = source_version(&texts);
    let filter = ui::webview::adblock::compile_filter_lists_chunked(
        texts.iter().map(String::as_str),
        ui::webview::adblock::MAX_RULES_PER_FILTER,
    )
    .map_err(|e| e.to_string())?;
    Ok((version, filter))
}

/// FNV-1a over the concatenated source lists: the cache version tag.
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

    #[test]
    fn bundled_user_scripts_are_present_and_skip_youtube_ads() {
        {
            let scripts = user_scripts();
            assert!(!scripts.is_empty(), "expected at least one user script");
            for (i, s) in scripts.iter().enumerate() {
                assert!(!s.trim().is_empty(), "user script {i} is empty");
            }
        }

        {
            let scripts = user_scripts();
            let yt = scripts
                .iter()
                .find(|s| s.contains("ytp-ad"))
                .expect("a YouTube ad-skip script referencing ytp-ad");

            assert!(!yt.trim().is_empty(), "YouTube script is empty");
            assert!(yt.contains("ytp-ad-skip-button"), "no skip-button click");
            assert!(
                yt.to_lowercase().contains("skip"),
                "no skip logic in YouTube script"
            );
            assert!(yt.contains("ad-showing"), "no ad-in-progress detection");
            assert!(yt.contains("currentTime"), "no fast-forward seek");
            assert!(
                yt.contains("MutationObserver") && yt.contains("setInterval"),
                "no SPA-navigation resilience",
            );
        }
    }

    #[test]
    fn filter_list_set_covers_sources_and_excludes_page_blanking_lists() {
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
            for (_, url) in LISTS {
                assert!(url.starts_with("https://"), "non-https list URL: {url}");
            }
        }

        {
            // uBO quick fixes and AdGuard Base convert to over-broad `block`
            // rules that catch a page's own first-party document, so a
            // navigation goes permanently blank. They were reinstated once by
            // accident; this asserts their absence.
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
