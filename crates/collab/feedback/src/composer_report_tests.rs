use std::cell::RefCell;
use std::rc::Rc;

use durable_store::{MemoryStore, StorageKey};

use super::*;
use crate::report::{Area, CropCapture, Pick, PickedText, TextKind};
use crate::{Composer, DeliveryState, Destination, FeedbackHost, Outbox, Receipt, ReviewText};

#[derive(Default)]
struct Calls {
    persisted: Vec<Vec<u8>>,
    sent: Vec<Submission>,
}

struct Host(Rc<RefCell<Calls>>);

impl FeedbackHost for Host {
    fn offers_screenshots(&self) -> bool {
        true
    }
    fn context(&mut self) -> Result<Vec<ReviewText>, Error> {
        Ok(Vec::new())
    }
    fn capture(&mut self) -> Result<MaskedScreenshot, Error> {
        Err(Error::CaptureProtected)
    }
    fn persist_screenshot(&mut self, image: &MaskedScreenshot) -> Result<(), Error> {
        self.0.borrow_mut().persisted.push(image.png().to_vec());
        Ok(())
    }
    fn send(&mut self, submission: &Submission) -> Result<(), Error> {
        self.0.borrow_mut().sent.push(submission.clone());
        Ok(())
    }
    fn receipt(&mut self, _: &Submission) -> Result<Option<Receipt>, Error> {
        Ok(None)
    }
}

fn composer(calls: &Rc<RefCell<Calls>>) -> Composer {
    Composer::new(
        Destination {
            room: "room".into(),
            provider: "record:so.tilo.chat:".into(),
            channel: "c_feedback".into(),
        },
        Outbox::open(Rc::new(MemoryStore::new()), StorageKey::package("feedback")).unwrap(),
        Box::new(Host(Rc::clone(calls))),
    )
    .unwrap()
}

fn picked() -> PickedReport {
    let pick = Pick {
        app: "so.tilo.chat".into(),
        app_name: "Chat".into(),
        build: None,
        path: "Chat[0]/Row[2]".into(),
        role: "ListItem".into(),
        label: None,
        record: None,
        area: Area {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        },
        texts: vec![
            PickedText {
                kind: TextKind::Content,
                text: "Thanks Ada Lovelace".into(),
                area: None,
            },
            PickedText {
                kind: TextKind::Interface,
                text: "Reply".into(),
                area: None,
            },
        ],
    };
    let crop = CropCapture {
        width: 2,
        height: 2,
        rgba: vec![255; 16],
        sensitive: Vec::new(),
    };
    PickedReport {
        draft: ReportDraft {
            picks: vec![pick],
            names: vec!["Ada Lovelace".into()],
            ..ReportDraft::default()
        },
        crops: vec![crop],
    }
}

fn words(summary: &str) -> ReportWords {
    ReportWords {
        summary: summary.into(),
        notes: vec!["the row is cut off".into()],
        slow: false,
    }
}

#[test]
fn a_picked_report_sends_exactly_its_review_with_its_picture() {
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut composer = composer(&calls);
    composer.begin_report(picked());
    let id = "0123456789abcdef0123456789abcdef".to_owned();
    assert!(matches!(
        composer.submit_report(id.clone(), &words("Cut off")),
        Err(Error::NotApproved)
    ));
    composer.review_report(&words("Cut off")).unwrap();
    let review = composer.report_review().unwrap().clone();
    assert!(review.feedback.screenshot.is_some());
    assert!(review.feedback.selections[0].crop.is_some());
    assert!(
        matches!(
            composer.submit_report(id.clone(), &words("Cut off, edited")),
            Err(Error::NotApproved)
        ),
        "edited words need another look"
    );
    composer.submit_report(id, &words("Cut off")).unwrap();
    let calls = calls.borrow();
    assert_eq!(
        calls.persisted.len(),
        1,
        "the picture is kept before it is queued"
    );
    assert_eq!(calls.sent.len(), 1);
    assert_eq!(calls.sent[0].feedback, review.feedback);
    assert!(
        !serde_json::to_string(&calls.sent[0])
            .unwrap()
            .contains("Ada")
    );
    assert!(composer.report_sent(), "the sent report stays shown");
    assert!(matches!(
        composer.review_report(&words("Again")),
        Err(Error::Conflict)
    ));
    assert_eq!(
        composer.state().unwrap(),
        Some(DeliveryState::AwaitingReceipt)
    );
}

#[test]
fn toggling_shows_content_and_a_model_only_adds_to_the_review_it_saw() {
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut composer = composer(&calls);
    composer.begin_report(picked());
    let words = words("");
    composer.review_report(&words).unwrap();
    assert!(
        composer.toggle_redaction(99).is_err(),
        "only a listed span toggles"
    );
    let index = composer
        .report_review()
        .unwrap()
        .redactions
        .iter()
        .position(|redaction| redaction.value() == "Thanks Ada Lovelace")
        .unwrap();
    composer.toggle_redaction(index).unwrap();
    let shown = composer.report_review().unwrap();
    assert_eq!(
        shown.feedback.selections[0].texts[0].plain(),
        "Thanks \u{2039}name\u{203A}",
        "the content shows, but the name of someone here stays hidden"
    );
    composer.model_pending(&words, MODEL);
    let pending = ModelCheck::Pending { by: MODEL.into() };
    assert_eq!(composer.model_check(), Some(&pending));
    composer.apply_model(&self::words("other words"), None);
    assert_eq!(
        composer.model_check(),
        Some(&pending),
        "a pass over other words is ignored"
    );
    composer.apply_model(&words, None);
    assert_eq!(
        composer.model_check(),
        Some(&ModelCheck::Failed { by: MODEL.into() })
    );
    assert!(
        composer.report_review().is_some(),
        "the rules' review stands"
    );
    assert!(composer.report_request().is_some());
    composer.abandon_review();
    assert!(composer.picked().is_none(), "closing discards the picks");
}

const MODEL: &str = "Anthropic claude-haiku-5-5";

/// The model is asked about the model view alone, and what it hides is laid
/// over the rules' review, which it cannot undo; the review names it.
#[test]
fn a_model_sees_the_model_view_and_adds_to_the_floor_by_name() {
    use crate::report::{Exposure, model_prompt, model_proposals};
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut composer = composer(&calls);
    composer.begin_report(picked());
    let words = words("Ada says hi");
    composer.review_report(&words).unwrap();
    let floor = composer.report_review().unwrap().hidden();
    let (prompt, view) = model_prompt(&composer.report_request().unwrap(), Exposure::Placeholders);
    assert!(
        !prompt.contains("Lovelace") && !prompt.contains("Thanks"),
        "personal values reach the model only as placeholders: {prompt}"
    );
    assert!(prompt.contains("Reply"), "{prompt}");
    composer.apply_model(&words, None);
    assert_eq!(
        composer.model_check(),
        Some(&ModelCheck::Unavailable),
        "an answer nobody asked for is not laid over the review"
    );
    composer.model_pending(&words, MODEL);
    let index = view.iter().position(|text| text.shown == "Reply").unwrap();
    let answer = format!("{{\"hide\":[{{\"text\":{index},\"quote\":\"Reply\"}}]}}");
    composer.apply_model(&words, Some(model_proposals(&view, &answer).unwrap()));
    assert_eq!(
        composer.model_check(),
        Some(&ModelCheck::Applied {
            added: 1,
            by: MODEL.into()
        })
    );
    let review = composer.report_review().unwrap();
    assert_eq!(review.hidden(), floor + 1, "the floor stands beside it");
    assert!(
        !serde_json::to_string(&review.feedback)
            .unwrap()
            .contains("Lovelace")
    );
}
