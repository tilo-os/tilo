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
use ui::tilo_resource::{Edit, Id, Invoke};

use crate::markup::{PdfMarkup, StoredSignature};
use crate::model::{
    AnnoKind, Annotation, Comment, GeomSpace, HighlightColor, PdfAnchor, PtRect, Reply,
    SignatureInk,
};

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
    "form",
    "loading",
    "error",
];

const WITHOUT_DOCUMENT: &[&str] = &["empty", "loading", "error"];

pub(crate) const SAMPLE_PDF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sample.pdf"));
pub(crate) const SAMPLE_NAME: &str = "Field Notes on Estuarine Birds.pdf";

pub(crate) const FORM_PDF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/form.pdf"));
const FORM_NAME: &str = "Application.pdf";
const FORM: &str = "form";

pub(crate) const SELECTED_WORD: &str = "harrier";

pub(crate) const NOTE: &str = "sample-note";
pub(crate) const NOTE_COMMENT: &str = "sample-note-comment";
pub(crate) const NOTE_COMMENT_BODY: &str = "pdf-viewer-sample-comment";
const HIGHLIGHT: &str = "sample-highlight";

pub(crate) const SIGNED_ON: &str = "2026-06-30";

pub(crate) const SIGNER: &str = "Alice Example";

pub(crate) fn shows_sample(scenario: Option<&str>) -> bool {
    scenario.is_none_or(|name| !WITHOUT_DOCUMENT.contains(&name))
}

pub(crate) fn fixture(fixture: &mut Fixture<PdfMarkup>) {
    let document = if fixture.scenario() == Some(FORM) {
        FORM_PDF
    } else {
        SAMPLE_PDF
    };
    if shows_sample(fixture.scenario())
        && let Err(error) = fixture.content().put(document)
    {
        tracing::warn!(%error, "keeping the sample document failed");
    }
}

pub(crate) fn scenario(scenario: Option<&str>) -> Vec<Invoke<PdfMarkup>> {
    if !shows_sample(scenario) {
        return Vec::new();
    }
    if scenario == Some(FORM) {
        return filled_form();
    }
    let mut actions = vec![crate::actions::pin_source_action(
        &ui::tilo_resource::Bytes::of(SAMPLE_PDF),
        SAMPLE_NAME,
    )];
    actions.extend(sample_review().iter().map(Invoke::from));
    actions
}

fn filled_form() -> Vec<Invoke<PdfMarkup>> {
    use crate::actions::set_form_value_action;
    use crate::model::FormEntry;
    vec![
        crate::actions::pin_source_action(&ui::tilo_resource::Bytes::of(FORM_PDF), FORM_NAME),
        set_form_value_action("name", &FormEntry::Text("Amara Okafor".into())),
        set_form_value_action(
            "agree",
            &FormEntry::Checked {
                on: true,
                export: None,
            },
        ),
        set_form_value_action("colour", &FormEntry::Choice("Blue".into())),
    ]
}

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
    let underline = vec![(0.14, 0.86), (0.5, 0.9), (0.82, 0.8)];
    vec![flourish, underline]
}

#[allow(
    clippy::too_many_lines,
    reason = "one demonstration review, item by item"
)]
pub fn sample_review() -> Vec<Edit<PdfMarkup>> {
    let paths = PdfMarkup::paths();
    let markup = |id: &str, mark: AnnoKind, color: HighlightColor, author: &str| {
        paths.annotations.insert(&Annotation {
            id: id.into(),
            page_id: "source-1".into(),
            kind: mark,
            color,
            author: author.into(),
            ..Annotation::default()
        })
    };
    let comment = |id: &str,
                   anno: Option<&str>,
                   author: &str,
                   body: &str,
                   resolved: bool,
                   replies: &[(&str, &str, &str)]| {
        let anchor = PdfAnchor {
            page_id: "source-1".into(),
            annotation: anno.unwrap_or_default().into(),
        };
        let mut thread = Comment {
            resolved,
            ..Comment::new(anchor, author.into(), String::new(), body.into())
        };
        for (id, author, body) in replies {
            thread.replies.push(
                Id::from_raw(*id),
                Reply::new((*author).into(), String::new(), (*body).into()),
            );
        }
        paths
            .comments
            .insert_as(&Id::from_raw(id), usize::MAX, &thread)
    };
    vec![
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
            &crate::t(NOTE_COMMENT_BODY),
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
            "This sentence is the thesis of the whole notebook — let's keep it verbatim.",
            false,
            &[],
        ),
        comment(
            "sample-page-comment",
            None,
            "Carol",
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

#[cfg(test)]
pub(crate) fn sample_state() -> ui::State {
    let mut state = record_state();
    for edit in sample_review() {
        let action = Invoke::from(edit).build(0).expect("the review builds");
        ui::tilo_actions::Action::apply(&action, &mut state.doc).expect("the review applies");
    }
    state
}

#[cfg(test)]
pub(crate) fn sample_reader() -> crate::PdfApp {
    let mut app = crate::PdfApp::blank_document(3);
    app.apply_stage(None);
    app
}
