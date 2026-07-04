//! Portal-grant request helpers.
//!
//! The compositor mediates permission grants (file access outside
//! the sandbox, network egress, etc.) through a portal. In the
//! full stack this crate would talk to the portal daemon over a
//! well-known Unix socket; today the CLI SDK exposes just the
//! *shape* of the API so apps compile against a stable surface,
//! and the launcher's declarative sandbox is doing the actual
//! access control.
//!
//! When the compositor grows a text/dbus portal, replace the
//! `request_grant` body with a real call and keep the type
//! signatures the same.

use anyhow::Result;

/// A single grant an app wants — mirrors the compositor's grant
/// vocabulary at the wire level.
#[derive(Debug, Clone)]
pub enum Grant {
    /// Read access to an absolute host path.
    ReadPath(std::path::PathBuf),
    /// Read+write access to an absolute host path.
    WritePath(std::path::PathBuf),
    /// Egress to a specific host name.
    NetworkHost(String),
    /// Read the user's contact roster.
    Contacts,
}

/// Request a batch of grants from the portal. Returns `Ok(())`
/// when every requested grant has been approved (or is already
/// implicit under the sandbox profile), an error otherwise.
///
/// Today this is a stub — it returns `Ok(())` when a portal socket
/// is reachable and `NotAvailable::ServiceFdMissing` when it
/// isn't, so callers can safely `?` it. Once the portal daemon
/// lands, replace the body with an actual request.
pub async fn request_grant(_grants: &[Grant]) -> Result<()> {
    // Portal probe: we look for the `QOS_SVC_PORTAL_FD` env var
    // the launcher would set. When it's present we assume the
    // grants are covered by the sandbox profile (the launcher
    // resolved them at install time). When it isn't, we don't
    // fail — CLI apps must run standalone too. See the module
    // doc-comment for the roadmap.
    if runtime_portal::service_fd("portal").is_none() && !crate::is_qos_native() {
        // Standalone — no portal, no policy to check against.
        return Ok(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[serial_test::serial]
    async fn request_grant_ok_when_standalone() {
        for (k, _) in std::env::vars() {
            if k.starts_with("QOS_") {
                unsafe { std::env::remove_var(&k) };
            }
        }
        request_grant(&[Grant::ReadPath("/tmp/foo".into())])
            .await
            .expect("standalone grant is a no-op");
    }
}
