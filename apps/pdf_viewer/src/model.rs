//! Pure data model for the PDF viewer.
//!
//! Nothing here touches the CRDT, taffy, vello or winit — it's the
//! plain-Rust core: the page model, the annotation / comment / signature
//! value types, the tool enum, and the pure search routine. Everything is
//! unit-testable in isolation.
//!
//! A [`Page`] is a list of [`Block`]s in PDF points (1pt = 1px at 100% zoom).
//! For a loaded PDF the blocks hold the document's extracted text (text
//! backend) or are empty placeholders whose pixels come from pdfium (raster
//! backend); [`sample_document`] is the bundled demonstration used by the
//! screenshot harness and tests.

use uuid::Uuid;

/// US-Letter page geometry, in PDF points.
pub const PAGE_W: f64 = 612.0;
pub const PAGE_H: f64 = 792.0;
/// Inner text margin on every page edge.
pub const MARGIN: f64 = 64.0;

/// A single laid-out content block on a page. The renderer walks these
/// top-to-bottom, measuring each so the next starts below it.
#[derive(Clone, Debug)]
pub enum Block {
    Title(String),
    H1(String),
    H2(String),
    Body(String),
    Caption(String),
    Quote(String),
    /// A reserved figure box of `height` points with a caption beneath.
    Figure {
        height: f64,
        label: String,
    },
    Divider,
    Spacer(f64),
}

impl Block {
    /// The text a search query runs against, if any.
    pub fn search_text(&self) -> Option<&str> {
        match self {
            Block::Title(t)
            | Block::H1(t)
            | Block::H2(t)
            | Block::Body(t)
            | Block::Caption(t)
            | Block::Quote(t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    /// 1-based page number.
    pub number: usize,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
pub struct OutlineItem {
    pub title: String,
    pub page: usize,
    /// 0 = section, 1 = sub-section (controls indent in the drawer).
    pub level: u8,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub pages: Vec<Page>,
    pub outline: Vec<OutlineItem>,
    /// Uniform page geometry, in PDF points. For a loaded PDF this is the
    /// first page's media box; for the bundled reader model it is US-Letter.
    /// Pages stack uniformly against this card size (the common case where a
    /// document has one page size throughout).
    pub page_w: f64,
    pub page_h: f64,
}

impl Document {
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Borrow a page by 1-based number.
    pub fn page(&self, number: usize) -> Option<&Page> {
        self.pages.iter().find(|p| p.number == number)
    }

    /// Build a document from text already extracted from a real PDF, one
    /// string per page. Paragraphs are split on blank lines; a short leading
    /// line is promoted to a heading so the reflow keeps some structure.
    /// The content is genuine — search, selection and markup all run against
    /// the source PDF's own words.
    pub fn from_text_pages(title: &str, author: &str, page_texts: &[String]) -> Self {
        let mut pages = Vec::with_capacity(page_texts.len());
        for (i, raw) in page_texts.iter().enumerate() {
            pages.push(Page {
                number: i + 1,
                blocks: blocks_from_text(raw),
            });
        }
        if pages.is_empty() {
            pages.push(Page {
                number: 1,
                blocks: vec![Block::Body(String::new())],
            });
        }
        Document {
            title: title.to_string(),
            author: author.to_string(),
            subject: String::new(),
            pages,
            outline: Vec::new(),
            page_w: PAGE_W,
            page_h: PAGE_H,
        }
    }

    /// An empty document with `count` blank pages of the given size, used for
    /// the raster backend where page pixels come from pdfium rather than the
    /// text model.
    pub fn blank_pages(title: &str, author: &str, count: usize, page_w: f64, page_h: f64) -> Self {
        let count = count.max(1);
        let pages = (1..=count)
            .map(|n| Page {
                number: n,
                blocks: Vec::new(),
            })
            .collect();
        Document {
            title: title.to_string(),
            author: author.to_string(),
            subject: String::new(),
            pages,
            outline: Vec::new(),
            page_w,
            page_h,
        }
    }
}

/// Split one page's extracted text into typographic blocks. Blank lines
/// separate paragraphs; the lines inside a paragraph are joined with spaces
/// (PDF text extraction hard-wraps at the original column). A single short
/// paragraph that reads like a heading is rendered as one.
fn blocks_from_text(raw: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut first_para = true;
    let flush = |blocks: &mut Vec<Block>, para: &mut Vec<String>, first: &mut bool| {
        if para.is_empty() {
            return;
        }
        let joined = para.join(" ");
        let text = joined.split_whitespace().collect::<Vec<_>>().join(" ");
        para.clear();
        if text.is_empty() {
            return;
        }
        // A short, single-line-ish leading paragraph with no terminal
        // punctuation reads as a title/heading.
        let looks_heading = text.chars().count() <= 64
            && !text.ends_with('.')
            && !text.ends_with(',')
            && !text.ends_with(';');
        if *first && looks_heading {
            blocks.push(Block::Title(text));
        } else if looks_heading && text.chars().count() <= 48 {
            blocks.push(Block::H2(text));
        } else {
            blocks.push(Block::Body(text));
        }
        if !blocks.is_empty() {
            *first = false;
        }
        blocks.push(Block::Spacer(6.0));
    };
    for line in raw.lines() {
        if line.trim().is_empty() {
            flush(&mut blocks, &mut para, &mut first_para);
        } else {
            para.push(line.trim().to_string());
        }
    }
    flush(&mut blocks, &mut para, &mut first_para);
    if blocks.is_empty() {
        blocks.push(Block::Body(String::new()));
    }
    blocks
}

// ----------------------------------------------------------------------
// Tools
// ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Select,
    Highlight,
    Ink,
    Note,
    Sign,
}

impl Tool {
    pub const ALL: [Tool; 5] = [
        Tool::Select,
        Tool::Highlight,
        Tool::Ink,
        Tool::Note,
        Tool::Sign,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Highlight => "Highlight",
            Tool::Ink => "Draw",
            Tool::Note => "Note",
            Tool::Sign => "Sign",
        }
    }

    /// One-key shortcut hint shown in the tooltip / status bar.
    pub fn hotkey(self) -> char {
        match self {
            Tool::Select => '1',
            Tool::Highlight => '2',
            Tool::Ink => '3',
            Tool::Note => '4',
            Tool::Sign => '5',
        }
    }
}

// ----------------------------------------------------------------------
// Annotation colour swatches (resolved against the OS theme at draw time)
// ----------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swatch {
    Yellow,
    Green,
    Pink,
    Blue,
    Purple,
}

impl Swatch {
    pub const ALL: [Swatch; 5] = [
        Swatch::Yellow,
        Swatch::Green,
        Swatch::Pink,
        Swatch::Blue,
        Swatch::Purple,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Swatch::Yellow => "yellow",
            Swatch::Green => "green",
            Swatch::Pink => "pink",
            Swatch::Blue => "blue",
            Swatch::Purple => "purple",
        }
    }

    pub fn parse(s: &str) -> Swatch {
        match s {
            "green" => Swatch::Green,
            "pink" => Swatch::Pink,
            "blue" => Swatch::Blue,
            "purple" => Swatch::Purple,
            _ => Swatch::Yellow,
        }
    }

    /// A fixed sRGB triple for this swatch, used when **flattening** markup into
    /// a saved PDF. On screen a swatch resolves through the live OS theme (see
    /// `render::swatch_color`), but a written-to-disk `.pdf` must carry a
    /// concrete, theme-independent colour so it looks the same in every viewer.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Swatch::Yellow => (250, 204, 21),
            Swatch::Green => (34, 197, 94),
            Swatch::Pink => (236, 72, 153),
            Swatch::Blue => (59, 130, 246),
            Swatch::Purple => (139, 92, 246),
        }
    }
}

// ----------------------------------------------------------------------
// Geometry
// ----------------------------------------------------------------------

/// A rectangle in page-point space (origin top-left of the page).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PtRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl PtRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Build a normalised rect from two corner points (any order).
    pub fn from_corners(ax: f64, ay: f64, bx: f64, by: f64) -> Self {
        let x = ax.min(bx);
        let y = ay.min(by);
        Self {
            x,
            y,
            w: (ax - bx).abs(),
            h: (ay - by).abs(),
        }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

// ----------------------------------------------------------------------
// Annotations
// ----------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum AnnoKind {
    /// A translucent highlight over a region of the page.
    Highlight { rect: PtRect },
    /// A freehand ink stroke — a polyline in page-point space.
    Ink { points: Vec<(f64, f64)> },
    /// A sticky note anchored at a point. The body text is mirrored into
    /// the linked top-level comment so the comments rail can thread it.
    Note { x: f64, y: f64 },
}

impl AnnoKind {
    pub fn type_key(&self) -> &'static str {
        match self {
            AnnoKind::Highlight { .. } => "highlight",
            AnnoKind::Ink { .. } => "ink",
            AnnoKind::Note { .. } => "note",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Annotation {
    pub id: String,
    pub page: usize,
    pub kind: AnnoKind,
    pub color: Swatch,
    pub author: String,
}

impl Annotation {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }

    /// The anchor point (page-point space) used to position the comment
    /// connector and selection chrome.
    pub fn anchor(&self) -> (f64, f64) {
        match &self.kind {
            AnnoKind::Highlight { rect } => (rect.x, rect.y),
            AnnoKind::Ink { points } => points.first().copied().unwrap_or((0.0, 0.0)),
            AnnoKind::Note { x, y } => (*x, *y),
        }
    }
}

// ----------------------------------------------------------------------
// Comments (threaded)
// ----------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Reply {
    pub author: String,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: String,
    /// The annotation this comment is pinned to, if any.
    pub anno_id: Option<String>,
    pub page: usize,
    pub author: String,
    pub body: String,
    /// Monotonic ordinal for stable sort (seed time / creation order).
    pub created: i64,
    pub resolved: bool,
    pub replies: Vec<Reply>,
}

impl Comment {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }
}

// ----------------------------------------------------------------------
// Signatures
// ----------------------------------------------------------------------

/// The visible content of a placed signature — **real user input**, not a
/// canned style picked from a name hash. A signature is either the signer's
/// typed name (rendered in a signature script) or the freehand strokes they
/// actually drew, normalised into the field's `0..1` box so it scales to any
/// placement size.
#[derive(Clone, Debug, PartialEq)]
pub enum SignatureInk {
    /// Render the signer's name as a script-style typed signature.
    Typed,
    /// The signer drew this by hand: one or more polylines in normalised
    /// `[0,1]²` field space (origin top-left).
    Drawn { strokes: Vec<Vec<(f64, f64)>> },
}

impl SignatureInk {
    /// A drawn signature with no ink is empty (nothing was actually drawn).
    pub fn is_empty(&self) -> bool {
        match self {
            SignatureInk::Typed => false,
            SignatureInk::Drawn { strokes } => strokes.iter().all(|s| s.len() < 2),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Signature {
    pub id: String,
    pub page: usize,
    pub rect: PtRect,
    pub signer: String,
    pub date: String,
    /// The real signature content (typed name or drawn strokes).
    pub ink: SignatureInk,
}

impl Signature {
    pub fn new_id() -> String {
        Uuid::new_v4().to_string()
    }
}

// ----------------------------------------------------------------------
// Search
// ----------------------------------------------------------------------

/// A single search hit: which page, which block, and the byte range of
/// the match within that block's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub page: usize,
    pub block: usize,
    pub start: usize,
    pub len: usize,
}

/// Standard find-bar toggles. Both default off, matching the historical
/// case-insensitive substring behaviour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Match only when the query is bounded by non-word characters on both
    /// sides (so "ink" doesn't match inside "thinking").
    pub whole_word: bool,
    /// Match the query's letter case exactly.
    pub case_sensitive: bool,
}

/// True when the byte at `idx` (or just before it) is part of a "word":
/// ASCII letters, digits and underscore. Used for whole-word boundaries.
fn is_word_byte(bytes: &[u8], idx: usize) -> bool {
    bytes
        .get(idx)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// Substring search across all renderable text, honouring the find-bar
/// [`SearchOptions`]. The content is ASCII, so lower-casing preserves byte
/// offsets and `start` indexes directly into the original block text.
pub fn search(doc: &Document, query: &str, opts: SearchOptions) -> Vec<Match> {
    let mut out = Vec::new();
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return out;
    }
    let needle = if opts.case_sensitive {
        trimmed.to_string()
    } else {
        trimmed.to_lowercase()
    };
    for page in &doc.pages {
        for (bi, block) in page.blocks.iter().enumerate() {
            let Some(text) = block.search_text() else {
                continue;
            };
            let hay = if opts.case_sensitive {
                text.to_string()
            } else {
                text.to_lowercase()
            };
            let hay_bytes = hay.as_bytes();
            let mut from = 0usize;
            while from <= hay.len() {
                let Some(rel) = hay.get(from..).and_then(|rest| rest.find(&needle)) else {
                    break;
                };
                let start = from + rel;
                let end = start + needle.len();
                let boundary_ok = !opts.whole_word
                    || (!is_word_byte(hay_bytes, start.wrapping_sub(1))
                        && !is_word_byte(hay_bytes, end));
                if boundary_ok {
                    out.push(Match {
                        page: page.number,
                        block: bi,
                        start,
                        len: needle.len(),
                    });
                }
                from = start + needle.len().max(1);
            }
        }
    }
    out
}

// ----------------------------------------------------------------------
// The bundled sample document
// ----------------------------------------------------------------------

fn sample_body(s: &str) -> Block {
    Block::Body(s.to_string())
}

fn sample_page_abstract() -> Page {
    Page {
        number: 1,
        blocks: vec![
            Block::Spacer(40.0),
            Block::Title("qOS: A Collaborative Spatial Operating System".to_string()),
            Block::Spacer(6.0),
            Block::Caption("Technical Overview  ·  Revision 7  ·  Example Systems".to_string()),
            Block::Spacer(20.0),
            Block::H1("Abstract".to_string()),
            sample_body(
                "qOS is an immutable, container-based Linux platform that treats \
collaboration and spatial organisation as first-class primitives rather than \
features bolted onto a single-user desktop. This document describes the layered \
architecture: an immutable bootc image, a tiling Wayland compositor with a \
two-dimensional cluster model, a peer-to-peer filesystem that projects remote \
files and hydrates them on demand, and a CRDT-native user-interface toolkit \
shared by every system application.",
            ),
            Block::Spacer(8.0),
            sample_body(
                "The design goal is a daily-driver operating system where two people \
can sit inside the same workspace, edit the same document, and watch each other's \
cursors move, with the security boundary enforced by the compositor's per-cluster \
sandbox instead of by convention.",
            ),
            Block::Spacer(16.0),
            Block::H2("Document scope".to_string()),
            sample_body(
                "Sections two through five cover the compositor, the filesystem, the \
toolkit and the security model in turn. Appendix A records the on-disk image \
layout. Readers implementing a competing client should treat section four as \
normative.",
            ),
            Block::Spacer(18.0),
            Block::Quote(
                "\"Make the collaborative case the default case, and the \
single-user case a degenerate instance of it.\""
                    .to_string(),
            ),
        ],
    }
}

fn sample_page_compositor() -> Page {
    Page {
        number: 2,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("1  The Compositor".to_string()),
            sample_body(
                "The compositor arranges windows into clusters. A cluster is a group \
of tiled windows that never overlap; clusters themselves tile across an infinite \
two-dimensional canvas. Panning the canvas is the primary navigation gesture, \
replacing the alt-tab stack of conventional desktops.",
            ),
            Block::Spacer(10.0),
            Block::H2("1.1  Per-cluster sandboxing".to_string()),
            sample_body(
                "Each cluster runs inside its own Bubblewrap sandbox with an explicit \
file-access policy. When an application requests a path outside the cluster's \
allow-list the compositor pauses the request and prompts the user, who can grant \
access temporarily or permanently. Because the sandbox is the boundary, sharing a \
cluster with a remote peer never widens the host's exposure beyond that cluster.",
            ),
            Block::Spacer(10.0),
            Block::Figure {
                height: 150.0,
                label:
                    "Figure 1. Two clusters tiled on the spatial canvas, each a separate sandbox."
                        .to_string(),
            },
            Block::Spacer(10.0),
            Block::H2("1.2  Remote input injection".to_string()),
            sample_body(
                "Collaboration streams Wayland buffers to invited peers and injects \
their pointer and keyboard events back into the originating cluster. Coordination \
runs over an Iroh gossip mesh reached through authenticated invite addresses \
segment and through relays elsewhere.",
            ),
        ],
    }
}

fn sample_page_filesystem() -> Page {
    Page {
        number: 3,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("2  The Filesystem".to_string()),
            sample_body(
                "The peer-to-peer filesystem mounts at a well-known path and projects \
metadata for remote files immediately while deferring content until first read. \
Metadata propagates through the same gossip layer the compositor uses; content is \
addressed by a BLAKE3 hash and fetched on demand from whichever peer holds it.",
            ),
            Block::Spacer(10.0),
            Block::H2("2.1  Projection versus hydration".to_string()),
            sample_body(
                "A directory listing is cheap: it is a projection of gossiped \
metadata. Opening a file is where hydration happens. The first read blocks while \
the content is fetched and verified against its hash, then subsequent reads are \
served from a local cache with a short metadata time-to-live.",
            ),
            Block::Spacer(10.0),
            Block::H2("2.2  Conflict handling".to_string()),
            sample_body(
                "Concurrent writes to the same path produce conflict copies rather \
than silent overwrites. The losing writer's bytes are preserved under a suffixed \
name so no edit is ever destroyed, and the user resolves the divergence \
explicitly. This mirrors the toolkit's CRDT philosophy at the file granularity.",
            ),
            Block::Spacer(12.0),
            Block::Quote("Never assume that open() means the bytes are already local.".to_string()),
        ],
    }
}

fn sample_page_toolkit() -> Page {
    Page {
        number: 4,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("3  The UI Toolkit".to_string()),
            sample_body(
                "Every system application is built on a retained-mode toolkit whose \
widgets read their state from a shared CRDT document rather than from local \
fields. A frame is five passes: synchronise from the document, build the layout \
tree, apply the computed layout, draw into a vector scene, and handle input \
events that may mutate the document directly.",
            ),
            Block::Spacer(10.0),
            Block::H2("3.1  State lives in the document".to_string()),
            sample_body(
                "Because all durable state is in the CRDT, undo and redo are simply \
snapshots of the document, and live collaboration falls out for free: two \
instances editing the same object converge without a message-passing layer. \
Widgets keep only ephemeral caches such as caret position and scroll offset.",
            ),
            Block::Spacer(10.0),
            Block::H2("3.2  Theming is OS-controlled".to_string()),
            sample_body(
                "Widgets never hard-code colour. Every fill and stroke is read from a \
theme handed in at draw time, which lets the operating system enforce a coherent \
look across applications and switch palettes without each app participating. This \
PDF reader obeys the same rule: its highlights, ink and chrome all resolve through \
the active theme.",
            ),
        ],
    }
}

fn sample_page_security() -> Page {
    Page {
        number: 5,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("4  Security Model".to_string()),
            sample_body(
                "Trust in qOS flows outward from the compositor. The sandbox is the \
unit of isolation; a cluster cannot read files, capture input, or stream buffers \
beyond the grants the user has approved for it. Collaboration never relaxes this: \
an invited peer is granted presence inside one cluster, not access to the host.",
            ),
            Block::Spacer(10.0),
            Block::H2("4.1  Capability grants".to_string()),
            sample_body(
                "Grants are explicit, revocable, and scoped to a single cluster. A \
temporary grant expires when the cluster closes; a permanent grant is recorded \
against the cluster's policy and surfaced for review. There is no ambient \
authority that an application can escalate into.",
            ),
            Block::Spacer(10.0),
            Block::H2("4.2  Signing and attestation".to_string()),
            sample_body(
                "Documents carry detached signatures. A signature records the signer, \
the time, and the page region it attests to. Cryptographic verification of the \
signing key against a workspace identity is layered on top and is described in the \
companion identity specification.",
            ),
            Block::Spacer(14.0),
            Block::Divider,
            Block::Spacer(10.0),
            Block::Caption("Signed below to acknowledge review of this revision.".to_string()),
        ],
    }
}

fn sample_page_appendix() -> Page {
    Page {
        number: 6,
        blocks: vec![
            Block::Spacer(28.0),
            Block::H1("Appendix A  Image Layout".to_string()),
            sample_body(
                "The production image is derived from a Fedora bootc base. Persistent \
state lives under a writable subtree while the root filesystem stays read-only, \
which is what makes atomic updates and rollback possible. Binaries for the \
compositor, the filesystem driver and the system applications are baked into the \
image at build time.",
            ),
            Block::Spacer(10.0),
            Block::H2("A.1  Boot chain".to_string()),
            sample_body(
                "Firmware hands off to the boot loader, which starts the login \
greeter. The greeter launches the session, which mounts the peer-to-peer \
filesystem before starting the compositor. First-boot setup runs once to seed \
per-user state and then disables itself.",
            ),
            Block::Spacer(10.0),
            Block::H2("A.2  Updates".to_string()),
            sample_body(
                "A timer pulls new image layers in the background and stages them for \
the next boot. Because the root is immutable, a failed update never leaves the \
system in a half-written state: the previous deployment remains bootable and is \
selected automatically if the new one fails its health check.",
            ),
        ],
    }
}

fn sample_pages() -> Vec<Page> {
    vec![
        sample_page_abstract(),
        sample_page_compositor(),
        sample_page_filesystem(),
        sample_page_toolkit(),
        sample_page_security(),
        sample_page_appendix(),
    ]
}

fn sample_outline() -> Vec<OutlineItem> {
    vec![
        OutlineItem {
            title: "Abstract".into(),
            page: 1,
            level: 0,
        },
        OutlineItem {
            title: "1  The Compositor".into(),
            page: 2,
            level: 0,
        },
        OutlineItem {
            title: "1.1  Per-cluster sandboxing".into(),
            page: 2,
            level: 1,
        },
        OutlineItem {
            title: "1.2  Remote input injection".into(),
            page: 2,
            level: 1,
        },
        OutlineItem {
            title: "2  The Filesystem".into(),
            page: 3,
            level: 0,
        },
        OutlineItem {
            title: "2.1  Projection vs. hydration".into(),
            page: 3,
            level: 1,
        },
        OutlineItem {
            title: "2.2  Conflict handling".into(),
            page: 3,
            level: 1,
        },
        OutlineItem {
            title: "3  The UI Toolkit".into(),
            page: 4,
            level: 0,
        },
        OutlineItem {
            title: "3.1  State in the document".into(),
            page: 4,
            level: 1,
        },
        OutlineItem {
            title: "3.2  Theming".into(),
            page: 4,
            level: 1,
        },
        OutlineItem {
            title: "4  Security Model".into(),
            page: 5,
            level: 0,
        },
        OutlineItem {
            title: "4.1  Capability grants".into(),
            page: 5,
            level: 1,
        },
        OutlineItem {
            title: "4.2  Signing".into(),
            page: 5,
            level: 1,
        },
        OutlineItem {
            title: "Appendix A  Image Layout".into(),
            page: 6,
            level: 0,
        },
    ]
}

/// Build the bundled sample PDF: a believable multi-page technical
/// document. Real text so search, highlight ranges and selection all
/// operate on genuine content.
pub fn sample_document() -> Document {
    Document {
        title: "qOS Technical Overview".into(),
        author: "Example Systems".into(),
        subject: "Architecture specification".into(),
        pages: sample_pages(),
        outline: sample_outline(),
        page_w: PAGE_W,
        page_h: PAGE_H,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_has_pages_and_outline() {
        let d = sample_document();
        assert_eq!(d.page_count(), 6);
        assert!(d.page(1).is_some());
        assert!(d.page(7).is_none());
        assert!(!d.outline.is_empty());
        // Every outline entry points at a real page.
        for item in &d.outline {
            assert!(
                d.page(item.page).is_some(),
                "outline -> missing page {}",
                item.page
            );
        }
    }

    #[test]
    fn search_finds_known_term_case_insensitively() {
        let d = sample_document();
        let hits = search(&d, "sandbox", SearchOptions::default());
        assert!(!hits.is_empty(), "expected to find 'sandbox'");
        // Page 2 discusses sandboxing.
        assert!(hits.iter().any(|m| m.page == 2));
        // Byte range points at the real text.
        let m = &hits[0];
        let page = d.page(m.page).unwrap();
        let text = page.blocks[m.block].search_text().unwrap();
        assert_eq!(text[m.start..m.start + m.len].to_lowercase(), "sandbox");
    }

    #[test]
    fn search_empty_query_returns_nothing() {
        let d = sample_document();
        assert!(search(&d, "   ", SearchOptions::default()).is_empty());
        assert!(search(&d, "", SearchOptions::default()).is_empty());
    }

    #[test]
    fn search_overlapping_progresses() {
        // Guard against an infinite loop when a match has length that
        // could re-find at the same spot.
        let d = Document {
            title: "t".into(),
            author: "a".into(),
            subject: "s".into(),
            pages: vec![Page {
                number: 1,
                blocks: vec![Block::Body("aaaa".into())],
            }],
            outline: vec![],
            page_w: PAGE_W,
            page_h: PAGE_H,
        };
        let hits = search(&d, "aa", SearchOptions::default());
        // "aaaa" -> matches at 0 and 2 (non-overlapping stepping).
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].start, 0);
        assert_eq!(hits[1].start, 2);
    }

    #[test]
    fn search_case_sensitive_distinguishes_case() {
        let d = Document {
            title: "t".into(),
            author: "a".into(),
            subject: "s".into(),
            pages: vec![Page {
                number: 1,
                blocks: vec![Block::Body("The qOS shell. the OS.".into())],
            }],
            outline: vec![],
            page_w: PAGE_W,
            page_h: PAGE_H,
        };
        let insensitive = search(
            &d,
            "the",
            SearchOptions {
                case_sensitive: false,
                ..Default::default()
            },
        );
        assert_eq!(
            insensitive.len(),
            2,
            "case-insensitive matches both 'The' and 'the'"
        );
        let sensitive = search(
            &d,
            "the",
            SearchOptions {
                case_sensitive: true,
                ..Default::default()
            },
        );
        assert_eq!(
            sensitive.len(),
            1,
            "case-sensitive matches only lowercase 'the'"
        );
        assert_eq!(sensitive[0].start, 15);
    }

    #[test]
    fn search_whole_word_rejects_substrings() {
        let d = Document {
            title: "t".into(),
            author: "a".into(),
            subject: "s".into(),
            pages: vec![Page {
                number: 1,
                blocks: vec![Block::Body("ink is thinking and ink.".into())],
            }],
            outline: vec![],
            page_w: PAGE_W,
            page_h: PAGE_H,
        };
        let any = search(&d, "ink", SearchOptions::default());
        assert_eq!(any.len(), 3, "substring search finds 'ink' three times");
        let words = search(
            &d,
            "ink",
            SearchOptions {
                whole_word: true,
                ..Default::default()
            },
        );
        assert_eq!(
            words.len(),
            2,
            "whole-word skips the 'ink' inside 'thinking'"
        );
    }

    #[test]
    fn ptrect_from_corners_normalises() {
        let r = PtRect::from_corners(100.0, 80.0, 40.0, 200.0);
        assert!((r.x - 40.0).abs() < f64::EPSILON);
        assert!((r.y - 80.0).abs() < f64::EPSILON);
        assert!((r.w - 60.0).abs() < f64::EPSILON);
        assert!((r.h - 120.0).abs() < f64::EPSILON);
        assert!(r.contains(50.0, 100.0));
        assert!(!r.contains(10.0, 10.0));
    }

    #[test]
    fn swatch_round_trips() {
        for s in Swatch::ALL {
            assert_eq!(Swatch::parse(s.key()), s);
        }
        assert_eq!(Swatch::parse("nonsense"), Swatch::Yellow);
    }
}
