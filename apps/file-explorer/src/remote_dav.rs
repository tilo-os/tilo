//! Bounded HTTPS `WebDAV` transport for Files' canonical external accounts.
//!
//! This is intentionally a small *transport*, not a second account store or
//! a synthetic mount.  It reads one account and its sealed secret only while a
//! caller is performing a requested operation, rejects credential-bearing
//! redirects, and returns data for the app to materialise as resources.

use std::io::Read;
use std::time::Duration;

use accounts::{Account, AccountId, AccountSyncReporter, SecretVault};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{CONTENT_TYPE, IF_MATCH, LOCATION};
use reqwest::{Method, StatusCode, Url};
use zeroize::Zeroizing;

use crate::remote_accounts::{NEXTCLOUD_TYPE, WEBDAV_TYPE};

pub const MAX_LISTING_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TRANSFER_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REMOTE_ENTRIES: usize = 512;
const MAX_REMOTE_NAME_CHARS: usize = 255;
const MAX_REMOTE_HREF_CHARS: usize = 2_048;
const MAX_REMOTE_CONTENT_TYPE_CHARS: usize = 255;
const MAX_REMOTE_ETAG_CHARS: usize = 1_024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub href: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DavError {
    AccountUnavailable,
    SignInRequired,
    Offline,
    InvalidConfiguration,
    CrossOriginRedirect,
    InvalidRemoteResponse,
    ResponseTooLarge,
    Conflict,
    RemoteFailure,
}

impl DavError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::AccountUnavailable => "This remote file account is unavailable",
            Self::SignInRequired => "Sign in again in Settings before opening remote files",
            Self::Offline => "The remote file service is unavailable while offline",
            Self::InvalidConfiguration => "This remote account needs attention in Settings",
            Self::CrossOriginRedirect => {
                "The server redirected to another origin; credentials were not sent"
            }
            Self::InvalidRemoteResponse => "The remote server returned an invalid file response",
            Self::ResponseTooLarge => "The remote response is larger than Files can safely handle",
            Self::Conflict => "The remote file changed before this operation completed",
            Self::RemoteFailure => "The remote file operation failed",
        }
    }
}

/// A connection retains the secret only for the duration of one explicit
/// Files operation. It is not serializable, searchable, logged, or copied to
/// a UI string.
pub struct DavConnection {
    account_id: AccountId,
    origin: Url,
    root: Url,
    username: String,
    password: Zeroizing<String>,
    client: Client,
}

impl DavConnection {
    /// # Errors
    ///
    /// Returns an error if the account, sealed credential, or HTTPS client
    /// cannot be opened safely.
    pub fn open(account_id: &AccountId) -> Result<Self, DavError> {
        let account = accounts::AccountStore::load()
            .get(account_id)
            .cloned()
            .ok_or(DavError::AccountUnavailable)?;
        let (root, username) = configured_root(&account)?;
        if !account.enabled {
            return Err(DavError::AccountUnavailable);
        }
        let secret = SecretVault::open()
            .get(account_id)
            .ok_or(DavError::SignInRequired)?;
        let password = secret.password;
        if password.is_empty() {
            return Err(DavError::SignInRequired);
        }
        let origin = origin_of(&root).ok_or(DavError::InvalidConfiguration)?;
        let client = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Tilo-Files/0.2")
            .build()
            .map_err(|_| DavError::RemoteFailure)?;
        Ok(Self {
            account_id: account.id,
            origin,
            root,
            username,
            password: Zeroizing::new(password),
            client,
        })
    }

    /// # Errors
    ///
    /// Returns an error for an invalid path, rejected request, or a response
    /// larger than the metadata bound.
    pub fn list(&self, path: &str) -> Result<Vec<RemoteEntry>, DavError> {
        let url = self.url_for(path)?;
        let method = Method::from_bytes(b"PROPFIND").map_err(|_| DavError::RemoteFailure)?;
        let response = self.send(
            method,
            url,
            Some(("Depth", "1")),
            Some(PROPFIND_BODY.as_bytes()),
        )?;
        let body = self.read_bounded(response, MAX_LISTING_BYTES)?;
        let entries = parse_listing(&body, &self.root)?;
        self.report_success();
        Ok(entries)
    }

    /// # Errors
    ///
    /// Returns an error for an invalid path, rejected request, or a download
    /// larger than [`MAX_TRANSFER_BYTES`].
    pub fn download(&self, path: &str) -> Result<Vec<u8>, DavError> {
        let response = self.send(Method::GET, self.url_for(path)?, None, None)?;
        let body = self.read_bounded(response, MAX_TRANSFER_BYTES)?;
        self.report_success();
        Ok(body)
    }

    /// Download a previously listed, canonical remote URL.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::download`], and rejects an URL
    /// outside this account's configured origin/root.
    pub fn download_url(&self, url: &str) -> Result<Vec<u8>, DavError> {
        let response = self.send(Method::GET, self.known_url(url)?, None, None)?;
        let body = self.read_bounded(response, MAX_TRANSFER_BYTES)?;
        self.report_success();
        Ok(body)
    }

    /// # Errors
    ///
    /// Returns an error for an invalid path, oversized payload, rejected
    /// request, or conditional-write conflict.
    pub fn upload(&self, path: &str, bytes: &[u8], etag: Option<&str>) -> Result<(), DavError> {
        if bytes.len() > MAX_TRANSFER_BYTES {
            return Err(DavError::ResponseTooLarge);
        }
        let mut request = self.authenticated(self.client.put(self.url_for(path)?));
        if let Some(etag) = etag {
            request = request.header(IF_MATCH, etag);
        } else {
            request = request.header("If-None-Match", "*");
        }
        let response = self.send_request(request.body(bytes.to_vec()))?;
        self.expect_success(&response)?;
        self.report_success();
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error for an invalid path or rejected remote mutation.
    pub fn create_folder(&self, path: &str) -> Result<(), DavError> {
        let method = Method::from_bytes(b"MKCOL").map_err(|_| DavError::RemoteFailure)?;
        let response = self.send(method, self.url_for(path)?, None, None)?;
        self.expect_success(&response)?;
        self.report_success();
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error for invalid paths, a rejected remote mutation, or a
    /// conditional-write conflict.
    pub fn rename(&self, from: &str, to: &str, etag: Option<&str>) -> Result<(), DavError> {
        let method = Method::from_bytes(b"MOVE").map_err(|_| DavError::RemoteFailure)?;
        let destination = self.url_for(to)?;
        let mut request = self
            .authenticated(self.client.request(method, self.url_for(from)?))
            .header("Destination", destination.as_str())
            .header("Overwrite", "F");
        if let Some(etag) = etag {
            request = request.header(IF_MATCH, etag);
        }
        let response = self.send_request(request)?;
        self.expect_success(&response)?;
        self.report_success();
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error for an invalid path, rejected remote mutation, or a
    /// conditional-write conflict.
    pub fn delete(&self, path: &str, etag: Option<&str>) -> Result<(), DavError> {
        let mut request = self.authenticated(self.client.delete(self.url_for(path)?));
        if let Some(etag) = etag {
            request = request.header(IF_MATCH, etag);
        }
        let response = self.send_request(request)?;
        self.expect_success(&response)?;
        self.report_success();
        Ok(())
    }

    /// Rename a previously listed, canonical remote URL within this root.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::rename`], and rejects URLs outside
    /// this account's configured origin/root.
    pub fn rename_url(
        &self,
        from: &str,
        new_name: &str,
        etag: Option<&str>,
    ) -> Result<(), DavError> {
        if new_name.is_empty()
            || new_name.contains('/')
            || new_name.contains('\\')
            || new_name == "."
            || new_name == ".."
        {
            return Err(DavError::InvalidConfiguration);
        }
        let source = self.known_url(from)?;
        let parent = source
            .join(".")
            .map_err(|_| DavError::InvalidConfiguration)?;
        let destination = parent
            .join(new_name)
            .map_err(|_| DavError::InvalidConfiguration)?;
        let method = Method::from_bytes(b"MOVE").map_err(|_| DavError::RemoteFailure)?;
        let mut request = self
            .authenticated(self.client.request(method, source))
            .header("Destination", destination.as_str())
            .header("Overwrite", "F");
        if let Some(etag) = etag {
            request = request.header(IF_MATCH, etag);
        }
        let response = self.send_request(request)?;
        self.expect_success(&response)?;
        self.report_success();
        Ok(())
    }

    /// Delete a previously listed, canonical remote URL.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::delete`], and rejects URLs outside
    /// this account's configured origin/root.
    pub fn delete_url(&self, url: &str, etag: Option<&str>) -> Result<(), DavError> {
        let mut request = self.authenticated(self.client.delete(self.known_url(url)?));
        if let Some(etag) = etag {
            request = request.header(IF_MATCH, etag);
        }
        let response = self.send_request(request)?;
        self.expect_success(&response)?;
        self.report_success();
        Ok(())
    }

    fn url_for(&self, path: &str) -> Result<Url, DavError> {
        if path.is_empty() {
            return Ok(self.root.clone());
        }
        if path.starts_with('/')
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(DavError::InvalidConfiguration);
        }
        let mut url = self.root.clone();
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| DavError::InvalidConfiguration)?;
        for segment in path.split('/') {
            segments.push(segment);
        }
        drop(segments);
        same_origin(&self.origin, &url)
            .then_some(url)
            .ok_or(DavError::CrossOriginRedirect)
    }

    fn known_url(&self, raw: &str) -> Result<Url, DavError> {
        let url = Url::parse(raw).map_err(|_| DavError::InvalidConfiguration)?;
        (safe_response_url(&url, &self.root) && url.path().starts_with(self.root.path()))
            .then_some(url)
            .ok_or(DavError::InvalidConfiguration)
    }

    fn authenticated(&self, request: RequestBuilder) -> RequestBuilder {
        request.basic_auth(&self.username, Some(self.password.as_str()))
    }

    fn send(
        &self,
        method: Method,
        url: Url,
        header: Option<(&str, &str)>,
        body: Option<&[u8]>,
    ) -> Result<Response, DavError> {
        let mut request = self.authenticated(self.client.request(method, url));
        if let Some((name, value)) = header {
            request = request.header(name, value);
        }
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/xml; charset=utf-8")
                .body(body.to_vec());
        }
        self.send_request(request)
    }

    fn send_request(&self, request: RequestBuilder) -> Result<Response, DavError> {
        let response = request.send().map_err(|_| {
            let _ = AccountSyncReporter::new(self.account_id.clone()).offline();
            DavError::Offline
        })?;
        if response.status().is_redirection() {
            // The request builder cannot be safely replayed without retaining
            // arbitrary upload bytes. All methods therefore reject redirects;
            // endpoint discovery belongs to Settings and no credential can
            // cross an origin boundary by accident.
            let _ = response.headers().get(LOCATION);
            return Err(DavError::CrossOriginRedirect);
        }
        Ok(response)
    }

    fn expect_success(&self, response: &Response) -> Result<(), DavError> {
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                let _ = AccountSyncReporter::new(self.account_id.clone()).needs_authentication();
                Err(DavError::SignInRequired)
            }
            StatusCode::PRECONDITION_FAILED => Err(DavError::Conflict),
            _ => {
                let _ = AccountSyncReporter::new(self.account_id.clone()).failed();
                Err(DavError::RemoteFailure)
            }
        }
    }

    fn read_bounded(&self, response: Response, limit: usize) -> Result<Vec<u8>, DavError> {
        self.expect_success_status(response.status())?;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            return Err(DavError::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        response
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| DavError::Offline)?;
        if bytes.len() > limit {
            return Err(DavError::ResponseTooLarge);
        }
        Ok(bytes)
    }

    fn expect_success_status(&self, status: StatusCode) -> Result<(), DavError> {
        match status {
            status if status.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                let _ = AccountSyncReporter::new(self.account_id.clone()).needs_authentication();
                Err(DavError::SignInRequired)
            }
            _ => Err(DavError::RemoteFailure),
        }
    }

    fn report_success(&self) {
        let _ = AccountSyncReporter::new(self.account_id.clone()).succeeded();
    }
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:prop><D:displayname/><D:getcontentlength/><D:getcontenttype/><D:getetag/><D:resourcetype/></D:prop></D:propfind>"#;

fn configured_root(account: &Account) -> Result<(Url, String), DavError> {
    let type_id = account.type_id.as_str();
    if !matches!(type_id, WEBDAV_TYPE | NEXTCLOUD_TYPE) {
        return Err(DavError::AccountUnavailable);
    }
    if type_id == NEXTCLOUD_TYPE
        && !account
            .config
            .get("use_files")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true)
    {
        return Err(DavError::AccountUnavailable);
    }
    let base = account
        .config
        .get("server_url")
        .and_then(serde_json::Value::as_str)
        .ok_or(DavError::InvalidConfiguration)?;
    let mut url = Url::parse(base).map_err(|_| DavError::InvalidConfiguration)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(DavError::InvalidConfiguration);
    }
    if type_id == NEXTCLOUD_TYPE {
        let username = account
            .config
            .get("username")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 256)
            .ok_or(DavError::InvalidConfiguration)?;
        url.path_segments_mut()
            .map_err(|()| DavError::InvalidConfiguration)?
            .extend(["remote.php", "dav", "files", username]);
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    let username = account
        .config
        .get("username")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((url, username))
}

fn origin_of(url: &Url) -> Option<Url> {
    let mut origin = Url::parse(&format!("{}://{}", url.scheme(), url.host_str()?)).ok()?;
    origin.set_port(url.port()).ok()?;
    Some(origin)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn parse_listing(xml: &[u8], root: &Url) -> Result<Vec<RemoteEntry>, DavError> {
    let text = std::str::from_utf8(xml).map_err(|_| DavError::InvalidRemoteResponse)?;
    let doc = roxmltree::Document::parse(text).map_err(|_| DavError::InvalidRemoteResponse)?;
    let mut entries = Vec::new();
    for response in doc
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "response")
    {
        if entries.len() == MAX_REMOTE_ENTRIES {
            break;
        }
        let href = response
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "href")
            .and_then(|node| node.text())
            .map(str::trim)
            .ok_or(DavError::InvalidRemoteResponse)?;
        let url = root
            .join(href)
            .map_err(|_| DavError::InvalidRemoteResponse)?;
        if !safe_response_url(&url, root)
            || !url.path().starts_with(root.path())
            || url.path().trim_end_matches('/') == root.path().trim_end_matches('/')
        {
            continue;
        }
        let is_dir = response
            .descendants()
            .any(|node| node.is_element() && node.tag_name().name() == "collection");
        let name = response
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "displayname")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| {
                url.path_segments()
                    .and_then(Iterator::last)
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned)
            })
            .and_then(|name| bounded_server_text(&name, MAX_REMOTE_NAME_CHARS))
            .ok_or(DavError::InvalidRemoteResponse)?;
        let size = response
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "getcontentlength")
            .and_then(|node| node.text())
            .and_then(|value| value.trim().parse().ok());
        let content_type = response
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "getcontenttype")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| bounded_server_text(value, MAX_REMOTE_CONTENT_TYPE_CHARS));
        let etag = response
            .descendants()
            .find(|node| node.is_element() && node.tag_name().name() == "getetag")
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| bounded_server_text(value, MAX_REMOTE_ETAG_CHARS));
        let href = bounded_server_text(url.as_str(), MAX_REMOTE_HREF_CHARS)
            .ok_or(DavError::InvalidRemoteResponse)?;
        entries.push(RemoteEntry {
            name,
            href,
            is_dir,
            size,
            content_type,
            etag,
        });
    }
    Ok(entries)
}

fn bounded_server_text(value: &str, limit: usize) -> Option<String> {
    (!value.is_empty() && value.chars().count() <= limit && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}

/// Reject any response URL that could change request semantics when it is
/// later materialised into a typed action. `Url` normalises literal dot
/// components, but encoded separators/dots need an explicit check too.
fn safe_response_url(url: &Url, root: &Url) -> bool {
    same_origin(root, url)
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && !url.path().chars().any(char::is_control)
        && !contains_unsafe_percent_escape(url.path())
}

fn contains_unsafe_percent_escape(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes.get(index) == Some(&b'%') {
            let Some((&high, &low)) = bytes.get(index + 1).zip(bytes.get(index + 2)) else {
                return true;
            };
            let Some(value) = hex_byte(high, low) else {
                return true;
            };
            if value.is_ascii_control() || matches!(value, b'/' | b'\\' | b'.') {
                return true;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    false
}

fn hex_byte(high: u8, low: u8) -> Option<u8> {
    fn nibble(value: u8) -> Option<u8> {
        match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            b'A'..=b'F' => Some(value - b'A' + 10),
            _ => None,
        }
    }
    Some(
        nibble(high)?
            .saturating_mul(16)
            .saturating_add(nibble(low)?),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nextcloud_root_is_derived_from_canonical_metadata() {
        let mut account = Account::new("cloud", NEXTCLOUD_TYPE);
        account.config = serde_json::json!({"server_url":"https://cloud.example/base", "username":"ada", "use_files":true});
        let (root, username) = configured_root(&account).expect("valid account");
        assert_eq!(username, "ada");
        assert_eq!(
            root.as_str(),
            "https://cloud.example/base/remote.php/dav/files/ada/"
        );
    }

    #[test]
    fn listing_refuses_cross_origin_response_hrefs() {
        let root = Url::parse("https://cloud.example/dav/").unwrap();
        let xml = br#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>https://attacker.example/secret</D:href><D:propstat><D:prop><D:displayname>bad</D:displayname></D:prop></D:propstat></D:response><D:response><D:href>good.txt</D:href><D:propstat><D:prop><D:displayname>good.txt</D:displayname><D:resourcetype/></D:prop></D:propstat></D:response></D:multistatus>"#;
        let entries = parse_listing(xml, &root).expect("valid response");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "good.txt");
    }

    #[test]
    fn paths_cannot_escape_the_configured_root() {
        let root = Url::parse("https://cloud.example/dav/").unwrap();
        let connection = DavConnection {
            account_id: "id".into(),
            origin: origin_of(&root).unwrap(),
            root,
            username: "u".into(),
            password: Zeroizing::new("p".into()),
            client: tilo_net::http::blocking_client()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
        };
        assert!(connection.url_for("../secret").is_err());
        assert!(connection.url_for("folder/file.txt").is_ok());
    }

    #[test]
    fn encoded_separators_and_query_hrefs_are_not_materialized() {
        let root = Url::parse("https://cloud.example/dav/").unwrap();
        let encoded = br#"<D:multistatus xmlns:D="DAV:"><D:response><D:href>bad%2Fname</D:href><D:propstat><D:prop><D:displayname>bad</D:displayname></D:prop></D:propstat></D:response><D:response><D:href>report.txt?token=secret</D:href><D:propstat><D:prop><D:displayname>report</D:displayname></D:prop></D:propstat></D:response></D:multistatus>"#;
        assert!(parse_listing(encoded, &root).unwrap().is_empty());
    }

    #[test]
    fn server_text_bounds_reject_controls_and_overlong_values() {
        assert_eq!(bounded_server_text("good", 4), Some("good".into()));
        assert_eq!(bounded_server_text("bad\nname", 32), None);
        assert_eq!(bounded_server_text("long", 3), None);
    }
}
