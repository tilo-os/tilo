//! The screenshot scenarios: a real PDF reviewed by three people, staged
//! through the reader's own actions.
//!
//! The document is the frozen test asset `sample.pdf` ("Field Notes on
//! Estuarine Birds", three US-Letter pages), kept in the capture's content
//! store and pinned as the record's source, so the reader opens it through
//! the PDF worker exactly as it opens a file it was handed. The markup is
//! written as the field edits that put each item in place, with fixed ids,
//! dates and authors, so every capture of a scenario is the same frame. The
//! view state a scenario shows (an open editor, a selection, a menu) is
//! staged by the reader itself once the document has opened
//! (`crate::PdfApp::stage`).

use ui::framework::Fixture;
use ui::tilo_resource::{Collection, Edit, Id, Invoke};

use crate::markup::{PdfMarkup, StoredAnnotation, StoredComment, StoredReply, StoredSignature};
use crate::model::{AnnoKind, GeomSpace, HighlightColor, PtRect, SignatureInk};

/// Every named scenario `--shots --scenario NAME` captures.
pub const SCENARIOS: &[&str] = &[
    "empty",
    "edit-comment",
    "selection-hud",
    "page-bubble",
    "context-menu",
    "search",
    "sign",
    "reorder",
    "import",
    "loading",
    "error",
];

/// The scenarios that show the reader before any document is open.
const WITHOUT_DOCUMENT: &[&str] = &["empty", "loading", "error"];

/// The document the scenarios show.
pub(crate) const SAMPLE_PDF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sample.pdf"));
/// The name it is pinned under.
pub(crate) const SAMPLE_NAME: &str = "Field Notes on Estuarine Birds.pdf";

/// The word of page 1 the selection scenario selects.
pub(crate) const SELECTED_WORD: &str = "harrier";

/// The sticky note the review lands on, and the comment pinned to it.
pub(crate) const NOTE: &str = "sample-note";
pub(crate) const NOTE_COMMENT: &str = "sample-note-comment";
pub(crate) const NOTE_COMMENT_BODY: &str =
    "Should we name the harrier's species here, as the later sections do?";
const HIGHLIGHT: &str = "sample-highlight";

/// Fixed date stamped on the demonstration signature, so captures stay the
/// same frame; signatures placed at runtime read the system clock.
pub(crate) const SIGNED_ON: &str = "2026-06-30";

/// The signer the demonstration review is signed by.
pub(crate) const SIGNER: &str = "Alice Example";

/// Whether `scenario` shows the sample document.
pub(crate) fn shows_sample(scenario: Option<&str>) -> bool {
    scenario.is_none_or(|name| !WITHOUT_DOCUMENT.contains(&name))
}

/// What the capture of `fixture`'s scenario reads that its actions do not
/// stage: the document's bytes, in the content store the record's source
/// is read from.
pub(crate) fn fixture(fixture: &mut Fixture<PdfMarkup>) {
    if shows_sample(fixture.scenario())
        && let Err(error) = fixture.content().put(SAMPLE_PDF)
    {
        tracing::warn!(%error, "keeping the sample document failed");
    }
}

/// The actions that stage `scenario`: the sample document pinned as the
/// record's source and the review on it, on every scenario that shows a
/// document; nothing on the others.
pub(crate) fn scenario(scenario: Option<&str>) -> Vec<Invoke<PdfMarkup>> {
    if !shows_sample(scenario) {
        return Vec::new();
    }
    let len = u64::try_from(SAMPLE_PDF.len()).unwrap_or(u64::MAX);
    let mut actions = vec![crate::actions::pin_source_action(
        &tilo_content::hash_bytes(SAMPLE_PDF),
        len,
        SAMPLE_NAME,
    )];
    actions.extend(sample_review().iter().map(Invoke::from));
    actions
}

/// A plausible hand-drawn signature as normalised `[0,1]²` strokes, so a
/// capture shows a real drawn signature rather than a canned style.
pub(crate) fn sample_drawn_signature() -> Vec<Vec<(f64, f64)>> {
    use std::f64::consts::PI;
    let n = 64;
    let flourish: Vec<(f64, f64)> = (0..=n)
        .map(|i| {
            let t = f64::from(i) / f64::from(n);
            let x = 0.06 + 0.86 * t;
            let y = 0.55 - 0.34 * (t * PI * 2.6).sin() * (1.0 - 0.35 * t);
            (x, y)
        })
        .collect();
    // A short underline flick beneath the flourish.
    let underline = vec![(0.14, 0.86), (0.5, 0.9), (0.82, 0.8)];
    vec![flourish, underline]
}

/// The review on page 1 of the sample document: a highlight over the
/// sentence that opens its second paragraph, a sticky note with a thread
/// beside the third, a comment on the highlight, a resolved page comment and
/// a drawn signature in the blank lower half of the page.
#[allow(
    clippy::too_many_lines,
    reason = "one demonstration review, item by item"
)]
pub fn sample_review() -> Vec<Edit<PdfMarkup>> {
    let paths = PdfMarkup::paths();
    let markup = |id: &str, mark: AnnoKind, color: HighlightColor, author: &str| {
        paths.annotations.insert(&StoredAnnotation {
            id: id.into(),
            page_id: "source-1".into(),
            mark,
            color,
            author: author.into(),
            chars: None,
            created_ms: 0,
            space: GeomSpace::Page,
        })
    };
    let comment = |id: &str,
                   anno: Option<&str>,
                   author: &str,
                   created: i64,
                   body: &str,
                   resolved: bool,
                   replies: &[(&str, &str, &str)]| {
        let mut thread = Collection::new();
        for (id, author, body) in replies {
            thread.push(
                Id::from_raw(*id),
                StoredReply {
                    author: (*author).into(),
                    text: (*body).into(),
                    rich: Vec::new(),
                },
            );
        }
        paths.comments.insert(&StoredComment {
            id: id.into(),
            anno: anno.unwrap_or_default().into(),
            page_id: "source-1".into(),
            author: author.into(),
            text: body.into(),
            rich: Vec::new(),
            created,
            resolved,
            replies: thread,
        })
    };
    vec![
        // The lines of 11 pt Helvetica sit on baselines 16 pt apart; the
        // second paragraph's first two are at 155.65 and 171.65 pt from the
        // page top.
        markup(
            HIGHLIGHT,
            AnnoKind::highlight(PtRect::new(31.0, 146.5, 550.0, 12.5)),
            HighlightColor::Yellow,
            "Bob",
        ),
        markup(
            "sample-highlight-2",
            AnnoKind::highlight(PtRect::new(31.0, 162.5, 78.0, 12.5)),
            HighlightColor::Yellow,
            "Bob",
        ),
        markup(
            NOTE,
            AnnoKind::Note { x: 540.0, y: 222.0 },
            HighlightColor::Blue,
            "Alice",
        ),
        paths.signatures.insert(&StoredSignature {
            id: "sample-signature".into(),
            page_id: "source-1".into(),
            rect: PtRect::new(318.0, 612.0, 218.0, 74.0),
            signer: SIGNER.into(),
            date: SIGNED_ON.into(),
            ink: SignatureInk::Drawn {
                strokes: sample_drawn_signature(),
            },
            space: GeomSpace::Page,
        }),
        comment(
            NOTE_COMMENT,
            Some(NOTE),
            "Alice",
            1,
            NOTE_COMMENT_BODY,
            false,
            &[
                (
                    "sample-reply-1",
                    "Bob",
                    "Good catch — a northern harrier; added it.",
                ),
                ("sample-reply-2", "Alice", "Perfect, thanks."),
            ],
        ),
        comment(
            "sample-highlight-comment",
            Some(HIGHLIGHT),
            "Bob",
            2,
            "This sentence is the thesis of the whole notebook — let's keep it verbatim.",
            false,
            &[],
        ),
        comment(
            "sample-page-comment",
            None,
            "Carol",
            3,
            "Typography reads cleanly at fit-page zoom. Approved.",
            true,
            &[],
        ),
    ]
}

/// A brand-new markup record, as a host makes one: its genesis, at the
/// storage version the reader reads (a bare document reads as version 1,
/// which the reader refuses until it is migrated).
#[cfg(test)]
pub(crate) fn record_state() -> ui::State {
    let mut state = ui::State::new();
    <crate::markup::PdfMarkup as ui::tilo_resource::Resource>::initialize(&mut state.doc)
        .expect("the genesis writes");
    state
}

/// A record holding the review, as its actions write it: what a test of the
/// reader starts from.
#[cfg(test)]
pub(crate) fn sample_state() -> ui::State {
    let mut state = record_state();
    for edit in sample_review() {
        let action = Invoke::from(edit).build(0).expect("the review builds");
        ui::tilo_actions::Action::apply(&action, &mut state.doc).expect("the review applies");
    }
    state
}

/// A reader showing three blank US-Letter pages as the review's view state
/// stages it, before its first frame: read with [`sample_state`]. For the
/// tests of pages, geometry and markup, which need no text; the tests of
/// text open the sample document through the PDF worker.
#[cfg(test)]
pub(crate) fn sample_reader() -> crate::PdfApp {
    let mut app = crate::PdfApp::blank_document(3);
    app.apply_stage(None);
    app
}
