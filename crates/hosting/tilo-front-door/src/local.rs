//! Hosted Tilo on loopback, for the clients of the door (the SDKs, the CLI)
//! to test against: the real door, serving HTTP, in front of a person's real
//! host and runtime ([`crate::runtime_support`]).
//!
//! The person is played by this process, which holds their root: it
//! approves the API key the door asks them to certify, through the door's
//! own `/keys/new`, sign-in and `/keys/poll`, exactly as their Tilo on the
//! web would. Their workspaces' actions come from an in-memory action host
//! publishing a note (`so.tilo.test`), answering the agent tools.

use crate::runtime_support::{Runtime, Served, enrol, serve, serve_runtime};
use crate::{Config, Door, hosts::Hosts, keys, store::Store};
use hyper::Method;
use serde_json::{Value, json};
use std::{io, sync::Arc};
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, SignInProof};

/// The door on loopback, its person's host and runtime, and a key.
pub struct Local {
    /// The door's origin, `http://127.0.0.1:<port>`.
    pub door: String,
    /// An API key reaching all of the person's workspaces.
    pub key: String,
    /// The person, by their identity root key, hex.
    pub person: String,
    root: ed25519_dalek::SigningKey,
    device: crate::runtime_support::Device,
    handle: Arc<Door>,
    served: Served,
    _runtime: Runtime,
    _files: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

impl Local {
    /// Start everything and approve a key called `name`.
    ///
    /// # Errors
    /// Loopback cannot be bound, randomness failed, or the door refused.
    pub async fn start(name: &str) -> io::Result<Self> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(other)?;
        let root = ed25519_dalek::SigningKey::from_bytes(&seed);
        let person = hex::encode(root.verifying_key().to_bytes());
        let device = enrol(&root, 21, "Laptop");
        let served = serve(&root).await;
        let runtime = serve_runtime(root.verifying_key().to_bytes(), true).await;
        let files = tempfile::tempdir()?;
        let token_file = files.path().join("agents.token");
        std::fs::write(&token_file, format!("{}\n", runtime.token))?;
        let registry = json!({"people": {person.clone(): {"hosts": [{
            "name": "local.tilo.test",
            "default": true,
            "device": hex::encode(served.device.cert.device_public_key),
            "addr": served.host.node_addr(),
            "agents": runtime.base,
            "agents_token_file": token_file,
        }]}}});
        let hosts_file = files.path().join("hosts.json");
        std::fs::write(&hosts_file, registry.to_string())?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let door = format!("http://{}", listener.local_addr()?);
        let handle = Door::new(
            &Config {
                issuer: door.clone(),
                web_entry: format!("{door}/person"),
            },
            Store::in_memory(),
            Arc::new(Hosts::new(hosts_file)?),
        )?;
        let task = tokio::spawn(crate::serve(Arc::clone(&handle), listener));
        let mut local = Self {
            door,
            key: String::new(),
            person,
            root,
            device,
            handle,
            served,
            _runtime: runtime,
            _files: files,
            task,
        };
        local.key = local.key_named(name, None).await?;
        Ok(local)
    }

    /// A new key called `name`, reaching `workspace` or all workspaces,
    /// asked for and approved as a terminal and the person would.
    ///
    /// # Errors
    /// The door refused a step.
    pub async fn key_named(&self, name: &str, workspace: Option<&str>) -> io::Result<String> {
        let asked = self
            .post(
                keys::NEW_PATH,
                json!({"name": name, "workspace": workspace}),
            )
            .await?;
        let approve = asked
            .get("approve_url")
            .and_then(Value::as_str)
            .ok_or_else(|| other(format!("no approval link: {asked}")))?;
        self.approve(approve).await?;
        let poll = asked.get("poll").cloned().unwrap_or_default();
        let collected = self.post(keys::POLL_PATH, json!({"poll": poll})).await?;
        collected
            .get("key")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| other(format!("no key: {collected}")))
    }

    /// Approve, as the person, the key the approval link `approve` names.
    ///
    /// # Errors
    /// The link is not the door's, or the door refused the approval.
    pub async fn approve(&self, approve: &str) -> io::Result<()> {
        let path = approve
            .strip_prefix(&self.door)
            .ok_or_else(|| other("the link is not this door's"))?;
        let reply = self
            .handle
            .dispatch(&Method::GET, path, "", &hyper::HeaderMap::new(), b"")
            .await;
        let location = reply
            .headers()
            .get(hyper::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| other("the link led nowhere"))?;
        let asked: std::collections::HashMap<String, String> = url::Url::parse(location)
            .map_err(other)?
            .query_pairs()
            .into_owned()
            .collect();
        let field = |name: &str| asked.get(name).cloned().unwrap_or_default();
        let challenge: [u8; 32] = hex::decode(field("challenge"))
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| other("no challenge"))?;
        let key: [u8; 32] = hex::decode(field("grant_key"))
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| other("no key to certify"))?;
        let scope = AgentScope::parse(&field("grant_scope")).ok_or_else(|| other("no scope"))?;
        let expires_ms: u64 = field("grant_expires").parse().map_err(other)?;
        let proof = SignInProof::sign(
            self.device.cert.clone(),
            &self.device.key,
            &self.door,
            challenge,
        )
        .map_err(other)?;
        let mut cert = DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key: key,
            device_label: field("client"),
            capabilities: DeviceCapabilities::agent_grant(scope),
            issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
            expires_at_ns: Some(expires_ms.saturating_mul(1_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &self.root).map_err(other)?;
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("pending", &field("pending"))
            .append_pair(
                "proof",
                &crate::sign_in::encode_proof(&proof).ok_or_else(|| other("no proof"))?,
            )
            .append_pair(
                "grant",
                &crate::sign_in::encode_cert(&cert).ok_or_else(|| other("no certificate"))?,
            )
            .finish();
        let answered = self
            .handle
            .dispatch(
                &Method::POST,
                "/sign-in",
                "",
                &hyper::HeaderMap::new(),
                form.as_bytes(),
            )
            .await;
        if answered.status() == hyper::StatusCode::OK {
            Ok(())
        } else {
            Err(other(format!(
                "the door refused the approval: {}",
                answered.status()
            )))
        }
    }

    async fn post(&self, path: &str, body: Value) -> io::Result<Value> {
        use http_body_util::BodyExt as _;
        let reply = self
            .handle
            .dispatch(
                &Method::POST,
                path,
                "",
                &hyper::HeaderMap::new(),
                body.to_string().as_bytes(),
            )
            .await;
        let bytes = reply.into_body().collect().await.map_err(other)?.to_bytes();
        serde_json::from_slice(&bytes).map_err(other)
    }

    /// Stop serving.
    pub async fn shutdown(self) {
        self.task.abort();
        self.served.shutdown().await;
    }
}
