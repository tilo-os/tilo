//! The Chip Design app's engine: what one imported engineering document says, and the
//! structural operator design a person builds from it.
//!
//! The app runs sandboxed, so it asks here by name. Imported documents are checked against the
//! bounded contract their own evidence class declares before any of it reaches the workspace:
//! arithmetic against stated assumptions, immutable identities, the exclusions a claim must
//! keep, and the device paths a producer left in its failure text, which never leave this
//! device. The operator design is resolved here too: the typed ports each operator offers, the
//! connections that hold, the ones that do not with the reason, and every connection's content
//! identity.
//!
//! Nothing here runs, simulates or qualifies hardware. Every answer is a reported claim.

mod facts;
pub mod graph;
pub mod model;
pub mod operator;
mod ops;
pub mod verification;

pub use model::{AttentionCache, Assumptions, MAX_REPORT_BYTES, MAX_REPORTS, StorageReport, report_id};
pub use operator::{Connection, OperatorKind, OperatorNode, OperatorState};
pub use verification::{CheckStatus, ReceiptStatus, VerificationReceipt, VerificationRecord};

/// The one app this engine answers for.
pub const APP: &str = "so.tilo.chip-design";

/// One request answered now.
///
/// # Errors
///
/// The ask is not one this engine reads, or the document it names fails its own contract.
pub fn call(op: &str, input: &str) -> Result<String, String> {
    match op {
        "import" => ops::import(input),
        "graph" => ops::graph(input),
        other => Err(format!("`{other}` is no op this device runs")),
    }
}

/// No ask of this engine runs as a job: a document is checked in one breath.
///
/// # Errors
///
/// Every op, since none of them runs as work a person waits for or stops.
pub fn start(op: &str, _input: &str) -> Result<String, String> {
    Err(format!("`{op}` is no work this device starts"))
}

/// No ask of this engine runs as a job.
///
/// # Errors
///
/// Every job, since this engine starts none.
pub fn status(job: &str) -> Result<String, String> {
    Err(format!("no work `{job}` runs on this device"))
}

/// No ask of this engine runs as a job, so there is none to stop.
#[must_use]
pub fn cancel(_job: &str) -> bool {
    false
}

#[cfg(test)]
mod tests;
