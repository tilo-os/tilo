//! A person's key to the server's `/v1` API, and the person it belongs to: what the branch tests
//! (a workspace branched, its files put by a key, a mail delivery rate) ask the server with.

use super::api_on_server::{OnServer, approved_key, headers, json_of};
use super::*;
use serde_json::Value;
use std::sync::Arc;
use tilo_api::Server;

/// A key's calls of `/v1`.
pub(super) struct Key<'a> {
    pub(super) api: &'a Arc<Server>,
    pub(super) token: String,
}

impl Key<'_> {
    pub(super) async fn send(
        &self,
        method: hyper::Method,
        path: &str,
        body: &[u8],
        extra: &[(&'static str, &str)],
    ) -> tilo_api::Reply {
        let bearer = format!("Bearer {}", self.token);
        let mut pairs = vec![
            ("authorization", bearer.as_str()),
            ("content-type", "application/json"),
        ];
        pairs.extend_from_slice(extra);
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        self.api
            .dispatch(&method, path, query, &headers(&pairs), body)
            .await
    }

    pub(super) async fn get(&self, path: &str) -> (u16, Value) {
        json_of(self.send(hyper::Method::GET, path, b"", &[]).await).await
    }

    pub(super) async fn post(
        &self,
        path: &str,
        body: &Value,
        extra: &[(&'static str, &str)],
    ) -> (u16, Value) {
        let body = body.to_string();
        json_of(
            self.send(hyper::Method::POST, path, body.as_bytes(), extra)
                .await,
        )
        .await
    }

    /// `POST path` with `body`, which must succeed: its answer.
    pub(super) async fn did(&self, path: &str, body: &Value) -> Value {
        let (status, answer) = self.post(path, body, &[]).await;
        assert!((200..300).contains(&status), "{path} {body}: {answer}");
        answer
    }
}

/// A person on `on_server`, their page and a key reaching all their workspaces.
pub(super) async fn person(on_server: &OnServer, name: &str) -> (SigningKey, String) {
    let (root, _) = tilo_user_identity::UserIdentity::generate(name, 1);
    let page = enrol(
        &root,
        &format!("{name}'s browser"),
        DeviceCapabilities::member(),
    );
    let token = approved_key(on_server, &root, &page, &format!("{name}'s backend")).await;
    (root, token)
}
