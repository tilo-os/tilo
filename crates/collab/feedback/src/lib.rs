//! Feedback leaves the device only after review and durable local queueing.
//!
//! The outbox retains one immutable submission identity through failures and
//! restarts. A destination receipt binds that identity, destination and payload
//! digest; local dispatch alone cannot mark a submission delivered.
//!
//! Hosts supply their existing durable profile store and authenticated room
//! transport. This crate neither captures surfaces nor connects accounts.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test assertions exercise production storage and privacy paths"
    )
)]

mod capture;
mod outbox;
mod review;

pub use capture::{CapturePermit, MaskedScreenshot, PixelRect, SurfaceCapture};
pub use outbox::{DeliveryState, Outbox};
pub use review::{Draft, Preview};

use serde::{Deserialize, Serialize};

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

    /// The immutable report and routing binding used by destination receipts.
    ///
    /// # Errors
    /// Refuses an invalid submission or an unencodable payload.
    pub fn digest(&self) -> Result<String, Error> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
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
