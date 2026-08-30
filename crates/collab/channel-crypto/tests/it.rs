// One test binary instead of one per file; #[path] keeps the files in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

#[path = "interop_with_signed_op.rs"]
mod interop_with_signed_op;
#[path = "outsider_cannot_decrypt.rs"]
mod outsider_cannot_decrypt;
#[path = "third_member_joins.rs"]
mod third_member_joins;
#[path = "two_member_round_trip.rs"]
mod two_member_round_trip;
