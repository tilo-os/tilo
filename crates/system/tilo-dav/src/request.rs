//! The request vocabulary both DAV clients share: credentials, transport options,
//! `Depth` and write preconditions.

use std::fmt;
use std::time::Duration;

use zeroize::Zeroizing;

pub(crate) const XML_CONTENT_TYPE: &str = "application/xml; charset=utf-8";

pub struct Credentials {
    username: String,
    password: Zeroizing<String>,
    bearer: bool,
}

impl Credentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: Zeroizing::new(password.into()),
            bearer: false,
        }
    }

    pub fn bearer(token: impl Into<String>) -> Self {
        Self {
            username: String::new(),
            password: Zeroizing::new(token.into()),
            bearer: true,
        }
    }

    pub(crate) fn is_bearer(&self) -> bool {
        self.bearer
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("bearer", &self.bearer)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redirects {
    /// Follow up to ten hops. Both clients drop `Authorization` when a hop
    /// changes host, so credentials never follow a cross-origin redirect.
    Follow,
    /// Return the 3xx to the caller. Stored endpoints use this: discovery
    /// belongs to account setup, and a request body is never replayed.
    Refuse,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub user_agent: Option<&'static str>,
    pub timeout: Duration,
    pub connect_timeout: Option<Duration>,
    pub redirects: Redirects,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            user_agent: None,
            timeout: Duration::from_secs(20),
            connect_timeout: None,
            redirects: Redirects::Follow,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    Zero,
    One,
    Infinity,
}

impl Depth {
    pub(crate) fn header(self) -> &'static str {
        match self {
            Self::Zero => "0",
            Self::One => "1",
            Self::Infinity => "infinity",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precondition<'a> {
    None,
    /// `If-Match: <etag>`; `"*"` requires only that the resource exists.
    IfMatch(&'a str),
    /// `If-None-Match: *`: create only, never overwrite.
    IfNoneMatchAny,
}
