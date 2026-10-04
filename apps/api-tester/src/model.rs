//! Pure data model for the API tester.
//!
//! Everything here is plain Rust + serde — no rendering, no IO, no CRDT
//! plumbing. The live document (collections, environments, history, the
//! request currently being edited) is mirrored into an Automerge document
//! by [`crate::doc`], which is what makes a workspace collaborative. This
//! module is the in-memory source of truth the UI reads and the CRDT
//! layer serialises.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifier for a saved request, a collection, or an environment.
pub type Id = Uuid;

/// HTTP verb. Ordered the way the method dropdown presents them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl HttpMethod {
    /// Every verb, in dropdown order.
    pub const ALL: [HttpMethod; 7] = [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
        HttpMethod::Head,
        HttpMethod::Options,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Head => "HEAD",
            HttpMethod::Options => "OPTIONS",
        }
    }

    /// Parse a verb from its wire string, defaulting unknown values to
    /// GET. The inverse of [`HttpMethod::as_str`]; used when importing a
    /// request whose method arrives as raw text.
    pub fn from_str(s: &str) -> HttpMethod {
        match s.to_ascii_uppercase().as_str() {
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "PATCH" => HttpMethod::Patch,
            "DELETE" => HttpMethod::Delete,
            "HEAD" => HttpMethod::Head,
            "OPTIONS" => HttpMethod::Options,
            _ => HttpMethod::Get,
        }
    }

    /// Whether this verb conventionally carries a request body. Used to
    /// decide whether the Body tab is meaningful for the current request.
    pub fn has_body(self) -> bool {
        matches!(self, HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch)
    }
}

/// A single editable key/value row. Used uniformly for query params,
/// headers, and form-body fields. `enabled` lets a row stay in the list
/// while being excluded from the sent request (the checkbox in the UI).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyValue {
    /// Durable row identity. The visible key may be renamed or duplicated
    /// (notably for HTTP headers), so it must not be used as CRDT identity.
    #[serde(default)]
    pub id: String,
    pub key: String,
    pub value: String,
    pub enabled: bool,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            key: key.into(),
            value: value.into(),
            enabled: true,
        }
    }

    /// A blank, enabled row — the trailing "add a new pair" affordance.
    pub fn blank() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            key: String::new(),
            value: String::new(),
            enabled: true,
        }
    }

    pub fn is_blank(&self) -> bool {
        self.key.trim().is_empty() && self.value.trim().is_empty()
    }
}

/// Authentication scheme applied to a request. The variants mirror the
/// Auth tab's mode selector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Auth {
    None,
    /// `Authorization: Bearer <token>`.
    Bearer {
        token: String,
    },
    /// `Authorization: Basic base64(user:pass)`.
    Basic {
        username: String,
        password: String,
    },
    /// A header `name: value` carrying an API key (e.g. `X-Api-Key`).
    ApiKey {
        header: String,
        value: String,
    },
}

impl Auth {
    /// Stable label for the Auth-mode selector.
    pub fn label(&self) -> &'static str {
        match self {
            Auth::None => "No auth",
            Auth::Bearer { .. } => "Bearer token",
            Auth::Basic { .. } => "Basic auth",
            Auth::ApiKey { .. } => "API key",
        }
    }

    pub const LABELS: [&'static str; 4] = ["No auth", "Bearer token", "Basic auth", "API key"];
}

/// How the request body is encoded. The Body tab switches between these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyMode {
    None,
    Json,
    Form,
    Raw,
}

impl BodyMode {
    pub fn label(self) -> &'static str {
        match self {
            BodyMode::None => "None",
            BodyMode::Json => "JSON",
            BodyMode::Form => "Form",
            BodyMode::Raw => "Raw",
        }
    }

    pub const ALL: [BodyMode; 4] = [
        BodyMode::None,
        BodyMode::Json,
        BodyMode::Form,
        BodyMode::Raw,
    ];
}

/// The four editing tabs in the request builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestTab {
    Params,
    Headers,
    Auth,
    Body,
}

impl RequestTab {
    pub fn label(self) -> &'static str {
        match self {
            RequestTab::Params => "Params",
            RequestTab::Headers => "Headers",
            RequestTab::Auth => "Auth",
            RequestTab::Body => "Body",
        }
    }

    pub const ALL: [RequestTab; 4] = [
        RequestTab::Params,
        RequestTab::Headers,
        RequestTab::Auth,
        RequestTab::Body,
    ];
}

/// The three response-body view modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseView {
    Pretty,
    Raw,
    Headers,
}

impl ResponseView {
    pub fn label(self) -> &'static str {
        match self {
            ResponseView::Pretty => "Pretty",
            ResponseView::Raw => "Raw",
            ResponseView::Headers => "Headers",
        }
    }

    pub const ALL: [ResponseView; 3] = [
        ResponseView::Pretty,
        ResponseView::Raw,
        ResponseView::Headers,
    ];
}

/// One saved/active HTTP request. This is the unit a collection holds and
/// the request builder edits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: Id,
    pub name: String,
    pub method: HttpMethod,
    pub url: String,
    pub params: Vec<KeyValue>,
    pub headers: Vec<KeyValue>,
    pub auth: Auth,
    pub body_mode: BodyMode,
    /// Raw / JSON body text.
    pub body_text: String,
    /// Form-encoded body fields (used when `body_mode == Form`).
    pub form: Vec<KeyValue>,
}

impl Request {
    /// A fresh empty GET request.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            method: HttpMethod::Get,
            url: String::new(),
            params: Vec::new(),
            headers: Vec::new(),
            auth: Auth::None,
            body_mode: BodyMode::None,
            body_text: String::new(),
            form: Vec::new(),
        }
    }

    /// Count of enabled key/value rows for a section — drives the little
    /// count badge shown on each tab.
    pub fn enabled_count(rows: &[KeyValue]) -> usize {
        rows.iter().filter(|r| r.enabled && !r.is_blank()).count()
    }
}

/// A named group of saved requests, shown as a foldable node in the
/// sidebar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub id: Id,
    pub name: String,
    pub requests: Vec<Request>,
    pub expanded: bool,
}

impl Collection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            requests: Vec::new(),
            expanded: true,
        }
    }
}

/// A set of `{{variable}}` substitutions. The active environment's
/// variables are interpolated into the URL, headers, params, and body
/// just before a request is sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: Id,
    pub name: String,
    pub variables: Vec<KeyValue>,
}

impl Environment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            variables: Vec::new(),
        }
    }
}

/// A finished request/response pair, recorded for the History list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: Id,
    pub method: HttpMethod,
    pub url: String,
    pub status: u16,
    /// Round-trip time in milliseconds.
    pub duration_ms: u64,
    /// Wall-clock label, e.g. "2m ago" — precomputed at record time.
    pub when: String,
}

/// The outcome of a sent request, rendered by the response viewer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResponseData {
    pub status: u16,
    pub status_text: String,
    pub duration_ms: u64,
    /// Size of the response body in bytes.
    pub size_bytes: usize,
    pub headers: Vec<KeyValue>,
    /// The raw response body text.
    pub body: String,
    /// Best-effort content type pulled from the response headers.
    pub content_type: String,
}

impl ResponseData {
    /// Whether the body is JSON (so the viewer should pretty-print +
    /// syntax-highlight it).
    pub fn is_json(&self) -> bool {
        self.content_type.contains("json")
            || self.body.trim_start().starts_with('{')
            || self.body.trim_start().starts_with('[')
    }

    /// A short human label for the HTTP status family — drives the status
    /// pill's colour.
    pub fn status_class(&self) -> StatusClass {
        match self.status {
            200..=299 => StatusClass::Success,
            300..=399 => StatusClass::Redirect,
            400..=499 => StatusClass::ClientError,
            500..=599 => StatusClass::ServerError,
            _ => StatusClass::Other,
        }
    }
}

/// Coarse HTTP status family, used to colour the status pill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusClass {
    Success,
    Redirect,
    ClientError,
    ServerError,
    Other,
}

/// Format a millisecond duration compactly ("84 ms", "1.2 s").
pub fn human_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.2} s", ms as f64 / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_method_round_trips_and_only_write_verbs_carry_a_body() {
        for m in HttpMethod::ALL {
            assert_eq!(HttpMethod::from_str(m.as_str()), m, "round trip {m:?}");
        }
        // Unknown verbs fall back to GET.
        assert_eq!(HttpMethod::from_str("frobnicate"), HttpMethod::Get);

        for (method, has_body) in [
            (HttpMethod::Post, true),
            (HttpMethod::Put, true),
            (HttpMethod::Patch, true),
            (HttpMethod::Get, false),
            (HttpMethod::Delete, false),
        ] {
            assert_eq!(method.has_body(), has_body, "has_body {method:?}");
        }
    }

    /// The summaries the builder and response chrome display: enabled row
    /// counts, status families, durations, and the bundled sample response.
    #[test]
    fn request_and_response_summaries_count_rows_bucket_statuses_and_format_durations() {
        // Case "enabled count": skips disabled and blank rows.
        let rows = vec![
            KeyValue::new("a", "1"),
            {
                let mut r = KeyValue::new("b", "2");
                r.enabled = false;
                r
            },
            KeyValue::blank(),
        ];
        assert_eq!(Request::enabled_count(&rows), 1, "case enabled count");

        // Case "status class": buckets by family.
        let mut r = ResponseData {
            status: 200,
            status_text: "OK".into(),
            duration_ms: 1,
            size_bytes: 0,
            headers: vec![],
            body: String::new(),
            content_type: String::new(),
        };
        for (status, class) in [
            (200, StatusClass::Success),
            (301, StatusClass::Redirect),
            (404, StatusClass::ClientError),
            (503, StatusClass::ServerError),
        ] {
            r.status = status;
            assert_eq!(r.status_class(), class, "case status {status}");
        }

        // Case "human duration": switches to seconds.
        assert_eq!(human_duration(84), "84 ms", "case duration 84");
        assert_eq!(human_duration(1200), "1.20 s", "case duration 1200");

        // Case "sample response": valid JSON with a 200 status.
        let sample = crate::http::sample_response();
        assert!(sample.is_json(), "case sample response");
        assert!(
            serde_json::from_str::<serde_json::Value>(&sample.body).is_ok(),
            "case sample response"
        );
        assert_eq!(sample.status, 200, "case sample response");
    }
}
