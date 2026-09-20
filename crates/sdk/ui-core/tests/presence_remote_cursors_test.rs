#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use ui::presence::{PointerOwner, PresenceCtx, RemoteCursor};

#[test]
fn presence_ctx_holds_remote_cursors() {
    let mut ctx = PresenceCtx::default();
    ctx.remote_cursors.push(RemoteCursor {
        user_id: "peer-1".into(),
        color: [1.0, 0.3, 0.2, 1.0],
        position: [120.0, 80.0],
        label: "Ada".into(),
        owner: PointerOwner::Window,
    });
    assert_eq!(ctx.remote_cursors.len(), 1);
    assert_eq!(ctx.remote_cursors[0].user_id, "peer-1");
}
