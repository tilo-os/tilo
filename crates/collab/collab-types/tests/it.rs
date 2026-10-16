// One test binary instead of one per file; #[path] keeps the files in place.

#[path = "canonical.rs"]
mod canonical;
#[path = "capability_model.rs"]
mod capability_model;
#[path = "caps.rs"]
mod caps;
#[path = "compaction.rs"]
mod compaction;
#[path = "invite.rs"]
mod invite;
#[path = "replay_defence.rs"]
mod replay_defence;
#[path = "signing.rs"]
mod signing;
#[path = "validator.rs"]
mod validator;
