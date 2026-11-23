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

#[derive(Clone)]
pub struct Options {
    pub user_agent: Option<&'static str>,
    pub timeout: Duration,
    pub connect_timeout: Option<Duration>,
    pub redirects: Redirects,
    /// What every connection of the client asks where its server is. For a
    /// name: the addresses it is dialled at, from the one look-up the
    /// connection itself makes, so nothing an earlier look-up was answered
    /// decides where a request goes. For an address a URL writes out, which
    /// no connection looks up: the same question, before the request is sent
    /// ([`tilo_net::address::asked_as_written`]). A client given one takes no
    /// proxy from the environment, which would look names up behind it.
    /// `None`: the system's resolver and any address. Whoever keeps its
    /// requests on the public internet hands the one judge's look-up here
    /// ([`tilo_net::address::public_only`]).
    #[cfg(not(target_family = "wasm"))]
    pub lookup: Option<tilo_net::Lookup>,
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut shown = f.debug_struct("Options");
        shown
            .field("user_agent", &self.user_agent)
            .field("timeout", &self.timeout)
            .field("connect_timeout", &self.connect_timeout)
            .field("redirects", &self.redirects);
        #[cfg(not(target_family = "wasm"))]
        shown.field("lookup", &self.lookup.is_some());
        shown.finish()
    }
}

/// The look-up a client was given ([`Options::lookup`]), which has no `Debug`
/// of its own.
#[cfg(not(target_family = "wasm"))]
#[derive(Clone)]
pub(crate) struct Asked(pub(crate) tilo_net::Lookup);

#[cfg(not(target_family = "wasm"))]
impl fmt::Debug for Asked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Asked")
    }
}

#[cfg(not(target_family = "wasm"))]
impl Asked {
    /// Whether a request to `url` is sent: the address it writes out, where
    /// it writes one, is asked of the look-up here, since the connection
    /// looks no such address up.
    pub(crate) fn of(&self, url: &tilo_net::Url) -> std::io::Result<()> {
        match tilo_net::address::written_out(url) {
            Some(address) => tilo_net::address::asked_as_written(&self.0, address),
            None => Ok(()),
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            user_agent: None,
            timeout: Duration::from_secs(20),
            connect_timeout: None,
            redirects: Redirects::Follow,
            #[cfg(not(target_family = "wasm"))]
            lookup: None,
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
