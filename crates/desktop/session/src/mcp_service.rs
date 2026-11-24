//! The person's MCP server as this session publishes it to its apps: every agent the server
//! let in, the reach it was let in for and the sessions open now, read as `mcp.server` off the
//! daemon this session's shell connection reaches.
//!
//! The reading is the daemon's ([`crate::mcp_service`] follows it), so a window listing the
//! agents that act here reads the same state that stops one or lets it go on; the two verbs
//! are asked of the same daemon, on the same connection.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};
use tilo_actions::remote::mcp_server;
use tilo_lock::MutexRecover as _;
use tilo_service::{Service, Started};

/// The resource the server reads as, and the two verbs are asked on.
pub(crate) use tilo_actions::remote::mcp_server::RESOURCE as SERVER;

/// The server, followed off the daemon `link` reaches.
pub(crate) struct Served {
    /// Its reading as the daemon last published it, or `None` while none came.
    reading: Arc<Mutex<Option<String>>>,
    /// The revision the daemon published it at: what [`Service::poll`] compares.
    revision: Arc<AtomicU64>,
    seen: u64,
    moved: bool,
    /// Where the verbs are asked: the same daemon the reading comes from.
    link: Option<runtime_collab::ShellClient>,
}

impl Served {
    /// Follow the server the daemon `link` publishes, now and at each change.
    pub(crate) fn follow(link: runtime_collab::ShellClient) -> Self {
        let reading = Arc::new(Mutex::new(None));
        let revision = Arc::new(AtomicU64::new(0));
        let (held, at) = (Arc::clone(&reading), Arc::clone(&revision));
        let following = link.clone();
        tilo_rt::app().spawn(async move {
            let mut followed = match following.standing().watch(SERVER).await {
                Ok(followed) => followed,
                Err(why) => {
                    tracing::warn!(%why, "the MCP server is not followed here");
                    return;
                }
            };
            loop {
                match followed.read().await {
                    Ok(read) => {
                        *held.lock_recover() = read.reading;
                        at.store(read.revision, Ordering::Relaxed);
                    }
                    Err(why) => {
                        tracing::warn!(%why, "the MCP server's reading stopped");
                        return;
                    }
                }
            }
        });
        Self {
            reading,
            revision,
            seen: 0,
            moved: false,
            link: Some(link),
        }
    }

    /// The reading now, as the app reads it.
    fn reading(&self, resource: &str) -> Option<Value> {
        if resource != SERVER {
            return None;
        }
        let text = self.reading.lock_recover().clone()?;
        match serde_json::from_str(&text) {
            Ok(value) => Some(value),
            Err(error) => {
                tracing::warn!(%error, "the MCP server's reading is not JSON");
                None
            }
        }
    }
}

/// The server's root: a reading, not a document.
#[derive(tilo_resource::Row)]
#[row(kind = "server", label = "MCP server")]
struct ServerRoot;

/// One agent session's grant and name, as `stop` and `go-on` take them.
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
)]
#[serde(default)]
struct SessionRef {
    grant: String,
    session: String,
}

/// What a person asks of the server: stop an agent, or let it go on.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.mcp-server")]
enum ServerAction {
    /// Stop an agent now: its running calls are cancelled and every next one is refused, in every workspace it works in, until it is let go on.
    #[effect(kind = "undoable", destructive)]
    Stop {
        /// The grant it came in by, and the name its session runs under.
        #[param("The grant it came in by, and the name its session runs under")]
        session: SessionRef,
    },
    /// Let an agent its person stopped go on: its calls are answered again.
    #[effect(kind = "undoable")]
    GoOn {
        /// The grant it came in by, and the name its session runs under.
        #[param("The grant it came in by, and the name its session runs under")]
        session: SessionRef,
    },
}

impl Service for Served {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![tilo_resource::offer::<ServerAction>(
            tilo_resource::typed_resource::<ServerRoot>(SERVER, ""),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        self.reading(resource)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, tilo_actions::ApiError> {
        use tilo_actions::ApiError;
        if resource != SERVER {
            return Err(ApiError::Unknown(tilo_actions::ActionAddress::new(
                resource, action,
            )));
        }
        let Some(link) = self.link.clone() else {
            return Err(ApiError::Unavailable {
                code: "no_daemon".to_owned(),
                reason: "this session reached no daemon to ask the server".to_owned(),
            });
        };
        // The person's own ask, as the app's call: it reaches the device's one dispatch as the
        // person sitting here.
        let request = tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: mcp_server::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(resource, action),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
        };
        let (complete, pending) = tilo_actions::remote::pending();
        tilo_rt::app().spawn(async move {
            let answered = link.dispatch().settled(None, request).await;
            complete.complete(answered.map_or_else(
                |reason| {
                    Err(ApiError::Refused {
                        code: "mcp_refused".to_owned(),
                        reason,
                    })
                },
                |value| {
                    Ok(tilo_actions::ActionOutput {
                        value: Some(value),
                        resources: Vec::new(),
                    })
                },
            ));
        });
        Ok(Started::Later(pending))
    }

    fn poll(&mut self) -> bool {
        let revision = self.revision.load(Ordering::Relaxed);
        if revision == self.seen {
            return false;
        }
        self.seen = revision;
        self.moved = true;
        true
    }

    fn moved(&mut self) -> Option<Vec<String>> {
        if !self.moved {
            return None;
        }
        self.moved = false;
        Some(vec![SERVER.to_owned()])
    }
}
