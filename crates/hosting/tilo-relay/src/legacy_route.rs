//! Legacy launches from the environments this worker hosts.
//!
//! A guest's `code .` reaches its broker, inside the member's `ush`
//! supervisor on this worker. On a device the broker would run the app in
//! the user's own legacy VM; here it asks the worker instead, on a socket
//! made for that member ([`ush::legacy_relay::LEGACY_LAUNCHES_ENV`]). The
//! worker sends the launch to the relay ([`request_legacy_launch`]) as one
//! of the member's sessions on this worker, and the relay routes it to a
//! worker with KVM; the app's windows reach the member's devices on their
//! legacy-windows stream.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use tilo_relay_protocol::environment::{
    LegacyGuestOs, LegacyLaunch, LegacyLaunchOutcome, MemberIdentity,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use ush::legacy_relay::{LaunchReply, LaunchRequest, MAX_LINE_BYTES};

use crate::environment_host::WeakEnvironmentHost;
use crate::legacy_host::request_legacy_launch;

/// Routes members' legacy launches from this worker to the relay.
#[derive(Clone, Default)]
pub struct LegacyLaunchRoute {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    host: Mutex<Option<WeakEnvironmentHost>>,
    relay: Mutex<Option<iroh::endpoint::Connection>>,
}

impl LegacyLaunchRoute {
    /// A route with no environment host and no relay yet: launches are
    /// refused until both are set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The environment host whose sessions launches are made from.
    pub fn set_host(&self, host: WeakEnvironmentHost) {
        *self
            .inner
            .host
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(host);
    }

    /// The connection to the relay this worker serves, once attached.
    pub fn set_relay(&self, connection: iroh::endpoint::Connection) {
        *self
            .inner
            .relay
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(connection);
    }

    /// Route one launch for `member`.
    pub async fn launch(&self, member: &MemberIdentity, asked: LaunchRequest) -> LaunchReply {
        let refused = |detail: &str| LaunchReply::Refused {
            detail: detail.to_string(),
        };
        let host = self
            .inner
            .host
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(WeakEnvironmentHost::upgrade);
        let Some((workspace_id, session)) = host.and_then(|host| host.member_session(&member.user))
        else {
            return refused("you have no environment session on this worker");
        };
        let relay = self
            .inner
            .relay
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Some(relay) = relay else {
            return refused("this worker is not attached to its relay");
        };
        let request = LegacyLaunch {
            guest: LegacyGuestOs::Linux,
            app: asked.app,
            args: asked.args,
            cwd: asked.cwd,
        };
        if !request.valid() {
            return refused("invalid legacy launch");
        }
        match request_legacy_launch(&relay, workspace_id, session, request).await {
            Ok(LegacyLaunchOutcome::Launched { launch }) => LaunchReply::Launched {
                launch: hex::encode(launch),
            },
            Ok(LegacyLaunchOutcome::Refused { detail, .. }) => refused(&detail),
            Err(error) => refused(&format!("the relay: {error}")),
        }
    }

    /// Listen for `member`'s supervisor on `socket`, reachable only by
    /// `uid` (the worker's own when members share it). Ends when the
    /// returned task is aborted.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket cannot be made, or there is no
    /// runtime to serve it on.
    pub fn listen(
        &self,
        member: MemberIdentity,
        socket: &Path,
        uid: Option<u32>,
    ) -> std::io::Result<tokio::task::JoinHandle<()>> {
        let runtime = tokio::runtime::Handle::try_current().map_err(std::io::Error::other)?;
        let listener = tilo_files::native::private::bind_socket(socket)?;
        if let Some(uid) = uid {
            std::os::unix::fs::chown(socket, Some(uid), Some(uid))?;
        }
        listener.set_nonblocking(true)?;
        let _entered = runtime.enter();
        let listener = tokio::net::UnixListener::from_std(listener)?;
        let route = self.clone();
        Ok(runtime.spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let expected = uid.unwrap_or_else(|| {
                    // SAFETY: geteuid cannot fail.
                    unsafe { libc::geteuid() }
                });
                if stream.peer_cred().map(|cred| cred.uid()).ok() != Some(expected) {
                    continue;
                }
                let route = route.clone();
                tokio::spawn(async move {
                    if let Err(error) = route.serve(&member, stream).await {
                        tracing::debug!(%error, "legacy launch connection ended");
                    }
                });
            }
        }))
    }

    async fn serve(
        &self,
        member: &MemberIdentity,
        stream: tokio::net::UnixStream,
    ) -> std::io::Result<()> {
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        tokio::io::BufReader::new(read)
            .take(u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX))
            .read_line(&mut line)
            .await?;
        let reply = match serde_json::from_str::<LaunchRequest>(&line) {
            Ok(asked) => self.launch(member, asked).await,
            Err(error) => LaunchReply::Refused {
                detail: format!("malformed legacy launch: {error}"),
            },
        };
        let mut bytes = serde_json::to_vec(&reply).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        write.write_all(&bytes).await
    }
}
