//! The content column: the open article in the shared reader.
//!
//! An article opens in [`ui::reading::ReaderView`], set in the person's
//! reading preferences, at once from what the library holds (the markup the
//! feed sent, lowered as it opens). Its own page is fetched beside it, its
//! article found the way reader modes find it, and kept for reading offline;
//! when that holds more of the article than the feed sent, it takes the
//! excerpt's place without moving the passage on screen. Where the person
//! is in each article is kept in their own record and restored when it
//! opens again, on any of their devices.

use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::AccessNode;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::reading::{Block, Document, ReaderEvent, ReaderPalette, ReadingPlace, Span};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::find_bar::{FindMatch, FindOptions, Findable};
use ui::widget::{EmptyState, EmptyStateContent, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::app::{Core, Shared};
use crate::fetch::FetchRequest;
use crate::model::{Article, BlockKind, ContentBlock, Feed, Id, Passage};

/// A leaf that fills its column, registered for the host's sweeps.
pub(crate) fn fill_leaf<W: Widget + 'static>(
    taffy: &mut TaffyTree<()>,
    node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    widget: &mut W,
) -> NodeId {
    let node = taffy.leaf_node(Style {
        size: taffy::Size::from_percent(1.0, 1.0),
        ..Default::default()
    });
    node_map.insert(node, std::ptr::from_mut::<dyn Widget>(widget));
    node
}

/// What a tag's link in an article's header starts with: choosing it shows
/// the articles carrying the tag.
const TAG_LINK: &str = "feeds-tag:";

/// How often a reading position is kept while the person reads.
const KEEP_PLACE_EVERY: Duration = Duration::from_secs(2);

/// The document an article reads as before its page has been read: what
/// the feed sent, under the article's title, author, site and date.
pub(crate) fn feed_document(article: &Article, feed: Option<&Feed>) -> Document {
    let mut document = if article.content.trim().is_empty() {
        Document {
            blocks: earlier_blocks(&article.blocks),
            ..Document::default()
        }
    } else {
        ui::reading::extract::fragment(
            &article.content,
            ui::reading::extract::Options {
                base_url: Some(article.url.as_str()).filter(|url| !url.is_empty()),
                images: ui::reading::extract::Images::Shown,
            },
        )
    };
    dress(&mut document, article, feed);
    document
}

/// Give `document` the article's own title, author, site and date: the
/// feed names them better than a page's markup does.
fn dress(document: &mut Document, article: &Article, feed: Option<&Feed>) {
    document.title.clone_from(&article.title);
    if !article.author.trim().is_empty() {
        article.author.trim().clone_into(&mut document.byline);
    }
    document.site = feed.map(Feed::shown_title).unwrap_or_default();
    document.published = (article.published > 0).then_some(article.published);
    if !article.url.is_empty() {
        document.url.clone_from(&article.url);
    }
    // Its tags, each a link to the articles carrying it.
    let tags = article.tag_list();
    if !tags.is_empty() {
        let mut spans = vec![Span::plain("Tagged")];
        for (index, tag) in tags.iter().enumerate() {
            let text = if index + 1 < tags.len() {
                format!("{tag},")
            } else {
                tag.clone()
            };
            spans.push(Span {
                link: Some(std::sync::Arc::from(format!("{TAG_LINK}{tag}"))),
                ..Span::plain(text).spaced()
            });
        }
        document.header = vec![Block::Meta { spans }];
    }
}

/// Blocks a refresh stored before articles kept their markup (and a Gemini
/// capsule's), as the reader sets them.
fn earlier_blocks(blocks: &[ContentBlock]) -> Vec<Block> {
    blocks
        .iter()
        .map(|block| {
            let spans = vec![Span::plain(block.text.clone())];
            match block.kind {
                BlockKind::Heading1 => Block::Heading { level: 1, spans },
                BlockKind::Heading2 => Block::Heading { level: 2, spans },
                BlockKind::Heading3 => Block::Heading { level: 3, spans },
                BlockKind::Paragraph => Block::Paragraph { spans },
                BlockKind::Bullet => Block::ListItem {
                    spans,
                    ordered: false,
                    index: 1,
                    depth: 0,
                },
                BlockKind::Quote => Block::Quote { spans },
                BlockKind::Code => Block::Code {
                    text: block.text.clone(),
                    language: None,
                },
                BlockKind::Link => Block::Paragraph {
                    spans: vec![Span {
                        link: block.href.as_deref().map(std::sync::Arc::from),
                        ..Span::plain(block.text.clone())
                    }],
                },
                BlockKind::Divider => Block::Separator,
            }
        })
        .collect()
}

/// Whether the article found on its page says more than the feed sent: a
/// feed that sends only an excerpt is replaced, one that sends the whole
/// article is kept.
pub(crate) fn fuller(page: &Document, feed: &Document) -> bool {
    let page_words = page.word_count();
    let feed_words = feed.word_count();
    page_words >= 80 && page_words * 10 >= feed_words * 12
}

/// What the reader keeps of each article's documents.
#[derive(Default)]
pub(crate) struct Articles {
    /// What each opened article reads as from its feed, with a signature
    /// of what it was made from.
    feed: HashMap<Id, (u64, Rc<Document>)>,
    /// The articles found on their pages that say more than their feeds.
    full: HashMap<Id, Rc<Document>>,
    /// The articles whose pages were asked for.
    asked: std::collections::HashSet<Id>,
}

impl Core {
    /// The document the article `id` reads as now: its whole text once its
    /// page was read and says more, else what its feed sent.
    pub(crate) fn article_document(&mut self, id: &Id) -> Option<Rc<Document>> {
        if let Some(full) = self.articles.full.get(id) {
            return Some(Rc::clone(full));
        }
        let article = self.store.article(id)?;
        let signature = made_from(article.item());
        if let Some((made, feed)) = self.articles.feed.get(id)
            && *made == signature
        {
            return Some(Rc::clone(feed));
        }
        let feed = self
            .store
            .feed(&article.feed_id)
            .map(|feed| feed.item().clone());
        let document = Rc::new(feed_document(article.item(), feed.as_ref()));
        self.articles
            .feed
            .insert(id.clone(), (signature, Rc::clone(&document)));
        Some(document)
    }

    /// The library changed: an article's documents follow what it holds now,
    /// and the open article's highlights are painted as they are.
    pub(crate) fn forget_documents(&mut self) {
        let store = Rc::clone(&self.store);
        self.articles.feed.retain(|id, (made, _)| {
            store
                .article(id)
                .is_some_and(|article| made_from(article.item()) == *made)
        });
        self.articles
            .full
            .retain(|id, _| store.article(id).is_some());
        if let Some(id) = &self.selected
            && self.reader.key() == Some(id.to_string().as_str())
        {
            self.reader.set_marks(store.marks_of(id));
        }
    }

    /// Fetch the pages of the articles a refresh just brought in, now that
    /// the library holds them.
    pub(crate) fn fetch_new_pages(&mut self) {
        if self.prefetch.is_empty() {
            return;
        }
        let store = Rc::clone(&self.store);
        let mut waiting = Vec::new();
        for (feed, url) in std::mem::take(&mut self.prefetch) {
            let held = store
                .article_rows()
                .find(|article| article.feed_id == feed && article.url == url)
                .map(|article| article.id.clone());
            match held {
                Some(id) => self.ask_for_page(&id),
                // Not landed yet: the next pass asks.
                None if self.pending_landings.iter().any(|landing| {
                    landing.fetched.feed == feed
                        && landing.fetched.articles.iter().any(|a| a.url == url)
                }) =>
                {
                    waiting.push((feed, url));
                }
                None => {}
            }
        }
        self.prefetch = waiting;
    }

    /// Ask for the page of the article `id`, once.
    pub(crate) fn ask_for_page(&mut self, id: &Id) {
        if self.articles.asked.contains(id) {
            return;
        }
        let Some(url) = self
            .store
            .article(id)
            .map(|article| article.url.clone())
            .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        else {
            return;
        };
        // A capture and a test stay off the network.
        if !self.live {
            return;
        }
        self.articles.asked.insert(id.clone());
        self.worker().send(FetchRequest::Page {
            article: id.clone(),
            url,
        });
    }

    /// The article `id`'s page was read: keep its article when it says more
    /// than the feed sent.
    pub(crate) fn page_arrived(&mut self, id: &Id, page: Option<Document>) {
        let Some(mut page) = page else {
            return;
        };
        let Some(article) = self.store.article(id).map(|article| article.item().clone()) else {
            return;
        };
        let feed = self
            .store
            .feed(&article.feed_id)
            .map(|feed| feed.item().clone());
        let byline = std::mem::take(&mut page.byline);
        dress(&mut page, &article, feed.as_ref());
        if page.byline.is_empty() {
            page.byline = byline;
        }
        let Some(from_feed) = self.article_document(id) else {
            return;
        };
        if fuller(&page, &from_feed) {
            self.articles.full.insert(id.clone(), Rc::new(page));
        }
    }

    /// Show the open article in the reader, at the place the person left it.
    fn show_open_article(&mut self) {
        let Some(id) = self
            .selected
            .clone()
            .filter(|id| self.store.article(id).is_some())
        else {
            if self.reader.key().is_some() {
                self.keep_place();
                self.reader.clear();
            }
            return;
        };
        let key = id.to_string();
        let opening = self.reader.key() != Some(key.as_str());
        if opening {
            self.keep_place();
        }
        let Some(document) = self.article_document(&id) else {
            return;
        };
        let place = if opening {
            self.positions
                .as_ref()
                .and_then(|positions| positions.with(|state| state.place(&key)))
        } else {
            None
        };
        self.reader.set_document(&key, document, place);
        if opening {
            self.reader.set_marks(self.store.marks_of(&id));
            self.ask_for_page(&id);
        }
    }

    /// Keep where the person is in the open article, in their own record.
    pub(crate) fn keep_place(&mut self) {
        let (Some(key), Some(place)) = (self.reader.key().map(str::to_owned), self.reader.place())
        else {
            return;
        };
        let Some(positions) = &self.positions else {
            return;
        };
        let unchanged = self
            .kept_place
            .as_ref()
            .is_some_and(|(kept_key, kept, _)| *kept_key == key && same_place(kept, &place));
        if unchanged {
            return;
        }
        if let Err(error) =
            positions.invoke(&ui::reading_positions::ReadingPositions::KEEP_PLACE.with((
                key.clone(),
                usize::try_from(place.block).unwrap_or(usize::MAX),
                place.fraction,
                place.progress,
            )))
        {
            tracing::warn!(%error, "the reading position was not kept");
        }
        self.kept_place = Some((key, place, ui::time::now()));
    }

    /// Keep the place now and then while the person reads.
    fn keep_place_while_reading(&mut self) {
        let due = self.kept_place.as_ref().is_none_or(|(_, _, at)| {
            ui::time::now().saturating_duration_since(*at) >= KEEP_PLACE_EVERY
        });
        if due {
            self.keep_place();
        }
    }

    /// Carry out what the person asked of the reader.
    fn reader_events(&mut self) {
        for event in self.reader.take_events() {
            match event {
                ReaderEvent::OpenLink(url) => match url.strip_prefix(TAG_LINK) {
                    Some(tag) => self.set_filter(crate::resource::tag_filter(tag)),
                    None => self.open_url(&url),
                },
                ReaderEvent::Highlight { passage, color } => {
                    if let Some(passage) = self.passage(passage) {
                        self.pending_highlights
                            .push((passage, color, String::new()));
                    }
                }
                ReaderEvent::Note { passage } => {
                    if let Some(passage) = self.passage(passage) {
                        self.open_note_editor(passage);
                    }
                }
                ReaderEvent::Share { passage } => {
                    let link = self
                        .open_article()
                        .map(|article| (article.title.clone(), article.url.clone()));
                    let text = match link {
                        Some((title, url)) if !url.is_empty() => {
                            format!("\u{201c}{}\u{201d}\n{title}\n{url}", passage.text)
                        }
                        _ => format!("\u{201c}{}\u{201d}", passage.text),
                    };
                    ui::text_selection::clipboard_copy(&text);
                    self.set_toast("Copied the passage with its link");
                }
            }
        }
    }

    /// The reader's passage as a passage of the open article.
    fn passage(&self, passage: ui::reading::Passage) -> Option<Passage> {
        Some(Passage {
            article: self.selected.clone()?,
            block: usize::try_from(passage.block).ok()?,
            start: usize::try_from(passage.start).ok()?,
            end: usize::try_from(passage.end).ok()?,
            excerpt: passage.text,
        })
    }

    pub(crate) fn paint_reader(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        self.reader_rect = area;
        self.reader.set_preferences(self.prefs);
        // Pictures load once the window shows an article.
        if !self.images_attached && self.live {
            self.reader.set_images(crate::fetch::image_store());
            self.images_attached = true;
        }
        self.show_open_article();
        if self.open_article().is_none() {
            let palette = ReaderPalette::resolve(self.prefs.paper, theme);
            if let Some(page) = palette.page {
                scene.fill(vello::peniko::Fill::NonZero, t, page, None, &area);
            }
            // An empty library has one empty state, in the list.
            if !self.store.feeds.is_empty() {
                let page_theme = palette.page_theme(theme);
                let content = EmptyStateContent {
                    icon: Some(Icon::FileText),
                    ..EmptyStateContent::new("No article selected")
                };
                EmptyState::paint(
                    &mut PaintCtx::new(scene, t, text, &page_theme),
                    area,
                    &content,
                    Point::new(f64::NAN, f64::NAN),
                );
            }
            return;
        }
        self.reader.paint(scene, t, text, theme, area);
        self.reader_events();
        self.keep_place_while_reading();
    }

    /// Route `event` to the reader. Returns whether the reader used it.
    pub(crate) fn reader_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if self.open_article().is_none() {
            return false;
        }
        let used = self.reader.handle_event(event, ctx);
        self.reader_events();
        used
    }

    pub(crate) fn reader_accessibility(&self) -> AccessNode {
        self.reader
            .accessibility("No article selected")
            .with_bounds(self.reader_rect)
    }
}

/// A signature of what an article's document is made from: its markup,
/// its stored blocks, title, author and tags.
fn made_from(article: &Article) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    article.content.hash(&mut hasher);
    article.blocks.len().hash(&mut hasher);
    article.title.hash(&mut hasher);
    article.author.hash(&mut hasher);
    article.tag_list().hash(&mut hasher);
    hasher.finish()
}

/// Whether two places show the same passage.
fn same_place(a: &ReadingPlace, b: &ReadingPlace) -> bool {
    a.block == b.block && (a.fraction - b.fraction).abs() < 0.05
}

/// The content column widget: paints the open article from the shared core.
pub(crate) struct ReaderPane {
    core: Shared,
    rect: Rect,
}

impl ReaderPane {
    pub(crate) fn new(core: &Shared) -> Self {
        Self {
            core: Rc::clone(core),
            rect: Rect::ZERO,
        }
    }

    /// Whether an article is open to search in.
    fn has_article(&self) -> bool {
        self.core
            .try_borrow()
            .is_ok_and(|core| core.open_article().is_some())
    }
}

impl Findable for ReaderPane {
    fn find(&mut self, query: &str, options: FindOptions) -> Vec<FindMatch> {
        self.core.borrow_mut().reader.find(query, options)
    }

    fn reveal(&mut self, index: usize) {
        self.core.borrow_mut().reader.reveal(index);
    }

    fn clear_find(&mut self) {
        self.core.borrow_mut().reader.clear_find();
    }

    fn content_revision(&self) -> u64 {
        self.core
            .try_borrow()
            .map_or(0, |core| core.reader.content_revision())
    }
}

impl Widget for ReaderPane {
    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        // The window's search finds in the open article; without one, it
        // narrows the list.
        if request.wants::<dyn Findable>() && self.has_article() {
            request.provide::<dyn Findable>(self);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn Findable>() && self.has_article() {
            request.provide::<dyn Findable>(self);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        fill_leaf(taffy, node_map, self)
    }

    ui::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.core
            .borrow_mut()
            .paint_reader(scene, transform, text, theme, self.rect);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.core.borrow_mut().pointer = ctx.logical(position);
        }
        if self.core.borrow_mut().reader_event(event, ctx) {
            *ctx.handled = true;
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(self.core.borrow().reader_accessibility())
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.core.borrow().reader.tooltip_at(point)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let core = self.core.borrow();
        if core.open_article().is_none() {
            return None;
        }
        core.reader.cursor_at(point)
    }

    fn needs_animation_frame(&self) -> bool {
        self.core
            .try_borrow()
            .is_ok_and(|core| core.reader.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.core
            .try_borrow()
            .ok()
            .and_then(|core| core.reader.next_wake_deadline())
    }

    fn primary_vertical_scroll(&self) -> Option<ui::widget::PrimaryScrollRegion> {
        self.core
            .try_borrow()
            .ok()
            .and_then(|core| core.reader.primary_vertical_scroll())
    }

    fn apply_primary_vertical_scroll(
        &mut self,
        action: ui::widget::PrimaryScrollAction,
    ) -> ui::widget::PrimaryScrollResult {
        self.core
            .borrow_mut()
            .reader
            .apply_primary_vertical_scroll(action)
    }

    fn name(&self) -> &'static str {
        "FeedsReader"
    }
}
