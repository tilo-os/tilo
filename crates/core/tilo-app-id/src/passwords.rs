//! Where the workspace's vault is kept: the record of Passwords (`apps/tl/passwords`) that every
//! device's vault actions take their steps in, and the policy declares sealed.

/// The kind of the workspace's vault.
pub const VAULT_KIND: &str = "passwords.workspace-vault";
/// The id of the workspace's vault record, which its declaration binds to [`VAULT_KIND`].
pub const VAULT_RECORD: &str = "uuid:56daeee1-adbe-4b63-b0cc-5fd549e93077";
