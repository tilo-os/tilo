//! A server's sign-in: HTTP authentication (Basic, Digest), and a proxy's.
//!
//! A server may answer a request with a challenge for a user name and a
//! password. An engine that can holds the request and says who asks
//! ([`WebViewEvent::HttpAuth`](super::WebViewEvent::HttpAuth)); the embedder
//! asks the person, naming that asker, and answers with their credentials or
//! with none, which cancels and shows the server's own page
//! ([`WebViewCommand::AnswerHttpAuth`](super::WebViewCommand::AnswerHttpAuth)).
//!
//! The asker is where the credentials go, and the only thing a prompt may be
//! named by: the challenged request's origin for a server, the proxy's host
//! and port for a proxy. The realm is the server's own text and names
//! nobody. An answer names the asker it was shown, so credentials never
//! answer a question other than the one the person saw.
//!
//! The engine asks only for the page's own document and what it loads from
//! its own origin; a challenge for anything a page loads from another origin
//! is cancelled and never arrives here. It ends a challenge nobody answers
//! ([`WebViewEvent::HttpAuthClosed`](super::WebViewEvent::HttpAuthClosed)),
//! so no request waits on a prompt for good.
//!
//! The password is a secret: [`HttpAuthCredentials`] zeroes it when dropped
//! and prints none of it.

use zeroize::Zeroizing;

/// The longest asker a challenge names, in bytes.
pub const MAX_HTTP_AUTH_ASKER: usize = 512;
/// The longest realm a challenge carries, in bytes.
pub const MAX_HTTP_AUTH_REALM: usize = 256;
/// The longest scheme name a challenge carries, in bytes.
pub const MAX_HTTP_AUTH_SCHEME: usize = 32;
/// The longest user name, and the longest password, of an answer, in bytes.
pub const MAX_HTTP_AUTH_CREDENTIAL: usize = 1024;

/// A request that waits for credentials until it is answered or the engine
/// says it no longer waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpAuthChallenge {
    pub id: u32,
    /// Who asks, and where the credentials go: the challenged request's
    /// origin (`https://example.org`, `http://127.0.0.1:8080`) for a server,
    /// `host:port` for a proxy.
    pub asker: String,
    /// A proxy asks, not the site.
    pub proxy: bool,
    /// What the server calls what it protects: its own text, never who asks.
    pub realm: String,
    /// The scheme in lower case (`basic`, `digest`); empty when unknown.
    pub scheme: String,
    /// The credentials given last for this asker and realm were not
    /// accepted.
    pub retry: bool,
}

impl HttpAuthChallenge {
    /// The asker as a person reads a site: `example.org`, `127.0.0.1:8080`.
    pub fn site(&self) -> &str {
        self.asker
            .strip_prefix("https://")
            .or_else(|| self.asker.strip_prefix("http://"))
            .unwrap_or(&self.asker)
    }

    /// Whether what is typed crosses a network unprotected: a server asked
    /// over plain `http`, and it is not this device itself. Nothing is known
    /// of how a proxy is reached, so nothing is claimed of one.
    pub fn sends_in_the_clear(&self) -> bool {
        let Some(site) = self.asker.strip_prefix("http://").filter(|_| !self.proxy) else {
            return false;
        };
        let host = if site.starts_with('[') {
            site.split_inclusive(']').next().unwrap_or(site)
        } else {
            site.rsplit_once(':').map_or(site, |(host, _)| host)
        };
        let this_device = host == "localhost"
            || host.ends_with(".localhost")
            || host == "[::1]"
            || host
                .parse::<std::net::Ipv4Addr>()
                .is_ok_and(|address| address.is_loopback());
        !this_device
    }

    /// Whether this is a challenge an engine may report: an asker of the
    /// shape its kind has, and texts within their bounds.
    pub fn well_formed(&self) -> bool {
        self.id > 0
            && names_asker(&self.asker, self.proxy)
            && self.realm.len() <= MAX_HTTP_AUTH_REALM
            && !self.realm.chars().any(char::is_control)
            && self.scheme.len() <= MAX_HTTP_AUTH_SCHEME
            && self
                .scheme
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    }
}

/// Whether `asker` names an origin (a server's) or a host and port (a
/// proxy's) and nothing more: no path, no user, nothing that is not shown.
fn names_asker(asker: &str, proxy: bool) -> bool {
    let plain = |text: &str| {
        !text.is_empty()
            && text.bytes().all(|byte| {
                byte.is_ascii_graphic() && !matches!(byte, b'/' | b'@' | b'?' | b'#' | b'\\')
            })
    };
    if asker.len() > MAX_HTTP_AUTH_ASKER {
        return false;
    }
    if proxy {
        return asker.rsplit_once(':').is_some_and(|(host, port)| {
            plain(host) && !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
        });
    }
    asker
        .strip_prefix("https://")
        .or_else(|| asker.strip_prefix("http://"))
        .is_some_and(plain)
}

/// The user name and password a challenge is answered with. The password is
/// zeroed when this is dropped, and `Debug` prints neither.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpAuthCredentials {
    username: String,
    password: Zeroizing<String>,
}

impl HttpAuthCredentials {
    /// `password` is moved in, so no copy of it is left with the caller.
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: Zeroizing::new(password.into()),
        }
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> &str {
        &self.password
    }

    /// Whether an engine takes them: each within its bound.
    pub fn within_limits(&self) -> bool {
        self.username.len() <= MAX_HTTP_AUTH_CREDENTIAL
            && self.password.len() <= MAX_HTTP_AUTH_CREDENTIAL
    }
}

impl std::fmt::Debug for HttpAuthCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpAuthCredentials")
            .field("username", &"<withheld>")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Whether an answer naming `asker` with `credentials` (none cancels) is one
/// an engine takes.
pub fn answers(asker: &str, credentials: Option<&HttpAuthCredentials>) -> bool {
    !asker.is_empty()
        && asker.len() <= MAX_HTTP_AUTH_ASKER
        && credentials.is_none_or(HttpAuthCredentials::within_limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(asker: &str, proxy: bool) -> HttpAuthChallenge {
        HttpAuthChallenge {
            id: 1,
            asker: asker.into(),
            proxy,
            realm: "Staff room".into(),
            scheme: "basic".into(),
            retry: false,
        }
    }

    #[test]
    fn an_asker_is_an_origin_or_a_proxys_host_and_port_and_nothing_more() {
        for (asker, proxy) in [
            ("https://example.org", false),
            ("http://127.0.0.1:8080", false),
            ("http://[::1]:8080", false),
            ("proxy.example:3128", true),
        ] {
            assert!(challenge(asker, proxy).well_formed(), "{asker}");
        }
        for (asker, proxy) in [
            ("", false),
            ("example.org", false),
            ("https://", false),
            ("https://example.org/login", false),
            ("https://ada@example.org", false),
            ("https://example.org?x", false),
            ("https://exa mple.org", false),
            ("ftp://example.org", false),
            ("https://example.org", true),
            ("proxy.example", true),
            ("proxy.example:", true),
            ("proxy.example:http", true),
        ] {
            assert!(!challenge(asker, proxy).well_formed(), "{asker}");
        }
        let long = format!("https://{}", "a".repeat(MAX_HTTP_AUTH_ASKER));
        assert!(!challenge(&long, false).well_formed());
        let mut odd = challenge("https://example.org", false);
        odd.realm = "Staff\nroom".into();
        assert!(!odd.well_formed(), "a realm is one line of text");
        odd.realm = "x".repeat(MAX_HTTP_AUTH_REALM + 1);
        assert!(!odd.well_formed());
        odd.realm = String::new();
        odd.scheme = "Basic realm".into();
        assert!(!odd.well_formed());
        odd.scheme = String::new();
        assert!(odd.well_formed(), "a realm and a scheme may be empty");
        odd.id = 0;
        assert!(!odd.well_formed());
    }

    #[test]
    fn a_site_is_read_without_its_scheme_and_plain_http_is_said() {
        assert_eq!(
            challenge("https://example.org", false).site(),
            "example.org"
        );
        assert_eq!(
            challenge("http://192.168.1.1:8080", false).site(),
            "192.168.1.1:8080"
        );
        assert_eq!(
            challenge("proxy.example:3128", true).site(),
            "proxy.example:3128"
        );
        for clear in [
            "http://example.org",
            "http://192.168.1.1",
            "http://router.lan:8080",
            "http://localhost.example",
            "http://[2001:db8::1]:8080",
        ] {
            assert!(challenge(clear, false).sends_in_the_clear(), "{clear}");
        }
        for kept in [
            "https://example.org",
            "http://localhost",
            "http://localhost:8080",
            "http://app.localhost:3000",
            "http://127.0.0.1:8080",
            "http://127.9.9.9",
            "http://[::1]:8080",
        ] {
            assert!(!challenge(kept, false).sends_in_the_clear(), "{kept}");
        }
        assert!(!challenge("proxy.example:3128", true).sends_in_the_clear());
    }

    #[test]
    fn credentials_are_never_printed_and_stay_within_their_bounds() {
        let credentials = HttpAuthCredentials::new("ada", "correct-horse-9");
        let printed = format!("{credentials:?}");
        assert!(!printed.contains("correct-horse-9") && !printed.contains("ada"));
        assert_eq!(credentials.username(), "ada");
        assert_eq!(credentials.password(), "correct-horse-9");
        assert!(answers("https://example.org", Some(&credentials)));
        assert!(answers("https://example.org", None), "none cancels");
        assert!(!answers("", None));
        assert!(!answers(&"a".repeat(MAX_HTTP_AUTH_ASKER + 1), None));
        let long = "x".repeat(MAX_HTTP_AUTH_CREDENTIAL + 1);
        assert!(!answers(
            "https://example.org",
            Some(&HttpAuthCredentials::new(long.clone(), "p"))
        ));
        assert!(!answers(
            "https://example.org",
            Some(&HttpAuthCredentials::new("u", long))
        ));
        assert!(answers(
            "https://example.org",
            Some(&HttpAuthCredentials::new("", ""))
        ));
    }
}
