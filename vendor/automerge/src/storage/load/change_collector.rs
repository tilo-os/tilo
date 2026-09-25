#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("a change referenced an actor index we couldn't find")]
    MissingActor,
    #[error("changes out of order")]
    ChangesOutOfOrder,
    #[error("incorrect max op")]
    InvalidState,
    #[error("invalid internal state")]
    IncorrectMaxOp,
    #[error("missing ops")]
    MissingOps,
    #[error("missing ops")]
    MissingDep(#[from] crate::change_graph::MissingDep),
    /// qOS load-path hardening: an op or change the collector cannot encode
    /// (actor index out of range, non-numeric increment, inconsistent
    /// start/max op) in hostile storage.
    #[error("invalid op or change metadata")]
    InvalidOp,
}
