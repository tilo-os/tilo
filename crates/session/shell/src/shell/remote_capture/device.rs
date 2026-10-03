//! Typed authority and bounded wire model for using one certified personal
//! device as another device's camera, microphone, or document scanner.
//!
//! This module deliberately contains no `PipeWire`, V4L2, portal, or transport
//! handles. Those are local adapters. The personal lane carries only a
//! negotiated semantic format plus bounded media chunks, and the registry
//! binds every transition to the exact peer, app, workspace, kind, and random
//! session identity approved by the receiving shell.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};

use crate::shell::personal_continuity::DeviceId;

pub const REMOTE_CAPTURE_PROTOCOL_VERSION: u16 = 1;
pub const MAX_REMOTE_CAPTURE_SESSIONS: usize = 8;
pub const MAX_REMOTE_CAPTURE_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_REMOTE_CAPTURE_LABEL_BYTES: usize = 256;
pub const MAX_REMOTE_CAPTURE_APP_ID_BYTES: usize = 256;
pub const MAX_REMOTE_CAPTURE_WORKSPACE_BYTES: usize = 512;
pub const MAX_REMOTE_CAPTURE_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_REMOTE_CAPTURE_PAGES: u32 = 1_024;
pub const REMOTE_CAPTURE_GRANT_TTL: Duration = Duration::from_secs(5 * 60);

/// When a capture grant issued or refreshed at `now` lapses. An expiry the
/// clock cannot represent fails closed and lapses at `now` rather than never.
pub(crate) fn grant_expiry(now: Instant) -> Instant {
    now.checked_add(REMOTE_CAPTURE_GRANT_TTL).unwrap_or(now)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteCaptureKind {
    Camera,
    Microphone,
    DocumentScanner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteVideoFormat {
    Nv12,
    Rgba8,
    Mjpeg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteAudioFormat {
    PcmS16Le,
    PcmF32Le,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteDocumentFormat {
    Jpeg,
    Png,
    Pdf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RemoteCaptureProfile {
    Camera {
        format: RemoteVideoFormat,
        width: u16,
        height: u16,
        frames_per_second: u8,
    },
    Microphone {
        format: RemoteAudioFormat,
        sample_rate: u32,
        channels: u8,
        frame_millis: u8,
    },
    DocumentScanner {
        format: RemoteDocumentFormat,
        max_pages: u32,
        max_document_bytes: u64,
    },
}

impl RemoteCaptureProfile {
    pub fn kind(&self) -> RemoteCaptureKind {
        match self {
            Self::Camera { .. } => RemoteCaptureKind::Camera,
            Self::Microphone { .. } => RemoteCaptureKind::Microphone,
            Self::DocumentScanner { .. } => RemoteCaptureKind::DocumentScanner,
        }
    }

    pub fn valid(&self) -> bool {
        match self {
            Self::Camera {
                width,
                height,
                frames_per_second,
                ..
            } => {
                (160..=4096).contains(width)
                    && (120..=2160).contains(height)
                    && (1..=60).contains(frames_per_second)
            }
            Self::Microphone {
                sample_rate,
                channels,
                frame_millis,
                ..
            } => {
                matches!(*sample_rate, 16_000 | 24_000 | 44_100 | 48_000)
                    && (1..=2).contains(channels)
                    && (5..=40).contains(frame_millis)
            }
            Self::DocumentScanner {
                max_pages,
                max_document_bytes,
                ..
            } => {
                (1..=MAX_REMOTE_CAPTURE_PAGES).contains(max_pages)
                    && (1..=MAX_REMOTE_CAPTURE_DOCUMENT_BYTES).contains(max_document_bytes)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCaptureRequest {
    pub version: u16,
    pub request_id: [u8; 16],
    pub app_id: String,
    pub workspace_id: String,
    pub app_label: String,
    pub profile: RemoteCaptureProfile,
}

impl RemoteCaptureRequest {
    pub fn valid(&self) -> bool {
        self.version == REMOTE_CAPTURE_PROTOCOL_VERSION
            && self.request_id != [0; 16]
            && !self.app_id.is_empty()
            && self.app_id.len() <= MAX_REMOTE_CAPTURE_APP_ID_BYTES
            && !self.workspace_id.is_empty()
            && self.workspace_id.len() <= MAX_REMOTE_CAPTURE_WORKSPACE_BYTES
            && !self.app_label.is_empty()
            && self.app_label.len() <= MAX_REMOTE_CAPTURE_LABEL_BYTES
            && self.profile.valid()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoteCaptureGrant([u8; 32]);

impl RemoteCaptureGrant {
    fn mint() -> Self {
        let mut bytes = [0; 32];
        while bytes == [0; 32] {
            UnwrapErr(SysRng).fill_bytes(&mut bytes);
        }
        Self(bytes)
    }

    pub fn valid(self) -> bool {
        self.0 != [0; 32]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RemoteCaptureSignal {
    Request(RemoteCaptureRequest),
    Decision {
        request_id: [u8; 16],
        accepted: bool,
        session_id: Option<[u8; 16]>,
        grant: Option<RemoteCaptureGrant>,
        profile: Option<RemoteCaptureProfile>,
    },
    Stop {
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
    },
}

impl RemoteCaptureSignal {
    pub fn valid(&self) -> bool {
        match self {
            Self::Request(request) => request.valid(),
            Self::Decision {
                request_id,
                accepted,
                session_id,
                grant,
                profile,
            } => {
                *request_id != [0; 16]
                    && if *accepted {
                        session_id.is_some_and(|id| id != [0; 16])
                            && grant.is_some_and(RemoteCaptureGrant::valid)
                            && profile.as_ref().is_some_and(RemoteCaptureProfile::valid)
                    } else {
                        session_id.is_none() && grant.is_none() && profile.is_none()
                    }
            }
            Self::Stop { session_id, grant } => *session_id != [0; 16] && grant.valid(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteCaptureChunk {
    pub session_id: [u8; 16],
    pub grant: RemoteCaptureGrant,
    pub sequence: u64,
    pub offset: u64,
    pub total_bytes: u64,
    pub page_index: Option<u32>,
    pub end_of_unit: bool,
    pub payload: Vec<u8>,
}

impl RemoteCaptureChunk {
    pub fn valid_for(&self, kind: RemoteCaptureKind) -> bool {
        self.session_id != [0; 16]
            && self.grant.valid()
            && !self.payload.is_empty()
            && self.payload.len() <= MAX_REMOTE_CAPTURE_CHUNK_BYTES
            && self.offset <= self.total_bytes
            && u64::try_from(self.payload.len()).is_ok_and(|len| {
                len.checked_add(self.offset)
                    .is_some_and(|end| end <= self.total_bytes)
            })
            && match kind {
                RemoteCaptureKind::Camera | RemoteCaptureKind::Microphone => {
                    self.page_index.is_none()
                        && self.offset == 0
                        && self.total_bytes <= MAX_REMOTE_CAPTURE_CHUNK_BYTES as u64
                }
                RemoteCaptureKind::DocumentScanner => {
                    self.page_index
                        .is_some_and(|page| page < MAX_REMOTE_CAPTURE_PAGES)
                        && self.total_bytes <= MAX_REMOTE_CAPTURE_DOCUMENT_BYTES
                }
            }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteCapturePhase {
    Granted,
    Streaming,
    Stopped,
}

#[derive(Clone, Debug)]
struct RemoteCaptureBinding {
    source: DeviceId,
    target: DeviceId,
    app_id: String,
    workspace_id: String,
    profile: RemoteCaptureProfile,
    grant: RemoteCaptureGrant,
    phase: RemoteCapturePhase,
    last_sequence: Option<u64>,
    expires_at: Instant,
}

#[derive(Default)]
pub struct RemoteCaptureRegistry {
    sessions: BTreeMap<[u8; 16], RemoteCaptureBinding>,
}

impl RemoteCaptureRegistry {
    /// Mint a fresh session and grant for a validated request from `source`
    /// to `target`.
    ///
    /// # Errors
    ///
    /// Returns a message when the request is invalid, names the same device
    /// twice, or the session limit is reached.
    pub fn grant(
        &mut self,
        request: &RemoteCaptureRequest,
        source: DeviceId,
        target: DeviceId,
        now: Instant,
    ) -> Result<([u8; 16], RemoteCaptureGrant), String> {
        self.expire(now);
        if !request.valid() || source == target {
            return Err("invalid remote capture request".into());
        }
        if self.sessions.len() >= MAX_REMOTE_CAPTURE_SESSIONS {
            return Err("too many active remote capture sessions".into());
        }
        let mut session_id = [0; 16];
        while session_id == [0; 16] || self.sessions.contains_key(&session_id) {
            UnwrapErr(SysRng).fill_bytes(&mut session_id);
        }
        let grant = RemoteCaptureGrant::mint();
        self.sessions.insert(
            session_id,
            RemoteCaptureBinding {
                source,
                target,
                app_id: request.app_id.clone(),
                workspace_id: request.workspace_id.clone(),
                profile: request.profile.clone(),
                grant,
                phase: RemoteCapturePhase::Granted,
                last_sequence: None,
                expires_at: grant_expiry(now),
            },
        );
        Ok((session_id, grant))
    }

    /// Install a grant minted by the certified source after matching it to an
    /// outstanding target request. The caller owns that request correlation;
    /// this registry owns the exact data-plane binding from this point on.
    pub fn accept_grant(
        &mut self,
        request: &RemoteCaptureRequest,
        source: DeviceId,
        target: DeviceId,
        session_id: [u8; 16],
        grant: RemoteCaptureGrant,
        profile: RemoteCaptureProfile,
        now: Instant,
    ) -> bool {
        self.expire(now);
        if !request.valid()
            || source == target
            || session_id == [0; 16]
            || !grant.valid()
            || profile != request.profile
            || self.sessions.contains_key(&session_id)
            || self.sessions.len() >= MAX_REMOTE_CAPTURE_SESSIONS
        {
            return false;
        }
        self.sessions.insert(
            session_id,
            RemoteCaptureBinding {
                source,
                target,
                app_id: request.app_id.clone(),
                workspace_id: request.workspace_id.clone(),
                profile,
                grant,
                phase: RemoteCapturePhase::Granted,
                last_sequence: None,
                expires_at: grant_expiry(now),
            },
        );
        true
    }

    pub fn accept_chunk(
        &mut self,
        source: &DeviceId,
        target: &DeviceId,
        app_id: &str,
        workspace_id: &str,
        chunk: &RemoteCaptureChunk,
        now: Instant,
    ) -> bool {
        self.expire(now);
        let Some(binding) = self.sessions.get_mut(&chunk.session_id) else {
            return false;
        };
        if &binding.source != source
            || &binding.target != target
            || binding.app_id != app_id
            || binding.workspace_id != workspace_id
            || binding.grant != chunk.grant
            || binding.phase == RemoteCapturePhase::Stopped
            || !chunk.valid_for(binding.profile.kind())
            || binding
                .last_sequence
                .is_some_and(|last| chunk.sequence <= last)
        {
            return false;
        }
        binding.last_sequence = Some(chunk.sequence);
        binding.phase = RemoteCapturePhase::Streaming;
        binding.expires_at = grant_expiry(now);
        true
    }

    pub fn revoke(&mut self, session_id: [u8; 16], grant: RemoteCaptureGrant) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|binding| binding.grant == grant)
            && self.sessions.remove(&session_id).is_some()
    }

    pub fn revoke_peer(&mut self, peer: &DeviceId) -> usize {
        let before = self.sessions.len();
        self.sessions
            .retain(|_, binding| &binding.source != peer && &binding.target != peer);
        before.saturating_sub(self.sessions.len())
    }

    pub fn revoke_app(&mut self, app_id: &str, workspace_id: &str) -> usize {
        let before = self.sessions.len();
        self.sessions
            .retain(|_, binding| binding.app_id != app_id || binding.workspace_id != workspace_id);
        before.saturating_sub(self.sessions.len())
    }

    pub fn revoke_all(&mut self) -> usize {
        let count = self.sessions.len();
        self.sessions.clear();
        count
    }

    pub fn phase(&self, session_id: [u8; 16]) -> Option<RemoteCapturePhase> {
        self.sessions.get(&session_id).map(|binding| binding.phase)
    }

    fn expire(&mut self, now: Instant) {
        self.sessions.retain(|_, binding| binding.expires_at > now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn capture_device_grants_are_exactly_bound_bounded_and_expiring() {
        // Phase `grant_expiry_adds_the_ttl`.
        {
            let now = Instant::now();
            assert_eq!(
                Some(grant_expiry(now)),
                now.checked_add(REMOTE_CAPTURE_GRANT_TTL)
            );
            assert!(grant_expiry(now) > now);
        }

        // Phase `exact_binding_rejects_peer_app_workspace_and_grant_spoofing`.
        {
            let now = Instant::now();
            let source = device("phone");
            let target = device("laptop");
            let mut registry = RemoteCaptureRegistry::default();
            let (session_id, grant) = registry
                .grant(
                    &request(RemoteCaptureKind::Camera),
                    source.clone(),
                    target.clone(),
                    now,
                )
                .unwrap();
            let chunk = RemoteCaptureChunk {
                session_id,
                grant,
                sequence: 1,
                offset: 0,
                total_bytes: 4,
                page_index: None,
                end_of_unit: true,
                payload: vec![1, 2, 3, 4],
            };
            assert!(!registry.accept_chunk(
                &device("other"),
                &target,
                "so.tilo.Camera",
                "room-a",
                &chunk,
                now
            ));
            assert!(!registry.accept_chunk(
                &source,
                &target,
                "so.tilo.Other",
                "room-a",
                &chunk,
                now
            ));
            assert!(!registry.accept_chunk(
                &source,
                &target,
                "so.tilo.Camera",
                "room-b",
                &chunk,
                now
            ));
            let mut forged = chunk.clone();
            forged.grant = RemoteCaptureGrant([9; 32]);
            assert!(!registry.accept_chunk(
                &source,
                &target,
                "so.tilo.Camera",
                "room-a",
                &forged,
                now
            ));
            assert!(registry.accept_chunk(
                &source,
                &target,
                "so.tilo.Camera",
                "room-a",
                &chunk,
                now
            ));
            assert_eq!(
                registry.phase(session_id),
                Some(RemoteCapturePhase::Streaming)
            );
        }

        // Phase `chunks_profiles_and_sequences_are_bounded`.
        {
            assert!(
                !RemoteCaptureProfile::Camera {
                    format: RemoteVideoFormat::Nv12,
                    width: 8192,
                    height: 1080,
                    frames_per_second: 30,
                }
                .valid()
            );
            let chunk = RemoteCaptureChunk {
                session_id: [1; 16],
                grant: RemoteCaptureGrant([2; 32]),
                sequence: 1,
                offset: 0,
                total_bytes: (MAX_REMOTE_CAPTURE_CHUNK_BYTES + 1) as u64,
                page_index: None,
                end_of_unit: true,
                payload: vec![0; MAX_REMOTE_CAPTURE_CHUNK_BYTES + 1],
            };
            assert!(!chunk.valid_for(RemoteCaptureKind::Camera));

            let now = Instant::now();
            let source = device("phone");
            let target = device("laptop");
            let mut registry = RemoteCaptureRegistry::default();
            let (session_id, grant) = registry
                .grant(
                    &request(RemoteCaptureKind::Microphone),
                    source.clone(),
                    target.clone(),
                    now,
                )
                .unwrap();
            let audio = RemoteCaptureChunk {
                session_id,
                grant,
                sequence: 7,
                offset: 0,
                total_bytes: 2,
                page_index: None,
                end_of_unit: true,
                payload: vec![0; 2],
            };
            assert!(registry.accept_chunk(
                &source,
                &target,
                "so.tilo.Camera",
                "room-a",
                &audio,
                now
            ));
            assert!(!registry.accept_chunk(
                &source,
                &target,
                "so.tilo.Camera",
                "room-a",
                &audio,
                now
            ));
        }

        // Phase `peer_app_policy_and_expiry_teardown_are_authoritative`.
        {
            let now = Instant::now();
            let source = device("phone");
            let target = device("laptop");
            let mut registry = RemoteCaptureRegistry::default();
            registry
                .grant(
                    &request(RemoteCaptureKind::DocumentScanner),
                    source.clone(),
                    target.clone(),
                    now,
                )
                .unwrap();
            assert_eq!(registry.revoke_peer(&source), 1);
            registry
                .grant(
                    &request(RemoteCaptureKind::Camera),
                    source.clone(),
                    target.clone(),
                    now,
                )
                .unwrap();
            assert_eq!(registry.revoke_app("so.tilo.Camera", "room-a"), 1);
            let (session_id, _) = registry
                .grant(&request(RemoteCaptureKind::Microphone), source, target, now)
                .unwrap();
            registry.expire(now + REMOTE_CAPTURE_GRANT_TTL + Duration::from_millis(1));
            assert_eq!(registry.phase(session_id), None);
            assert_eq!(registry.revoke_all(), 0);
        }
    }

    fn device(id: &str) -> DeviceId {
        DeviceId::new(id).expect("valid device")
    }

    fn request(kind: RemoteCaptureKind) -> RemoteCaptureRequest {
        let profile = match kind {
            RemoteCaptureKind::Camera => RemoteCaptureProfile::Camera {
                format: RemoteVideoFormat::Nv12,
                width: 1920,
                height: 1080,
                frames_per_second: 30,
            },
            RemoteCaptureKind::Microphone => RemoteCaptureProfile::Microphone {
                format: RemoteAudioFormat::PcmS16Le,
                sample_rate: 48_000,
                channels: 1,
                frame_millis: 20,
            },
            RemoteCaptureKind::DocumentScanner => RemoteCaptureProfile::DocumentScanner {
                format: RemoteDocumentFormat::Pdf,
                max_pages: 20,
                max_document_bytes: 8 * 1024 * 1024,
            },
        };
        RemoteCaptureRequest {
            version: REMOTE_CAPTURE_PROTOCOL_VERSION,
            request_id: [3; 16],
            app_id: "so.tilo.Camera".into(),
            workspace_id: "room-a".into(),
            app_label: "Camera".into(),
            profile,
        }
    }
}
