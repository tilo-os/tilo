//! A local terminal carried to a session on a relay.
//!
//! The device's `ush` supervisor answers a Terminal asking for an
//! environment on a relay the way it answers any other target: with a pty.
//! Behind that pty runs `tilo-relay-environment`, whose stdio is the pty's
//! other end and whose loop is [`run_bridge`]: keystrokes go up, output
//! comes down, the pty's size follows the Terminal's, and when the
//! connection drops the bridge re-attaches to the same session by its id
//! and shows only the output it missed.

use std::future::Future;
use std::time::Duration;

use serde::Serialize;
use tilo_relay_client::{EnvironmentClientError, PtyEvent, PtyStream};
use tilo_relay_protocol::environment::{
    DirectoryGrant, EnvironmentRecordRef, EnvironmentSpec, HomePolicy, MAX_SCROLLBACK_BYTES,
    PtySize, Refusal, RuntimeImages, SessionId, ToolImage, this_architecture,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

/// The wire form of an environment record: what a relay builds the guest
/// from. The record's images are this device's architecture's; a relay of
/// another architecture builds the tools it lacks. Grants are the record's
/// `FsPath` grants for the runtime; read-write when the grant allows
/// writing.
#[must_use]
pub fn environment_spec(environment: &tilo_environment::Environment) -> EnvironmentSpec {
    use collab_types::caps::{Scope, Verb};
    let arch = this_architecture().to_owned();
    EnvironmentSpec {
        name: environment.name.clone(),
        runtime: RuntimeImages {
            release: String::new(),
            images: [(arch.clone(), environment.runtime.clone())].into(),
        },
        tools: environment
            .tools
            .iter()
            .map(|tool| ToolImage {
                packages: tool.packages.clone(),
                images: [(arch.clone(), tool.id.clone())].into(),
            })
            .collect(),
        grants: environment
            .grants
            .0
            .iter()
            .filter(|grant| tilo_environment::is_runtime_grant(grant))
            .filter_map(|grant| match &grant.scope {
                Scope::FsPath(path) => Some(DirectoryGrant {
                    path: path.clone(),
                    writable: matches!(grant.verb, Verb::Write | Verb::Admin),
                }),
                _ => None,
            })
            .collect(),
        home: match environment.home {
            tilo_environment::HomePolicy::Persistent => HomePolicy::Persistent,
            tilo_environment::HomePolicy::Ephemeral => HomePolicy::Ephemeral,
        },
    }
}

/// How long the bridge keeps trying to re-attach after the connection
/// dropped.
pub const REATTACH_PATIENCE: Duration = Duration::from_secs(60);

/// What the bridge reports on its status channel, one JSON line each.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BridgeStatus {
    /// The session is attached; its id re-attaches to it.
    Session {
        /// Hex session id.
        session: String,
        /// Whether it was already running.
        resumed: bool,
    },
    /// A tool was built for the environment on the relay.
    ToolBuilt {
        /// The environment.
        record: String,
        /// The workspace it lives in.
        workspace_id: u64,
        /// The packages it provides.
        packages: Vec<String>,
        /// The image built, by architecture.
        images: std::collections::BTreeMap<String, String>,
    },
    /// The bridge ended.
    Ended {
        /// The guest's exit code, when it exited.
        code: Option<i32>,
        /// Why, when it did not simply exit.
        reason: Option<String>,
    },
}

impl BridgeStatus {
    fn tool(record: &EnvironmentRecordRef, tool: &ToolImage) -> Self {
        Self::ToolBuilt {
            record: record.record_id.clone(),
            workspace_id: record.workspace_id,
            packages: tool.packages.clone(),
            images: tool.images.clone(),
        }
    }
}

/// How a bridge ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeEnd {
    /// The guest exited.
    Exited(Option<i32>),
    /// Another terminal took the session over.
    Superseded,
    /// The local terminal closed; the session keeps running on the relay.
    Detached,
    /// The session could not be reached again.
    Lost(String),
}

/// Carry `local` (the pty the Terminal sees) to `stream`, re-attaching with
/// `reattach` when the connection drops.
///
/// # Errors
///
/// Returns an error only when the local terminal fails.
pub async fn run_bridge<I, O, R, F>(
    mut stream: PtyStream,
    input: &mut I,
    output: &mut O,
    sizes: &mut mpsc::Receiver<PtySize>,
    status: &mut (dyn FnMut(BridgeStatus) + Send),
    mut reattach: R,
) -> std::io::Result<BridgeEnd>
where
    I: AsyncRead + Unpin + Send,
    O: AsyncWrite + Unpin + Send,
    R: FnMut(SessionId) -> F + Send,
    F: Future<Output = Result<PtyStream, EnvironmentClientError>> + Send,
{
    let session = stream.session();
    let mut seen = Seen::default();
    let scrollback = stream.take_scrollback();
    output.write_all(&scrollback).await?;
    output.flush().await?;
    seen.extend(&scrollback);
    status(BridgeStatus::Session {
        session: hex::encode(session),
        resumed: stream.resumed(),
    });
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let lost = tokio::select! {
            read = input.read(&mut buffer) => {
                let read = read?;
                if read == 0 {
                    stream.detach().await;
                    return Ok(BridgeEnd::Detached);
                }
                stream.write(buffer.get(..read).unwrap_or_default()).await.err()
            }
            size = sizes.recv() => match size {
                Some(size) => stream.resize(size).await.err(),
                None => None,
            },
            event = stream.next() => match event {
                Ok(Some(PtyEvent::Output(bytes))) => {
                    output.write_all(&bytes).await?;
                    output.flush().await?;
                    seen.extend(&bytes);
                    None
                }
                Ok(Some(PtyEvent::Exited(code))) => return Ok(BridgeEnd::Exited(code)),
                Ok(Some(PtyEvent::Superseded)) => return Ok(BridgeEnd::Superseded),
                Ok(Some(PtyEvent::ToolBuilt { record, tool })) => {
                    status(BridgeStatus::tool(&record, &tool));
                    None
                }
                Ok(None) => Some(EnvironmentClientError::Connection("the relay closed the stream".into())),
                Err(error) => Some(error),
            },
        };
        let Some(lost) = lost else { continue };
        if let EnvironmentClientError::Refused { reason, detail } = &lost {
            return Ok(BridgeEnd::Lost(format!("{reason:?}: {detail}")));
        }
        tracing::info!(error = %lost, "relay environment connection lost; re-attaching");
        stream = match reattach_with_patience(session, &mut reattach).await {
            Ok(stream) => stream,
            Err(reason) => return Ok(BridgeEnd::Lost(reason)),
        };
        let scrollback = stream.take_scrollback();
        let unseen = seen.unseen(&scrollback);
        output.write_all(unseen).await?;
        output.flush().await?;
        seen.extend(unseen);
    }
}

async fn reattach_with_patience<R, F>(
    session: SessionId,
    reattach: &mut R,
) -> Result<PtyStream, String>
where
    R: FnMut(SessionId) -> F + Send,
    F: Future<Output = Result<PtyStream, EnvironmentClientError>> + Send,
{
    let deadline = tokio::time::Instant::now()
        .checked_add(REATTACH_PATIENCE)
        .unwrap_or_else(tokio::time::Instant::now);
    let mut backoff = Duration::from_millis(100);
    loop {
        match reattach(session).await {
            Ok(stream) => return Ok(stream),
            Err(EnvironmentClientError::Refused {
                reason: reason @ (Refusal::UnknownSession | Refusal::NotOwner | Refusal::NotAMember),
                detail,
            }) => return Err(format!("{reason:?}: {detail}")),
            Err(error) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(error.to_string());
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff.saturating_mul(2)).min(Duration::from_secs(5));
            }
        }
    }
}

/// The tail of the output this terminal already showed, so a re-attach
/// shows only what it missed.
#[derive(Default)]
struct Seen(std::collections::VecDeque<u8>);

impl Seen {
    fn extend(&mut self, bytes: &[u8]) {
        self.0.extend(bytes);
        let excess = self.0.len().saturating_sub(MAX_SCROLLBACK_BYTES);
        self.0.drain(..excess);
    }

    /// The part of `scrollback` after the longest prefix of it this
    /// terminal already showed last.
    fn unseen<'a>(&mut self, scrollback: &'a [u8]) -> &'a [u8] {
        let seen = self.0.make_contiguous();
        let longest = scrollback.len().min(seen.len());
        (1..=longest)
            .rev()
            .find(|length| {
                scrollback
                    .get(..*length)
                    .is_some_and(|prefix| seen.ends_with(prefix))
            })
            .and_then(|length| scrollback.get(length..))
            .unwrap_or(scrollback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_record_becomes_the_spec_a_relay_builds_from() {
        let runtime = tilo_environment::PackageId::from_bytes([1; 32]);
        let mut environment = tilo_environment::Environment::new("Work", &runtime);
        environment.tools.push(tilo_environment::ToolRef {
            id: tilo_environment::PackageId::from_bytes([2; 32]).to_string(),
            packages: vec!["pandoc".into()],
        });
        environment.grants.0.push(
            tilo_environment::dir_grant(std::path::Path::new("/home/alice/src"), true).unwrap(),
        );
        environment.grants.0.push(
            tilo_environment::dir_grant(std::path::Path::new("/home/alice/docs"), false).unwrap(),
        );
        let spec = environment_spec(&environment);
        assert!(spec.valid());
        assert_eq!(
            spec.runtime.images.get(this_architecture()),
            Some(&runtime.to_string())
        );
        assert_eq!(spec.tools[0].packages, vec!["pandoc".to_owned()]);
        assert_eq!(
            spec.grants,
            vec![
                DirectoryGrant {
                    path: "/home/alice/src".into(),
                    writable: true,
                },
                DirectoryGrant {
                    path: "/home/alice/docs".into(),
                    writable: false,
                },
            ]
        );
    }

    #[test]
    fn a_reattach_shows_only_the_output_it_missed() {
        let mut seen = Seen::default();
        seen.extend(b"$ make\nok\n");
        assert_eq!(seen.unseen(b"$ make\nok\n$ "), b"$ ");
        assert_eq!(seen.unseen(b"ok\n$ ls\n"), b"$ ls\n");
        assert_eq!(seen.unseen(b"$ make\nok\n"), b"");
        // No overlap at all: everything is new.
        assert_eq!(seen.unseen(b"fresh"), b"fresh");
    }
}
