//! A report's way out of the device it was written on: the authenticated
//! transport that files it as the destination app's own action, and the
//! canonical owner's signed receipt that says the owner durably kept it.
//!
//! The transport holds no destination of its own: a host supplies an
//! already-admitted enrollment, and the owner validates every request against
//! its current policy. This crate captures no surface, connects no account and
//! links no desktop.

mod transport;

pub use transport::{
    CanonicalDestination, CanonicalReceiptSource, CanonicalRecord, Capture, Enrollment, Identity,
    NoCapture, ReceiptSigner, ReceiptSigningRequest, ReportProjection, SessionTransport,
    SignedReceipt, Startup, Transport,
};
