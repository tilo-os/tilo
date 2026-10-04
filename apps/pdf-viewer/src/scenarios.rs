//! The screenshot scenarios: the bundled demonstration document reviewed by
//! three people, staged through the reader's own actions.
//!
//! The markup is ordinary [`PdfViewerAction`]s with fixed ids, dates and
//! authors, so every capture of a scenario is the same frame. The view state
//! a scenario shows (an open editor, a selection, a menu) is staged by the
//! reader itself once these have applied ([`crate::PdfApp::stage`]).

use ui::framework::Fixture;
use ui::tilo_resource::Invoke;

use crate::actions::{PdfViewerAction, add_annotation_on};
use crate::model::{AnnoKind, Annotation, GeomSpace, PtRect, Swatch};
use crate::resource::PdfViewer;

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

/// The sticky note the review lands on, and the comment pinned to it.
pub(crate) const NOTE: &str = "sample-note";
pub(crate) const NOTE_COMMENT: &str = "sample-note-comment";
pub(crate) const NOTE_COMMENT_BODY: &str =
    "Should we spell out the Iroh relay and persistent-host fallback paths here?";
const HIGHLIGHT: &str = "sample-highlight";

/// Fixed date stamped on the demonstration signature, so captures stay the
/// same frame; signatures placed at runtime read the system clock.
pub(crate) const SIGNED_ON: &str = "2026-06-30";

/// The signer the demonstration review is signed by.
pub(crate) const SIGNER: &str = "Alice Daskju";

/// A capture of a scenario that shows a document reads the bundled
/// demonstration document rather than a file.
pub struct SampleDocument;

/// Whether `scenario` shows the bundled document.
pub(crate) fn shows_sample(scenario: Option<&str>) -> bool {
    scenario.is_none_or(|name| !WITHOUT_DOCUMENT.contains(&name))
}

/// What the capture of `fixture`'s scenario reads that its actions do not
/// stage.
pub(crate) fn fixture(fixture: &mut Fixture<PdfViewer>) {
    if shows_sample(fixture.scenario()) {
        fixture.provide(SampleDocument);
    }
}

/// The actions that stage `scenario`: the demonstration review on every
/// scenario that shows the bundled document, nothing on the others.
pub(crate) fn scenario(scenario: Option<&str>) -> Vec<Invoke<PdfViewer>> {
    if !shows_sample(scenario) {
        return Vec::new();
    }
    sample_review()
        .iter()
        .filter_map(|action| Invoke::apply(action).ok())
        .collect()
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

/// The demonstration review on page 1: two highlights over the abstract, a
/// sticky note with a thread, a comment on a highlight, a resolved page
/// comment and a drawn signature — real review markup anchored to real text.
pub fn sample_review() -> Vec<PdfViewerAction> {
    let markup = |id: &str, kind: AnnoKind, color: Swatch, author: &str| {
        add_annotation_on(
            &Annotation {
                id: id.into(),
                page: 1,
                kind,
                color,
                author: author.into(),
                chars: None,
                created_ms: 0,
                space: GeomSpace::Page,
            },
            Some("source-1"),
        )
    };
    let comment = |id: &str, anno: Option<&str>, author: &str, created: i64, body: &str| {
        PdfViewerAction::AddComment {
            id: id.into(),
            anno_id: anno.map(str::to_owned),
            page: 1,
            author: author.into(),
            created,
            body: body.into(),
        }
    };
    let reply = |id: &str, author: &str, body: &str| PdfViewerAction::AddReply {
        comment_id: NOTE_COMMENT.into(),
        id: id.into(),
        author: author.into(),
        body: body.into(),
        rich: Vec::new(),
    };
    vec![
        markup(
            HIGHLIGHT,
            AnnoKind::highlight(PtRect::new(64.0, 196.0, 420.0, 14.0)),
            Swatch::Yellow,
            "Bob",
        ),
        markup(
            "sample-highlight-2",
            AnnoKind::highlight(PtRect::new(64.0, 210.0, 300.0, 14.0)),
            Swatch::Yellow,
            "Bob",
        ),
        markup(
            NOTE,
            AnnoKind::Note { x: 506.0, y: 300.0 },
            Swatch::Blue,
            "Alice",
        ),
        PdfViewerAction::AddSignature {
            id: "sample-signature".into(),
            page: 1,
            x: 318.0,
            y: 612.0,
            w: 218.0,
            h: 74.0,
            signer: SIGNER.into(),
            date: SIGNED_ON.into(),
            strokes: Some(sample_drawn_signature()),
            page_id: Some("source-1".into()),
            user_space: false,
        },
        comment(NOTE_COMMENT, Some(NOTE), "Alice", 1, NOTE_COMMENT_BODY),
        reply(
            "sample-reply-1",
            "Bob",
            "Good catch — folded it into section 1.2.",
        ),
        reply("sample-reply-2", "Alice", "Perfect, thanks."),
        comment(
            "sample-highlight-comment",
            Some(HIGHLIGHT),
            "Bob",
            2,
            "This sentence is the thesis of the whole paper — let's keep it verbatim.",
        ),
        comment(
            "sample-page-comment",
            None,
            "Carol",
            3,
            "Typography reads cleanly at fit-page zoom. Approved.",
        ),
        PdfViewerAction::SetCommentResolved {
            id: "sample-page-comment".into(),
            resolved: true,
        },
    ]
}

/// A document holding the demonstration review, as its actions write it:
/// what a test of the reader starts from.
#[cfg(test)]
pub(crate) fn sample_state() -> ui::State {
    let mut state = ui::State::new();
    for action in sample_review() {
        ui::tilo_actions::Action::apply(&action, &mut state.doc).expect("the review applies");
    }
    state
}

/// The demonstration document as a capture of the review shows it, before
/// its first frame: read with [`sample_state`].
#[cfg(test)]
pub(crate) fn sample_reader() -> crate::PdfApp {
    let mut app = crate::PdfApp::sample();
    app.stage(None);
    app
}
