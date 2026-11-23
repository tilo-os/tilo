//! Why something one of the person's devices said, or asked, is not taken.

use thiserror::Error;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FabricError {
    #[error("what a device says of itself exceeds its bound")]
    TooLarge,
    #[error("a value was authored by a different device")]
    ForeignAuthorship,
    #[error("invalid device label")]
    InvalidLabel,
    #[error("invalid identifier")]
    InvalidIdentifier,
    #[error("clipboard item exceeds the sync limit")]
    ClipboardItemTooLarge,
    #[error("clipboard MIME type is invalid or secret-only")]
    InvalidClipboardMime,
    #[error("clipboard content hash does not match its payload")]
    ClipboardHashMismatch,
    #[error("clipboard history is full of pinned entries")]
    ClipboardHistoryFull,
    #[error("invalid encrypted Wi-Fi credential")]
    InvalidWifiCredential,
    #[error("invalid display placement")]
    InvalidDisplayPlacement,
    #[error("invalid input lease")]
    InvalidInputLease,
    #[error("input lease has expired")]
    ExpiredInputLease,
    #[error("a device says one thing twice")]
    DuplicateItem,
    #[error("unknown target device")]
    UnknownDevice,
    #[error("invalid personal-fabric action")]
    InvalidAction,
    #[error("personal-fabric action exceeds its wire bound")]
    ActionTooLarge,
    #[error("invalid speaker group")]
    InvalidSpeakerGroup,
    #[error("that device cannot play in this speaker group")]
    IneligibleSpeakerMember,
    #[error("invalid disk-unlock enrolment or setting")]
    InvalidUnlockEnrolment,
    #[error("invalid wake route")]
    InvalidWakeRoute,
}
