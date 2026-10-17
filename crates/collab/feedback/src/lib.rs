//! Feedback leaves the device only after review and durable local queueing.
//!
//! The outbox retains one immutable submission identity through failures and
//! restarts. A destination receipt binds that identity, destination and payload
//! digest; local dispatch alone cannot mark a submission delivered.
//!
//! Hosts supply their existing durable profile store and authenticated room
//! transport. This crate neither captures surfaces nor connects accounts.

mod capture;
mod composer;
mod outbox;
mod review;
#[cfg(test)]
mod test_store;

pub use capture::{CapturePermit, MaskedScreenshot, PixelRect, SurfaceCapture};
pub use composer::{Composer, Consent, FeedbackHost, Subject};
pub use durable_store::{DurableStore, MemoryStore, StorageKey};
pub use outbox::{DeliveryState, Outbox};
pub use review::{Draft, Preview};
pub use tilo_feedback_review::{DeclaredSensitivity, RedactionCategory, ReviewText, TextOrigin};

use serde::{Deserialize, Serialize};

/// What the destination app publishes for feedback: the host decides a
/// reporter's grants at these actions, and the app declares them under these
/// names. Items are `<RESOURCE_KIND>/<field>/<id>`, of kind
/// `<RESOURCE_KIND>.<field>`.
pub mod destination {
    /// The destination app's package.
    pub const PACKAGE: &str = "so.tilo.chat";
    /// Its resource kind.
    pub const RESOURCE_KIND: &str = "chat.workspace";
    /// The field of the channels reports are filed in.
    pub const CHANNELS: &str = "channels";
    /// The field of the filed reports.
    pub const REPORTS: &str = "feedback";
    /// The channel item's action that files a report.
    pub const SUBMIT: &str = "submit-feedback";
    /// The field filing a report sits under: it is a comment, so whoever may
    /// comment may file one.
    pub const SUBMIT_FIELD: &str = "comments";
    /// The report item's action that answers its receipt.
    pub const READ_RECEIPT: &str = "read-feedback-receipt";
    /// The channel item's read that proves the channel exists as a text
    /// channel, without joining or creating it.
    pub const READ_DESTINATION: &str = "read-feedback-destination";

    /// The resource id of the destination's item `id` in `field`.
    #[must_use]
    pub fn item(field: &str, id: &str) -> String {
        format!("{RESOURCE_KIND}/{field}/{id}")
    }
}

/// A configured destination. A label or a URL is never routing authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub room: String,
    pub provider: String,
    pub channel: String,
}

impl Destination {
    /// Validate the bounded identifiers supplied by the admitted room host.
    ///
    /// # Errors
    /// Refuses empty, oversized or control-character identifiers.
    pub fn validate(&self) -> Result<(), Error> {
        for value in [&self.room, &self.provider, &self.channel] {
            if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(Error::Invalid("destination"));
            }
        }
        Ok(())
    }
}

/// Exactly the text shown in the approved preview. No original trace or
/// account credential is a field of the submission protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedFeedback {
    pub text: String,
    pub context: Vec<ContextField>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<ScreenshotRef>,
}

/// Content-addressed masked PNG; raw surface bytes never enter the outbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotRef {
    pub hash: String,
    pub width: u32,
    pub height: u32,
}

/// A reviewable context field, already masked before persistence or transport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextField {
    pub key: String,
    pub value: String,
}

/// Mint the id once at authoring time. The same submission is retried unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub id: String,
    pub destination: Destination,
    pub feedback: ReviewedFeedback,
}

impl Submission {
    /// Validate incoming and restored payloads independently of the composer.
    ///
    /// # Errors
    /// Refuses malformed ids, destinations, empty reports or oversized context.
    pub fn validate(&self) -> Result<(), Error> {
        self.validated_bytes().map(|_| ())
    }

    fn validate_fields(&self) -> Result<(), Error> {
        if self.id.len() != 32 || !self.id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Invalid("submission id"));
        }
        self.destination.validate()?;
        if self.feedback.text.trim().is_empty()
            || self.feedback.text.len() > 8_000
            || self.feedback.context.len() > 48
        {
            return Err(Error::Invalid("feedback"));
        }
        let mut size = 0_usize;
        let mut keys = std::collections::BTreeSet::new();
        for field in &self.feedback.context {
            if field.key.is_empty()
                || field.key.len() > 256
                || field.key.chars().any(char::is_control)
                || !keys.insert(&field.key)
                || field.value.len() > 8_000
            {
                return Err(Error::Invalid("context"));
            }
            size = size.saturating_add(field.value.len());
        }
        if size > 48_000 {
            return Err(Error::Invalid("context size"));
        }
        if let Some(screenshot) = &self.feedback.screenshot
            && (screenshot.hash.len() != 64
                || !screenshot.hash.bytes().all(|c| c.is_ascii_hexdigit())
                || screenshot.width == 0
                || screenshot.height == 0
                || u64::from(screenshot.width).saturating_mul(u64::from(screenshot.height))
                    > 4_000_000)
        {
            return Err(Error::Invalid("screenshot"));
        }
        Ok(())
    }

    // Validation and receipt hashing share one bounded canonical encoder.
    // Keep its serializer out of line instead of duplicating it in callers.
    #[inline(never)]
    fn validated_bytes(&self) -> Result<Vec<u8>, Error> {
        self.validate_fields()?;
        // The existing native action transport allows 32 KiB of arguments.
        // Leave room for its action wrapper and escaped JSON, and refuse an
        // oversized report rather than truncate what the person approved.
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > 28_000 {
            return Err(Error::Invalid("encoded submission size"));
        }
        Ok(bytes)
    }

    /// The immutable report and routing binding used by destination receipts.
    ///
    /// # Errors
    /// Refuses an invalid submission or an unencodable payload.
    pub fn digest(&self) -> Result<String, Error> {
        let bytes = self.validated_bytes()?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
}

/// Read from the destination after its report record is durably committed.
/// The transport adapter must authenticate the source before passing this to
/// the outbox; a receipt supplied by a report author is not confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub id: String,
    pub digest: String,
    pub destination: Destination,
    pub report: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid feedback {0}")]
    Invalid(&'static str),
    #[error("submission id is already bound to a different report")]
    Conflict,
    #[error("the local outbox is not durably committed")]
    NotDurable,
    #[error("the feedback preview has not been approved")]
    NotApproved,
    #[error("the destination receipt does not match the submission")]
    ReceiptMismatch,
    #[error("the feedback outbox is full")]
    Full,
    #[error(transparent)]
    Store(#[from] durable_store::StoreError),
    #[error(transparent)]
    Encoding(#[from] serde_json::Error),
    #[error(transparent)]
    Review(#[from] tilo_feedback_review::ContractError),
    #[error("the requested surface is protected or its redaction geometry is unavailable")]
    CaptureProtected,
    #[error("the captured surface or revision differs from the approved capture request")]
    CaptureChanged,
    #[error(transparent)]
    Image(#[from] image::ImageError),
}

#[cfg(test)]
mod submission_tests {
    use super::*;

    fn report() -> Submission {
        Submission {
            id: "0123456789abcdef".repeat(2),
            destination: Destination {
                room: "demo".into(),
                provider: "record:chat:feedback".into(),
                channel: "feedback".into(),
            },
            feedback: ReviewedFeedback {
                text: "A useful report".into(),
                context: Vec::new(),
                screenshot: None,
            },
        }
    }

    #[test]
    fn receipt_digest_keeps_the_exact_canonical_payload_and_validation_bounds() {
        let mut submission = report();
        submission.validate().unwrap();
        assert_eq!(
            submission.digest().unwrap(),
            blake3::hash(&serde_json::to_vec(&submission).unwrap())
                .to_hex()
                .to_string()
        );
        submission.feedback.text = "\"".repeat(8_000);
        submission.feedback.context.push(ContextField {
            key: "context:1".into(),
            value: "\\".repeat(8_000),
        });
        assert!(serde_json::to_vec(&submission).unwrap().len() > 28_000);
        assert!(matches!(
            submission.validate(),
            Err(Error::Invalid("encoded submission size"))
        ));
        assert!(matches!(
            submission.digest(),
            Err(Error::Invalid("encoded submission size"))
        ));
    }
}
