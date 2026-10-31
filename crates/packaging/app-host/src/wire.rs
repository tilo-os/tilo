//! The app-host wire: postcard messages behind `wire_frame` length prefixes
//! on the launcher-issued `app-host` connection.
//!
//! A caller reads the host's resources ([`Resource`]) and invokes its
//! actions ([`Action`]). Every call carries an id the answer echoes, so
//! several calls are in flight on one connection at once: a slow catalogue
//! read never holds up a pin or a list. All `id` fields carry the raw
//! 32-byte BLAKE3 package id; presentation (`tapp:<hex>`) is the caller's.

use std::future::Future;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::client::Refusal;

/// Both directions' framing. Large enough for the `.tapp` an install hands
/// over in one message.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

/// A raw 32-byte package id.
pub type WireId = [u8; 32];

/// One call: the caller's id for it, and what it asks.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    pub request: Operation,
}

/// The host's answer to the call `id`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub answer: Result<Answer, String>,
}

/// What a caller asks the host.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Operation {
    /// Read one of the host's resources; never changes anything.
    Read(Resource),
    /// Invoke one of the host's actions.
    Invoke(Action),
}

/// The resources the host answers reads of.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Resource {
    /// All currently known package entries.
    Packages,
    /// The app repositories this person configured, as `app repo list --json`
    /// reports them: the one list `app`, `tilo update` and the Store share.
    Repositories,
    /// Fixed read-only host Flatpak probes; the confined Store never runs
    /// host tools itself.
    FlatpakStatus,
    /// Verified entries from one enabled host-configured Tilo repository.
    /// The host owns durable rollback checkpoints; callers supply no keys
    /// or URLs.
    RegistryCatalogue { registry: String },
    /// The catalogue the host last accepted from one enabled repository,
    /// read without the network and verified again.
    LastVerifiedCatalogue { registry: String },
    /// Cached `AppStream` rows from one configured Flatpak repository.
    FlatpakCatalogue { remote: String, user_managed: bool },
    /// A precise runtime from enabled, pinned repositories, as its JSON
    /// catalogue entry (`null` when none is offered).
    RuntimeOffer { registry: String, runtime: String },
    /// Review handed bytes using this person's scoped attestors and
    /// revocations.
    LocalReview { tapp_bytes: Vec<u8> },
}

/// Retention of an installed package.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WirePinning {
    Tried,
    Pinned,
}

/// The catalogue identity an install must match; none of it is advisory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Expected {
    pub root: WireId,
    pub publisher: [u8; 32],
    pub app_id: String,
    pub version: String,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlatpakVerb {
    Install,
    Update,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteVerb {
    Add,
    Remove,
    Enable,
    Disable,
}

/// The actions the host performs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Action {
    /// Tried -> Pinned, or back (idempotent).
    SetPinned {
        id: WireId,
        pinned: bool,
    },
    /// Remove a Tried package; refuses Pinned ones.
    Remove {
        id: WireId,
    },
    /// Install a `.tapp` only when its bytes match the registry-authenticated
    /// identity.
    Install {
        tapp_bytes: Vec<u8>,
        pinning: WirePinning,
        expected: Expected,
    },
    /// Resolve the identity against this device's signed catalogue, then use
    /// the shared installer, including thin payload and runtime setup.
    InstallOffer {
        registry: String,
        expected: Expected,
    },
    /// Install the exact archive and root a [`Resource::LocalReview`]
    /// reported, reverified against current host trust.
    InstallLocal {
        tapp_bytes: Vec<u8>,
        reviewed_root: String,
        reviewed_archive_hash: String,
    },
    /// Launch a package this host resolved, through the host's launcher.
    Launch {
        id: WireId,
    },
    LaunchDesktop {
        desktop_id: String,
    },
    Flatpak {
        verb: FlatpakVerb,
        app_id: String,
        reference: Vec<u8>,
    },
    Remote {
        verb: RemoteVerb,
        name: String,
        url: String,
    },
    /// Install from an existing repository in this account's Flatpak scope.
    FlatpakInstallRemote {
        remote: String,
        app_id: String,
        branch: String,
    },
}

/// One package as the host lists it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageEntry {
    pub id: WireId,
    pub pinning: WirePinning,
    /// The materialised payload tree the launcher passes as `--payload-root`.
    pub payload_root: String,
    /// Raw TOML text of the package manifest.
    pub manifest_text: String,
    pub size_bytes: u64,
    pub last_used_unix: u64,
    /// False for a read-only system package, which may be launched but not
    /// pinned or removed here.
    pub user_managed: bool,
    pub manifest_path: String,
}

/// What a catalogue read gives: its rows, or why the host refused it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Catalogue {
    Rows(String),
    Refused { refusal: Refusal, detail: String },
}

/// The host's answer to a request that succeeded.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Answer {
    Done,
    Packages(Vec<PackageEntry>),
    Text(String),
    FlatpakStatus {
        available: bool,
        installed: String,
        remotes: String,
    },
    Catalogue(Catalogue),
    /// A [`crate::local_package::LocalReview`] as JSON: the manifest it
    /// carries skips absent fields, which postcard cannot read back.
    Review(String),
    Installed(WireId),
}

/// A host's side of the wire: one answer per request. Calls run
/// concurrently on one connection, and a call still running when its
/// connection ends is dropped.
pub trait Host: 'static {
    fn handle(
        self: Rc<Self>,
        request: Operation,
    ) -> impl Future<Output = Result<Answer, String>> + 'static;
}

type Next<R> = (R, Result<Option<Call>, wire_frame::FrameError>);

async fn next_call<R: AsyncRead + Unpin>(mut reader: R) -> Next<R> {
    let call = FRAMING.read_value_async(&mut reader).await;
    (reader, call)
}

/// Serve `host` on one connection until the peer hangs up. Must run on a
/// `LocalSet`. A host panic resumes here, ending the connection.
///
/// # Errors
///
/// A malformed frame or a failed write.
#[allow(
    clippy::future_not_send,
    reason = "calls run on the connection's LocalSet and never cross a thread"
)]
pub async fn serve<H, R, W>(
    host: Rc<H>,
    reader: R,
    mut writer: W,
) -> Result<(), wire_frame::FrameError>
where
    H: Host,
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;
    let mut calls = tokio::task::JoinSet::new();
    let mut next = Box::pin(next_call(reader));
    loop {
        tokio::select! {
            (reader, call) = &mut next => {
                let Some(Call { id, request }) = call? else {
                    return Ok(());
                };
                let answer = Rc::clone(&host).handle(request);
                calls.spawn_local(async move { (id, answer.await) });
                next = Box::pin(next_call(reader));
            }
            Some(done) = calls.join_next() => {
                let (id, answer) = match done {
                    Ok(done) => done,
                    Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                    Err(_) => continue,
                };
                FRAMING.write_value_async(&mut writer, &Reply { id, answer }).await?;
                writer.flush().await?;
            }
        }
    }
}
