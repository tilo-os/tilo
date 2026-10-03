//! Interactive environment sessions a relay runs for its members.
//!
//! A member's device reaches an environment on a relay over the relay's
//! compute ALPN ([`crate::transport::COMPUTE_ALPN`]). After the certified
//! handshake on the connection's first (control) stream, each further
//! bidirectional stream carries one thing, named by its first frame:
//!
//! | Opener | First frame | Then |
//! |---|---|---|
//! | member → relay | [`StreamOpen::Environment`] | [`ServerFrame`]s down, [`ClientFrame`]s up |
//! | member → relay | [`StreamOpen::LegacyWindows`] | [`WindowFrame`]s down, [`WindowInput`]s up |
//! | relay → worker | [`WorkerStreamOpen::Environment`] | as the member's stream, spliced |
//! | relay → worker | [`WorkerStreamOpen::Legacy`] | [`LegacyFrame`]s up, [`WindowInput`]s down |
//! | worker → relay | [`StreamOpen::LegacyLaunch`] | one [`LegacyLaunchOutcome`] |
//!
//! The relay never trusts a member-supplied identity: it stamps the
//! certified peer as [`MemberIdentity`] on everything it forwards to a
//! worker, and a worker only ever receives streams from the relay it dialled
//! itself.
//!
//! The environment travels as a [`EnvironmentRecordRef`] (which synced record)
//! plus the record's resolved [`EnvironmentSpec`] at the member's current
//! heads. A worker builds the guest from the spec alone; the spec can only
//! describe the member's own environment, because every path it grants is
//! resolved inside that member's own file area on the relay.
//!
//! Frames are postcard, length-prefixed with the relay frame codec, at most
//! [`MAX_ENVIRONMENT_FRAME_BYTES`] each.

use serde::{Deserialize, Serialize};

use crate::legacy_window::{InputEvent, MAX_INPUT_EVENTS};

/// Version carried by every stream-opening frame.
pub const ENVIRONMENT_PROTOCOL_VERSION: u16 = 1;
/// Largest encoded frame on any environment or legacy stream.
pub const MAX_ENVIRONMENT_FRAME_BYTES: usize = 256 * 1024;
/// Largest pty chunk one [`ClientFrame::Input`] or [`ServerFrame::Output`]
/// carries; larger writes are split.
pub const MAX_PTY_CHUNK_BYTES: usize = 32 * 1024;
/// Scrollback a session keeps for re-attachment, newest bytes kept.
pub const MAX_SCROLLBACK_BYTES: usize = 192 * 1024;
/// Tool images one environment may list.
pub const MAX_TOOLS: usize = 64;
/// Directory grants one environment may list.
pub const MAX_GRANTS: usize = 64;
/// Packages one tool reference may name.
pub const MAX_TOOL_PACKAGES: usize = 64;
/// Longest string (name, id, path, argument) anywhere in a request.
pub const MAX_STRING_BYTES: usize = 4096;
/// Arguments one legacy launch may carry.
pub const MAX_LEGACY_ARGS: usize = 256;
/// Longest refusal or diagnostic text.
pub const MAX_DETAIL_BYTES: usize = 1024;

/// Opaque id of one session on a relay; the Terminal re-attaches by it.
pub type SessionId = [u8; 16];
/// Opaque id of one legacy launch, unique on the relay that routed it.
pub type LaunchId = [u8; 16];

/// Which synced environment record a session was opened from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentRecordRef {
    /// Workspace the record lives in.
    pub workspace_id: u64,
    /// The record's id within that workspace.
    pub record_id: String,
}

/// Whether an environment's home outlives its sessions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum HomePolicy {
    /// One home per environment, kept between sessions.
    #[default]
    Persistent,
    /// A fresh home per session, deleted when it ends.
    Ephemeral,
}

/// One tool image an environment mounts over its runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolImage {
    /// The tool image's package id.
    pub id: String,
    /// What the member asked for, e.g. `["pandoc"]`.
    pub packages: Vec<String>,
}

/// A directory granted into the guest, at the same path it has for the
/// member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryGrant {
    /// Absolute path, as the member's device names it.
    pub path: String,
    /// Read-write rather than read-only.
    pub writable: bool,
}

/// The resolved contents of an environment record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSpec {
    /// Display name.
    pub name: String,
    /// The `env-runtime` image's package id.
    pub runtime: String,
    /// Tool images built for `runtime`, in the record's order (topmost
    /// last).
    pub tools: Vec<ToolImage>,
    /// Directories granted into the guest.
    pub grants: Vec<DirectoryGrant>,
    /// Whether the home is kept.
    pub home: HomePolicy,
}

/// A terminal size in character cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySize {
    /// Columns.
    pub cols: u16,
    /// Rows.
    pub rows: u16,
}

impl PtySize {
    /// Both dimensions are usable.
    #[must_use]
    pub const fn valid(self) -> bool {
        self.cols > 0 && self.rows > 0
    }
}

/// What a member asks of an environment stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionRequest {
    /// Start a new session from an environment record.
    Open {
        /// The record the session belongs to.
        record: EnvironmentRecordRef,
        /// The record's resolved contents.
        spec: EnvironmentSpec,
        /// Initial terminal size.
        size: PtySize,
        /// `TERM` for the guest; `xterm-256color` when absent.
        term: Option<String>,
    },
    /// Re-attach to a running session, replaying its scrollback.
    Attach {
        /// The session to re-attach to.
        session: SessionId,
        /// The re-attaching terminal's size.
        size: PtySize,
    },
}

impl SessionRequest {
    /// Every bound holds and every path is absolute and normal.
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Open {
                record,
                spec,
                size,
                term,
            } => {
                size.valid()
                    && bounded(&record.record_id)
                    && !record.record_id.is_empty()
                    && term.as_deref().is_none_or(|term| {
                        bounded(term) && term.bytes().all(|byte| byte.is_ascii_graphic())
                    })
                    && spec.valid()
            }
            Self::Attach { size, .. } => size.valid(),
        }
    }
}

impl EnvironmentSpec {
    /// Every bound holds and every grant is an absolute, normal path.
    #[must_use]
    pub fn valid(&self) -> bool {
        bounded(&self.name)
            && bounded(&self.runtime)
            && !self.runtime.is_empty()
            && self.tools.len() <= MAX_TOOLS
            && self.tools.iter().all(|tool| {
                bounded(&tool.id)
                    && !tool.id.is_empty()
                    && tool.packages.len() <= MAX_TOOL_PACKAGES
                    && tool.packages.iter().all(|package| bounded(package))
            })
            && self.grants.len() <= MAX_GRANTS
            && self
                .grants
                .iter()
                .all(|grant| normal_absolute_path(&grant.path))
    }
}

/// The first frame of a member-opened stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamOpen {
    /// An interactive environment session.
    Environment {
        /// [`ENVIRONMENT_PROTOCOL_VERSION`].
        version: u16,
        /// Workspace whose membership admits the member.
        workspace_id: u64,
        /// Open or re-attach.
        request: SessionRequest,
    },
    /// The windows of the member's legacy apps launched from relay
    /// environments in this workspace.
    LegacyWindows {
        /// [`ENVIRONMENT_PROTOCOL_VERSION`].
        version: u16,
        /// Workspace whose membership admits the member.
        workspace_id: u64,
    },
    /// Worker only: a guest in `session`, which this worker hosts, asked
    /// to launch a legacy app.
    LegacyLaunch {
        /// [`ENVIRONMENT_PROTOCOL_VERSION`].
        version: u16,
        /// Workspace the session runs in.
        workspace_id: u64,
        /// The session that asked.
        session: SessionId,
        /// What to run.
        request: LegacyLaunch,
    },
}

/// Member → worker, after the session opened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientFrame {
    /// Bytes typed into the terminal.
    Input(#[serde(with = "serde_bytes")] Vec<u8>),
    /// The terminal was resized.
    Resize(PtySize),
    /// Leave the session running; re-attach later by its id.
    Detach,
    /// End the session.
    Terminate,
}

/// Why a session request was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The device is not a member of the workspace.
    NotAMember,
    /// No worker in the workspace runs environments right now.
    NoWorker,
    /// No such session.
    UnknownSession,
    /// The session belongs to someone else.
    NotOwner,
    /// A per-member quota would be exceeded.
    Quota(QuotaKind),
    /// The request is malformed or out of bounds.
    InvalidRequest,
    /// The guest could not be started.
    LaunchFailed,
    /// The version is not one this relay speaks.
    UnsupportedVersion,
}

/// Which quota refused a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuotaKind {
    /// Concurrent sessions.
    Sessions,
    /// Disk used by the member's environments and files.
    Disk,
    /// Concurrent legacy VMs.
    LegacyVms,
}

/// Worker → member on an environment stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerFrame {
    /// The session is running and this stream is attached to it.
    Opened {
        /// The session's id, for re-attachment.
        session: SessionId,
        /// Whether this re-attached to an existing session.
        resumed: bool,
        /// The most recent output, at most [`MAX_SCROLLBACK_BYTES`].
        #[serde(with = "serde_bytes")]
        scrollback: Vec<u8>,
    },
    /// Guest output.
    Output(#[serde(with = "serde_bytes")] Vec<u8>),
    /// The guest ended; the session is gone.
    Exited {
        /// Exit code, when the guest reported one.
        code: Option<i32>,
    },
    /// Another terminal attached to this session; this stream is done.
    Superseded,
    /// The session was not opened.
    Refused {
        /// Why.
        reason: Refusal,
        /// Human-readable detail, at most [`MAX_DETAIL_BYTES`].
        detail: String,
    },
    /// A tool was built for this environment on the relay; the member's
    /// device applies the record's `add-tool` action with it.
    ToolBuilt {
        /// The environment the tool was built for.
        record: EnvironmentRecordRef,
        /// The built (or cached) tool image.
        tool: ToolImage,
    },
}

/// The certified member a relay forwards a request for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MemberIdentity {
    /// The member's user root.
    pub user: [u8; 32],
    /// The device the request came from.
    pub device: [u8; 32],
}

/// Relay → worker: the first frame of a relay-opened stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerStreamOpen {
    /// Host (or re-attach to) an environment session for `member`.
    Environment {
        /// [`ENVIRONMENT_PROTOCOL_VERSION`].
        version: u16,
        /// Workspace that admitted the member.
        workspace_id: u64,
        /// The certified member.
        member: MemberIdentity,
        /// Open or re-attach. For an open, the relay already chose the
        /// session id, so routing and hosting agree on it.
        request: SessionRequest,
        /// The session id (new for an open, existing for an attach).
        session: SessionId,
    },
    /// Run a legacy app for `member` on this KVM worker.
    Legacy {
        /// [`ENVIRONMENT_PROTOCOL_VERSION`].
        version: u16,
        /// Workspace that admitted the member.
        workspace_id: u64,
        /// The member the app runs for.
        member: MemberIdentity,
        /// This launch's id.
        launch: LaunchId,
        /// What to run.
        request: LegacyLaunch,
    },
}

/// Which guest OS a legacy app needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LegacyGuestOs {
    /// The Linux legacy VM.
    Linux,
    /// The Windows legacy VM.
    Windows,
}

/// A legacy app launch, as the environment's broker asked for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyLaunch {
    /// The guest OS.
    pub guest: LegacyGuestOs,
    /// The app (shim name or installed id).
    pub app: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Working directory, as the environment names it.
    pub cwd: String,
}

impl LegacyLaunch {
    /// Every bound holds.
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.app.is_empty()
            && bounded(&self.app)
            && self.args.len() <= MAX_LEGACY_ARGS
            && self.args.iter().all(|argument| bounded(argument))
            && normal_absolute_path(&self.cwd)
    }
}

/// Relay → worker: the answer to a [`StreamOpen::LegacyLaunch`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyLaunchOutcome {
    /// Routed to a KVM worker; windows reach the member as a window stream.
    Launched {
        /// The launch's id.
        launch: LaunchId,
    },
    /// Not launched.
    Refused {
        /// Why.
        reason: Refusal,
        /// Human-readable detail.
        detail: String,
    },
}

/// KVM worker → relay on a legacy stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LegacyFrame {
    /// The app is running.
    Started,
    /// One encoded [`crate::legacy_window::Frame`] of one of its windows.
    Window(#[serde(with = "serde_bytes")] Vec<u8>),
    /// Metered usage since the previous report.
    Usage(LegacyUsage),
    /// The app ended.
    Exited {
        /// Exit code, when the guest reported one.
        code: Option<i32>,
    },
    /// The app could not be started.
    Refused {
        /// Why.
        reason: Refusal,
        /// Human-readable detail.
        detail: String,
    },
}

/// Legacy VM resources one member consumed, for metering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyUsage {
    /// Wall time the member's VM was running, milliseconds.
    pub vm_running_ms: u64,
    /// Peak guest memory, bytes.
    pub peak_memory_bytes: u64,
}

/// Relay → member on a legacy-windows stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WindowFrame {
    /// A launch from one of the member's relay environments started.
    Launched {
        /// The launch.
        launch: LaunchId,
        /// The session it was launched from.
        session: SessionId,
        /// What runs.
        app: String,
    },
    /// One encoded [`crate::legacy_window::Frame`] of the launch's windows.
    Window {
        /// The launch.
        launch: LaunchId,
        /// The encoded frame.
        #[serde(with = "serde_bytes")]
        frame: Vec<u8>,
    },
    /// The launch ended.
    Exited {
        /// The launch.
        launch: LaunchId,
        /// Exit code, when reported.
        code: Option<i32>,
    },
    /// The stream is registered: every launch from now on reaches it. The
    /// first frame of every legacy-windows stream.
    Watching,
}

/// Member → relay (→ KVM worker): input for one legacy window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowInput {
    /// The launch whose window it is.
    pub launch: LaunchId,
    /// The window's surface within that launch.
    pub surface: u64,
    /// The events, at most [`MAX_INPUT_EVENTS`].
    pub events: Vec<InputEvent>,
}

impl WindowInput {
    /// Within bounds.
    #[must_use]
    pub fn valid(&self) -> bool {
        !self.events.is_empty() && self.events.len() <= MAX_INPUT_EVENTS
    }
}

fn bounded(text: &str) -> bool {
    text.len() <= MAX_STRING_BYTES && !text.contains('\0')
}

/// An absolute path with no `.`/`..`/empty components and no NUL.
#[must_use]
pub fn normal_absolute_path(path: &str) -> bool {
    bounded(path)
        && path.starts_with('/')
        && path
            .split('/')
            .skip(1)
            .all(|component| path == "/" || !matches!(component, "" | "." | ".."))
}

/// Truncate `detail` to [`MAX_DETAIL_BYTES`] on a character boundary.
#[must_use]
pub fn bounded_detail(detail: impl Into<String>) -> String {
    let mut detail = detail.into();
    if detail.len() > MAX_DETAIL_BYTES {
        let mut end = MAX_DETAIL_BYTES;
        while !detail.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        detail.truncate(end);
    }
    detail
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> EnvironmentSpec {
        EnvironmentSpec {
            name: "dev".into(),
            runtime: "so.tilo.env.fedora".into(),
            tools: vec![ToolImage {
                id: "so.tilo.tool.pandoc".into(),
                packages: vec!["pandoc".into()],
            }],
            grants: vec![DirectoryGrant {
                path: "/home/alice/Projects".into(),
                writable: true,
            }],
            home: HomePolicy::Persistent,
        }
    }

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let bytes = postcard::to_allocvec(value).unwrap();
        assert!(bytes.len() <= MAX_ENVIRONMENT_FRAME_BYTES);
        postcard::from_bytes(&bytes).unwrap()
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one table of every frame kind, read top to bottom"
    )]
    fn every_stream_frame_round_trips() {
        let open = StreamOpen::Environment {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id: 7,
            request: SessionRequest::Open {
                record: EnvironmentRecordRef {
                    workspace_id: 7,
                    record_id: "env-1".into(),
                },
                spec: spec(),
                size: PtySize { cols: 80, rows: 24 },
                term: Some("xterm-256color".into()),
            },
        };
        assert_eq!(round_trip(&open), open);
        let attach = StreamOpen::Environment {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id: 7,
            request: SessionRequest::Attach {
                session: [3; 16],
                size: PtySize {
                    cols: 120,
                    rows: 40,
                },
            },
        };
        assert_eq!(round_trip(&attach), attach);
        for frame in [
            ClientFrame::Input(b"ls\r".to_vec()),
            ClientFrame::Resize(PtySize { cols: 1, rows: 1 }),
            ClientFrame::Detach,
            ClientFrame::Terminate,
        ] {
            assert_eq!(round_trip(&frame), frame);
        }
        for frame in [
            ServerFrame::Opened {
                session: [1; 16],
                resumed: true,
                scrollback: vec![b'x'; MAX_SCROLLBACK_BYTES],
            },
            ServerFrame::Output(b"$ ".to_vec()),
            ServerFrame::Exited { code: Some(3) },
            ServerFrame::Superseded,
            ServerFrame::Refused {
                reason: Refusal::Quota(QuotaKind::Disk),
                detail: "full".into(),
            },
            ServerFrame::ToolBuilt {
                record: EnvironmentRecordRef {
                    workspace_id: 7,
                    record_id: "env-1".into(),
                },
                tool: spec().tools.remove(0),
            },
        ] {
            assert_eq!(round_trip(&frame), frame);
        }
        let member = MemberIdentity {
            user: [4; 32],
            device: [5; 32],
        };
        let launch = LegacyLaunch {
            guest: LegacyGuestOs::Linux,
            app: "code".into(),
            args: vec![".".into()],
            cwd: "/home/alice/Projects".into(),
        };
        let worker = WorkerStreamOpen::Legacy {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id: 7,
            member,
            launch: [6; 16],
            request: launch.clone(),
        };
        assert_eq!(round_trip(&worker), worker);
        let upstream = StreamOpen::LegacyLaunch {
            version: ENVIRONMENT_PROTOCOL_VERSION,
            workspace_id: 7,
            session: [1; 16],
            request: launch,
        };
        assert_eq!(round_trip(&upstream), upstream);
        for frame in [
            LegacyFrame::Started,
            LegacyFrame::Window(vec![0xF1, 2]),
            LegacyFrame::Usage(LegacyUsage {
                vm_running_ms: 10,
                peak_memory_bytes: 20,
            }),
            LegacyFrame::Exited { code: None },
        ] {
            assert_eq!(round_trip(&frame), frame);
        }
        let input = WindowInput {
            launch: [6; 16],
            surface: 9,
            events: vec![InputEvent::Key {
                keysym: 30,
                pressed: true,
                modifiers: 0,
            }],
        };
        assert_eq!(round_trip(&input), input);
        assert!(input.valid());
    }

    #[test]
    fn requests_out_of_bounds_are_invalid() {
        let valid = SessionRequest::Open {
            record: EnvironmentRecordRef {
                workspace_id: 1,
                record_id: "r".into(),
            },
            spec: spec(),
            size: PtySize { cols: 80, rows: 24 },
            term: None,
        };
        assert!(valid.valid());
        let mut cases = Vec::new();
        for grant in ["relative", "/a/../etc", "/a//b", "/a/./b", "/a/\0"] {
            let mut spec = spec();
            spec.grants[0].path = grant.into();
            cases.push(spec);
        }
        let mut no_runtime = spec();
        no_runtime.runtime.clear();
        cases.push(no_runtime);
        let mut many_grants = spec();
        many_grants.grants = vec![many_grants.grants[0].clone(); MAX_GRANTS + 1];
        cases.push(many_grants);
        for spec in cases {
            assert!(!spec.valid(), "{spec:?}");
        }
        let zero = SessionRequest::Attach {
            session: [0; 16],
            size: PtySize { cols: 0, rows: 24 },
        };
        assert!(!zero.valid());
        let bad_term = SessionRequest::Open {
            record: EnvironmentRecordRef {
                workspace_id: 1,
                record_id: "r".into(),
            },
            spec: spec(),
            size: PtySize { cols: 80, rows: 24 },
            term: Some("xterm\n".into()),
        };
        assert!(!bad_term.valid());
        assert!(normal_absolute_path("/"));
        assert!(
            !LegacyLaunch {
                guest: LegacyGuestOs::Windows,
                app: String::new(),
                args: Vec::new(),
                cwd: "/".into(),
            }
            .valid()
        );
        assert_eq!(
            bounded_detail("é".repeat(MAX_DETAIL_BYTES)).len(),
            MAX_DETAIL_BYTES
        );
    }
}
