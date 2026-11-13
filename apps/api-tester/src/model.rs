//! The values the window works on, projected from the record in [`crate::record`].
//! Credentials are the exception: a request's auth secret and a secret variable's value
//! stay on this device.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Model};
use uuid::Uuid;

/// The id of a saved request, a collection, an environment or a sent request.
pub type Id = ui::tilo_resource::Id;

pub fn fresh_id() -> Id {
    Id::from_raw(Uuid::new_v4().to_string())
}

/// HTTP verb, in the order the method dropdown presents them.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl HttpMethod {
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

    /// The verb of a wire string, unknown values reading as GET.
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

    /// Whether this verb conventionally carries a body, which decides whether the Body tab
    /// means anything.
    pub fn has_body(self) -> bool {
        matches!(self, HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch)
    }
}

/// One editable key/value row, used for query params, headers and form fields. `enabled`
/// keeps a row listed while excluding it from the sent request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyValue {
    /// The row's identity: a key may be renamed or duplicated, so it cannot serve as
    /// CRDT identity.
    #[serde(default)]
    pub id: String,
    pub key: String,
    pub value: String,
    pub enabled: bool,
    /// A variable whose value is a credential: the document holds its key and this flag.
    #[serde(default)]
    pub secret: bool,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            key: key.into(),
            value: value.into(),
            enabled: true,
            secret: false,
        }
    }

    #[must_use]
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    pub fn blank() -> Self {
        Self::new(String::new(), String::new())
    }
}

/// The authentication scheme applied to a request.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub enum Auth {
    /// An opaque, device-local secret-store key, safe to collaborate.
    SecretRef {
        id: String,
        /// The scheme's public label, never credential material.
        #[serde(default)]
        scheme: String,
    },
    None,
    Bearer {
        token: String,
    },
    Basic {
        username: String,
        password: String,
    },
    ApiKey {
        header: String,
        value: String,
    },
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.scheme())
    }
}

impl Serialize for Auth {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::None => serializer.serialize_str("None"),
            Self::SecretRef { id, scheme } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry(
                    "SecretRef",
                    &serde_json::json!({ "id": id, "scheme": scheme }),
                )?;
                map.end()
            }
            _ => Err(serde::ser::Error::custom(
                "authentication must be sealed before persistence",
            )),
        }
    }
}

impl Auth {
    /// The scheme's public name as records carry it: a credential's stored scheme is
    /// matched back to its fields by exactly this word, so it stays the same in every
    /// language.
    pub fn scheme(&self) -> &'static str {
        match self {
            Auth::SecretRef { scheme, .. } => match scheme.as_str() {
                "Bearer Token" => "Bearer Token",
                "Basic Auth" => "Basic Auth",
                "API Key" => "API Key",
                _ => "Stored Authentication",
            },
            Auth::None => "No Auth",
            Auth::Bearer { .. } => "Bearer Token",
            Auth::Basic { .. } => "Basic Auth",
            Auth::ApiKey { .. } => "API Key",
        }
    }

    pub fn scheme_message(scheme: &str) -> &'static str {
        match scheme {
            "Bearer Token" => "api-tester-auth-bearer",
            "Basic Auth" => "api-tester-auth-basic",
            "API Key" => "api-tester-auth-api-key",
            "Stored Authentication" => "api-tester-auth-stored",
            _ => "api-tester-auth-none",
        }
    }

    pub fn message(&self) -> &'static str {
        Self::scheme_message(self.scheme())
    }

    pub const SCHEMES: [&'static str; 4] = ["No Auth", "Bearer Token", "Basic Auth", "API Key"];
    /// The credential of this scheme: the token, the password, the key.
    pub fn secret(&self) -> Option<&str> {
        match self {
            Auth::None | Auth::SecretRef { .. } => None,
            Auth::Bearer { token } => Some(token),
            Auth::Basic { password, .. } => Some(password),
            Auth::ApiKey { value, .. } => Some(value),
        }
    }

    /// This scheme with its credential replaced.
    #[must_use]
    pub fn with_secret(&self, secret: &str) -> Self {
        let secret = secret.to_owned();
        match self {
            Auth::SecretRef { .. } => self.clone(),
            Auth::None => Auth::None,
            Auth::Bearer { .. } => Auth::Bearer { token: secret },
            Auth::Basic { username, .. } => Auth::Basic {
                username: username.clone(),
                password: secret,
            },
            Auth::ApiKey { header, .. } => Auth::ApiKey {
                header: header.clone(),
                value: secret,
            },
        }
    }

    /// This scheme as the document holds it: everything but the credential.
    #[must_use]
    pub fn without_secret(&self) -> Self {
        self.with_secret("")
    }
}

#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyMode {
    #[default]
    None,
    Json,
    Form,
    Raw,
}

impl BodyMode {
    pub fn message(self) -> &'static str {
        match self {
            BodyMode::None => "api-tester-body-none",
            BodyMode::Json => "api-tester-body-json",
            BodyMode::Form => "api-tester-body-form",
            BodyMode::Raw => "api-tester-body-raw",
        }
    }

    pub const ALL: [BodyMode; 4] = [
        BodyMode::None,
        BodyMode::Json,
        BodyMode::Form,
        BodyMode::Raw,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestTab {
    Params,
    Headers,
    Auth,
    Body,
}

impl RequestTab {
    pub fn message(self) -> &'static str {
        match self {
            RequestTab::Params => "api-tester-tab-params",
            RequestTab::Headers => "api-tester-tab-headers",
            RequestTab::Auth => "api-tester-tab-auth",
            RequestTab::Body => "api-tester-tab-body",
        }
    }

    pub const ALL: [RequestTab; 4] = [
        RequestTab::Params,
        RequestTab::Headers,
        RequestTab::Auth,
        RequestTab::Body,
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseView {
    Pretty,
    Raw,
    Headers,
}

impl ResponseView {
    pub fn message(self) -> &'static str {
        match self {
            ResponseView::Pretty => "api-tester-view-pretty",
            ResponseView::Raw => "api-tester-view-raw",
            ResponseView::Headers => "api-tester-view-headers",
        }
    }

    pub const ALL: [ResponseView; 3] = [
        ResponseView::Pretty,
        ResponseView::Raw,
        ResponseView::Headers,
    ];
}

/// One saved or active HTTP request.
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
    /// Raw or JSON body text.
    pub body_text: String,
    /// Form fields, sent when the body mode is Form.
    pub form: Vec<KeyValue>,
}

impl Request {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: fresh_id(),
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
}

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
            id: fresh_id(),
            name: name.into(),
            requests: Vec::new(),
            expanded: true,
        }
    }
}

/// A set of `{{variable}}` substitutions, resolved into a request just before it is
/// sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: Id,
    pub name: String,
    pub variables: Vec<KeyValue>,
}

impl Environment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: fresh_id(),
            name: name.into(),
            variables: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: Id,
    pub method: HttpMethod,
    pub url: String,
    pub status: u16,
    /// Round-trip time in milliseconds.
    pub duration_ms: u64,
    /// The request as it was authored, so replaying the entry replays the request.
    pub replay: Option<RequestSnapshot>,
}

/// A request as it was authored, with the credential's reference rather than the
/// credential.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestSnapshot {
    pub method: HttpMethod,
    pub url: String,
    pub params: Vec<KeyValue>,
    pub headers: Vec<KeyValue>,
    pub body_mode: BodyMode,
    pub body: String,
    pub form: Vec<KeyValue>,
    /// The scheme's public label, if one was set.
    pub scheme: String,
    /// The device-local reference to the credential, never its value.
    pub credential: String,
}

impl RequestSnapshot {
    pub fn of(request: &Request) -> Self {
        let (scheme, credential) = match &request.auth {
            Auth::SecretRef { id, scheme } => (scheme.clone(), id.clone()),
            Auth::None => (String::new(), String::new()),
            // A literal credential is on its way to the secret store: its scheme is public,
            // its value is not recorded.
            other => (other.scheme().to_owned(), String::new()),
        };
        Self {
            method: request.method,
            url: request.url.clone(),
            params: request.params.clone(),
            headers: request.headers.clone(),
            body_mode: request.body_mode,
            body: request.body_text.clone(),
            form: request.form.clone(),
            scheme,
            credential,
        }
    }

    /// The request this snapshot held, as a new request in the builder.
    pub fn to_request(&self) -> Request {
        let mut request = Request::new("New request");
        request.method = self.method;
        request.url.clone_from(&self.url);
        request.params.clone_from(&self.params);
        request.headers.clone_from(&self.headers);
        request.body_mode = self.body_mode;
        request.body_text.clone_from(&self.body);
        request.form.clone_from(&self.form);
        request.auth = if self.credential.is_empty() {
            Auth::None
        } else {
            Auth::SecretRef {
                id: self.credential.clone(),
                scheme: self.scheme.clone(),
            }
        };
        request
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResponseData {
    pub status: u16,
    pub status_text: String,
    pub duration_ms: u64,
    /// The body's size in bytes: what arrived, or the length the server declared when only
    /// its start was downloaded.
    pub size_bytes: usize,
    /// Whether `size_bytes` is the body's size, rather than how much of an undeclared body
    /// arrived before the download stopped.
    pub size_exact: bool,
    /// How many of the body's bytes `body` shows.
    pub shown_bytes: usize,
    /// Whether the body is binary, so `body` is a hex dump of its start.
    pub binary: bool,
    pub headers: Vec<KeyValue>,
    /// The body as text: as received, or a hex dump when it is binary.
    pub body: String,
    /// Best-effort content type pulled from the response headers.
    pub content_type: String,
}

impl ResponseData {
    /// Whether the body is JSON (so the viewer should pretty-print +
    /// syntax-highlight it).
    pub fn is_json(&self) -> bool {
        !self.binary
            && (self.content_type.contains("json")
                || self.body.trim_start().starts_with('{')
                || self.body.trim_start().starts_with('['))
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

    /// The summaries the response chrome displays: status families,
    /// durations, and the bundled sample response.
    #[test]
    fn response_summaries_bucket_statuses_and_format_durations() {
        // Case "status class": buckets by family.
        let mut r = ResponseData {
            status: 200,
            status_text: "OK".into(),
            duration_ms: 1,
            size_bytes: 0,
            size_exact: true,
            shown_bytes: 0,
            binary: false,
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
