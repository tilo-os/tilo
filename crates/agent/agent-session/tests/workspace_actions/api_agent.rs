//! A key's calls of the API's `/v1` routes, as an agent or `tilo` makes them: the HTTP request
//! and its JSON answer, and what the tests that measure their work ask it for.
use super::api_on_server::{headers, json_of};
use serde_json::{Value, json};
use std::sync::Arc;
use tilo_api::Server;

/// The most members one `group` call carries, as the SDK sends them.
pub(super) const GROUP_MEMBERS: usize = 500;

/// A key's calls of the API.
pub(super) struct Agent<'a> {
    pub(super) api: &'a Arc<Server>,
    pub(super) token: String,
}

impl Agent<'_> {
    /// `method` of the route at `path`, its body of `kind`: the status and the reply.
    pub(super) async fn send(
        &self,
        method: hyper::Method,
        path: &str,
        body: &[u8],
        kind: &str,
    ) -> (u16, Value) {
        let bearer = format!("Bearer {}", self.token);
        let pairs = [("authorization", bearer.as_str()), ("content-type", kind)];
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let reply = Box::pin(
            self.api
                .dispatch(&method, path, query, &headers(&pairs), body),
        )
        .await;
        json_of(reply).await
    }

    /// `path`, which answers 200.
    pub(super) async fn get(&self, path: &str) -> Value {
        let (status, value) = self
            .send(hyper::Method::GET, path, b"", "application/json")
            .await;
        assert_eq!(status, 200, "GET {path}: {value}");
        value
    }

    /// `action` of `resource` with `args`, which ran: its result.
    pub(super) async fn act(
        &self,
        base: &str,
        resource: &str,
        action: &str,
        args: &Value,
    ) -> Value {
        let (status, reply) = self
            .send(
                hyper::Method::POST,
                &format!("{base}/resources/{resource}/actions/{action}"),
                args.to_string().as_bytes(),
                "application/json",
            )
            .await;
        assert_eq!(status, 200, "{resource}#{action} {args}: {reply}");
        assert_eq!(reply["status"], "ok", "{resource}#{action} {args}: {reply}");
        reply["result"].clone()
    }

    /// `action` once for each of `items`, as the SDK's bulk call sends it: `group` calls of at
    /// most [`GROUP_MEMBERS`] members. A resource that takes no group is answered by why.
    pub(super) async fn many(
        &self,
        base: &str,
        resource: &str,
        action: &str,
        items: &[Value],
    ) -> Result<(), String> {
        for chunk in items.chunks(GROUP_MEMBERS) {
            let members: Vec<Value> = chunk
                .iter()
                .map(|args| json!({"action": action, "args": args}))
                .collect();
            let (status, reply) = self
                .send(
                    hyper::Method::POST,
                    &format!("{base}/resources/{resource}/actions/group"),
                    json!({ "actions": members }).to_string().as_bytes(),
                    "application/json",
                )
                .await;
            if status != 200 || reply["status"] != "ok" {
                return Err(format!("{status} {}", reply["error"]["message"]));
            }
        }
        Ok(())
    }

    /// [`Self::many`], or one call after another where the resource takes no group: why it
    /// went one at a time, where it did.
    pub(super) async fn many_or_each(
        &self,
        base: &str,
        resource: &str,
        action: &str,
        items: &[Value],
    ) -> Option<String> {
        let refused = self.many(base, resource, action, items).await.err()?;
        for args in items {
            self.act(base, resource, action, args).await;
        }
        Some(refused)
    }

    /// `bytes` into the workspace in one request: their `tilo.content:` name.
    pub(super) async fn upload(&self, base: &str, bytes: &[u8]) -> String {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let (status, reply) = self
            .send(
                hyper::Method::PUT,
                &format!("{base}/contents/{hash}"),
                bytes,
                "application/octet-stream",
            )
            .await;
        assert!(status == 200 || status == 201, "upload: {status} {reply}");
        let text = reply.to_string();
        let at = text
            .find("tilo.content:")
            .unwrap_or_else(|| panic!("an upload names its content: {text}"));
        text[at..].split('"').next().unwrap_or_default().to_owned()
    }
}

/// A workspace of the key's person, through the API: its base path and id.
pub(super) async fn workspace(agent: &Agent<'_>, name: &str) -> (String, String) {
    let (status, made) = agent
        .send(
            hyper::Method::POST,
            "/v1/resources/account/workspaces/actions/create",
            json!({ "name": name }).to_string().as_bytes(),
            "application/json",
        )
        .await;
    assert_eq!(status, 200, "{made}");
    let id = made["result"]["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (format!("/v1/workspaces/{id}"), id)
}
