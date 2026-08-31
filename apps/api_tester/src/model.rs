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
    #[allow(dead_code)]
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
    pub key: String,
    pub value: String,
    pub enabled: bool,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
        }
    }

    /// A blank, enabled row — the trailing "add a new pair" affordance.
    pub fn blank() -> Self {
        Self {
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
            Auth::None => "No Auth",
            Auth::Bearer { .. } => "Bearer Token",
            Auth::Basic { .. } => "Basic Auth",
            Auth::ApiKey { .. } => "API Key",
        }
    }

    pub const LABELS: [&'static str; 4] = ["No Auth", "Bearer Token", "Basic Auth", "API Key"];
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
    Preview,
}

impl ResponseView {
    pub fn label(self) -> &'static str {
        match self {
            ResponseView::Pretty => "Pretty",
            ResponseView::Raw => "Raw",
            ResponseView::Preview => "Preview",
        }
    }

    pub const ALL: [ResponseView; 3] = [
        ResponseView::Pretty,
        ResponseView::Raw,
        ResponseView::Preview,
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

/// Format a byte count as a compact human string ("512 B", "1.4 KB").
pub fn human_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
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
    fn method_round_trips_through_string() {
        for m in HttpMethod::ALL {
            assert_eq!(HttpMethod::from_str(m.as_str()), m);
        }
        // Unknown verbs fall back to GET.
        assert_eq!(HttpMethod::from_str("frobnicate"), HttpMethod::Get);
    }

    #[test]
    fn only_write_verbs_carry_a_body() {
        assert!(HttpMethod::Post.has_body());
        assert!(HttpMethod::Put.has_body());
        assert!(HttpMethod::Patch.has_body());
        assert!(!HttpMethod::Get.has_body());
        assert!(!HttpMethod::Delete.has_body());
    }

    #[test]
    fn enabled_count_skips_disabled_and_blank() {
        let rows = vec![
            KeyValue::new("a", "1"),
            {
                let mut r = KeyValue::new("b", "2");
                r.enabled = false;
                r
            },
            KeyValue::blank(),
        ];
        assert_eq!(Request::enabled_count(&rows), 1);
    }

    #[test]
    fn status_class_buckets_by_family() {
        let mut r = ResponseData {
            status: 200,
            status_text: "OK".into(),
            duration_ms: 1,
            size_bytes: 0,
            headers: vec![],
            body: String::new(),
            content_type: String::new(),
        };
        assert_eq!(r.status_class(), StatusClass::Success);
        r.status = 301;
        assert_eq!(r.status_class(), StatusClass::Redirect);
        r.status = 404;
        assert_eq!(r.status_class(), StatusClass::ClientError);
        r.status = 503;
        assert_eq!(r.status_class(), StatusClass::ServerError);
    }

    #[test]
    fn human_size_uses_units() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
    }

    #[test]
    fn human_duration_switches_to_seconds() {
        assert_eq!(human_duration(84), "84 ms");
        assert_eq!(human_duration(1200), "1.20 s");
    }
}
