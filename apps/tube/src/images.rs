//! Real image loading for video thumbnails, channel avatars and banners.

use std::sync::Arc;
use std::time::Duration;

use ui::{Fetcher, ImageStore};

pub fn image_fetcher() -> Fetcher {
    const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
    let client = tilo_net::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0")
        .timeout(Duration::from_secs(10))
        .build()
        .ok();
    Arc::new(move |url: &str| {
        let client = client.clone()?;
        let url = if let Some(rest) = url.strip_prefix("//") {
            format!("https://{rest}")
        } else {
            url.to_string()
        };
        ui::rt::app()
            .block_on(async move {
                let resp = client.get(url).send().await.ok()?.error_for_status().ok()?;
                resp.bytes_limited(MAX_IMAGE_BYTES).await.ok()
            })
            .ok()?
            .map(|b| b.to_vec())
    })
}

pub fn image_store() -> ImageStore {
    ImageStore::new(&image_fetcher())
}

#[derive(Clone, Copy)]
pub struct Imgs<'a>(pub Option<&'a ImageStore>);

impl Imgs<'_> {
    pub fn store_handle(&self) -> Option<ImageStore> {
        self.0.cloned()
    }
}
