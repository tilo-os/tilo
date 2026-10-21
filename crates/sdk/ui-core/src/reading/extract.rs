//! From HTML to a [`Document`]: the article a page holds, or a fragment
//! (a feed entry's content, a mail's body) lowered whole.
//!
//! [`article`] finds a page's article the way reader modes do: it sets aside
//! what is plainly not the text (navigation, sidebars, comments, sharing
//! widgets, forms), scores the elements that hold paragraphs by how much
//! prose they carry and how little of it is links, and takes the best with
//! its siblings that belong to it. [`fragment`] lowers markup that already
//! is the text.
//!
//! Lowering keeps the structure a reader sets: headings, paragraphs, lists
//! (ordered, numbered from their start, nested), block quotes, code as
//! written, figures with captions and their sizes, tables, footnotes (which
//! open in place) and links. Typewriter quotes, double hyphens and three
//! dots become proper quotes, dashes and an ellipsis outside code.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::document::{Baseline, Block, Document, Footnote, Image, Span, TableCell, TableRow};
use super::html::{Dom, collapse_whitespace};

/// Which pictures a lowered document keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Images {
    /// Every picture, fetched from where it lives.
    #[default]
    Shown,
    /// Only pictures carried inside the document (`data:` URLs); a remote one
    /// shows its description instead, so opening the document tells nobody
    /// it was read (a mail before the person allows its pictures).
    Embedded,
}

/// How markup is lowered.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    /// The address relative links and pictures are resolved against.
    pub base_url: Option<&'a str>,
    pub images: Images,
}

/// The fewest characters of paragraph text an article has.
const MIN_ARTICLE_TEXT: usize = 400;

/// Class and id words that mark what is not the text.
const NEGATIVE: &[&str] = &[
    "comment",
    "sidebar",
    "footer",
    "masthead",
    "menu",
    "nav",
    "share",
    "sharing",
    "social",
    "related",
    "recommend",
    "promo",
    "advert",
    "sponsor",
    "banner",
    "popup",
    "modal",
    "newsletter",
    "subscribe",
    "signup",
    "cookie",
    "consent",
    "breadcrumb",
    "pagination",
    "pager",
    "widget",
    "disqus",
    "outbrain",
    "taboola",
    "feedflare",
    "toolbar",
    "skip-link",
    "byline-tools",
    "tags",
];
/// The negatives that are so plainly not the text that even a fragment
/// (a feed entry, a mail) loses them.
const STRONG_NEGATIVE: &[&str] = &[
    "share",
    "sharing",
    "social",
    "feedflare",
    "newsletter",
    "advert",
    "sponsor",
];
/// Class and id words that mark the text.
const POSITIVE: &[&str] = &[
    "article", "body", "content", "entry", "main", "post", "story", "text", "prose", "blog",
];
/// Elements that never hold the text.
const DROPPED: &[&str] = &[
    "script", "style", "noscript", "template", "iframe", "object", "embed", "form", "button",
    "input", "select", "textarea", "nav", "aside", "footer", "dialog", "svg", "canvas", "video",
    "audio", "map", "head", "link", "meta",
];

/// The article `html` (a whole page at `url`) holds, or `None` when it holds
/// too little prose to be one.
#[must_use]
pub fn article(html: &str, url: &str) -> Option<Document> {
    let dom = Dom::parse(html);
    let meta = Metadata::read(&dom, url);
    let dropped = dropped_nodes(&dom, Dom::root(), false);
    let roots = article_roots(&dom, &dropped)?;
    let options = Options {
        base_url: Some(url),
        images: Images::Shown,
    };
    let mut lowerer = Lowerer::new(&dom, options, dropped);
    lowerer.collect_footnotes(&roots);
    for root in roots {
        lowerer.walk(root);
        lowerer.flush();
    }
    let (blocks, footnotes) = lowerer.finish();
    let text: usize = blocks.iter().map(|block| block.plain_text().len()).sum();
    if text < MIN_ARTICLE_TEXT {
        return None;
    }
    Some(Document {
        title: meta.title,
        byline: meta.byline,
        site: meta.site,
        published: meta.published,
        language: meta.language,
        url: url.to_owned(),
        blocks,
        footnotes,
    })
}

/// `html`, markup that is the text itself (a feed entry's content, a mail's
/// body), lowered whole: the document's blocks and footnotes, without a
/// title.
#[must_use]
pub fn fragment(html: &str, options: Options<'_>) -> Document {
    let dom = Dom::parse(html);
    let dropped = dropped_nodes(&dom, Dom::root(), true);
    let language = dom
        .find(Dom::root(), "html")
        .and_then(|html| dom.attr(html, "lang"))
        .unwrap_or_default()
        .to_owned();
    let mut lowerer = Lowerer::new(&dom, options, dropped);
    lowerer.collect_footnotes(&[Dom::root()]);
    lowerer.walk(Dom::root());
    lowerer.flush();
    let (blocks, footnotes) = lowerer.finish();
    Document {
        language,
        url: options.base_url.unwrap_or_default().to_owned(),
        blocks,
        footnotes,
        ..Document::default()
    }
}

/// Plain text of `html` (a feed's summary), its whitespace collapsed: what a
/// list row shows as an excerpt.
#[must_use]
pub fn plain_text(html: &str) -> String {
    let document = fragment(html, Options::default());
    let text: Vec<String> = document
        .blocks
        .iter()
        .map(Block::plain_text)
        .filter(|text| !text.is_empty())
        .collect();
    if text.is_empty() {
        return collapse_whitespace(&super::html::decode_entities(html));
    }
    text.join(" ")
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Metadata {
    title: String,
    byline: String,
    site: String,
    published: Option<i64>,
    language: String,
}

impl Metadata {
    fn read(dom: &Dom, url: &str) -> Self {
        let mut meta: HashMap<String, String> = HashMap::new();
        for node in dom.find_all(Dom::root(), "meta") {
            let key = dom
                .attr(node, "property")
                .or_else(|| dom.attr(node, "name"))
                .or_else(|| dom.attr(node, "itemprop"))
                .map(str::to_ascii_lowercase);
            if let (Some(key), Some(content)) = (key, dom.attr(node, "content")) {
                meta.entry(key)
                    .or_insert_with(|| collapse_whitespace(content));
            }
        }
        let get = |keys: &[&str]| {
            keys.iter()
                .find_map(|key| meta.get(*key).filter(|value| !value.is_empty()).cloned())
        };
        let site = get(&["og:site_name", "application-name"]).unwrap_or_else(|| host_of(url));
        let document_title = dom
            .find(Dom::root(), "title")
            .map(|title| dom.collapsed_text(title))
            .unwrap_or_default();
        let title = get(&["og:title", "twitter:title", "dc.title"])
            .or_else(|| {
                dom.find(Dom::root(), "h1")
                    .map(|h1| dom.collapsed_text(h1))
                    .filter(|h1| !h1.is_empty() && document_title.contains(h1.as_str()))
            })
            .unwrap_or_else(|| without_site_suffix(&document_title, &site));
        let byline = get(&["author", "article:author", "dc.creator", "parsely-author"])
            .filter(|author| !author.starts_with("http"))
            .unwrap_or_default();
        let published = get(&[
            "article:published_time",
            "datepublished",
            "dc.date",
            "pubdate",
        ])
        .or_else(|| {
            dom.find(Dom::root(), "time")
                .and_then(|time| dom.attr(time, "datetime"))
                .map(str::to_owned)
        })
        .and_then(|date| chrono::DateTime::parse_from_rfc3339(date.trim()).ok())
        .map(|date| date.timestamp());
        let language = dom
            .find(Dom::root(), "html")
            .and_then(|html| dom.attr(html, "lang"))
            .unwrap_or_default()
            .trim()
            .to_owned();
        Self {
            title,
            byline,
            site,
            published,
            language,
        }
    }
}

/// The host of `url` without `www.`.
fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .map(|host| {
            let bare = host.strip_prefix("www.").map(str::to_owned);
            bare.unwrap_or(host)
        })
        .unwrap_or_default()
}

/// A page title without the site's name it ends with ("Title | Site").
fn without_site_suffix(title: &str, site: &str) -> String {
    for separator in [" | ", " - ", " – ", " — ", " · ", " :: "] {
        if let Some((head, tail)) = title.rsplit_once(separator)
            && (site.is_empty()
                || tail.to_lowercase().contains(&site.to_lowercase())
                || site.to_lowercase().contains(&tail.to_lowercase()))
            && !head.trim().is_empty()
        {
            return head.trim().to_owned();
        }
    }
    title.trim().to_owned()
}

// ---------------------------------------------------------------------------
// Finding the article
// ---------------------------------------------------------------------------

/// The class and id of `node`, lower case.
fn hints(dom: &Dom, node: usize) -> String {
    let mut out = String::new();
    for key in ["class", "id"] {
        if let Some(value) = dom.attr(node, key) {
            out.push_str(&value.to_ascii_lowercase());
            out.push(' ');
        }
    }
    out
}

fn has_any(hints: &str, words: &[&str]) -> bool {
    words.iter().any(|word| hints.contains(word))
}

/// Whether `node` is hidden from people: `hidden`, `aria-hidden`, or styled
/// out of sight.
fn is_hidden(dom: &Dom, node: usize) -> bool {
    if dom.attr(node, "hidden").is_some() || dom.attr(node, "aria-hidden") == Some("true") {
        return true;
    }
    dom.attr(node, "style").is_some_and(|style| {
        let style: String = style
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        style.contains("display:none") || style.contains("visibility:hidden")
    })
}

/// The nodes under `root` that are not the text, with everything under them
/// (only the topmost of each is listed). A `fragment` keeps all but the
/// plainest of them.
fn dropped_nodes(dom: &Dom, root: usize, fragment: bool) -> HashSet<usize> {
    let mut dropped = HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node != root && drops(dom, node, fragment) {
            dropped.insert(node);
            continue;
        }
        stack.extend(dom.children(node).iter().copied());
    }
    dropped
}

fn drops(dom: &Dom, node: usize, fragment: bool) -> bool {
    let Some(name) = dom.name(node) else {
        return false;
    };
    if DROPPED.contains(&name) || is_hidden(dom, node) {
        return true;
    }
    if matches!(name, "body" | "html" | "article" | "main") {
        return false;
    }
    let hints = hints(dom, node);
    if hints.is_empty() {
        return matches!(
            dom.attr(node, "role"),
            Some("navigation" | "complementary" | "contentinfo" | "dialog" | "banner" | "alert")
        );
    }
    if fragment {
        return has_any(&hints, STRONG_NEGATIVE);
    }
    if matches!(
        dom.attr(node, "role"),
        Some("navigation" | "complementary" | "contentinfo" | "dialog" | "banner" | "alert")
    ) {
        return true;
    }
    has_any(&hints, NEGATIVE) && !has_any(&hints, POSITIVE)
}

/// Whether `node` lies under a dropped node.
fn under_dropped(dom: &Dom, node: usize, dropped: &HashSet<usize>) -> bool {
    dropped.contains(&node) || dom.ancestors(node).iter().any(|a| dropped.contains(a))
}

/// How much paragraph text `node` holds: the text of its paragraphs of at
/// least 25 characters, outside dropped nodes.
fn paragraph_text(dom: &Dom, node: usize, dropped: &HashSet<usize>) -> usize {
    dom.find_all(node, "p")
        .into_iter()
        .filter(|&p| !under_dropped(dom, p, dropped))
        .map(|p| dom.collapsed_text(p).len())
        .filter(|&len| len >= 25)
        .sum()
}

/// The share of `node`'s text that is links, 0 to 1.
fn link_density(dom: &Dom, node: usize) -> f64 {
    let total = dom.collapsed_text(node).len();
    if total == 0 {
        return 0.0;
    }
    let links: usize = dom
        .find_all(node, "a")
        .into_iter()
        .map(|a| dom.collapsed_text(a).len())
        .sum();
    (crate::num::f64_of_usize(links) / crate::num::f64_of_usize(total)).min(1.0)
}

/// The elements the article is made of, in document order.
fn article_roots(dom: &Dom, dropped: &HashSet<usize>) -> Option<Vec<usize>> {
    // A page that marks its article: the marked element with the most prose.
    let marked = dom
        .descendants(Dom::root())
        .into_iter()
        .filter(|&node| {
            dom.is(node, "article")
                || dom.is(node, "main")
                || dom.attr(node, "itemprop") == Some("articleBody")
                || dom.attr(node, "role") == Some("main")
        })
        .filter(|&node| !under_dropped(dom, node, dropped))
        .map(|node| (node, paragraph_text(dom, node, dropped)))
        .max_by_key(|&(_, text)| text);
    if let Some((node, text)) = marked
        && text >= MIN_ARTICLE_TEXT
    {
        // The most specific marked element that still holds most of it.
        let inner = dom
            .descendants(node)
            .into_iter()
            .filter(|&inner| {
                dom.attr(inner, "itemprop") == Some("articleBody") || dom.is(inner, "article")
            })
            .map(|inner| (inner, paragraph_text(dom, inner, dropped)))
            .filter(|&(_, inner_text)| inner_text * 10 >= text * 8)
            .max_by_key(|&(_, inner_text)| inner_text)
            .map_or(node, |(inner, _)| inner);
        return Some(vec![inner]);
    }
    let scores = score_candidates(dom, dropped);
    let (&top, &top_score) = scores.iter().max_by(|a, b| a.1.total_cmp(b.1))?;
    if paragraph_text(dom, top, dropped) < MIN_ARTICLE_TEXT / 2 {
        return None;
    }
    // Siblings that belong to it: scored nearly as well, or prose of their own.
    let threshold = (top_score * 0.2).max(10.0);
    let Some(parent) = dom.parent(top) else {
        return Some(vec![top]);
    };
    let roots = dom
        .children(parent)
        .iter()
        .copied()
        .filter(|&sibling| {
            if sibling == top {
                return true;
            }
            if under_dropped(dom, sibling, dropped) || dom.name(sibling).is_none() {
                return false;
            }
            if scores
                .get(&sibling)
                .is_some_and(|&score| score >= threshold)
            {
                return true;
            }
            dom.is(sibling, "p") && {
                let text = dom.collapsed_text(sibling);
                (text.len() > 80 && link_density(dom, sibling) < 0.25)
                    || (text.ends_with('.') && link_density(dom, sibling) < f64::EPSILON)
            }
        })
        .collect();
    Some(roots)
}

/// Each element's score as a container of prose.
fn score_candidates(dom: &Dom, dropped: &HashSet<usize>) -> HashMap<usize, f64> {
    let mut scores: HashMap<usize, f64> = HashMap::new();
    for node in dom.descendants(Dom::root()) {
        if !matches!(dom.name(node), Some("p" | "pre" | "td")) || under_dropped(dom, node, dropped)
        {
            continue;
        }
        let text = dom.collapsed_text(node);
        if text.len() < 25 {
            continue;
        }
        let commas = text.matches([',', '，', '、']).count();
        let length = (crate::num::f64_of_usize(text.len()) / 100.0).min(3.0);
        let score = 1.0 + crate::num::f64_of_usize(commas) + length;
        for (level, ancestor) in dom.ancestors(node).into_iter().take(3).enumerate() {
            if dom.name(ancestor).is_none() {
                break;
            }
            let base = *scores
                .entry(ancestor)
                .or_insert_with(|| initial_score(dom, ancestor));
            let share = match level {
                0 => score,
                1 => score / 2.0,
                _ => score / 3.0,
            };
            scores.insert(ancestor, base + share);
        }
    }
    for (node, score) in &mut scores {
        *score *= 1.0 - link_density(dom, *node);
    }
    scores
}

fn initial_score(dom: &Dom, node: usize) -> f64 {
    let base = match dom.name(node) {
        Some("div" | "article" | "section" | "main") => 5.0,
        Some("pre" | "td" | "blockquote") => 3.0,
        Some("address" | "ol" | "ul" | "dl" | "dd" | "dt" | "li" | "form") => -3.0,
        Some("h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "th") => -5.0,
        _ => 0.0,
    };
    let hints = hints(dom, node);
    let mut weight = 0.0;
    if has_any(&hints, NEGATIVE) {
        weight -= 25.0;
    }
    if has_any(&hints, POSITIVE) {
        weight += 25.0;
    }
    base + weight
}

// ---------------------------------------------------------------------------
// Lowering
// ---------------------------------------------------------------------------

/// The inline style in effect.
#[derive(Clone, Default)]
struct Inline {
    bold: u16,
    italic: u16,
    code: u16,
    small_caps: u16,
    baseline: Baseline,
    link: Option<Arc<str>>,
    note: Option<u16>,
}

/// What paragraphs inside the current element become; none open is a
/// plain paragraph.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    Heading(u8),
    Quote,
}

/// An open list.
struct List {
    ordered: bool,
    next: usize,
}

/// An open list item.
struct Item {
    ordered: bool,
    index: usize,
    depth: u8,
}

struct Lowerer<'d> {
    dom: &'d Dom,
    options: Options<'d>,
    base: Option<url::Url>,
    dropped: HashSet<usize>,
    blocks: Vec<Block>,
    footnotes: Vec<Footnote>,
    /// Footnote ids and the index each has in `footnotes`.
    note_ids: HashMap<String, u16>,
    /// Footnote containers, lowered as footnotes and not as text.
    note_nodes: HashSet<usize>,
    spans: Vec<Span>,
    /// Whitespace was seen since the last word.
    pending_space: bool,
    /// The next word starts a new line.
    pending_break: bool,
    inline: Inline,
    containers: Vec<Container>,
    lists: Vec<List>,
    items: Vec<Item>,
}

impl<'d> Lowerer<'d> {
    fn new(dom: &'d Dom, options: Options<'d>, dropped: HashSet<usize>) -> Self {
        Self {
            dom,
            options,
            base: options.base_url.and_then(|base| url::Url::parse(base).ok()),
            dropped,
            blocks: Vec::new(),
            footnotes: Vec::new(),
            note_ids: HashMap::new(),
            note_nodes: HashSet::new(),
            spans: Vec::new(),
            pending_space: false,
            pending_break: false,
            inline: Inline::default(),
            containers: Vec::new(),
            lists: Vec::new(),
            items: Vec::new(),
        }
    }

    fn finish(mut self) -> (Vec<Block>, Vec<Footnote>) {
        self.flush();
        // No section break at either end, and never two in a row.
        let mut blocks: Vec<Block> = Vec::with_capacity(self.blocks.len());
        for block in self.blocks {
            if block.is_separator() && blocks.last().is_none_or(Block::is_separator) {
                continue;
            }
            blocks.push(block);
        }
        while blocks.last().is_some_and(Block::is_separator) {
            blocks.pop();
        }
        (blocks, self.footnotes)
    }

    // --- footnotes ---------------------------------------------------------

    /// Find the footnotes the text under `roots` refers to, and set their
    /// containers aside so they open in place rather than end the text.
    fn collect_footnotes(&mut self, roots: &[usize]) {
        let dom = self.dom;
        let ids = dom.by_id();
        // Every in-document reference under the roots.
        let mut referenced: Vec<String> = Vec::new();
        for &root in roots {
            for node in dom.find_all(root, "a") {
                if let Some(target) = dom.attr(node, "href").and_then(|h| h.strip_prefix('#'))
                    && !target.is_empty()
                    && !referenced.iter().any(|seen| seen == target)
                {
                    referenced.push(target.to_owned());
                }
            }
        }
        for target in referenced {
            let Some(&note) = ids.get(&target) else {
                continue;
            };
            if !is_footnote(dom, note) || self.note_ids.len() >= usize::from(u16::MAX) {
                continue;
            }
            let Ok(index) = u16::try_from(self.footnotes.len()) else {
                break;
            };
            let spans = self.note_spans(note);
            if spans.is_empty() {
                continue;
            }
            self.footnotes.push(Footnote {
                label: (usize::from(index) + 1).to_string(),
                spans,
            });
            self.note_ids.insert(target, index);
            self.note_nodes.insert(note);
            // The list of notes it sits in goes with it.
            for ancestor in dom.ancestors(note).into_iter().take(4) {
                if is_notes_container(dom, ancestor) {
                    self.note_nodes.insert(ancestor);
                    break;
                }
            }
        }
    }

    /// The text of the footnote at `note`, without its way back.
    fn note_spans(&self, note: usize) -> Vec<Span> {
        let mut inner = Lowerer::new(self.dom, self.options, self.dropped.clone());
        inner.walk_inline_note(note);
        let mut spans = std::mem::take(&mut inner.spans);
        for block in inner.blocks {
            let more = block.spans().to_vec();
            if let Some(first) = more.first()
                && !spans.is_empty()
            {
                let mut first = first.clone();
                first.space_before = true;
                spans.push(first);
                spans.extend(more.into_iter().skip(1));
            } else {
                spans.extend(more);
            }
        }
        spans
    }

    // --- walking -----------------------------------------------------------

    fn walk(&mut self, node: usize) {
        let dom = self.dom;
        if self.dropped.contains(&node) || self.note_nodes.contains(&node) {
            return;
        }
        if let Some(text) = dom.text(node) {
            self.text(text);
            return;
        }
        let Some(name) = dom.name(node) else {
            // The root.
            self.children(node);
            return;
        };
        match name {
            "br" => self.pending_break = !self.spans.is_empty(),
            "wbr" => {}
            "p" | "div" | "section" | "article" | "main" | "header" | "center" | "address"
            | "details" | "summary" | "hgroup" | "body" | "html" | "dd" | "caption" => {
                self.block_boundary();
                self.children(node);
                self.block_boundary();
            }
            "dt" => {
                self.block_boundary();
                self.inline.bold += 1;
                self.children(node);
                self.inline.bold -= 1;
                self.block_boundary();
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = match name {
                    "h1" | "h2" => 1,
                    "h3" => 2,
                    _ => 3,
                };
                self.flush();
                self.containers.push(Container::Heading(level));
                self.children(node);
                self.flush();
                self.containers.pop();
            }
            "blockquote" => {
                self.flush();
                self.containers.push(Container::Quote);
                self.children(node);
                self.flush();
                self.containers.pop();
            }
            "ul" | "ol" | "menu" => self.list(node, name == "ol"),
            "dl" => {
                self.block_boundary();
                self.children(node);
                self.block_boundary();
            }
            "li" => self.item(node),
            "pre" => self.code_block(node),
            "figure" => self.figure(node),
            "img" => self.image(node),
            "picture" => {
                if let Some(img) = dom.find(node, "img") {
                    self.image(img);
                }
            }
            "table" => self.table(node),
            "hr" => {
                self.flush();
                self.blocks.push(Block::Separator);
            }
            "b" | "strong" => {
                self.inline.bold += 1;
                self.children(node);
                self.inline.bold -= 1;
            }
            "i" | "em" | "cite" | "dfn" | "var" => {
                self.inline.italic += 1;
                self.children(node);
                self.inline.italic -= 1;
            }
            "code" | "kbd" | "samp" | "tt" => {
                self.inline.code += 1;
                self.children(node);
                self.inline.code -= 1;
            }
            "sup" | "sub" => {
                let before = self.inline.baseline;
                self.inline.baseline = if name == "sup" {
                    Baseline::Super
                } else {
                    Baseline::Sub
                };
                self.children(node);
                self.inline.baseline = before;
            }
            "q" => {
                self.text("\u{201c}");
                self.children(node);
                self.glue("\u{201d}");
            }
            "a" => self.anchor(node),
            "span" | "font" | "abbr" => {
                let small_caps = is_small_caps(dom, node);
                if small_caps {
                    self.inline.small_caps += 1;
                }
                self.children(node);
                if small_caps {
                    self.inline.small_caps -= 1;
                }
            }
            "math" => {
                let alt = dom
                    .attr(node, "alttext")
                    .map(str::to_owned)
                    .unwrap_or_else(|| dom.collapsed_text(node));
                self.inline.italic += 1;
                self.text(&alt);
                self.inline.italic -= 1;
            }
            _ => self.children(node),
        }
    }

    fn children(&mut self, node: usize) {
        let dom = self.dom;
        for &child in dom.children(node) {
            self.walk(child);
        }
    }

    // --- inline text -------------------------------------------------------

    /// Append text as it is written in markup: runs of whitespace are one
    /// word space.
    fn text(&mut self, raw: &str) {
        let mut word = String::new();
        for c in raw.chars() {
            if is_html_space(c) {
                if !word.is_empty() {
                    self.word(&std::mem::take(&mut word));
                }
                self.pending_space = true;
            } else {
                word.push(c);
            }
        }
        if !word.is_empty() {
            self.word(&word);
        }
    }

    /// Append `text` to the previous word with no space between (a closing
    /// quote).
    fn glue(&mut self, text: &str) {
        self.pending_space = false;
        self.word(text);
    }

    fn word(&mut self, word: &str) {
        let space = self.pending_space && !self.spans.is_empty();
        let line_break = self.pending_break && !self.spans.is_empty();
        self.pending_space = false;
        self.pending_break = false;
        let code = self.inline.code > 0;
        let previous = if space || line_break {
            Some(' ')
        } else {
            self.spans.last().and_then(|span| span.text.chars().last())
        };
        let word = if code {
            word.to_owned()
        } else {
            smarten(word, previous)
        };
        let style = &self.inline;
        if let Some(last) = self.spans.last_mut()
            && !line_break
            && last.bold == (style.bold > 0)
            && last.italic == (style.italic > 0)
            && last.code == code
            && last.small_caps == (style.small_caps > 0)
            && last.baseline == style.baseline
            && last.link == style.link
            && last.note == style.note
            && last.math.is_none()
        {
            if space {
                last.text.push(' ');
            }
            last.text.push_str(&word);
            return;
        }
        let mut span = Span::plain(word);
        span.bold = style.bold > 0;
        span.italic = style.italic > 0;
        span.code = code;
        span.small_caps = style.small_caps > 0;
        span.baseline = style.baseline;
        span.link.clone_from(&style.link);
        span.note = style.note;
        span.space_before = space || line_break;
        span.line_break = line_break;
        self.spans.push(span);
    }

    // --- blocks ------------------------------------------------------------

    /// A block boundary inside the current container: a new paragraph, or a
    /// new line inside a list item.
    fn block_boundary(&mut self) {
        if self.items.is_empty() {
            self.flush();
        } else if !self.spans.is_empty() {
            self.pending_break = true;
        }
    }

    /// Emit the text gathered so far as a block of the current container.
    fn flush(&mut self) {
        if !self.items.is_empty() {
            // Inside a list item the text waits for the item's end.
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        self.pending_space = false;
        self.pending_break = false;
        if spans.iter().all(|span| span.text.trim().is_empty()) {
            return;
        }
        let block = match self.containers.last() {
            Some(Container::Heading(level)) => Block::Heading {
                level: *level,
                spans,
            },
            Some(Container::Quote) => Block::Quote { spans },
            None => Block::Paragraph { spans },
        };
        self.blocks.push(block);
    }

    fn list(&mut self, node: usize, ordered: bool) {
        let dom = self.dom;
        self.emit_item();
        self.flush();
        let start = dom
            .attr(node, "start")
            .and_then(|start| start.trim().parse::<usize>().ok())
            .unwrap_or(1);
        self.lists.push(List {
            ordered,
            next: start,
        });
        for &child in dom.children(node) {
            if dom.is(child, "li") {
                self.item(child);
            } else {
                self.walk(child);
            }
        }
        self.emit_item();
        self.lists.pop();
    }

    fn item(&mut self, node: usize) {
        let dom = self.dom;
        self.emit_item();
        let depth = u8::try_from(self.lists.len().saturating_sub(1)).unwrap_or(u8::MAX);
        let (ordered, index) = match self.lists.last_mut() {
            Some(list) => {
                let index = dom
                    .attr(node, "value")
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(list.next);
                list.next = index.saturating_add(1);
                (list.ordered, index)
            }
            None => (false, 1),
        };
        // The text gathered before the item is a paragraph of its own.
        let pending = std::mem::take(&mut self.items);
        self.flush();
        self.items = pending;
        self.items.push(Item {
            ordered,
            index,
            depth,
        });
        self.children(node);
        self.emit_item();
        self.items.pop();
    }

    /// Emit the open item's text as its list item, once.
    fn emit_item(&mut self) {
        let Some(item) = self.items.last() else {
            return;
        };
        let spans = std::mem::take(&mut self.spans);
        self.pending_space = false;
        self.pending_break = false;
        if spans.iter().all(|span| span.text.trim().is_empty()) {
            return;
        }
        let block = Block::ListItem {
            spans,
            ordered: item.ordered,
            index: item.index,
            depth: item.depth,
        };
        self.blocks.push(block);
        // Later text of the same item (after a nested list) continues it
        // without a second marker.
        if let Some(item) = self.items.last_mut() {
            item.index = 0;
            item.ordered = true;
        }
    }

    fn code_block(&mut self, node: usize) {
        let dom = self.dom;
        self.emit_item();
        self.flush();
        let mut text = String::new();
        preformatted_text(dom, node, &mut text);
        let text = text.trim_matches('\n').trim_end().to_owned();
        if text.trim().is_empty() {
            return;
        }
        let language = std::iter::once(node)
            .chain(dom.find(node, "code"))
            .find_map(|at| dom.attr(at, "class").and_then(code_language));
        self.blocks.push(Block::Code { text, language });
    }

    fn figure(&mut self, node: usize) {
        let dom = self.dom;
        let Some(img) = dom
            .find_all(node, "img")
            .into_iter()
            .find(|&img| !self.dropped.contains(&img))
        else {
            self.block_boundary();
            self.children(node);
            self.block_boundary();
            return;
        };
        let Some(image) = self.image_of(img) else {
            // A picture that is not shown leaves its caption as text.
            if let Some(caption) = dom.find(node, "figcaption") {
                self.block_boundary();
                self.children(caption);
                self.block_boundary();
            }
            return;
        };
        self.emit_item();
        self.flush();
        let caption = dom
            .find(node, "figcaption")
            .map(|caption| self.inline_spans(caption))
            .unwrap_or_default();
        self.blocks.push(Block::Figure { image, caption });
    }

    fn image(&mut self, node: usize) {
        let Some(image) = self.image_of(node) else {
            return;
        };
        // An icon in the text (an emoji, a badge) reads as its description.
        let small = image.width.is_some_and(|w| w <= 48) && image.height.is_some_and(|h| h <= 48);
        if small {
            if !image.alt.is_empty() {
                let alt = image.alt.clone();
                self.text(&alt);
            }
            return;
        }
        self.emit_item();
        self.flush();
        self.blocks.push(Block::Figure {
            image,
            caption: Vec::new(),
        });
    }

    /// The picture `img` shows, or `None` for none worth showing (a
    /// tracking pixel, a remote picture where only embedded ones show, which
    /// then reads as its description).
    fn image_of(&mut self, img: usize) -> Option<Image> {
        let dom = self.dom;
        let dimension = |key: &str| {
            dom.attr(img, key).and_then(|value| {
                let digits: String = value
                    .trim()
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits.parse::<u32>().ok()
            })
        };
        let width = dimension("width");
        let height = dimension("height");
        if width.is_some_and(|w| w < 8) || height.is_some_and(|h| h < 8) {
            return None;
        }
        let alt = collapse_whitespace(dom.attr(img, "alt").unwrap_or_default());
        let src = image_source(dom, img)?;
        let resolved = if src.starts_with("data:image/") {
            src
        } else {
            let url = self.resolve(&src)?;
            if !matches!(url.scheme(), "http" | "https") {
                return None;
            }
            url.to_string()
        };
        if self.options.images == Images::Embedded && !resolved.starts_with("data:") {
            if !alt.is_empty() {
                self.block_boundary();
                self.inline.italic += 1;
                self.text(&alt);
                self.inline.italic -= 1;
                self.block_boundary();
            }
            return None;
        }
        let lower = resolved.to_ascii_lowercase();
        if [
            "/pixel",
            "tracking",
            "feedburner.com/~",
            "/~ff/",
            "/~r/",
            "doubleclick",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            return None;
        }
        Some(Image {
            src: resolved,
            alt,
            width,
            height,
        })
    }

    fn table(&mut self, node: usize) {
        let dom = self.dom;
        let rows: Vec<usize> = dom
            .descendants(node)
            .into_iter()
            .filter(|&row| dom.is(row, "tr"))
            // Rows of this table, not of one nested in it.
            .filter(|&row| {
                dom.ancestors(row).into_iter().find(|&a| dom.is(a, "table")) == Some(node)
            })
            .collect();
        let cells_of = |row: usize| -> Vec<usize> {
            dom.children(row)
                .iter()
                .copied()
                .filter(|&cell| dom.is(cell, "td") || dom.is(cell, "th"))
                .collect()
        };
        let columns = rows
            .iter()
            .map(|&row| cells_of(row).len())
            .max()
            .unwrap_or(0);
        let holds_blocks = dom.descendants(node).into_iter().any(|inner| {
            matches!(
                dom.name(inner),
                Some("table" | "p" | "div" | "ul" | "ol" | "img" | "h1" | "h2" | "h3" | "pre")
            )
        });
        // A table used for layout (one column, or cells holding paragraphs
        // and pictures) is read as the blocks it holds.
        if columns <= 1 || holds_blocks || rows.is_empty() {
            self.block_boundary();
            self.children(node);
            self.block_boundary();
            return;
        }
        self.emit_item();
        self.flush();
        let mut table = Vec::with_capacity(rows.len());
        for row in rows {
            let cells: Vec<TableCell> = cells_of(row)
                .into_iter()
                .map(|cell| TableCell {
                    spans: self.inline_spans(cell),
                    header: dom.is(cell, "th"),
                })
                .collect();
            if cells.iter().any(|cell| !cell.spans.is_empty()) {
                table.push(TableRow { cells });
            }
        }
        if !table.is_empty() {
            self.blocks.push(Block::Table { rows: table });
        }
    }

    fn anchor(&mut self, node: usize) {
        let dom = self.dom;
        let href = dom.attr(node, "href").unwrap_or_default().trim();
        if let Some(target) = href.strip_prefix('#')
            && let Some(&note) = self.note_ids.get(target)
        {
            let before = (self.inline.note, self.inline.baseline);
            self.inline.note = Some(note);
            if self.inline.baseline == Baseline::Normal {
                self.inline.baseline = Baseline::Super;
            }
            // The reference reads as the note's number, against the word
            // before it.
            let label = (usize::from(note) + 1).to_string();
            self.glue(&label);
            (self.inline.note, self.inline.baseline) = before;
            return;
        }
        let link = self
            .resolve(href)
            .filter(|url| matches!(url.scheme(), "http" | "https" | "mailto" | "gemini"))
            .filter(|url| {
                // A jump within the same page is no link for a reader.
                self.base.as_ref().is_none_or(|base| {
                    let mut same = url.clone();
                    same.set_fragment(None);
                    let mut page = base.clone();
                    page.set_fragment(None);
                    same != page || url.fragment().is_none()
                })
            })
            .map(|url| Arc::<str>::from(url.as_str()));
        let before = self.inline.link.clone();
        if link.is_some() {
            self.inline.link = link;
        }
        self.children(node);
        self.inline.link = before;
    }

    /// The runs of `node`'s content alone, its blocks joined by spaces.
    fn inline_spans(&self, node: usize) -> Vec<Span> {
        let mut inner = Lowerer::new(self.dom, self.options, self.dropped.clone());
        inner.note_ids.clone_from(&self.note_ids);
        inner.children(node);
        inner.flush();
        let mut spans: Vec<Span> = Vec::new();
        for block in inner.blocks {
            let mut more = block.spans().to_vec();
            if let Some(first) = more.first_mut()
                && !spans.is_empty()
            {
                first.space_before = true;
                first.line_break = false;
            }
            spans.extend(more);
        }
        spans
    }

    fn resolve(&self, href: &str) -> Option<url::Url> {
        if href.is_empty() || href.starts_with("javascript:") {
            return None;
        }
        match &self.base {
            Some(base) => base.join(href).ok(),
            None => url::Url::parse(href).ok(),
        }
    }
}

// The footnote walk shares the lowerer but skips the way back to the text.
impl Lowerer<'_> {
    fn walk_inline_note(&mut self, note: usize) {
        let dom = self.dom;
        for &child in dom.children(note) {
            if is_backlink(dom, child) {
                continue;
            }
            self.walk(child);
        }
        self.flush();
    }
}

/// Whether `node` is a footnote: an element with an id inside a list of
/// notes, or marked as a note itself.
fn is_footnote(dom: &Dom, node: usize) -> bool {
    let own = hints(dom, node);
    let role = dom.attr(node, "role").unwrap_or_default();
    let epub = dom.attr(node, "epub:type").unwrap_or_default();
    if matches!(role, "doc-footnote" | "doc-endnote")
        || epub.contains("footnote")
        || epub.contains("endnote")
        || epub.contains("rearnote")
        || own.contains("footnote")
        || own.contains("endnote")
        || own.starts_with("fn")
    {
        return true;
    }
    dom.ancestors(node)
        .into_iter()
        .take(4)
        .any(|ancestor| is_notes_container(dom, ancestor))
}

/// Whether `node` is a list of notes (`.footnotes`, `role=doc-endnotes`).
fn is_notes_container(dom: &Dom, node: usize) -> bool {
    let hints = hints(dom, node);
    let role = dom.attr(node, "role").unwrap_or_default();
    let epub = dom.attr(node, "epub:type").unwrap_or_default();
    matches!(role, "doc-endnotes" | "doc-footnotes")
        || epub.contains("endnotes")
        || epub.contains("footnotes")
        || hints.contains("footnotes")
        || hints.contains("endnotes")
        || hints.contains("references")
}

/// Whether `node` is a footnote's way back to its reference (`↩`).
fn is_backlink(dom: &Dom, node: usize) -> bool {
    if !dom.is(node, "a") {
        return false;
    }
    let hints = hints(dom, node);
    let text = dom.collapsed_text(node);
    dom.attr(node, "role") == Some("doc-backlink")
        || hints.contains("backref")
        || hints.contains("footnote-back")
        || hints.contains("reversefootnote")
        || matches!(text.as_str(), "↩" | "↩︎" | "↑" | "^" | "⤴")
}

/// Whether `node` asks for small capitals.
fn is_small_caps(dom: &Dom, node: usize) -> bool {
    let hints = hints(dom, node);
    hints.contains("smallcaps")
        || hints.contains("small-caps")
        || hints.contains("smcp")
        || dom.attr(node, "style").is_some_and(|style| {
            style
                .to_ascii_lowercase()
                .replace(' ', "")
                .contains("font-variant:small-caps")
        })
}

/// Whitespace as HTML collapses it (a no-break space is not).
fn is_html_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}

/// The text of a `<pre>`, as written: its line breaks kept, `<br>` a line.
fn preformatted_text(dom: &Dom, node: usize, out: &mut String) {
    for &child in dom.children(node) {
        if let Some(text) = dom.text(child) {
            out.push_str(&text.replace("\r\n", "\n").replace('\t', "    "));
        } else if dom.is(child, "br") {
            out.push('\n');
        } else {
            preformatted_text(dom, child, out);
        }
    }
}

/// The language a code block's class names (`language-rust`, `lang-py`).
fn code_language(class: &str) -> Option<String> {
    class.split_whitespace().find_map(|word| {
        ["language-", "lang-", "highlight-source-"]
            .iter()
            .find_map(|prefix| word.strip_prefix(prefix))
            .filter(|language| !language.is_empty())
            .map(str::to_owned)
    })
}

/// Where a picture's pixels come from: its source, or the address a lazy
/// loader keeps until it scrolls into view, or the largest of its sources.
fn image_source(dom: &Dom, img: usize) -> Option<String> {
    let placeholder = |src: &str| {
        src.is_empty()
            || (src.starts_with("data:") && src.len() < 200)
            || src.ends_with("blank.gif")
            || src.ends_with("spacer.gif")
    };
    let lazy = [
        "data-src",
        "data-lazy-src",
        "data-original",
        "data-url",
        "data-hi-res-src",
    ]
    .iter()
    .find_map(|key| dom.attr(img, key).filter(|src| !src.trim().is_empty()));
    let src = dom.attr(img, "src").unwrap_or_default().trim();
    if !placeholder(src) {
        return Some(src.to_owned());
    }
    if let Some(lazy) = lazy {
        return Some(lazy.trim().to_owned());
    }
    ["srcset", "data-srcset"]
        .iter()
        .find_map(|key| dom.attr(img, key))
        .and_then(largest_candidate)
}

/// The widest candidate of a `srcset`, up to 2000 pixels.
fn largest_candidate(srcset: &str) -> Option<String> {
    srcset
        .split(',')
        .filter_map(|candidate| {
            let mut parts = candidate.split_whitespace();
            let url = parts.next()?;
            let width = parts
                .next()
                .and_then(|w| w.strip_suffix('w'))
                .and_then(|w| w.parse::<u32>().ok())
                .unwrap_or(1);
            Some((width, url.to_owned()))
        })
        .filter(|(width, _)| *width <= 2000)
        .max_by_key(|(width, _)| *width)
        .map(|(_, url)| url)
}

/// `word` with typewriter punctuation set properly: quotes curled by what
/// they follow, `--`/`---` as dashes, `...` as an ellipsis. `previous` is
/// the character before the word (a space when a word space separates
/// them).
#[must_use]
pub fn smarten(word: &str, previous: Option<char>) -> String {
    let word = word
        .replace("---", "\u{2014}")
        .replace("--", "\u{2013}")
        .replace("...", "\u{2026}");
    let mut out = String::with_capacity(word.len());
    let mut before = previous;
    for c in word.chars() {
        let opens = before.is_none_or(|b| {
            b.is_whitespace()
                || matches!(
                    b,
                    '(' | '[' | '{' | '\u{2014}' | '\u{2013}' | '\u{201c}' | '\u{2018}' | '/'
                )
        });
        let set = match c {
            '"' if opens => '\u{201c}',
            '"' => '\u{201d}',
            '\'' if opens => '\u{2018}',
            '\'' => '\u{2019}',
            other => other,
        };
        out.push(set);
        before = Some(set);
    }
    out
}
