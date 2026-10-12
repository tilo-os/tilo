//! Transport shared by sign-in and Drive: one blocking client on Tilo's TLS
//! defaults, a small cookie jar and bounded JSON bodies. Public so the
//! sign-in in `account-custody` speaks to Apple exactly as Drive does.

use std::fmt;
use std::time::{Duration, SystemTime};

use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, SET_COOKIE};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

/// Apple's endpoints answer browser clients; this is the Safari the
/// reference clients present.
pub const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.3.1 Safari/605.1.15";

/// The web origin every setup/drivews/docws request claims.
pub const ICLOUD_ORIGIN: &str = "https://www.icloud.com";

/// Largest JSON body read from any endpoint.
pub const MAX_JSON_BYTES: usize = 64 * 1024 * 1024;

/// A client that never follows redirects (Apple's 330 is handled by hand,
/// and cookies must not follow a hop) with a whole-request deadline.
pub fn client(timeout: Duration) -> Result<Client, reqwest::Error> {
    tilo_net::http::blocking_client()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Why a body could not be read as JSON.
#[derive(Debug)]
pub enum BodyError {
    TooLarge,
    Read,
    Json,
}

/// Read at most [`MAX_JSON_BYTES`] and parse them.
pub fn read_json<T: DeserializeOwned>(response: Response) -> Result<T, BodyError> {
    let bytes = read_bounded(response)?;
    serde_json::from_slice(&bytes).map_err(|_| BodyError::Json)
}

/// Read at most [`MAX_JSON_BYTES`] of a body, by
/// [`cloud_source::http::read_bounded`].
pub fn read_bounded(response: Response) -> Result<Vec<u8>, BodyError> {
    let declared = response.content_length();
    cloud_source::http::read_bounded(response, declared, MAX_JSON_BYTES).map_err(
        |error| match error {
            cloud_source::Error::TooLarge => BodyError::TooLarge,
            _ => BodyError::Read,
        },
    )
}

pub fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
}

/// The response's `Retry-After`, by [`cloud_source::http::parse_retry_after`].
pub fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    header(headers, "retry-after")
        .and_then(|value| cloud_source::http::parse_retry_after(value, SystemTime::now()))
}

/// Cookies by name, in the order first set. Values are secrets.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Cookies(Vec<(String, Zeroizing<String>)>);

impl fmt::Debug for Cookies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| name))
            .finish()
    }
}

impl Cookies {
    /// Parse a `Cookie` request header (`a=1; b=2`).
    pub fn from_header(header: &str) -> Self {
        let mut jar = Self::default();
        for pair in header.split(';') {
            if let Some((name, value)) = pair.split_once('=') {
                jar.set(name.trim(), value.trim());
            }
        }
        jar
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Set or, with an empty value, remove a cookie.
    pub fn set(&mut self, name: &str, value: &str) {
        if name.is_empty() {
            return;
        }
        if value.is_empty() || value == "\"\"" {
            self.0.retain(|(candidate, _)| candidate != name);
            return;
        }
        match self.0.iter_mut().find(|(candidate, _)| candidate == name) {
            Some((_, existing)) => *existing = Zeroizing::new(value.to_owned()),
            None => self
                .0
                .push((name.to_owned(), Zeroizing::new(value.to_owned()))),
        }
    }

    /// Apply every `Set-Cookie` of a response. A cookie expired with
    /// `Max-Age=0` or an empty value is removed.
    pub fn absorb(&mut self, headers: &HeaderMap) {
        for value in headers.get_all(SET_COOKIE) {
            let Ok(value) = value.to_str() else { continue };
            let mut parts = value.split(';');
            let Some((name, cookie)) = parts.next().and_then(|pair| pair.split_once('=')) else {
                continue;
            };
            let expired = parts.any(|attribute| {
                attribute
                    .trim()
                    .split_once('=')
                    .is_some_and(|(key, value)| {
                        key.trim().eq_ignore_ascii_case("max-age") && value.trim() == "0"
                    })
            });
            self.set(name.trim(), if expired { "" } else { cookie.trim() });
        }
    }

    /// Everything from `other` over `self`.
    pub fn merge(&mut self, other: &Self) {
        for (name, value) in &other.0 {
            self.set(name, value);
        }
    }

    /// The `Cookie` request header.
    pub fn header(&self) -> Zeroizing<String> {
        let mut out = Zeroizing::new(String::new());
        for (name, value) in &self.0 {
            if !out.is_empty() {
                out.push_str("; ");
            }
            out.push_str(name);
            out.push('=');
            out.push_str(value);
        }
        out
    }
}

impl Serialize for Cookies {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            self.0
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        )
    }
}

impl<'de> Deserialize<'de> for Cookies {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let pairs: Vec<(String, String)> = Vec::deserialize(deserializer)?;
        let mut jar = Self::default();
        for (name, value) in pairs {
            let value = Zeroizing::new(value);
            jar.set(&name, &value);
        }
        Ok(jar)
    }
}

/// Serde for secret strings: serialized as plain strings, held zeroized.
pub mod secret {
    use serde::{Deserialize, Deserializer, Serializer};
    use zeroize::Zeroizing;

    pub fn serialize<S: Serializer>(
        value: &Zeroizing<String>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Zeroizing<String>, D::Error> {
        String::deserialize(deserializer).map(Zeroizing::new)
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    #[test]
    fn set_cookie_headers_update_and_expire_the_jar() {
        let mut jar = Cookies::from_header("A=1; B=2");
        let mut headers = HeaderMap::new();
        headers.append(SET_COOKIE, HeaderValue::from_static("A=3; Path=/; Secure"));
        headers.append(SET_COOKIE, HeaderValue::from_static("B=gone; Max-Age=0"));
        headers.append(
            SET_COOKIE,
            HeaderValue::from_static("C=\"v=1:t=abc\"; Domain=.icloud.com"),
        );
        jar.absorb(&headers);
        assert_eq!(jar.header().as_str(), "A=3; C=\"v=1:t=abc\"");
        assert!(!format!("{jar:?}").contains('3'));
    }

    #[test]
    fn cookies_round_trip_through_serde() {
        let jar = Cookies::from_header("X=1; Y=2");
        let json = serde_json::to_string(&jar).unwrap();
        assert_eq!(serde_json::from_str::<Cookies>(&json).unwrap(), jar);
    }
}
