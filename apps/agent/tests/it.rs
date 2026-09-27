// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "typed_peer.rs"]
mod typed_peer;

#[path = "ai_transforms.rs"]
pub mod ai_transforms;
#[path = "authorship_is_recorded.rs"]
pub mod authorship_is_recorded;
#[path = "chat_summons.rs"]
pub mod chat_summons;
#[path = "concurrent_edit.rs"]
pub mod concurrent_edit;
#[path = "discovered_operations.rs"]
pub mod discovered_operations;
#[path = "harness_certificate_authority.rs"]
pub mod harness_certificate_authority;
#[path = "presence_semantics.rs"]
pub mod presence_semantics;
#[path = "relay_roundtrip.rs"]
pub mod relay_roundtrip;
