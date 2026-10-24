//! Reader view: the page's article in the reader every Tilo app shares,
//! over the page it came from (the page is left as it was, so closing the
//! reader shows it unchanged).

use vello::Scene;
use vello::kurbo::Affine;
use winit::event::{ElementState, WindowEvent};

use super::{BrowserOperation, BrowserWidget};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::webview::PageReply;

/// The article a page handed over for reader view, as the shared reader
/// shows it: lowered from its markup, under the page's title, site, author
/// and date. `None` when the page sent none (an engine without reader
/// view).
pub(super) fn reader_document(reply: &PageReply) -> Option<ui::reading::Document> {
    let data = reply.data.as_ref()?;
    let html = data.get("html")?.as_str()?;
    let text = |key: &str| {
        data.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    // The page's own article finder found the block; the shared extraction
    // sets aside what is plainly not the text inside it, and lowers it.
    let mut document = ui::reading::extract::article(html, &reply.url).unwrap_or_else(|| {
        ui::reading::extract::fragment(
            html,
            ui::reading::extract::Options {
                base_url: Some(&reply.url),
                images: ui::reading::extract::Images::Shown,
            },
        )
    });
    let title = text("title");
    if !title.is_empty() {
        document.title = title;
    }
    let site = text("site");
    if !site.is_empty() {
        document.site = site;
    }
    let byline = text("byline");
    if !byline.is_empty() {
        document.byline = byline;
    }
    let language = text("lang");
    if !language.is_empty() {
        document.language = language;
    }
    if let Ok(published) = chrono::DateTime::parse_from_rfc3339(&text("published")) {
        document.published = Some(published.timestamp());
    }
    document.url.clone_from(&reply.url);
    (!document.blocks.is_empty()).then_some(document)
}

/// The largest picture reader view reads.
const READER_PICTURE_BYTES: u64 = 12 * 1024 * 1024;

/// Where reader view's pictures come from: the web, as the page fetched
/// them, read off the UI thread.
fn reader_images() -> ui::ImageStore {
    let fetch: ui::Fetcher = std::sync::Arc::new(|url: &str| {
        use std::io::Read as _;
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return None;
        }
        let client = tilo_net::http::blocking_client()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .ok()?;
        let response = client.get(url).send().ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut bytes = Vec::new();
        response
            .take(READER_PICTURE_BYTES)
            .read_to_end(&mut bytes)
            .ok()?;
        (!bytes.is_empty()).then_some(bytes)
    });
    ui::ImageStore::with_max_dimension(&fetch, 2048)
}

/// The reader for the article `reply` handed over, set in the person's
/// reading preferences; `None` when the page sent none.
pub(super) fn open(reply: &PageReply) -> Option<Box<ui::reading::ReaderView>> {
    let document = reader_document(reply)?;
    let mut view = ui::reading::ReaderView::new();
    view.set_preferences(ui::storage::Settings::<ui::reading::ReadingPreferences>::shared().get());
    view.set_images(reader_images());
    let key = document.url.clone();
    view.set_document(&key, std::rc::Rc::new(document), None);
    Some(Box::new(view))
}

impl BrowserWidget {
    /// In reader view the reader takes the pointer over the page and the
    /// reading keys (Space pages, Home and End, Escape) while the page has
    /// the focus. Returns whether it took the event.
    pub(super) fn reader_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if let Some(url) = self.pending_reader_link.take()
            && let Err(error) = self.perform_browser_operation(BrowserOperation::Navigate { url })
        {
            tracing::warn!(%error, "a link chosen in reader view did not open");
        }
        let Some(area) = self.find_rect("browser.surface") else {
            return false;
        };
        let page_focused = self.control_focused("browser.surface");
        let pointer_over = area.contains(self.cursor);
        let Some(reader) = self.reader.as_mut() else {
            return false;
        };
        match event {
            WindowEvent::KeyboardInput { event: key, .. }
                if key.state == ElementState::Pressed && page_focused =>
            {
                reader.handle_key(&key.logical_key, ctx.modifiers.state())
            }
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::MouseInput { .. }
                if pointer_over =>
            {
                reader.handle_event(event, ctx);
                // Nothing reaches the page under the reader.
                true
            }
            _ => false,
        }
    }

    /// In reader view, paint the page's article over the page.
    pub(super) fn paint_reader(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.reader_view {
            self.reader = None;
        }
        if let Some(area) = self.find_rect("browser.surface")
            && let Some(reader) = self.reader.as_mut()
        {
            reader.paint(scene, transform, text_renderer, theme, area);
            for event in reader.take_events() {
                if let ui::reading::ReaderEvent::OpenLink(url) = event {
                    self.pending_reader_link = Some(url);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The article a page hands over reads in the shared reader, lowered by the same extraction
    /// Feeds uses, under the page's title, site and author, with its lists and pictures kept.
    #[test]
    fn reader_view_sets_the_pages_article_in_the_shared_reader() {
        let paragraph = "Prose the page carries, long enough to read as an article, with \
                         commas, clauses, and sentences that people write.";
        let body = format!("<p>{paragraph}</p>").repeat(6);
        let html = format!(
            "<article><h1>Story</h1>{body}<ol><li>One</li><li>Two</li></ol>\
             <img src=\"https://news.test/a.jpg\" width=\"800\" height=\"500\" alt=\"A view\"></article>"
        );
        let reply = PageReply {
            url: "https://news.test/story".into(),
            message: "Reader view".into(),
            outline: None,
            data: Some(serde_json::json!({
                "reader": true,
                "html": html,
                "title": "Story",
                "site": "News",
                "byline": "Ada",
                "lang": "en",
            })),
            navigation: ui::webview::PageNavigation::None,
            loading: false,
        };
        let document = reader_document(&reply).expect("an article");
        assert_eq!(document.title, "Story");
        assert_eq!(document.site, "News");
        assert_eq!(document.byline, "Ada");
        assert_eq!(document.url, "https://news.test/story");
        assert!(document.blocks.iter().any(|block| matches!(
            block,
            ui::reading::Block::ListItem {
                ordered: true,
                index: 2,
                ..
            }
        )));
        assert!(document.blocks.iter().any(|block| matches!(
            block,
            ui::reading::Block::Figure { image, .. } if image.width == Some(800)
        )));
        // An engine that hands nothing over shows the page as it is.
        let bare = PageReply {
            data: Some(serde_json::json!({"reader": true})),
            ..reply
        };
        assert!(reader_document(&bare).is_none());
    }
}
