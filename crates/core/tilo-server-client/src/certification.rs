//! The certificates a person's root issues: certifying a device whose key the
//! root's holder already has.

mod held_root;
pub use held_root::{
    GrantAsk, IssuedRole, SignInAsk, SignedIn, answer_sign_in, certify_with_root,
    issue_agent_grant, issue_device_cert, issue_sign_in_cert,
};
