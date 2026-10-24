//! Bundled sample feeds + articles.
//!
//! `--shots` has no event loop and can't hit the network, so the offline
//! screenshot is driven entirely by this fixture: a realistic multi-folder
//! subscription set with read/unread/starred articles and fully-rendered
//! bodies (including a gemtext capsule to exercise the Gemini path). The
//! live app never seeds this — it starts empty on the user's persisted
//! subscriptions and fetches real feeds over HTTP once the user adds one.

use crate::gemtext;
use crate::model::{Article, BlockKind, ContentBlock, Feed, FeedKind, Folder, Id, Library};

fn para(s: &str) -> ContentBlock {
    ContentBlock::new(BlockKind::Paragraph, s)
}
fn h2(s: &str) -> ContentBlock {
    ContentBlock::new(BlockKind::Heading2, s)
}
fn bullet(s: &str) -> ContentBlock {
    ContentBlock::new(BlockKind::Bullet, s)
}
fn quote(s: &str) -> ContentBlock {
    ContentBlock::new(BlockKind::Quote, s)
}
fn code(s: &str) -> ContentBlock {
    ContentBlock::new(BlockKind::Code, s)
}

const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

/// Build the full sample store. Timestamps are anchored to "now" so the
/// relative dates ("3h", "2d") read correctly whenever the app launches.
#[allow(
    clippy::too_many_lines,
    reason = "one block of fixture literals per bundled article; splitting it would only \
              relocate the sample data, not shrink what a reader must trace for one entry"
)]
pub fn store() -> Library {
    let now = ui::clock::local_now().timestamp();
    let mut s = Library::new();

    let tech = folder(&mut s, "Technology");
    let science = folder(&mut s, "Science");
    let design = folder(&mut s, "Design");

    let rust = feed(
        &mut s,
        "Rust Blog",
        "https://blog.rust-lang.org/feed.xml",
        "https://blog.rust-lang.org",
        Some(tech.clone()),
        FeedKind::Atom,
    );
    let hn = feed(
        &mut s,
        "Hacker News",
        "https://hnrss.org/frontpage",
        "https://news.ycombinator.com",
        Some(tech.clone()),
        FeedKind::Rss,
    );
    let lob = feed(
        &mut s,
        "Lobsters",
        "https://lobste.rs/rss",
        "https://lobste.rs",
        Some(tech),
        FeedKind::Rss,
    );
    let quanta = feed(
        &mut s,
        "Quanta Magazine",
        "https://www.quantamagazine.org/feed/",
        "https://www.quantamagazine.org",
        Some(science.clone()),
        FeedKind::Rss,
    );
    let nasa = feed(
        &mut s,
        "NASA Breaking News",
        "https://www.nasa.gov/rss/dyn/breaking_news.rss",
        "https://www.nasa.gov",
        Some(science),
        FeedKind::Rss,
    );
    let smashing = feed(
        &mut s,
        "Smashing Magazine",
        "https://www.smashingmagazine.com/feed/",
        "https://www.smashingmagazine.com",
        Some(design),
        FeedKind::Rss,
    );
    let antenna = feed(
        &mut s,
        "Gemini Antenna",
        "gemini://warmedal.se/~antenna/",
        "gemini://warmedal.se/~antenna/",
        None,
        FeedKind::Gemini,
    );

    article(
        &mut s,
        SampleArticle::new(
            rust.clone(),
            "Announcing Rust 1.96.1",
            "The Rust Release Team",
            "https://blog.rust-lang.org/2026/06/30/Rust-1.96.1/",
            now - 5 * HOUR,
            "The Rust team has published a new point release of Rust, 1.96.1.",
            vec![
            para(
                "The Rust team has published a new point release of Rust, 1.96.1. Rust is a programming language empowering everyone to build reliable and efficient software.",
            ),
            h2("What's in 1.96.1"),
            para(
                "Rust 1.96.1 resolves a few regressions reported since 1.96.0 was released at the end of May.",
            ),
            bullet("Incremental builds again notice changes to items re-exported through glob imports."),
            bullet(
                "Cargo respects the build target set in a workspace's configuration file once more.",
            ),
            bullet("Rustdoc shows the documentation of trait aliases again."),
            h2("Updating"),
            para(
                "If you have a previous version of Rust installed via rustup, getting Rust 1.96.1 is as easy as running:",
            ),
            code("rustup update stable"),
            para(
                "As always, if you spot a regression, please report it on the issue tracker — your reports keep the release train reliable.",
            ),
            ],
        )
        .starred(),
    );
    article(
        &mut s,
        SampleArticle::new(
            rust.clone(),
            "A new era for the borrow checker",
            "Niko Matsakis",
            "https://blog.rust-lang.org/2026/06/12/polonius.html",
            now - 2 * DAY,
            "Polonius lands on nightly, unlocking patterns the old checker rejected.",
            vec![
            para(
                "After years of design work, the Polonius borrow checker is available on nightly. It models loans with a more precise dataflow analysis.",
            ),
            para(
                "In practice this means several long-standing 'NLL problem cases' now compile without contortions.",
            ),
            quote(
                "The borrow checker should accept every program that is actually safe. Polonius gets us meaningfully closer to that goal.",
            ),
            ],
        )
        .read(),
    );
    article(
        &mut s,
        SampleArticle::new(
            rust,
            "Async fn in traits, one year on",
            "The Async WG",
            "",
            now - 9 * DAY,
            "Lessons learned from shipping AFIT.",
            vec![para(
                "Async functions in traits shipped a year ago. Here's what we learned from real-world adoption across the ecosystem.",
            )],
        )
        .read(),
    );

    // --- Hacker News -----------------------------------------------------
    article(
        &mut s,
        SampleArticle::new(
            hn.clone(),
            "Show HN: I built a spatial OS in Rust",
            "throwaway2026",
            "https://news.ycombinator.com/item?id=40000000",
            now - 3 * HOUR,
            "A collaborative, tiling desktop with a CRDT-native UI toolkit.",
            vec![
                para(
                    "I've spent the last eight months building a spatial operating system where every window tiles inside a 2D cluster and apps share a single CRDT document for live collaboration.",
                ),
                para(
                    "The UI toolkit is retained-mode and renders with wgpu + vello. State lives entirely in an Automerge document, which makes undo/redo and multiplayer fall out almost for free.",
                ),
                bullet("No floating windows — clusters tile on an infinite canvas."),
                bullet("Each cluster is its own bubblewrap sandbox."),
                para("Happy to answer questions in the comments!"),
            ],
        ),
    );
    article(
        &mut s,
        SampleArticle::new(
            hn.clone(),
            "The hidden cost of microservices",
            "patio11",
            "https://news.ycombinator.com/item?id=40000001",
            now - 7 * HOUR,
            "When a monolith is the right call after all.",
            vec![para(
                "A thoughtful retrospective on splitting a 40-person startup into 60 services, and quietly merging half of them back two years later.",
            )],
        ),
    );
    article(
        &mut s,
        SampleArticle::new(
            hn.clone(),
            "SQLite is not a toy database",
            "dang",
            "https://news.ycombinator.com/item?id=40000002",
            now - 14 * HOUR,
            "Why SQLite powers more apps than you think.",
            vec![para(
                "SQLite runs in your phone, your browser, your car, and most of the planes you fly on. A look at the engineering that makes it bulletproof.",
            )],
        )
        .read(),
    );
    article(
        &mut s,
        SampleArticle::new(
            hn,
            "Ask HN: How do you stay focused?",
            "anon",
            "https://news.ycombinator.com/item?id=40000003",
            now - 20 * HOUR,
            "The eternal question, asked again.",
            vec![para(
                "With infinite tabs and infinite notifications, how do you actually ship? The thread has 800 comments and zero consensus.",
            )],
        ),
    );

    article(
        &mut s,
        SampleArticle::new(
            lob.clone(),
            "Writing a WASM runtime in 500 lines",
            "calvin",
            "https://lobste.rs/s/abc/wasm_runtime",
            now - DAY,
            "A minimal but real interpreter you can read in an afternoon.",
            vec![
                para(
                    "WebAssembly's binary format is surprisingly approachable. This post implements a stack machine that runs real .wasm modules in half a kilobyte of code.",
                ),
                code(
                    "(module\n  (func (export \"add\") (param i32 i32) (result i32)\n    local.get 0\n    local.get 1\n    i32.add))",
                ),
                para("The decoder, validator, and interpreter together fit on one screen."),
            ],
        ),
    );
    article(
        &mut s,
        SampleArticle::new(
            lob,
            "The case against config files",
            "pushcx",
            "",
            now - 3 * DAY,
            "Code as configuration, revisited.",
            vec![para(
                "Every config language eventually grows conditionals and loops until it's a worse programming language. Maybe we should just use a real one.",
            )],
        )
        .read(),
    );

    // A long article as a feed sends it: its markup, with a list, a quote, a
    // picture, a table and a note, and an excerpt that opens with its title.
    article(
        &mut s,
        SampleArticle::new(
            quanta.clone(),
            "The Knot That Would Not Come Undone",
            "Erica Klarreich",
            "https://www.quantamagazine.org/the-knot-that-would-not-come-undone",
            now - 2 * HOUR,
            "The Knot That Would Not Come Undone For fifty years, one knot resisted every \
             attempt to tell whether it bounds a disk. Its answer changes how topologists \
             think about four dimensions.",
            Vec::new(),
        )
        .with_content(LONG_ARTICLE),
    );
    article(
        &mut s,
        SampleArticle::new(
            quanta.clone(),
            "Mathematicians prove a long-standing knot conjecture",
            "Erica Klarreich",
            "https://www.quantamagazine.org/knot-2026",
            now - 6 * HOUR,
            "A 50-year-old question about slice knots finally falls.",
            vec![
            para(
                "A pair of mathematicians have resolved a decades-old question in low-dimensional topology, proving that a famous family of knots cannot bound a smooth disk in four dimensions.",
            ),
            h2("Why knots are hard"),
            para(
                "Knot theory sits at the crossroads of algebra, geometry, and analysis. Questions that are trivial to state can resist proof for generations.",
            ),
            quote(
                "Topology is the mathematics of what survives stretching. Knots are where our intuition for that finally breaks down.",
            ),
            ],
        )
        .starred(),
    );
    article(
        &mut s,
        SampleArticle::new(
            quanta,
            "The physics of how cells know their size",
            "Yasemin Saplakoglu",
            "",
            now - 4 * DAY,
            "A surprising answer involves dilution.",
            vec![para(
                "How does a single cell measure itself? New experiments suggest the answer is a molecule whose concentration drops as the cell grows.",
            )],
        ),
    );

    article(
        &mut s,
        SampleArticle::new(
            nasa.clone(),
            "Webb captures the deepest infrared image yet",
            "NASA",
            "https://www.nasa.gov/webb-deep-field",
            now - 11 * HOUR,
            "Thousands of galaxies in a patch of sky the size of a grain of sand.",
            vec![
                para(
                    "The James Webb Space Telescope has produced its deepest image of the early universe to date, revealing galaxies whose light has travelled for more than 13 billion years.",
                ),
                bullet("Exposure time: 120 hours across six filters."),
                bullet("Faintest objects: roughly magnitude 31."),
            ],
        ),
    );
    article(
        &mut s,
        SampleArticle::new(
            nasa,
            "Artemis IV crew completes splashdown rehearsal",
            "NASA",
            "",
            now - 5 * DAY,
            "Final water-recovery test before launch.",
            vec![para(
                "The Artemis IV astronauts and recovery teams completed a full splashdown rehearsal off the coast of San Diego this week.",
            )],
        )
        .read(),
    );

    article(
        &mut s,
        SampleArticle::new(
            smashing.clone(),
            "Designing for the CRDT era",
            "Rachel Andrew",
            "https://www.smashingmagazine.com/2026/crdt-ux",
            now - 8 * HOUR,
            "What collaborative-by-default means for interface design.",
            vec![
                para(
                    "When every document is multiplayer, presence becomes a first-class design material. We explore patterns for cursors, selections, and conflict-free editing.",
                ),
                h2("Presence without noise"),
                para(
                    "Showing five remote cursors is delightful. Showing fifty is chaos. Good presence design fades collaborators into the periphery until they act.",
                ),
                bullet("Use colour sparingly — one hue per collaborator, nothing more."),
                bullet("Animate arrivals and departures so the eye can track them."),
            ],
        ),
    );
    article(
        &mut s,
        SampleArticle::new(
            smashing,
            "Modern CSS nesting in practice",
            "Stephanie Eckles",
            "",
            now - 6 * DAY,
            "Native nesting is here — use it well.",
            vec![para(
                "CSS nesting shipped in every major browser. A field guide to using it without recreating the specificity nightmares of preprocessors past.",
            )],
        )
        .read(),
    );

    let gem_body = "# Antenna — aggregated gemlogs\n\nWelcome to Antenna, a feed of recent posts from across Geminispace.\n\n## Recent entries\n\n=> gemini://example.org/log/2026-06-30.gmi On writing small software\n=> gemini://another.capsule/posts/tea.gmi A defence of loose-leaf tea\n\nGemini is a lightweight internet protocol sitting between Gopher and the Web. Pages are written in gemtext:\n\n```\n# A heading\n* a list item\n=> gemini://link.example a link line\n```\n\n> Slow down. Read one thing at a time.\n\nThat's the whole format. No tracking, no ads, no JavaScript.";
    let gem_blocks = gemtext::to_blocks(gem_body);
    article(
        &mut s,
        SampleArticle::new(
            antenna,
            "Antenna — fresh posts from Geminispace",
            "antenna",
            "gemini://warmedal.se/~antenna/",
            now - 90 * 60,
            "A digest of recent gemlog entries, rendered from gemtext.",
            gem_blocks,
        ),
    );

    s
}

/// A folder named `name`, under an id derived from it.
fn folder(s: &mut Library, name: &str) -> Id {
    let id = Id::from_raw(format!("sample-folder-{}", name.to_lowercase()));
    s.folders.push(id.clone(), Folder::named(name));
    id
}

fn feed(
    s: &mut Library,
    title: &str,
    url: &str,
    site: &str,
    folder: Option<Id>,
    kind: FeedKind,
) -> Id {
    let id = Id::from_raw(format!("sample-feed-{}", s.feeds.len()));
    s.feeds.push(
        id.clone(),
        Feed {
            title: title.into(),
            url: url.into(),
            site_url: site.into(),
            folder,
            kind,
            certificate_pin: None,
        },
    );
    id
}

struct SampleArticle<'a> {
    feed_id: Id,
    title: &'a str,
    author: &'a str,
    url: &'a str,
    published: i64,
    summary: &'a str,
    blocks: Vec<ContentBlock>,
    /// The markup a feed sent, for an article that came with it.
    content: &'a str,
    read: bool,
    starred: bool,
}

impl<'a> SampleArticle<'a> {
    fn new(
        feed_id: Id,
        title: &'a str,
        author: &'a str,
        url: &'a str,
        published: i64,
        summary: &'a str,
        blocks: Vec<ContentBlock>,
    ) -> Self {
        Self {
            feed_id,
            title,
            author,
            url,
            published,
            summary,
            blocks,
            content: "",
            read: false,
            starred: false,
        }
    }

    /// The article as a feed sends it: its markup.
    fn with_content(mut self, content: &'a str) -> Self {
        self.content = content;
        self
    }

    fn read(mut self) -> Self {
        self.read = true;
        self
    }

    fn starred(mut self) -> Self {
        self.starred = true;
        self
    }
}

fn article(s: &mut Library, fixture: SampleArticle<'_>) {
    let lower = fixture.title.to_lowercase();
    let tags = [
        lower.contains("rust").then_some("rust"),
        (lower.contains("design") || lower.contains("crdt")).then_some("design"),
        (lower.contains("space") || lower.contains("galax") || lower.contains("knot"))
            .then_some("science"),
    ]
    .into_iter()
    .flatten()
    .map(str::to_string)
    .collect();
    let id = Id::from_raw(format!("sample-article-{}", s.articles.len()));
    s.articles.push(
        id,
        Article {
            feed_id: fixture.feed_id,
            title: fixture.title.into(),
            author: fixture.author.into(),
            url: fixture.url.into(),
            published: fixture.published,
            summary: fixture.summary.into(),
            blocks: fixture.blocks,
            content: fixture.content.into(),
            read: fixture.read,
            starred: fixture.starred,
            tags,
        },
    );
}

/// The long article's markup, as its feed sends it.
const LONG_ARTICLE: &str = r##"<h1>The Knot That Would Not Come Undone</h1>
<p>For fifty years, one knot resisted every attempt to tell whether it bounds a disk in four dimensions. Mathematicians could draw it, twist it and tabulate its invariants, yet the question at its heart stayed open: is it "slice"? The answer, announced this spring, changes how topologists think about the space we cannot see.</p>
<p>A knot, to a mathematician, is a closed loop in ordinary space: a tangle of string with its ends fused. Two knots are the same if one can be wiggled into the other without cutting. The simplest knot is no knot at all, a plain circle, and much of knot theory is the art of proving that a given tangle is not secretly the circle in disguise.</p>
<h2>A question from the 1970s</h2>
<p>The knot at the centre of the story was first drawn in the 1970s, when topologists began asking which knots could be the edge of a disk that lives in four dimensions. Such knots are called slice, and telling them apart from the rest turned out to be one of the field's hardest problems<sup><a href="#fn1" id="ref1">1</a></sup>.</p>
<p>The proof that finally settled it rests on three ideas, each borrowed from a different corner of mathematics:</p>
<ol>
<li>a new invariant, computed from a family of surfaces the knot bounds;</li>
<li>a way to compare that invariant across knots that differ by a single twist; and</li>
<li>a computer search that ruled out every disk the first two ideas left standing.</li>
</ol>
<blockquote><p>Topology is the mathematics of what survives stretching. Knots are where our intuition for that finally breaks down.</p></blockquote>
<figure><img src="https://www.quantamagazine.org/wp-content/uploads/knot-diagram.png" width="1600" height="900" alt="A diagram of the knot, drawn as a loop that crosses itself eleven times"><figcaption>The knot crosses itself eleven times, few enough to draw by hand and enough to hide its secret for half a century.</figcaption></figure>
<h2>What the numbers say</h2>
<p>The invariant that cracked it can be computed for every knot in the tables. For the knots nearest this one, it reads:</p>
<table><tr><th>Knot</th><th>Crossings</th><th>Slice</th></tr><tr><td>Unknot</td><td>0</td><td>Yes</td></tr><tr><td>Trefoil</td><td>3</td><td>No</td></tr><tr><td>The knot</td><td>11</td><td>No</td></tr></table>
<p>"It's the kind of result that seems inevitable only after you've seen it," said one topologist who was not involved in the work. "Everyone had a guess. Nobody had a proof."</p>
<p>The answer is no: the knot is not slice. But the method matters more than the verdict. The same invariant now tells apart thousands of knots that no earlier tool could, and it hints at a structure in four-dimensional space that topologists are only beginning to map.</p>
<section class="footnotes"><ol><li id="fn1">Slice knots come in two kinds, smooth and topological, and the difference between them is itself a window onto the strangeness of four dimensions. <a href="#ref1" class="footnote-backref">↩</a></li></ol></section>"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_store_covers_demo_surfaces_and_preserves_states() {
        // --- phase: sample_store_covers_every_demo_surface ---
        {
            let s = store();
            assert_eq!(s.folders.len(), 3);
            assert_eq!(s.feeds.len(), 7);
            assert_eq!(s.articles.len(), 17);
            // One article comes as a feed sends it, with its markup.
            assert!(s.articles.values().any(|a| a.content.contains("<ol>")));
            assert!(s.feeds.values().any(|f| f.kind == FeedKind::Gemini));

            // The gemini article has gemtext-derived blocks (incl. a link).
            let gem_feed = s.feed_rows().find(|f| f.kind == FeedKind::Gemini).unwrap();
            let gem_art = s
                .articles
                .values()
                .find(|a| &a.feed_id == gem_feed.id)
                .unwrap();
            assert!(gem_art.blocks.iter().any(|b| b.kind == BlockKind::Link));
        }

        // --- phase: sample_article_states_and_inferred_tags_are_preserved ---
        {
            let s = store();
            let article = |title: &str| {
                s.articles
                    .values()
                    .find(|article| article.title == title)
                    .unwrap()
            };

            let release = article("Announcing Rust 1.96.1");
            assert!(!release.read);
            assert!(release.starred);
            assert_eq!(release.tag_list(), ["rust"]);

            let borrow_checker = article("A new era for the borrow checker");
            assert!(borrow_checker.read);
            assert!(!borrow_checker.starred);
            assert!(borrow_checker.tags.is_empty());

            let knot = article("Mathematicians prove a long-standing knot conjecture");
            assert!(!knot.read);
            assert!(knot.starred);
            assert_eq!(knot.tag_list(), ["science"]);

            let collaboration = article("Designing for the CRDT era");
            assert_eq!(collaboration.tag_list(), ["design"]);
        }
    }
}
