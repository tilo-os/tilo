//! Reports about picked parts of the screen. The desktop hands over what
//! the person picked; they add a note to each part and one about the whole,
//! choose what of the hidden content to show, and send exactly the report
//! they reviewed.

use tilo_feedback_review::{ReviewProposals, ReviewTraceRequest};

use super::Composer;
use crate::report::{CropCapture, CropRef, ReportDraft, ReportReview, attach_crops, compose_crops};
use crate::{Error, MaskedScreenshot, Submission};

/// What the desktop picked, with each part's masked crop in the order of
/// the picks. The crops become the report's one picture when it is first
/// reviewed, in the composer's own code rather than the desktop's.
pub struct PickedReport {
    pub draft: ReportDraft,
    pub crops: Vec<CropCapture>,
}

/// What the person wrote around the picks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportWords {
    pub summary: String,
    /// A note per pick, by its place.
    pub notes: Vec<String>,
    /// "This was slow": the frame timings go with the report.
    pub slow: bool,
}

/// How far a model looked over the review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelCheck {
    /// No model reviews reports here; the rules alone did.
    Unavailable,
    /// A model is looking.
    Pending,
    /// A model looked and hid `added` more spans.
    Applied { added: usize },
    /// The model failed or answered nonsense; the rules' review stands.
    Failed,
}

pub(super) struct Picked {
    /// The crops composed into one picture, once reviewed.
    picture: Option<(MaskedScreenshot, Vec<CropRef>)>,
    report: PickedReport,
    /// The words the review shows.
    words: Option<ReportWords>,
    /// A model's proposals over the request for `words`.
    model: Option<ReviewProposals>,
    check: ModelCheck,
    review: Option<ReportReview>,
    /// Queued: the review stays shown, as what was sent, and no longer
    /// changes.
    sent: bool,
}

impl Composer {
    /// Start a report about what the person picked. An earlier review of a
    /// typed report no longer shows what would be sent.
    pub fn begin_report(&mut self, report: PickedReport) {
        self.reviewed = None;
        self.picked = Some(Picked {
            report,
            picture: None,
            words: None,
            model: None,
            check: ModelCheck::Unavailable,
            review: None,
            sent: false,
        });
    }

    /// Leave the picked report, as when its window closes.
    pub fn end_report(&mut self) {
        self.picked = None;
    }

    /// What was picked, when the composer is reporting picks.
    pub fn picked(&self) -> Option<&ReportDraft> {
        self.picked.as_ref().map(|picked| &picked.report.draft)
    }

    /// The masked crops, as one picture, that go with the report.
    pub fn report_picture(&self) -> Option<&MaskedScreenshot> {
        self.picked
            .as_ref()
            .and_then(|picked| picked.picture.as_ref())
            .map(|(picture, _)| picture)
    }

    /// The reviewed report: exactly what Send sends.
    pub fn report_review(&self) -> Option<&ReportReview> {
        self.picked
            .as_ref()
            .and_then(|picked| picked.review.as_ref())
    }

    /// Whether the picked report was queued to send.
    pub fn report_sent(&self) -> bool {
        self.picked.as_ref().is_some_and(|picked| picked.sent)
    }

    pub fn model_check(&self) -> Option<&ModelCheck> {
        self.picked.as_ref().map(|picked| &picked.check)
    }

    /// Review the picks with `words`. New words drop a model's earlier
    /// proposals: they were about other texts.
    ///
    /// # Errors
    /// Refuses with no picks, while a report is still on its way, or
    /// a report beyond its bounds.
    pub fn review_report(&mut self, words: &ReportWords) -> Result<(), Error> {
        self.settle_sent()?;
        let picked = self.picked.as_mut().ok_or(Error::Invalid("report"))?;
        if picked.sent {
            return Err(Error::Conflict);
        }
        picked.review = None;
        if picked.words.as_ref() != Some(words) {
            picked.model = None;
            picked.check = ModelCheck::Unavailable;
        }
        if picked.picture.is_none() && !picked.report.crops.is_empty() {
            picked.picture = compose_crops(std::mem::take(&mut picked.report.crops)).ok();
        }
        let draft = &mut picked.report.draft;
        draft.summary.clone_from(&words.summary);
        draft.notes.clone_from(&words.notes);
        draft.slow = words.slow;
        picked.words = Some(words.clone());
        picked.review = Some(picked.reviewed()?);
        Ok(())
    }

    /// What a model reviews of the report as last reviewed.
    pub fn report_request(&self) -> Option<ReviewTraceRequest> {
        let picked = self.picked.as_ref()?;
        picked.words.as_ref()?;
        picked.report.draft.request().ok()
    }

    /// A model is looking over the review of `words`.
    pub fn model_pending(&mut self, words: &ReportWords) {
        if let Some(picked) = self.picked.as_mut()
            && picked.words.as_ref() == Some(words)
        {
            picked.check = ModelCheck::Pending;
        }
    }

    /// Lay a model's proposals over the review of `words`; `None` when the
    /// model failed. Ignored once the words changed: the proposals were
    /// about other texts. Proposals that do not fit the request are
    /// dropped and the rules' review stands.
    pub fn apply_model(&mut self, words: &ReportWords, proposals: Option<ReviewProposals>) {
        let Some(picked) = self.picked.as_mut() else {
            return;
        };
        if picked.sent || picked.words.as_ref() != Some(words) {
            return;
        }
        let before = picked.hidden();
        picked.model = proposals;
        picked.check = ModelCheck::Failed;
        if picked.model.is_some() {
            match picked.reviewed() {
                Ok(review) if review.feedback.validate_report().is_ok() => {
                    let added = review.hidden().saturating_sub(before);
                    picked.review = Some(review);
                    picked.check = ModelCheck::Applied { added };
                    return;
                }
                _ => picked.model = None,
            }
        }
        if let Ok(review) = picked.reviewed() {
            picked.review = Some(review);
        }
    }

    /// Show or hide again the span the review lists at `index`. A
    /// credential cannot be shown.
    ///
    /// # Errors
    /// Refuses with no review or an index it does not list.
    pub fn toggle_redaction(&mut self, index: usize) -> Result<(), Error> {
        let picked = self.picked.as_mut().ok_or(Error::NotApproved)?;
        if picked.sent {
            return Err(Error::Conflict);
        }
        let review = picked.review.as_ref().ok_or(Error::NotApproved)?;
        let redaction = review
            .redactions
            .get(index)
            .ok_or(Error::Invalid("redaction"))?;
        if !redaction.revealable {
            return Err(Error::Invalid("redaction"));
        }
        let key = redaction.key.clone();
        picked.report.draft.toggle(&key);
        picked.review = Some(picked.reviewed()?);
        Ok(())
    }

    /// Send exactly the reviewed report. Words changed since review need
    /// another look first.
    ///
    /// # Errors
    /// Refuses changed words, an unreviewed report, or storage and
    /// transport failures.
    pub fn submit_report(&mut self, id: String, words: &ReportWords) -> Result<(), Error> {
        if self.active.is_some() {
            return Err(Error::Conflict);
        }
        let picked = self.picked.as_ref().ok_or(Error::NotApproved)?;
        if picked.sent || picked.words.as_ref() != Some(words) {
            return Err(Error::NotApproved);
        }
        let review = picked.review.as_ref().ok_or(Error::NotApproved)?;
        let submission = Submission {
            id: id.clone(),
            destination: self.destination.clone(),
            feedback: review.feedback.clone(),
        };
        submission.validate()?;
        if let Some((picture, _)) = &picked.picture {
            self.host.persist_screenshot(picture)?;
        }
        if let Err(error) = self.outbox.queue(submission) {
            if matches!(error, Error::Store(_)) {
                self.active = Some(id);
            }
            return Err(error);
        }
        self.active = Some(id);
        if let Some(picked) = self.picked.as_mut() {
            picked.sent = true;
        }
        self.pump()
    }
}

impl Picked {
    fn reviewed(&self) -> Result<ReportReview, Error> {
        let draft = &self.report.draft;
        let mut review = draft.review(self.model.clone())?;
        if let Some((picture, places)) = &self.picture {
            attach_crops(&mut review.feedback, picture.reference(), places);
        }
        Ok(review)
    }

    fn hidden(&self) -> usize {
        self.review.as_ref().map_or(0, ReportReview::hidden)
    }
}

#[cfg(test)]
#[path = "composer_report_tests.rs"]
mod tests;
