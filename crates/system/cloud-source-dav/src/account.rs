//! From a Settings account to a [`DavConfig`].
//!
//! Settings stores `WebDAV` and Nextcloud accounts as opaque JSON
//! (`server_url`, `username`, Nextcloud's `use_files`); the password lives
//! in the vault. This is the one place that JSON becomes a mount
//! configuration, so Files, the daemon and the tests agree on it.

use tilo_dav::Url;

use crate::DavConfig;

pub const WEBDAV_TYPE: &str = "webdav";
pub const NEXTCLOUD_TYPE: &str = "nextcloud";

/// Why an account cannot be mounted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountError {
    /// Not a file account (another type, or Nextcloud with files off).
    NotFiles,
    /// The stored configuration is unusable (no URL, not HTTPS, userinfo
    /// or a query in the URL, no Nextcloud user).
    InvalidConfiguration,
}

/// The mount configuration and the login name of a `WebDAV` or Nextcloud
/// account. HTTPS is required, except for a loopback host, which never
/// leaves the machine (a local server, or a test's fake).
pub fn config_for_account(
    type_id: &str,
    config: &serde_json::Value,
) -> Result<(DavConfig, String), AccountError> {
    if !matches!(type_id, WEBDAV_TYPE | NEXTCLOUD_TYPE) {
        return Err(AccountError::NotFiles);
    }
    let text = |key: &str| config.get(key).and_then(serde_json::Value::as_str);
    if type_id == NEXTCLOUD_TYPE
        && !config
            .get("use_files")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
    {
        return Err(AccountError::NotFiles);
    }
    let url = Url::parse(text("server_url").ok_or(AccountError::InvalidConfiguration)?)
        .map_err(|_| AccountError::InvalidConfiguration)?;
    let secure = cloud_source::is_secure_endpoint(url.as_str());
    if !secure
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AccountError::InvalidConfiguration);
    }
    let username = text("username").unwrap_or_default().to_owned();
    let config = if type_id == NEXTCLOUD_TYPE {
        if username.is_empty() || username.len() > 256 {
            return Err(AccountError::InvalidConfiguration);
        }
        DavConfig::nextcloud(url, &username).ok_or(AccountError::InvalidConfiguration)?
    } else {
        DavConfig::generic(url)
    };
    Ok((config, username))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nextcloud_accounts_mount_their_files_root() {
        let (config, user) = config_for_account(
            NEXTCLOUD_TYPE,
            &json!({"server_url": "https://cloud.example/base", "username": "ada"}),
        )
        .unwrap();
        assert_eq!(user, "ada");
        assert_eq!(
            config.root.as_str(),
            "https://cloud.example/base/remote.php/dav/files/ada/"
        );
        assert_eq!(
            config_for_account(
                NEXTCLOUD_TYPE,
                &json!({"server_url": "https://cloud.example", "username": "ada", "use_files": false}),
            ),
            Err(AccountError::NotFiles)
        );
    }

    #[test]
    fn only_https_or_loopback_urls_without_secrets_are_mounted() {
        for url in [
            "http://files.example/dav",
            "https://ada:pw@files.example/dav",
            "https://files.example/dav?token=1",
        ] {
            assert_eq!(
                config_for_account(WEBDAV_TYPE, &json!({"server_url": url})),
                Err(AccountError::InvalidConfiguration),
                "{url}"
            );
        }
        let (config, _) = config_for_account(
            WEBDAV_TYPE,
            &json!({"server_url": "http://127.0.0.1:8080/dav"}),
        )
        .unwrap();
        assert_eq!(config.root.as_str(), "http://127.0.0.1:8080/dav/");
        assert_eq!(
            config_for_account("caldav", &json!({})),
            Err(AccountError::NotFiles)
        );
    }
}
