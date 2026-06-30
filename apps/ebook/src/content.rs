//! Library loader: the primary book comes from a real bundled `.epub`
//! (parsed via the ZIP container reader); two companion volumes are authored
//! as embedded XHTML and lowered through the same chapter parser, so every
//! book on the shelf is genuinely openable and reflowable.

use crate::epub::parse_epub;
use crate::model::{Book, Chapter};
use crate::xhtml::parse_xhtml;

/// The bundled EPUB, parsed by the real container reader.
const SAMPLE_EPUB: &[u8] = include_bytes!("../assets/sample.epub");

/// Build the full library.
pub fn load_library() -> Vec<Book> {
    let mut books = Vec::new();

    match parse_epub(SAMPLE_EPUB, 5, "Fable") {
        Ok(b) => books.push(b),
        Err(e) => tracing::error!(error = %e, "ebook: failed to parse bundled epub"),
    }

    books.push(authored(
        "book:field-notes",
        "Field Notes on Quiet Machines",
        "Pell Marchetti",
        "Iris House",
        7,
        "Essays",
        FIELD_NOTES,
    ));
    books.push(authored(
        "book:tideglass",
        "The Tideglass Letters",
        "Oda Fenwick",
        "Marrow & Vane",
        9,
        "Correspondence",
        TIDEGLASS,
    ));

    books
}

/// Lower a list of (title, xhtml-body) chapters into a Book.
fn authored(
    id: &str,
    title: &str,
    author: &str,
    publisher: &str,
    tint: usize,
    subject: &str,
    chapters: &[(&str, &str)],
) -> Book {
    let chapters = chapters
        .iter()
        .map(|(t, body)| Chapter {
            title: t.to_string(),
            href: format!("{}.xhtml", t),
            blocks: parse_xhtml(body),
        })
        .collect();
    Book {
        id: id.to_string(),
        title: title.to_string(),
        author: author.to_string(),
        publisher: publisher.to_string(),
        description: String::new(),
        chapters,
        cover_tint: tint,
        subject: subject.to_string(),
    }
}

const FIELD_NOTES: &[(&str, &str)] = &[
    ("On Switches", r#"<body>
<h1>On Switches</h1>
<p>The most honest object in any house is the light switch. It makes a single promise and keeps it, and it asks for nothing in return but the smallest motion of a finger. There is a whole philosophy folded into that click, if you care to unfold it.</p>
<p>Consider what a switch does <em>not</em> do. It does not explain itself. It does not update. It does not, in the small hours, decide that what you really wanted was a different room entirely. It simply is <strong>on</strong>, or it is <strong>off</strong>, and the distance between those two states is the width of a thought.</p>
<blockquote><p>We have spent a century teaching machines to guess. Perhaps the next century's luxury will be a machine that simply does as it is told.</p></blockquote>
<p>I keep a drawer of old switches. Bakelite, brass, porcelain. Each one outlived the wiring it was sold with, and most outlived the houses. They are small monuments to the idea that a thing can be finished — that design can arrive somewhere and decide to stay.</p>
</body>"#),
    ("On Waiting", r#"<body>
<h1>On Waiting</h1>
<p>A kettle is a teacher of patience that never raises its voice until the very end. You set it going and you are, for ninety seconds, returned to a slower century — there is nothing to do but wait, and waiting, it turns out, is a skill we have been quietly letting rust.</p>
<ul>
<li>The kettle does not ask you to rate your experience.</li>
<li>It does not suggest three other beverages you might enjoy.</li>
<li>It boils. It clicks off. It waits for you to come back.</li>
</ul>
<p>I have come to believe that the measure of a good machine is the quality of the waiting it asks of you. The bad ones fill the wait with noise and lights and small anxious demands. The good ones hand the time back to you, intact, and let you decide what it is for.</p>
</body>"#),
    ("On Repair", r#"<body>
<h1>On Repair</h1>
<p>To repair a thing is to argue, gently, with its ending. You take the broken machine to the bench, and you open it, and inside you find a small museum of decisions made by people you will never meet — and you add one of your own.</p>
<p>There is a particular silence in a workshop at dusk, when the soldering iron has cooled and the thing on the bench is, against all odds, working again. It is the silence of a sentence that has found its full stop. <em>There.</em> Done, for now, and done well.</p>
<blockquote><p>Nothing is finished while someone is still willing to mend it.</p></blockquote>
</body>"#),
];

const TIDEGLASS: &[(&str, &str)] = &[
    ("First Letter", r#"<body>
<h1>First Letter</h1>
<p>Dear M—,</p>
<p>The glassworks took me on this morning, which is to say a man with three fingers and a voice like a closing door looked at my hands for a long moment and then handed me an apron. I am, apparently, to learn the tide-glass: the trick of pouring colour into a pane so that it shifts as the day turns, blue at the flood and amber at the ebb.</p>
<p>He says it takes ten years to learn and a lifetime to forgive. I told him I had the lifetime to spare. He did not laugh, but the door of his voice opened, very slightly, and I think that is the local equivalent.</p>
<p>Write soon. Tell me the river is still where I left it.</p>
<p>— O.</p>
</body>"#),
    ("Second Letter", r#"<body>
<h1>Second Letter</h1>
<p>Dear M—,</p>
<p>I have ruined eleven panes. The master counts them each evening with the patience of a man counting waves, and each evening he says the same thing: <em>the glass remembers how you held it.</em> I did not understand this until today, when the twelfth pane came out of the annealer whole and humming, and I realised I had stopped fighting it somewhere around the ninth.</p>
<blockquote><p>You cannot force colour to stay. You can only make a place calm enough that it chooses to.</p></blockquote>
<p>The tide was at the half when I held it to the window, and the blue and the amber met in the middle of the pane like two people deciding, at last, to be kind to one another.</p>
<p>— O.</p>
</body>"#),
];
