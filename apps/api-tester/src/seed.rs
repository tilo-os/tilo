//! The sample workspace screenshots show.
//!
//! A representative set of collections, environments, a populated active
//! request, and a short history, built through the workspace's field edits
//! and verbs.

use ui::tilo_resource::{Collection as Items, Id, Invoke};

use crate::model::{
    Auth, BodyMode, Collection, Environment, HistoryEntry, HttpMethod, KeyValue, Request,
};
use crate::record::{self, RequestRecord, SavedCollection, Variable, Workspace};
use crate::resource::workspace as verbs;

/// Default collections grouping a handful of saved requests.
pub fn collections() -> Vec<Collection> {
    let mut users = Collection::new("Users API");
    users.requests = vec![
        {
            let mut r = Request::new("List users");
            r.method = HttpMethod::Get;
            r.url = "{{base_url}}/users".into();
            r.params = vec![KeyValue::new("page", "1"), KeyValue::new("per_page", "25")];
            r.headers = vec![KeyValue::new("Accept", "application/json")];
            r.auth = Auth::Bearer {
                token: "{{token}}".into(),
            };
            r
        },
        {
            let mut r = Request::new("Get user by id");
            r.method = HttpMethod::Get;
            r.url = "{{base_url}}/users/4291".into();
            r.auth = Auth::Bearer {
                token: "{{token}}".into(),
            };
            r
        },
        {
            let mut r = Request::new("Create user");
            r.method = HttpMethod::Post;
            r.url = "{{base_url}}/users".into();
            r.headers = vec![KeyValue::new("Content-Type", "application/json")];
            r.auth = Auth::Bearer {
                token: "{{token}}".into(),
            };
            r.body_mode = BodyMode::Json;
            r.body_text =
                "{\n  \"name\": \"Grace Hopper\",\n  \"email\": \"grace@navy.mil\"\n}".into();
            r
        },
        {
            let mut r = Request::new("Delete user");
            r.method = HttpMethod::Delete;
            r.url = "{{base_url}}/users/4291".into();
            r.auth = Auth::Bearer {
                token: "{{token}}".into(),
            };
            r
        },
    ];

    let mut auth = Collection::new("Auth");
    auth.expanded = false;
    auth.requests = vec![
        {
            let mut r = Request::new("Login");
            r.method = HttpMethod::Post;
            r.url = "{{base_url}}/auth/login".into();
            r.body_mode = BodyMode::Form;
            r.form = vec![
                KeyValue::new("username", "{{user}}"),
                KeyValue::new("password", "{{password}}"),
            ];
            r
        },
        {
            let mut r = Request::new("Refresh token");
            r.method = HttpMethod::Post;
            r.url = "{{base_url}}/auth/refresh".into();
            r
        },
    ];

    let mut webhooks = Collection::new("Webhooks");
    webhooks.expanded = false;
    webhooks.requests = vec![{
        let mut r = Request::new("Register webhook");
        r.method = HttpMethod::Post;
        r.url = "{{base_url}}/webhooks".into();
        r.body_mode = BodyMode::Json;
        r.body_text = "{\n  \"url\": \"https://hooks.example.dev/incoming\",\n  \"events\": [\"user.created\"]\n}".into();
        r
    }];

    vec![users, auth, webhooks]
}

/// Default environments. The first is the active one.
pub fn environments() -> Vec<Environment> {
    let mut production = Environment::new("Production");
    production.variables = vec![
        KeyValue::new("base_url", "https://api.example.com/v2"),
        KeyValue::new("token", "prod_sk_9f8c1a2b3d4e5f60").secret(),
        KeyValue::new("user", "ada"),
    ];

    let mut staging = Environment::new("Staging");
    staging.variables = vec![
        KeyValue::new("base_url", "https://staging.example.com/v2"),
        KeyValue::new("token", "stg_sk_0011223344556677").secret(),
        KeyValue::new("user", "ada"),
    ];

    let mut local = Environment::new("Local");
    local.variables = vec![
        KeyValue::new("base_url", "http://localhost:8080"),
        KeyValue::new("token", "dev-token").secret(),
        KeyValue::new("user", "dev"),
    ];

    vec![production, staging, local]
}

/// The request the builder opens on first run — the populated "List users"
/// call so every tab has something to show.
pub fn active_request() -> Request {
    let mut r = Request::new("List users");
    r.method = HttpMethod::Get;
    r.url = "{{base_url}}/users".into();
    r.params = vec![
        KeyValue::new("page", "1"),
        KeyValue::new("per_page", "25"),
        {
            let mut sort = KeyValue::new("sort", "created_at");
            sort.enabled = false;
            sort
        },
    ];
    r.headers = vec![
        KeyValue::new("Accept", "application/json"),
        KeyValue::new("X-Client", "tilo-api-tester"),
    ];
    r.auth = Auth::Bearer {
        token: "{{token}}".into(),
    };
    r
}

/// A short recent-request history for the History list.
/// A single blank starter request for the live app. Sending it produces a
/// response.
pub fn blank_request() -> Request {
    let mut r = Request::new("New request");
    r.method = HttpMethod::Get;
    r.url = String::new();
    r
}

pub fn history() -> Vec<HistoryEntry> {
    let sent = |method, url: &str, status, duration_ms| HistoryEntry {
        id: crate::model::fresh_id(),
        method,
        url: url.into(),
        status,
        duration_ms,
    };
    vec![
        sent(HttpMethod::Get, "https://api.example.com/v2/users", 200, 84),
        sent(
            HttpMethod::Post,
            "https://api.example.com/v2/users",
            201,
            142,
        ),
        sent(
            HttpMethod::Get,
            "https://api.example.com/v2/users/4291",
            404,
            61,
        ),
        sent(
            HttpMethod::Post,
            "https://staging.example.com/v2/auth/login",
            401,
            73,
        ),
    ]
}

/// The edits and verbs that build the sample workspace.
pub fn workspace() -> Vec<Invoke<Workspace>> {
    // Ids in order, so the sample lists the same way on every run.
    let mut next = 0u128;
    let mut id = || {
        next += 1;
        Id::from_raw(uuid::Uuid::from_u128(next).to_string())
    };
    let paths = Workspace::paths();
    let mut invocations = Vec::new();
    let mut first = None;
    for (at, collection) in collections().into_iter().enumerate() {
        let collection_id = id();
        let mut requests = Items::new();
        for request in &collection.requests {
            let request_id = id();
            let record = RequestRecord::of(request);
            first
                .get_or_insert_with(|| (collection_id.clone(), request_id.clone(), record.clone()));
            requests.push(request_id, record);
        }
        let saved = SavedCollection {
            name: collection.name,
            requests,
        };
        invocations.push(Invoke::edit(&paths.collections.insert_as(
            &collection_id,
            at,
            &saved,
        )));
    }
    let mut active = None;
    for (at, environment) in environments().into_iter().enumerate() {
        let environment_id = id();
        active.get_or_insert_with(|| environment_id.clone());
        let mut variables = Items::new();
        for variable in &environment.variables {
            variables.push(
                id(),
                Variable {
                    key: variable.key.clone(),
                    value: if variable.secret {
                        String::new()
                    } else {
                        variable.value.clone()
                    },
                    enabled: variable.enabled,
                    secret: variable.secret,
                },
            );
        }
        let saved = record::Environment {
            name: environment.name,
            variables,
        };
        invocations.push(Invoke::edit(&paths.environments.insert_as(
            &environment_id,
            at,
            &saved,
        )));
    }
    invocations.push(Invoke::edit(&paths.active_environment.set(active)));
    let mut invocations: Vec<Invoke<Workspace>> =
        invocations.into_iter().filter_map(Result::ok).collect();
    // The builder shows the first saved request, edited.
    if let Some((collection, request, saved)) = first {
        invocations.push(Workspace::OPEN_REQUEST.with(verbs::OpenRequest {
            collection,
            request,
        }));
        invocations.extend(
            paths
                .request
                .path()
                .edits(&saved, &RequestRecord::of(&active_request()))
                .iter()
                .filter_map(|edit| Invoke::edit(edit).ok()),
        );
    }
    invocations.extend(history().into_iter().rev().map(|sent| {
        Workspace::RECORD_SENT.with(verbs::RecordSent {
            method: sent.method.as_str().to_owned(),
            url: sent.url,
            status: sent.status,
            duration_ms: sent.duration_ms,
        })
    }));
    invocations
}
