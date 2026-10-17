//! Verify only an explicitly connected Files endpoint, before publishing custody.
use crate::account_store::ConnectionError;
use accounts::Account;
use std::time::Duration;
use tilo_dav::{Credentials, DavClient, Depth, Limits, Options, PropName, Redirects};

/// Normalize a user-entered DAV server with the same validation as connection setup.
///
/// # Errors
/// Refuses unsafe or invalid server URLs.
pub fn normalize_server(value: &str) -> Result<String, ConnectionError> {
    account_custody::nextcloud::server_url(value)
        .map(|url| url.to_string())
        .map_err(|_| ConnectionError::InvalidConfiguration)
}

/// Verify that the configured DAV root is a reachable collection with these credentials.
///
/// # Errors
/// Refuses invalid configuration, unavailable endpoints, failed authentication or invalid DAV data.
pub fn verify(account: &Account, username: &str, password: &str) -> Result<(), ConnectionError> {
    // This is the same configuration-to-root conversion used by the real mount.
    let (config, _) =
        cloud_source_dav::account::config_for_account(account.type_id.as_str(), &account.config)
            .map_err(|_| ConnectionError::InvalidConfiguration)?;
    let client = DavClient::new(&Options {
        timeout: Duration::from_secs(10),
        connect_timeout: Some(Duration::from_secs(5)),
        redirects: Redirects::Refuse,
        ..Options::default()
    })
    .map_err(|_| ConnectionError::DavEndpointUnavailable)?
    .with_credentials(Credentials::new(username, password));
    let response = client
        .propfind(
            config.root.clone(),
            Depth::Zero,
            &[PropName::dav("resourcetype")],
        )
        .map_err(|_| ConnectionError::DavEndpointUnavailable)?;
    match response.status().as_u16() {
        401 => return Err(ConnectionError::DavAuthenticationRefused),
        429 | 500..=599 => return Err(ConnectionError::DavEndpointUnavailable),
        207 => {}
        _ => return Err(ConnectionError::DavFilesUnavailable),
    }
    let listing = response
        .multistatus(&Limits {
            max_bytes: 64 * 1024,
            max_responses: 1,
            max_depth: 16,
            max_nodes: 1024,
            max_href_bytes: 4096,
        })
        .map_err(|error| match error {
            tilo_dav::Error::Read(_) | tilo_dav::Error::Transport(_) => {
                ConnectionError::DavEndpointUnavailable
            }
            _ => ConnectionError::DavFilesUnavailable,
        })?;
    let root = listing
        .responses
        .first()
        .filter(|_| listing.responses.len() == 1)
        .filter(|row| row.status.is_none_or(|status| (200..300).contains(&status)))
        .filter(|row| !row.href.is_empty())
        .filter(|row| {
            config
                .root
                .join(&row.href)
                .is_ok_and(|href| href == config.root)
        })
        .ok_or(ConnectionError::DavFilesUnavailable)?;
    let collection = root.propstats.iter().any(|properties| {
        properties
            .status
            .is_some_and(|status| (200..300).contains(&status))
            && properties.props.iter().any(|property| {
                property.name == "resourcetype"
                    && property
                        .namespace
                        .as_deref()
                        .is_none_or(|value| value == "DAV:")
                    && property.children.iter().any(|child| child == "collection")
            })
    });
    if collection {
        Ok(())
    } else {
        Err(ConnectionError::DavFilesUnavailable)
    }
}
