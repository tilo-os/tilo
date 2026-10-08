use tilo_feedback_review::{
    DeclaredSensitivity, MASK, ReviewStep, ReviewText, ReviewTraceRequest, StepKind, StepOutcome,
    TextOrigin,
};

use crate::{ContextField, Destination, Error, MaskedScreenshot, ReviewedFeedback, Submission};

/// A user-authored issue. Context starts empty and must be explicitly attached.
/// Raw draft/context text stays in memory until privacy review has masked it.
#[derive(Default)]
pub struct Draft {
    pub text: String,
    pub context: Vec<ReviewText>,
}

/// A masked local preview. Editing/redacting it invalidates prior approval.
pub struct Preview {
    feedback: ReviewedFeedback,
    approved: bool,
    screenshot: Option<MaskedScreenshot>,
}

impl Draft {
    /// Build a deterministic preview, treating user issue text as user-authored
    /// public text and preserving declared personal-value redactions everywhere.
    ///
    /// # Errors
    /// Refuses invalid or oversized review inputs; never truncates redactions.
    pub fn preview(&self) -> Result<Preview, Error> {
        let mut texts = vec![ReviewText {
            node: "feedback.issue".into(),
            sensitivity: DeclaredSensitivity::Public,
            kind: None,
            origin: TextOrigin::Semantic,
            text: self.text.clone(),
        }];
        texts.extend(self.context.clone());
        let request = ReviewTraceRequest {
            steps: vec![ReviewStep {
                kind: StepKind::Invoked,
                outcome: StepOutcome::Done,
                app: "Feedback".into(),
                action: "Submit feedback".into(),
                resource_kind: "tilo.feedback".into(),
                texts,
            }],
        };
        let proposals = tilo_feedback_review::review_redactions_without_model(&request)?;
        let mut fields = request
            .steps
            .iter()
            .flat_map(|step| &step.texts)
            .enumerate()
            .map(|(index, text)| {
                let mut spans: Vec<_> = proposals
                    .iter()
                    .filter(|proposal| proposal.step == 0 && proposal.node == text.node)
                    .map(|proposal| proposal.span)
                    .collect();
                spans.sort_unstable();
                let mut value = String::new();
                let mut cursor = 0_usize;
                for span in spans {
                    let start = usize::try_from(span.start)
                        .unwrap_or(usize::MAX)
                        .max(cursor);
                    let end = usize::try_from(span.end).unwrap_or(usize::MAX);
                    if start >= end {
                        continue;
                    }
                    value.push_str(text.text.get(cursor..start).unwrap_or_default());
                    let count = text
                        .text
                        .get(start..end)
                        .unwrap_or_default()
                        .chars()
                        .count();
                    value.extend(std::iter::repeat_n(MASK, count));
                    cursor = end;
                }
                value.push_str(text.text.get(cursor..).unwrap_or_default());
                ContextField {
                    // Semantic ids may embed a path or a personal value. They
                    // address local redactions, never travel as report context.
                    key: if index == 0 {
                        "feedback.issue".into()
                    } else {
                        format!("context:{index}")
                    },
                    value,
                }
            });
        let text = fields.next().ok_or(Error::Invalid("issue"))?.value;
        Ok(Preview {
            feedback: ReviewedFeedback {
                text,
                context: fields.collect(),
                screenshot: None,
            },
            approved: false,
            screenshot: None,
        })
    }
}

impl Preview {
    /// Adding or replacing a masked screenshot requires fresh preview approval.
    pub fn attach_screenshot(&mut self, screenshot: MaskedScreenshot) {
        self.feedback.screenshot = Some(screenshot.reference().clone());
        self.screenshot = Some(screenshot);
        self.approved = false;
    }

    pub fn screenshot(&self) -> Option<&MaskedScreenshot> {
        self.screenshot.as_ref()
    }

    /// Removing an attachment is a change to what the user approves.
    pub fn remove_screenshot(&mut self) {
        self.screenshot = None;
        self.feedback.screenshot = None;
        self.approved = false;
    }

    pub fn feedback(&self) -> &ReviewedFeedback {
        &self.feedback
    }

    /// Add a user-chosen redaction to the masked preview, never undo the floor.
    ///
    /// # Errors
    /// Refuses unknown fields and invalid UTF-8 boundaries or empty spans.
    pub fn hide(&mut self, key: &str, start: usize, end: usize) -> Result<(), Error> {
        let text = if key == "feedback.issue" {
            &mut self.feedback.text
        } else {
            &mut self
                .feedback
                .context
                .iter_mut()
                .find(|field| field.key == key)
                .ok_or(Error::Invalid("review field"))?
                .value
        };
        let slice = text
            .get(start..end)
            .filter(|slice| !slice.is_empty())
            .ok_or(Error::Invalid("review span"))?;
        let count = slice.chars().count();
        text.replace_range(
            start..end,
            &std::iter::repeat_n(MASK, count).collect::<String>(),
        );
        self.approved = false;
        Ok(())
    }

    /// The user's explicit confirmation of the currently displayed preview.
    pub fn approve(&mut self) {
        self.approved = true;
    }

    /// Produce the immutable payload only after the user approved its preview.
    ///
    /// # Errors
    /// Refuses unapproved previews or invalid submission identity/destination.
    pub fn submission(&self, id: String, destination: Destination) -> Result<Submission, Error> {
        if !self.approved {
            return Err(Error::NotApproved);
        }
        let submission = Submission {
            id,
            destination,
            feedback: self.feedback.clone(),
        };
        submission.validate()?;
        Ok(submission)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination() -> Destination {
        Destination {
            room: "demo".into(),
            provider: "record:chat:feedback".into(),
            channel: "feedback".into(),
        }
    }

    #[test]
    fn personal_context_and_repeated_credentials_are_masked_before_approval() {
        let draft = Draft {
            text: "Opening Jana's book failed; token=secret12345".into(),
            context: vec![ReviewText {
                node: "contact.name".into(),
                sensitivity: DeclaredSensitivity::Personal,
                kind: Some(tilo_feedback_review::RedactionCategory::Name),
                origin: TextOrigin::Semantic,
                text: "Jana".into(),
            }],
        };
        let mut preview = draft.preview().unwrap();
        let json = serde_json::to_string(preview.feedback()).unwrap();
        assert!(!json.contains("Jana"));
        assert!(!json.contains("secret12345"));
        let id = "0123456789abcdef0123456789abcdef".to_string();
        assert!(matches!(
            preview.submission(id.clone(), destination()),
            Err(Error::NotApproved)
        ));
        preview.approve();
        let submitted = preview.submission(id, destination()).unwrap();
        assert_eq!(&submitted.feedback, preview.feedback());
    }

    #[test]
    fn a_new_manual_redaction_requires_fresh_approval() {
        let mut preview = Draft {
            text: "Issue with my diagnosis".into(),
            context: Vec::new(),
        }
        .preview()
        .unwrap();
        preview.approve();
        preview.hide("feedback.issue", 14, 23).unwrap();
        assert!(matches!(
            preview.submission("0123456789abcdef0123456789abcdef".into(), destination()),
            Err(Error::NotApproved)
        ));
        assert!(!preview.feedback().text.contains("diagnosis"));
    }

    #[test]
    fn raw_semantic_identifiers_do_not_escape_in_context_keys() {
        let draft = Draft {
            text: "This control failed".into(),
            context: vec![ReviewText {
                node: "file:private-personal-name".into(),
                sensitivity: DeclaredSensitivity::Personal,
                kind: None,
                origin: TextOrigin::Semantic,
                text: "private value".into(),
            }],
        };
        let preview = draft.preview().unwrap();
        let json = serde_json::to_string(preview.feedback()).unwrap();
        assert!(!json.contains("private-personal-name"));
        assert!(!json.contains("private value"));
        assert_eq!(preview.feedback().context[0].key, "context:1");
    }
}
