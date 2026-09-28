//! Private hosted enrollment custody. Only the owner facade admits a Peer state.
use tilo_user_identity as identity;
// Browser-only owner continuation over `tilo_cloud_enrollment`.
mod owner;
// Bilateral machine-mesh approvals signed by enrolled owner roots.
pub(crate) mod mesh;
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(unused_imports, reason = "re-exports used only by the wasm32 build")
)]
pub(crate) use owner::{CLOCK_SKEW, CloudOwner, CloudStep, EnrolledOwner, create, scope};
