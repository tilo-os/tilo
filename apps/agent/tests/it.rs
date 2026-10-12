// One test binary instead of one per file: each tests/*.rs linked the whole
// dependency graph separately. Files stay where they are; #[path] pulls them
// in as modules (`pub`, so what each file exported stays exported). nextest
// still runs each test in its own process.

#[path = "typed_peer.rs"]
mod typed_peer;

#[path = "ai_transforms.rs"]
pub mod ai_transforms;
#[path = "harness_certificate_authority.rs"]
pub mod harness_certificate_authority;
#[path = "presence_semantics.rs"]
pub mod presence_semantics;
