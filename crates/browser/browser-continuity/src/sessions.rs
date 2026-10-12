//! Explicitly selected website sessions; all cookie values are private credentials.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

pub const MAX_COOKIES: usize = 1024;
pub const MAX_SESSION_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Service {
    Slack,
    Discord,
}
impl Service {
    pub fn allows_host(self, host: &str) -> bool {
        let base = match self {
            Self::Slack => "slack.com",
            Self::Discord => "discord.com",
        };
        valid_host(host)
            && (host == base
                || host
                    .strip_suffix(base)
                    .is_some_and(|prefix| prefix.ends_with('.')))
    }
    pub fn url(self) -> &'static str {
        match self {
            Self::Slack => "https://app.slack.com/",
            Self::Discord => "https://discord.com/app",
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Discord => "discord",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SameSite {
    Unspecified,
    None,
    Lax,
    Strict,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cookie {
    pub host: String,
    pub domain: String,
    pub host_only: bool,
    pub name: String,
    pub value: String,
    pub path: String,
    pub expires_unix_seconds: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
}
impl fmt::Debug for Cookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cookie { private credential }")
    }
}
impl Drop for Cookie {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.value.zeroize();
    }
}
impl Cookie {
    /// # Errors
    /// Refuses malformed or oversized cookies, foreign service domains and weakened prefixes.
    pub fn validate(&self, service: Service) -> std::io::Result<()> {
        let valid = service.allows_host(&self.host)
            && service.allows_host(&self.domain)
            && (self.host == self.domain
                || self
                    .host
                    .strip_suffix(&self.domain)
                    .is_some_and(|prefix| prefix.ends_with('.')))
            && (!self.host_only || self.host == self.domain)
            && !self.name.is_empty()
            && self.name.len() <= 256
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b))
            && self.value.len() <= 16384
            && self
                .value
                .bytes()
                .all(|b| matches!(b, 0x21 | 0x23..=0x2b | 0x2d..=0x3a | 0x3c..=0x5b | 0x5d..=0x7e))
            && self.path.starts_with('/')
            && self.path.len() <= 2048
            && !self.path.chars().any(|v| v.is_control() || v == ';')
            && self
                .expires_unix_seconds
                .is_none_or(|v| v > 0 && v <= 253_402_300_799)
            && (self.same_site != SameSite::None || self.secure)
            && (!self.name.starts_with("__Secure-") || self.secure)
            && (!self.name.starts_with("__Host-")
                || self.secure && self.host_only && self.path == "/");
        if valid { Ok(()) } else { Err(invalid()) }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginStorage {
    pub origin: String,
    pub key: String,
    pub value: String,
    pub source_snapshot_digest: [u8; 32],
}
impl fmt::Debug for OriginStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OriginStorage { private credential }")
    }
}
impl Drop for OriginStorage {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.value.zeroize();
    }
}
impl OriginStorage {
    /// # Errors
    /// Refuses any storage beyond the explicitly selected Discord token value.
    /// Values remain raw DOM strings, including original quotes; no token syntax
    /// or provider authentication is inferred from their presence.
    pub fn validate(&self, service: Service) -> std::io::Result<()> {
        if service != Service::Discord
            || self.origin != "https://discord.com"
            || self.key != "token"
            || self.value.is_empty()
            || self.value.len() > 16 * 1024
            || self.source_snapshot_digest == [0; 32]
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCookies {
    pub schema: u32,
    pub session_id: String,
    pub service: Service,
    pub identity_public_key: [u8; 32],
    pub setup_binding: String,
    pub source_snapshot_digest: [u8; 32],
    pub cookies: Vec<Cookie>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin_storage: Vec<OriginStorage>,
}
impl fmt::Debug for SessionCookies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionCookies { private selected session }")
    }
}
impl SessionCookies {
    /// # Errors
    /// Refuses empty/oversized selections, unsafe bindings, duplicate cookies or invalid credentials.
    pub fn validate(&self) -> std::io::Result<()> {
        if !matches!(self.schema, 1 | 2)
            || (self.schema == 1 && !self.origin_storage.is_empty())
            || !valid_id(&self.session_id)
            || self.identity_public_key == [0; 32]
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || (self.cookies.is_empty() && self.origin_storage.is_empty())
            || self.cookies.len() > MAX_COOKIES
            || self.origin_storage.len() > 1
        {
            return Err(invalid());
        }
        let mut keys = BTreeSet::new();
        let mut bytes = 0usize;
        for cookie in &self.cookies {
            cookie.validate(self.service)?;
            if !keys.insert((&cookie.domain, cookie.host_only, &cookie.path, &cookie.name)) {
                return Err(invalid());
            }
            for value in [
                &cookie.host,
                &cookie.domain,
                &cookie.path,
                &cookie.name,
                &cookie.value,
            ] {
                bytes = bytes.checked_add(value.len()).ok_or_else(invalid)?;
            }
        }
        for storage in &self.origin_storage {
            storage.validate(self.service)?;
            for value in [&storage.origin, &storage.key, &storage.value] {
                bytes = bytes.checked_add(value.len()).ok_or_else(invalid)?;
            }
        }
        if bytes > MAX_SESSION_BYTES {
            return Err(invalid());
        }
        Ok(())
    }
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || matches!(v, b'-' | b'_'))
}
fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'-')
        })
}
fn invalid() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "Selected website session is unavailable",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cookie() -> Cookie {
        Cookie {
            host: "app.slack.com".into(),
            domain: "slack.com".into(),
            host_only: false,
            name: "d".into(),
            value: "private-session-marker".into(),
            path: "/".into(),
            expires_unix_seconds: None,
            secure: true,
            http_only: true,
            same_site: SameSite::None,
        }
    }
    #[test]
    fn cookie_semantics_prefixes_origin_and_redaction_are_preserved() {
        let original = cookie();
        original.validate(Service::Slack).unwrap();
        let roundtrip: Cookie =
            serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(roundtrip, original);
        assert!(roundtrip.expires_unix_seconds.is_none());
        assert!(!roundtrip.host_only);
        assert!(!format!("{roundtrip:?}").contains("private-session-marker"));
        assert!(roundtrip.validate(Service::Discord).is_err());
        for name in ["__Host-auth", "bad;name", "bad name"] {
            let mut value = cookie();
            value.name = name.into();
            assert!(value.validate(Service::Slack).is_err());
        }
        let mut value = cookie();
        value.name = "__Host-auth".into();
        value.host_only = true;
        value.domain = value.host.clone();
        value.validate(Service::Slack).unwrap();
        value.secure = false;
        assert!(value.validate(Service::Slack).is_err());
        let mut value = cookie();
        value.value = "cookie\nheader".into();
        assert!(value.validate(Service::Slack).is_err());
    }
    #[test]
    fn selected_session_bounds_and_duplicate_cookie_identity_refuse() {
        let mut selection = SessionCookies {
            schema: 1,
            session_id: "connection-id".into(),
            service: Service::Slack,
            identity_public_key: [7; 32],
            setup_binding: "operation".into(),
            source_snapshot_digest: [1; 32],
            cookies: vec![cookie()],
            origin_storage: Vec::new(),
        };
        selection.validate().unwrap();
        assert!(!format!("{selection:?}").contains("private-session-marker"));
        selection.cookies.push(cookie());
        assert!(selection.validate().is_err());
        selection.cookies.pop();
        selection.identity_public_key = [0; 32];
        assert!(selection.validate().is_err());
    }

    #[test]
    fn selected_discord_origin_only_preserves_raw_dom_value_and_legacy_refuses() {
        let raw = "\"private-origin-marker\"";
        let mut session = SessionCookies {
            schema: 2,
            session_id: "selected-discord".into(),
            service: Service::Discord,
            identity_public_key: [7; 32],
            setup_binding: "operation".into(),
            source_snapshot_digest: [9; 32],
            cookies: Vec::new(),
            origin_storage: vec![OriginStorage {
                origin: "https://discord.com".into(),
                key: "token".into(),
                value: raw.into(),
                source_snapshot_digest: [8; 32],
            }],
        };
        session.validate().unwrap();
        let restored: SessionCookies =
            serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
        assert_eq!(restored, session);
        assert_eq!(restored.origin_storage[0].value, raw);
        assert!(
            !format!("{restored:?} {:?}", restored.origin_storage[0])
                .contains("private-origin-marker")
        );
        session.schema = 1;
        assert!(session.validate().is_err());
        session.schema = 2;
        session.service = Service::Slack;
        assert!(session.validate().is_err());
        session.service = Service::Discord;
        session.origin_storage[0].origin = "https://discord.com.evil.example".into();
        assert!(session.validate().is_err());
        session.origin_storage[0].origin = "https://discord.com".into();
        session
            .origin_storage
            .push(session.origin_storage[0].clone());
        assert!(session.validate().is_err());
    }
}
