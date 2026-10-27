//! What the shared workspace stores: saved collections of requests,
//! environments with their variables, the request in the builder and the
//! history of what was sent.
//!
//! `#[derive(Model)]` is the whole storage contract. Names, URLs, bodies and
//! row keys and values are text that merges character by character; rows,
//! requests, collections, environments, variables and history entries are
//! items with stable ids. A credential never enters the record: a request's
//! token or password and a secret variable's value stay in the device's
//! secret store, keyed by the record they belong to; [`Variable::valid`]
//! refuses a secret variable that carries a value.
//!
//! The window works on the plain values of [`crate::model`]; the
//! projections here turn the record into them.

use ui::tilo_resource::{self, Atom, Collection, Id, Model, Refusal};

use crate::model::{self, Auth, BodyMode, HttpMethod, KeyValue};

/// A param, header or form row.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// Key
    pub key: String,
    /// Value
    pub value: String,
    /// Included: an excluded row stays listed but is not sent.
    pub enabled: bool,
}

impl Row {
    /// A row that is sent.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
            enabled: true,
        }
    }

    fn of(row: &KeyValue) -> Self {
        Self {
            key: row.key.clone(),
            value: row.value.clone(),
            enabled: row.enabled,
        }
    }

    fn shown(&self, id: &Id) -> KeyValue {
        KeyValue {
            id: id.as_str().to_owned(),
            key: self.key.clone(),
            value: self.value.clone(),
            enabled: self.enabled,
            secret: false,
        }
    }
}

/// How a request authenticates.
#[derive(Model, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthScheme {
    #[default]
    None,
    Bearer,
    Basic,
    ApiKey,
}

impl AuthScheme {
    /// Every scheme, in the order the Auth tab offers them.
    pub const ALL: [AuthScheme; 4] = [
        AuthScheme::None,
        AuthScheme::Bearer,
        AuthScheme::Basic,
        AuthScheme::ApiKey,
    ];
}

/// A request's authentication, without its credential.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    /// Scheme
    pub scheme: AuthScheme,
    /// User name, for basic authentication.
    pub username: String,
    /// Key header, for an API key.
    pub header: String,
    /// Immutable device-local credential key, published only after durable storage.
    pub reference: String,
}

impl Credentials {
    /// The record of `auth`, without its credential.
    pub fn of(auth: &Auth) -> Self {
        match auth {
            Auth::SecretRef { id, scheme } => Self {
                scheme: match scheme.as_str() {
                    "Bearer Token" | "Bearer token" => AuthScheme::Bearer,
                    "Basic Auth" | "Basic auth" => AuthScheme::Basic,
                    "API Key" | "API key" => AuthScheme::ApiKey,
                    _ => AuthScheme::None,
                },
                reference: id.clone(),
                ..Self::default()
            },
            Auth::None => Self::default(),
            Auth::Bearer { .. } => Self {
                scheme: AuthScheme::Bearer,
                ..Self::default()
            },
            Auth::Basic { username, .. } => Self {
                scheme: AuthScheme::Basic,
                username: username.clone(),
                header: String::new(),
                reference: String::new(),
            },
            Auth::ApiKey { header, .. } => Self {
                scheme: AuthScheme::ApiKey,
                username: String::new(),
                header: header.clone(),
                reference: String::new(),
            },
        }
    }

    /// The scheme with an empty credential, for the window to fill from the
    /// device.
    pub fn shown(&self) -> Auth {
        match self.scheme {
            AuthScheme::None => Auth::None,
            AuthScheme::Bearer => Auth::Bearer {
                token: String::new(),
            },
            AuthScheme::Basic => Auth::Basic {
                username: self.username.clone(),
                password: String::new(),
            },
            AuthScheme::ApiKey => Auth::ApiKey {
                header: self.header.clone(),
                value: String::new(),
            },
        }
    }
}

/// One HTTP request: saved in a collection, or in the builder.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct RequestRecord {
    /// Name
    #[model(label)]
    pub name: String,
    /// Method
    pub method: HttpMethod,
    /// URL
    pub url: String,
    /// Query parameters
    #[model(remove = "quiet")]
    pub params: Collection<Row>,
    /// Headers
    #[model(remove = "quiet")]
    pub headers: Collection<Row>,
    /// Authentication
    pub auth: Credentials,
    /// Body encoding
    pub body_mode: BodyMode,
    /// Body: the raw or JSON text.
    pub body: String,
    /// Form fields, sent when the body is a form.
    #[model(remove = "quiet")]
    pub form: Collection<Row>,
}

impl RequestRecord {
    /// A new GET request named `name`.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// The record of `request`, without its credential. Rows keep their ids.
    pub fn of(request: &model::Request) -> Self {
        let rows = |rows: &[KeyValue]| {
            let mut out = Collection::new();
            for row in rows {
                out.push(Id::from_raw(row.id.clone()), Row::of(row));
            }
            out
        };
        Self {
            name: request.name.clone(),
            method: request.method,
            url: request.url.clone(),
            params: rows(&request.params),
            headers: rows(&request.headers),
            auth: Credentials::of(&request.auth),
            body_mode: request.body_mode,
            body: request.body_text.clone(),
            form: rows(&request.form),
        }
    }

    /// The request as the window works on it, as `id`, with an empty
    /// credential.
    pub fn shown(&self, id: &Id) -> model::Request {
        let rows = |rows: &Collection<Row>| -> Vec<KeyValue> {
            rows.iter().map(|(id, row)| row.shown(id)).collect()
        };
        model::Request {
            id: id.clone(),
            name: self.name.clone(),
            method: self.method,
            url: self.url.clone(),
            params: rows(&self.params),
            headers: rows(&self.headers),
            auth: self.auth.shown(),
            body_mode: self.body_mode,
            body_text: self.body.clone(),
            form: rows(&self.form),
        }
    }
}

/// A named group of saved requests.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct SavedCollection {
    /// Name
    #[model(label)]
    pub name: String,
    /// Requests
    pub requests: Collection<RequestRecord>,
}

/// One `{{variable}}` of an environment.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
#[model(check = Variable::valid)]
pub struct Variable {
    /// Key
    #[model(label)]
    pub key: String,
    /// Value, empty for a secret variable, whose value stays on each device.
    pub value: String,
    /// Included
    pub enabled: bool,
    /// Whether the value is a credential kept on each device. Set when the
    /// variable is added.
    #[model(readonly)]
    pub secret: bool,
}

impl Variable {
    /// A secret variable's value never enters the record.
    fn valid(&self) -> Result<(), Refusal> {
        if self.secret && !self.value.is_empty() {
            return Err(Refusal::new(
                "a secret variable's value stays on each device",
            ));
        }
        Ok(())
    }

    fn shown(&self, id: &Id) -> KeyValue {
        KeyValue {
            id: id.as_str().to_owned(),
            key: self.key.clone(),
            value: self.value.clone(),
            enabled: self.enabled,
            secret: self.secret,
        }
    }
}

/// A set of `{{variable}}` substitutions.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct Environment {
    /// Name
    #[model(label)]
    pub name: String,
    /// Variables
    #[model(remove = "quiet")]
    pub variables: Collection<Variable>,
}

/// A request that was sent: what went out and how it went.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
pub struct Sent {
    /// Route: the method and the URL without credentials, query values or
    /// fragment, so it can name the entry anywhere.
    #[model(label)]
    pub route: Atom,
    /// Method
    pub method: HttpMethod,
    /// URL as it was sent.
    pub url: Atom,
    /// Status
    pub status: u16,
    /// Round-trip time in milliseconds.
    pub duration_ms: u64,
    /// When it completed, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// The request as it was authored when it was sent, as JSON: its
    /// headers, body, parameters and the credential's *reference* (never the
    /// credential), so re-opening the entry brings the whole request back
    /// instead of a blank one with a method and a URL.
    pub replay: Atom,
}

impl Sent {
    /// What was sent, labelled by its safe route, with the request that
    /// produced it.
    pub fn new(
        method: HttpMethod,
        url: &str,
        status: u16,
        duration_ms: u64,
        at_ms: i64,
        request: Option<model::RequestSnapshot>,
    ) -> Self {
        Self {
            route: Atom(route_of(method, url, status)),
            method,
            url: Atom(url.to_owned()),
            status,
            duration_ms,
            at_ms,
            replay: Atom(
                request
                    .map(|request| serde_json::to_string(&request).unwrap_or_default())
                    .unwrap_or_default(),
            ),
        }
    }

    /// The request this entry sent, as it was authored, when the entry kept
    /// it.
    pub fn replay_of(&self) -> Option<model::RequestSnapshot> {
        (!self.replay.0.is_empty())
            .then(|| serde_json::from_str(&self.replay.0).ok())
            .flatten()
    }

    fn shown(&self, id: &Id) -> model::HistoryEntry {
        model::HistoryEntry {
            id: id.clone(),
            method: self.method,
            url: self.url.0.clone(),
            status: self.status,
            duration_ms: self.duration_ms,
            replay: self.replay_of(),
        }
    }
}

/// Enough of a request's route to find it again, without URL userinfo,
/// query values or fragments. A malformed URL gets a truthful but
/// non-sensitive label instead of its raw text.
pub fn route_of(method: HttpMethod, url: &str, status: u16) -> String {
    let method = method.as_str();
    let Some((scheme, remainder)) = url.split_once("://") else {
        return format!("{method} request returned status {status}");
    };
    let safe = remainder.split(['?', '#']).next().unwrap_or_default();
    let Some((authority, path)) = safe.split_once('/') else {
        let authority = safe.rsplit('@').next().unwrap_or_default();
        return format!("{method} {scheme}://{authority} returned status {status}");
    };
    let authority = authority.rsplit('@').next().unwrap_or_default();
    format!("{method} {scheme}://{authority}/{path} returned status {status}")
}

/// How many sent requests the history keeps.
pub const HISTORY_LIMIT: usize = 50;

/// The shared API workspace.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = Workspace::valid)]
pub struct Workspace {
    /// Collections
    pub collections: Collection<SavedCollection>,
    /// Environments
    pub environments: Collection<Environment>,
    /// Active environment: whose variables resolve into what is sent.
    pub active_environment: Option<Id>,
    /// The request in the builder.
    pub request: RequestRecord,
    /// The saved request the builder holds (or holds a copy of): its
    /// credential is the builder's. Changed by opening and saving.
    #[model(readonly)]
    pub opened: Atom,
    /// What was sent, newest first. Only sending adds to it.
    #[model(readonly)]
    pub history: Collection<Sent>,
}

impl Workspace {
    /// The active environment names one the workspace has.
    fn valid(&self) -> Result<(), Refusal> {
        match &self.active_environment {
            Some(id) if !self.environments.contains(id) => Err(Refusal::new("no such environment")),
            _ => Ok(()),
        }
    }

    /// The id the builder's request goes by.
    pub fn opened(&self) -> Id {
        Id::from_raw(self.opened.0.clone())
    }

    /// The collections as the window shows them.
    pub fn collections_shown(&self) -> Vec<model::Collection> {
        self.collections
            .iter()
            .map(|(id, collection)| model::Collection {
                id: id.clone(),
                name: collection.name.clone(),
                requests: collection
                    .requests
                    .iter()
                    .map(|(id, request)| request.shown(id))
                    .collect(),
                expanded: true,
            })
            .collect()
    }

    /// The environments as the window shows them.
    pub fn environments_shown(&self) -> Vec<model::Environment> {
        self.environments
            .iter()
            .map(|(id, environment)| model::Environment {
                id: id.clone(),
                name: environment.name.clone(),
                variables: environment
                    .variables
                    .iter()
                    .map(|(id, variable)| variable.shown(id))
                    .collect(),
            })
            .collect()
    }

    /// The history as the window shows it.
    pub fn history_shown(&self) -> Vec<model::HistoryEntry> {
        self.history
            .iter()
            .map(|(id, sent)| sent.shown(id))
            .collect()
    }

    /// The builder's request as the window shows it.
    pub fn request_shown(&self) -> model::Request {
        self.request.shown(&self.opened())
    }

    /// The saved request `request`, and the collection holding it.
    pub fn saved(&self, request: &Id) -> Option<(&Id, &RequestRecord)> {
        self.collections.iter().find_map(|(collection, saved)| {
            saved
                .requests
                .get(request)
                .map(|record| (collection, record))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request keeps everything but its credential through the record,
    /// rows keeping their ids.
    #[test]
    fn a_request_round_trips_without_its_credential() {
        let mut request = model::Request::new("List users");
        request.method = HttpMethod::Post;
        request.url = "https://api.example.com/users".into();
        request.params = vec![KeyValue::new("page", "1")];
        request.headers = vec![KeyValue::new("Accept", "application/json")];
        request.form = vec![KeyValue::new("name", "Ada")];
        request.body_mode = BodyMode::Json;
        request.body_text = "{}".into();
        request.auth = Auth::Basic {
            username: "ada".into(),
            password: "hunter2".into(),
        };
        let record = RequestRecord::of(&request);
        assert_eq!(record.auth.username, "ada");
        let shown = record.shown(&request.id);
        assert_eq!(
            shown.auth,
            Auth::Basic {
                username: "ada".into(),
                password: String::new(),
            },
            "the password is not in the record"
        );
        assert_eq!(
            model::Request {
                auth: request.auth.without_secret(),
                ..request
            },
            shown
        );
    }

    /// History is labelled by a route that leaves out what could be a
    /// credential.
    #[test]
    fn a_sent_requests_route_hides_credentials_and_query_values() {
        assert_eq!(
            route_of(
                HttpMethod::Get,
                "https://user:pw@api.example.com/users?token=abc#top",
                200
            ),
            "GET https://api.example.com/users returned status 200"
        );
        assert_eq!(
            route_of(HttpMethod::Post, "not a url", 500),
            "POST request returned status 500"
        );
    }
}
