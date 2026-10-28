//! An address typed without a scheme opens over `https://`. When the site
//! takes no connection there, or the two sides cannot agree on encryption,
//! the Browser opens it over `http://`, as other browsers do, and the
//! address field shows the page is not secure. An address typed with
//! `https://` is never retried over `http://`.

use super::{BrowserWidget, NavigationCommand};
use crate::omnibox;

/// A typed address's `https://` attempt, and where it goes if that fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HttpFallback {
    https: String,
    http: String,
}

impl HttpFallback {
    /// The retry for `target`, the address `typed` opens, when the Browser
    /// chose `https://` for it: `typed` named no scheme and is no search.
    pub(super) fn for_typed(typed: &str, target: &str) -> Option<Self> {
        let typed = typed.trim();
        if typed.contains("://") || omnibox::address_target(typed) != target {
            return None;
        }
        let rest = target.strip_prefix("https://")?;
        Some(Self {
            https: target.to_owned(),
            http: format!("http://{rest}"),
        })
    }

    /// The `http://` address to open after `uri` failed with `error`, when
    /// `uri` is this attempt and `http://` may answer what went wrong.
    fn retry(&self, uri: &str, error: &str) -> Option<&str> {
        (same_address(uri, &self.https) && answers_over_http(error)).then_some(self.http.as_str())
    }

    /// Whether `uri` is still this attempt (the engine names it with a
    /// trailing slash).
    fn attempts(&self, uri: &str) -> bool {
        same_address(uri, &self.https)
    }
}

fn same_address(a: &str, b: &str) -> bool {
    match (url::Url::parse(a), url::Url::parse(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Whether a failed `https://` load is one `http://` may answer: the site
/// took no connection on its secure port, closed it without a reply, or the
/// two sides could not agree on encryption or its certificate. An address
/// that names no site, or a page that is blocked, fails over `http://` too.
/// `error` is the engine's network error name (`ERR_CONNECTION_REFUSED`).
pub(super) fn answers_over_http(error: &str) -> bool {
    const CONNECTION: &[&str] = &[
        "ERR_CONNECTION_REFUSED",
        "ERR_CONNECTION_RESET",
        "ERR_CONNECTION_CLOSED",
        "ERR_CONNECTION_FAILED",
        "ERR_CONNECTION_TIMED_OUT",
        "ERR_TIMED_OUT",
        "ERR_EMPTY_RESPONSE",
        "ERR_ADDRESS_UNREACHABLE",
    ];
    let name = error_name(error);
    CONNECTION.contains(&name)
        || name.starts_with("ERR_SSL_")
        || name.starts_with("ERR_CERT_")
        || name == "ERR_BAD_SSL_CLIENT_AUTH_CERT"
}

/// The network error's name in the engine's message: `ERR_NAME` from
/// `net::ERR_NAME` or `ERR_NAME (-105)`.
pub(super) fn error_name(error: &str) -> &str {
    let error = error.trim();
    let error = error.strip_prefix("net::").unwrap_or(error);
    error
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or_default()
}

impl BrowserWidget {
    /// Remember where a navigation to `target`, opened for what was typed,
    /// goes if its `https://` attempt fails; any other navigation forgets it.
    pub(super) fn expect_http_fallback(&mut self, typed: Option<&str>, target: &str) {
        self.http_fallback = typed.and_then(|typed| HttpFallback::for_typed(typed, target));
    }

    /// The page moved to `uri`: a fallback is for its attempt alone.
    pub(super) fn follow_http_fallback(&mut self, uri: &str) {
        if self
            .http_fallback
            .as_ref()
            .is_some_and(|fallback| !fallback.attempts(uri))
        {
            self.http_fallback = None;
        }
    }

    /// After `uri` failed with `error`: open the typed address over
    /// `http://` when that is its fallback. The navigation the person or an
    /// action started goes on there and answers once that load settles.
    pub(super) fn retry_over_http(&mut self, uri: &str, error: &str) -> bool {
        let Some(http) = self
            .http_fallback
            .as_ref()
            .and_then(|fallback| fallback.retry(uri, error))
            .map(str::to_owned)
        else {
            return false;
        };
        self.http_fallback = None;
        tracing::info!(%uri, %error, "the typed address opens over http");
        // The action waiting on the navigation now waits on this one; the
        // failed attempt's outcome answers no request any more.
        let owner = self.next_navigation_request.checked_add(1).filter(|id| {
            i32::try_from(*id).is_ok() && self.resource_navigation_invocation.is_some()
        });
        if let Some(request) = owner {
            self.next_navigation_request = request;
            self.resource_navigation_request = Some(request);
            self.resource_navigation_deadline =
                Some(std::time::Instant::now() + super::NAVIGATION_TIMEOUT);
        }
        self.address_navigation_value = Some(self.address.with(Clone::clone));
        self.set_loading_progress(0.0);
        if let Err(error) = self.send_navigation(NavigationCommand::Navigate { url: http }, owner) {
            tracing::warn!(%error, "the typed address did not open over http");
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::BrowserOperation;
    use super::*;
    use ui::tilo_actions::{InvocationId, InvocationUpdate};
    use ui::webview::WebViewCommand;
    use ui::webview::test_engine::TestEngine;

    /// What the engine was asked to open, in order.
    fn opened(engine: &TestEngine) -> Vec<String> {
        engine
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::ScopedNavigation {
                    command: NavigationCommand::Navigate { url },
                    ..
                } => Some(url),
                _ => None,
            })
            .collect()
    }

    /// Type `address` and press Enter, as the address field's action does,
    /// and let the engine answer until it is done.
    fn open(widget: &mut BrowserWidget, address: &str) -> Option<InvocationUpdate> {
        let typed = InvocationId::new();
        widget
            .admit(
                typed,
                BrowserOperation::Navigate {
                    url: address.into(),
                },
            )
            .unwrap();
        for _ in 0..4 {
            widget.apply_engine_events();
        }
        widget.poll_invocation(typed)
    }

    #[test]
    fn a_typed_site_without_https_opens_over_http_and_says_it_is_not_secure() {
        let engine = TestEngine::new();
        engine.fail_address("https://neverssl.com", "ERR_CONNECTION_REFUSED");
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let answer = open(&mut widget, "neverssl.com");
        assert_eq!(
            opened(&engine),
            ["https://neverssl.com", "http://neverssl.com"]
        );
        assert_eq!(widget.current_uri, "http://neverssl.com");
        assert_eq!(widget.address.get(), "http://neverssl.com");
        assert!(widget.load_error.is_none(), "{:?}", widget.load_error);
        // The person's navigation, and an agent's, lands on the page.
        let Some(InvocationUpdate::Succeeded { output, .. }) = answer else {
            panic!("the navigation did not succeed: {answer:?}");
        };
        assert_eq!(
            output.value,
            Some(serde_json::Value::String("http://neverssl.com".into()))
        );
        let location = widget.window_toolbar().location.unwrap();
        assert_eq!(location.icon.as_deref(), Some("lock-open"));

        // A site whose secure connection fails falls back the same way.
        engine.fail_address("https://old.example/a", "ERR_SSL_PROTOCOL_ERROR");
        open(&mut widget, "old.example/a");
        assert_eq!(widget.current_uri, "http://old.example/a");
    }

    #[test]
    fn an_address_typed_with_https_is_never_opened_over_http() {
        let engine = TestEngine::new();
        engine.fail_address("https://neverssl.com", "ERR_CONNECTION_REFUSED");
        let mut widget = BrowserWidget::with_engine(engine.clone(), true);
        let answer = open(&mut widget, "https://neverssl.com");
        assert_eq!(opened(&engine), ["https://neverssl.com"]);
        assert_eq!(widget.current_uri, "https://neverssl.com");
        assert!(
            matches!(answer, Some(InvocationUpdate::Failed { .. })),
            "{answer:?}"
        );
        // The person reads what happened and what to do, not the engine's
        // error name.
        let shown = widget.load_error.clone().unwrap();
        assert_eq!(
            shown,
            super::super::load_failure::page_failed_message("ERR_CONNECTION_REFUSED")
        );
        assert!(!shown.contains("ERR_"), "{shown}");

        // Nor is a site whose name does not resolve: http cannot answer.
        engine.fail_address("https://nowhere.example", "ERR_NAME_NOT_RESOLVED");
        open(&mut widget, "nowhere.example");
        assert!(!opened(&engine).iter().any(|url| url.starts_with("http://")));
        assert_eq!(widget.current_uri, "https://nowhere.example");
        assert!(widget.load_error.is_some());
    }

    #[test]
    fn only_a_scheme_the_browser_chose_falls_back() {
        let fallback = HttpFallback::for_typed("neverssl.com", "https://neverssl.com").unwrap();
        assert_eq!(fallback.http, "http://neverssl.com");
        assert_eq!(
            HttpFallback::for_typed(" example.com/a?b ", "https://example.com/a?b").map(|f| f.http),
            Some("http://example.com/a?b".to_owned())
        );
        // Typed with https://: never downgraded.
        assert_eq!(
            HttpFallback::for_typed("https://neverssl.com", "https://neverssl.com"),
            None
        );
        // Local addresses open over http:// already; words are a search.
        assert_eq!(
            HttpFallback::for_typed("localhost:3000", "http://localhost:3000"),
            None
        );
        assert_eq!(
            HttpFallback::for_typed("cats", "https://duckduckgo.com/?q=cats"),
            None
        );
    }

    #[test]
    fn http_answers_a_refused_or_unencrypted_connection_only() {
        for error in [
            "ERR_CONNECTION_REFUSED",
            "net::ERR_CONNECTION_RESET",
            "ERR_EMPTY_RESPONSE",
            "ERR_SSL_PROTOCOL_ERROR",
            "ERR_CERT_COMMON_NAME_INVALID",
            "ERR_CONNECTION_TIMED_OUT (-118)",
        ] {
            assert!(answers_over_http(error), "{error}");
        }
        for error in [
            "ERR_NAME_NOT_RESOLVED",
            "ERR_INTERNET_DISCONNECTED",
            "ERR_BLOCKED_BY_CLIENT",
            "ERR_ABORTED",
            "The browser engine couldn’t start.",
        ] {
            assert!(!answers_over_http(error), "{error}");
        }
        let fallback = HttpFallback::for_typed("neverssl.com", "https://neverssl.com").unwrap();
        assert_eq!(
            fallback.retry("https://neverssl.com/", "ERR_SSL_PROTOCOL_ERROR"),
            Some("http://neverssl.com")
        );
        assert_eq!(
            fallback.retry("https://other.example/", "ERR_SSL_PROTOCOL_ERROR"),
            None
        );
    }
}
