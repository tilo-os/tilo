//! First-run / screenshot workspace seed.
//!
//! Produces a representative set of collections, environments, a populated
//! active request, and a short history so a brand-new workspace (and the
//! headless screenshot) reads as a real, lived-in API tool rather than an
//! empty shell.

use crate::model::{
    Auth, BodyMode, Collection, Environment, HistoryEntry, HttpMethod, KeyValue, Request,
};

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
        KeyValue::new("token", "prod_sk_9f8c1a2b3d4e5f60"),
        KeyValue::new("user", "ada"),
    ];

    let mut staging = Environment::new("Staging");
    staging.variables = vec![
        KeyValue::new("base_url", "https://staging.example.com/v2"),
        KeyValue::new("token", "stg_sk_0011223344556677"),
        KeyValue::new("user", "ada"),
    ];

    let mut local = Environment::new("Local");
    local.variables = vec![
        KeyValue::new("base_url", "http://localhost:8080"),
        KeyValue::new("token", "dev-token"),
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
        KeyValue::new("X-Client", "qos-api-tester"),
    ];
    r.auth = Auth::Bearer {
        token: "{{token}}".into(),
    };
    r
}

/// A short recent-request history for the History list.
/// A single blank starter request for the live app — a real Send is what
/// produces a response; nothing is fabricated.
pub fn blank_request() -> Request {
    let mut r = Request::new("New request");
    r.method = HttpMethod::Get;
    r.url = String::new();
    r
}

pub fn history() -> Vec<HistoryEntry> {
    vec![
        HistoryEntry {
            id: uuid::Uuid::new_v4(),
            method: HttpMethod::Get,
            url: "https://api.example.com/v2/users".into(),
            status: 200,
            duration_ms: 84,
            when: "just now".into(),
        },
        HistoryEntry {
            id: uuid::Uuid::new_v4(),
            method: HttpMethod::Post,
            url: "https://api.example.com/v2/users".into(),
            status: 201,
            duration_ms: 142,
            when: "2m ago".into(),
        },
        HistoryEntry {
            id: uuid::Uuid::new_v4(),
            method: HttpMethod::Get,
            url: "https://api.example.com/v2/users/4291".into(),
            status: 404,
            duration_ms: 61,
            when: "8m ago".into(),
        },
        HistoryEntry {
            id: uuid::Uuid::new_v4(),
            method: HttpMethod::Post,
            url: "https://staging.example.com/v2/auth/login".into(),
            status: 401,
            duration_ms: 73,
            when: "15m ago".into(),
        },
    ]
}
