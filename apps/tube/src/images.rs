//! Real image loading for video thumbnails, channel avatars and banners.
//!
//! Wires the toolkit's [`ui::ImageStore`] (async fetch + decode + GPU-image
//! cache, keyed by URL) to a blocking `reqwest` transport so the real
//! i.ytimg.com / yt3.googleusercontent.com images yt-dlp reports are fetched,
//! decoded and painted. Until a URL's image is ready — and always on the
//! hermetic `--shots`/offline path — the caller shows the seed gradient /
//! initials fallback.

use std::sync::Arc;
use std::time::Duration;

use ui::{Fetcher, ImageStore};
use vello::peniko::ImageData as PenikoImage;

/// A blocking-`reqwest` transport for the [`ImageStore`]. Runs on the store's
/// background threads, so it may block on I/O. A browser-ish User-Agent avoids
/// the occasional 403 the image CDNs return to unknown clients.
pub fn image_fetcher() -> Fetcher {
    let client = tilo_net::http::blocking_client()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0")
        .timeout(Duration::from_secs(10))
        .build()
        // A client that cannot be built leaves every image on its fallback;
        // a fetcher must not panic.
        .ok();
    Arc::new(move |url: &str| {
        let client = client.as_ref()?;
        // Promote protocol-relative CDN URLs (`//yt3.ggpht.com/…`) to https so
        // the request has a scheme — some sources hand these back and reqwest
        // rejects them otherwise, silently blanking the image.
        let url = if let Some(rest) = url.strip_prefix("//") {
            format!("https://{rest}")
        } else {
            url.to_string()
        };
        client
            .get(&url)
            .send()
            .ok()?
            .error_for_status()
            .ok()?
            .bytes()
            .ok()
            .map(|b| b.to_vec())
    })
}

pub fn image_store() -> ImageStore {
    ImageStore::new(&image_fetcher())
}

/// A cheap, `Copy` handle the screen builders thread down to the image-bearing
/// widgets. `None` on the hermetic offline / `--shots` path, so every lookup
/// returns `None` and the procedural gradient / initials fallback shows.
#[derive(Clone, Copy)]
pub struct Imgs<'a>(pub Option<&'a ImageStore>);

impl Imgs<'_> {
    /// The hermetic no-store handle (offline / `--shots`).
    pub fn none() -> Self {
        Imgs(None)
    }

    /// The decoded image for `url`, or `None` while loading / failed / offline.
    /// Kicks off an async load the first time a URL is seen.
    pub fn get(&self, url: &str) -> Option<PenikoImage> {
        match self.0 {
            Some(store) if !url.is_empty() => store.get(url),
            _ => None,
        }
    }

    /// An owned clone (an `Arc` bump) for a widget that resolves its own image
    /// lazily in `sync` rather than at build time: a freshly-decoded image
    /// then folds into the widget next frame without a whole-tree rebuild.
    /// `None` on the hermetic offline / `--shots` path.
    pub fn store_handle(&self) -> Option<ImageStore> {
        self.0.cloned()
    }

    /// In-flight image loads, so the app can keep redrawing until they settle.
    pub fn pending(&self) -> usize {
        self.0.map_or(0, ui::ImageStore::pending)
    }
}
