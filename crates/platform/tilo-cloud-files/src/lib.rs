//! Credential-free cloud Files access: credentials stay in the storage service.
//! Unsupported targets return an explicit error rather than falling back to a
//! pathname.

#[cfg(unix)]
pub use runtime_fs::cloud;
#[cfg(not(unix))]
pub mod cloud;

/// Make only a newly staged copy owner-writable, never the provider source.
pub fn owner_writable(mut permissions: std::fs::Permissions) -> std::fs::Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(false);
    permissions
}
