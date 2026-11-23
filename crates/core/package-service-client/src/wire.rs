//! The package service's wire: postcard messages behind `wire_frame` length prefixes on the
//! launcher-issued connection. Every call carries an id the answer echoes, so several calls are in
//! flight on one connection: a slow catalogue read never holds up a pin or a list. A call may also
//! report progress before its answer, on the same connection, named by its id.
//! All `id` fields carry the raw 32-byte BLAKE3 package id; presentation (`tapp:<hex>`) is the
//! caller's.

use std::collections::VecDeque;
use std::future::Future;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::client::Refusal;

/// Both directions' framing. Large enough for the `.tapp` an install hands
/// over in one message.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 << 20);

pub type WireId = [u8; 32];

/// What the launcher writes first on the connection it hands the service: the
/// app whose launch asked for it. The connection is the launcher's own, so the
/// app id is the launcher's statement, and the service decides what that app
/// may call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub app_id: String,
}

/// Prefix that separates the launcher's handoff from a raw diagnostic client.
pub const HANDOFF_MAGIC: [u8; 4] = *b"QPS1";

/// After [`HANDOFF_MAGIC`]: a little-endian `u32` length, then a postcard [`Handoff`].
pub const HANDOFF_FRAMING: wire_frame::Framing =
    wire_frame::Framing::little_endian(1024).reject_empty();

/// Write the launcher's handoff, which the service reads before any call.
///
/// # Errors
///
/// A write error, or a frame that does not fit [`HANDOFF_FRAMING`].
pub fn write_handoff(stream: &mut impl std::io::Write, app_id: &str) -> std::io::Result<()> {
    let handoff = Handoff {
        app_id: app_id.to_owned(),
    };
    stream.write_all(&HANDOFF_MAGIC)?;
    HANDOFF_FRAMING
        .write_value(stream, &handoff)
        .map_err(std::io::Error::other)?;
    stream.flush()
}

/// Read the launcher's handoff.
///
/// # Errors
///
/// A read error, a frame that is not a handoff, or a peer that hung up
/// before sending one.
pub fn read_handoff(stream: &mut impl std::io::Read) -> std::io::Result<Handoff> {
    let mut magic = [0_u8; 4];
    std::io::Read::read_exact(stream, &mut magic)?;
    if magic != HANDOFF_MAGIC {
        return Err(std::io::Error::other("not a package service handoff"));
    }
    HANDOFF_FRAMING
        .read_value::<_, Handoff>(stream)
        .map_err(std::io::Error::other)?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the peer hung up before its handoff",
            )
        })
}

/// Read the launcher's handoff from an async stream, as the service does.
///
/// # Errors
///
/// As [`read_handoff`].
pub async fn read_handoff_async(stream: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Handoff> {
    let mut magic = [0_u8; 4];
    tokio::io::AsyncReadExt::read_exact(stream, &mut magic).await?;
    if magic != HANDOFF_MAGIC {
        return Err(std::io::Error::other("not a package service handoff"));
    }
    HANDOFF_FRAMING
        .read_value_async::<_, Handoff>(stream)
        .await
        .map_err(std::io::Error::other)?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the peer hung up before its handoff",
            )
        })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    pub request: Operation,
}

/// One message the service sends: a call's answer, or progress on a call that
/// is still in flight.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Frame {
    Reply(Reply),
    /// Where the call `id` stands; its [`Frame::Reply`] still follows.
    Event(Event),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub answer: Result<Answer, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// The call this progresses.
    pub id: u64,
    pub progress: Progress,
}

/// Where a call in flight stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Progress {
    /// A language pack: the download's phase and bytes so far.
    LanguagePack {
        phase: Phase,
        received: u64,
        total: Option<u64>,
    },
}

/// Where a language-pack install stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Choosing the repository and the build to fetch.
    Preparing,
    Downloading,
    Installing,
}

/// One of the person's languages a repository offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedLanguage {
    pub locale: String,
    pub installed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Operation {
    /// Read one of the service's resources; never changes anything.
    Read(Resource),
    /// Invoke one of the service's actions.
    Invoke(Action),
}

/// The resources the service answers reads of.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Resource {
    /// All currently known package entries.
    Packages,
    /// The app repositories this person configured, as `app repo list --json`
    /// reports them: the one list `app`, `tilo update` and the Store share.
    Repositories,
    /// Fixed read-only Flatpak probes; the confined Store never runs
    /// service tools itself.
    FlatpakStatus,
    /// Verified entries from one enabled service-configured Tilo repository.
    /// The service owns durable rollback checkpoints; callers supply no keys
    /// or URLs.
    RegistryCatalogue { registry: String },
    /// The catalogue the service last accepted from one enabled repository,
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
    /// The language packs the repositories offer, by locale, and whether each
    /// is installed. Settings only.
    LanguagePacks,
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

/// The actions the service performs.
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
    /// reported, reverified against current process trust.
    InstallLocal {
        tapp_bytes: Vec<u8>,
        reviewed_root: String,
        reviewed_archive_hash: String,
    },
    /// Launch a package this service resolved, through the service's launcher.
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
    /// Make the language pack of `locale` (`de-DE`) installed, answering once
    /// it is, and reporting progress as it goes. Asking again while one is
    /// under way follows the same install instead of starting a second.
    /// Settings only.
    InstallLanguagePack {
        locale: String,
    },
    /// Append `chunk` to this connection's upload: the bytes of a file a
    /// [`Change`] names next, sent ahead of it because the service cannot
    /// open the caller's files (it has a private `/tmp`, and other mounts).
    /// Each change that takes a file consumes the upload.
    Upload {
        chunk: Vec<u8>,
    },
    /// Change the package store the person runs, as `tilo-install` and `app`
    /// do. `store` is the store the caller means; the service refuses when it
    /// writes another, so a shell pointed elsewhere never writes this one by
    /// mistake. Only the person's terminal ([`PERSON`]) may call it.
    Change {
        store: String,
        change: Change,
    },
}

/// The handoff `app_id` of the person at their own terminal: `app` and `tilo-install`, outside any
/// app sandbox. An app never reaches the socket itself (the launcher hands it a connection and
/// writes its id), so a caller that names itself this is the account's own.
pub const PERSON: &str = "person";

/// How much of an upload one [`Action::Upload`] carries, well inside [`FRAMING`].
pub const UPLOAD_CHUNK: u64 = 16 << 20;

/// The largest file one change takes: a runtime package with its whole payload.
pub const MAX_UPLOAD_BYTES: u64 = 2 << 30;

/// A file the caller uploaded on this connection ([`Action::Upload`]), and the name it knows the
/// file by, which the store records as where the package came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uploaded {
    pub source: String,
}

/// The trust a change holds a package to, as `tilo-install`'s flags state it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trust {
    pub require_package_root: Option<String>,
    pub require_publisher: Option<String>,
    pub require_attestor: Option<String>,
    pub allow_self_signed: bool,
}

/// What an install does when a package of the same id is installed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Replacement {
    #[default]
    Refuse,
    Replace,
    /// Replace, but keep a version the person rolled back to.
    PreserveHeld,
}

/// What an uninstall does with the app's saved data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SavedData {
    Delete,
    Keep,
    /// Not decided: a dry run says the person would be asked; a real
    /// uninstall keeps it.
    Ask,
}

/// The changes the person makes to their package store. Each is one action, with the service's
/// permissions and its record of who changed what.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// Install the uploaded `.tapp`.
    Install {
        file: Uploaded,
        trust: Trust,
        pinned: bool,
        replacement: Replacement,
        ignore_missing_runtime: bool,
        dry_run: bool,
        /// Registry to fetch payload blobs the package does not carry from.
        payload_from: Option<String>,
    },
    /// Swap in a newer version of `app` (of every installed package without
    /// one), from the uploaded file or the configured registries.
    Update {
        app: Option<String>,
        file: Option<Uploaded>,
        /// A semver requirement for the registries' offer.
        version: Option<String>,
        allow_downgrade: bool,
        dry_run: bool,
        trust: Trust,
        ignore_missing_runtime: bool,
        payload_from: Option<String>,
    },
    /// Remove the person's copy of `app`: payload, registry row and PATH shims.
    Uninstall {
        app: String,
        saved_data: SavedData,
        dry_run: bool,
    },
    SetPinned {
        app: String,
        pinned: bool,
    },
    /// Record that the person opened `app` from their terminal: what was
    /// used last is evicted last.
    Opened {
        app: String,
    },
    /// Reclaim what nothing needs: apps past the size cap, orphaned trees,
    /// runtime builds no package runs on.
    Collect {
        dry_run: bool,
        grace_secs: u64,
        max_store_bytes: u64,
    },
    /// Add the uploaded environment image.
    AddImage {
        file: Uploaded,
        publisher: Option<String>,
        payload_from: Option<String>,
    },
    RemoveImage {
        id: WireId,
    },
    /// Make the tree of image `id` present, fetching it from `registry` when
    /// no store holds it; answers its path.
    FetchImage {
        id: WireId,
        registry: String,
    },
    /// Build a runtime bundle from `from` (the host root when `None`) and
    /// install it, signed with the uploaded identity when there is one.
    InstallRuntime {
        version: String,
        from: Option<String>,
        arch: Option<String>,
        identity: Option<Uploaded>,
        force: bool,
    },
    RemoveRuntime {
        version: String,
    },
    PinRuntime {
        version: String,
        pinned: bool,
    },
    AddAttestor {
        id: String,
        pubkey: String,
        scope: String,
    },
    RemoveAttestor {
        id: String,
    },
    Revoke {
        pubkey: String,
        reason: String,
    },
}

/// What a change did, in the words the terminal shows: `lines` on standard output, `warnings` on
/// standard error.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeReport {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
}

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

/// What a catalogue read gives: its rows, or why the service refused it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Catalogue {
    Rows(String),
    Refused { refusal: Refusal, detail: String },
}

/// The service's answer to a request that succeeded.
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
    /// A [`crate::LocalReview`] as JSON: the manifest it
    /// carries skips absent fields, which postcard cannot read back.
    Review(String),
    Installed(WireId),
    /// The language packs the repositories offer. Settings only.
    Languages(Vec<OfferedLanguage>),
    /// What an [`Action::Change`] did.
    Report(ChangeReport),
}

/// Where a call in flight reports progress, before its answer. The service
/// hands one to every call; a call with nothing to report sends no [`Event`].
#[derive(Clone)]
pub struct Sink {
    outgoing: Arc<Mutex<VecDeque<Frame>>>,
    queued: Arc<tokio::sync::Notify>,
    id: u64,
}

impl Sink {
    /// Report where the call stands. A connection that has ended drops it.
    pub fn report(&self, progress: Progress) {
        self.outgoing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(Frame::Event(Event {
                id: self.id,
                progress,
            }));
        self.queued.notify_one();
    }
}

/// A service's side of the wire: one answer per request, with any progress
/// reported through its [`Sink`] before it. Calls are answered concurrently on
/// one connection; one still in flight when its connection ends is dropped.
pub trait Server: 'static {
    fn handle(
        self: Rc<Self>,
        request: Operation,
        progress: Sink,
    ) -> impl Future<Output = Result<Answer, String>> + 'static;
}

type Next<R> = (R, Result<Option<Call>, wire_frame::FrameError>);

async fn next_call<R: AsyncRead + Unpin>(mut reader: R) -> Next<R> {
    let call = FRAMING.read_value_async(&mut reader).await;
    (reader, call)
}

/// Serve `service` on one connection until the peer hangs up. Must be driven on
/// a `LocalSet`. A service panic resumes here, ending the connection.
#[allow(
    clippy::future_not_send,
    reason = "calls are answered on the connection's LocalSet and never cross a thread"
)]
pub async fn serve<H, R, W>(
    server: Rc<H>,
    reader: R,
    mut writer: W,
) -> Result<(), wire_frame::FrameError>
where
    H: Server,
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;
    // Every message the connection sends is queued here, so a call's progress
    // and its answer never race for the writer.
    let outgoing = Arc::new(Mutex::new(VecDeque::<Frame>::new()));
    let queued = Arc::new(tokio::sync::Notify::new());
    let mut calls = tokio::task::JoinSet::new();
    let mut next = Box::pin(next_call(reader));
    loop {
        tokio::select! {
            (reader, call) = &mut next => {
                let Some(Call { id, request }) = call? else {
                    return Ok(());
                };
                let progress = Sink {
                    outgoing: Arc::clone(&outgoing),
                    queued: Arc::clone(&queued),
                    id,
                };
                let answer = Rc::clone(&server).handle(request, progress);
                let (outgoing, queued) = (Arc::clone(&outgoing), Arc::clone(&queued));
                calls.spawn_local(async move {
                    let answer = answer.await;
                    outgoing
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push_back(Frame::Reply(Reply { id, answer }));
                    queued.notify_one();
                });
                next = Box::pin(next_call(reader));
            }
            () = queued.notified() => {
                loop {
                    let frame = outgoing
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .pop_front();
                    let Some(frame) = frame else {
                        break;
                    };
                    FRAMING.write_value_async(&mut writer, &frame).await?;
                    writer.flush().await?;
                }
            }
            Some(done) = calls.join_next() => {
                if let Err(error) = done
                    && error.is_panic()
                {
                    std::panic::resume_unwind(error.into_panic());
                }
            }
        }
    }
}
