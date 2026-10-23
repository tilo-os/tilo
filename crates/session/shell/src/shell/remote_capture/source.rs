//! Production source-side capture for certified-device camera, microphone,
//! and scanner continuity.
//!
//! Hardware discovery and subprocess handles remain compositor-private. The
//! runtime receives only bounded typed chunks for an already consented exact
//! session; when its queue is full, live media drops instead of accumulating
//! latency or memory.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek as _, SeekFrom, Write as _};
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd as _;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use crate::shell::remote_capture::adapter::RemoteCaptureAvailability;
use crate::shell::remote_capture::device::{
    MAX_REMOTE_CAPTURE_CHUNK_BYTES, MAX_REMOTE_CAPTURE_SESSIONS, RemoteAudioFormat,
    RemoteCaptureKind, RemoteCaptureProfile, RemoteVideoFormat,
};
use crate::shell::remote_capture::host::{CaptureHost, Probe};

const MAX_DRAIN_PER_PUMP: usize = 32;

#[derive(Debug)]
pub(crate) struct CapturedRemoteChunk {
    pub session_id: [u8; 16],
    pub offset: u64,
    pub total_bytes: u64,
    pub page_index: Option<u32>,
    pub end_of_unit: bool,
    pub payload: Vec<u8>,
}

pub(crate) trait RemoteCaptureSource: Send {
    fn availability(&self) -> RemoteCaptureAvailability;
    fn start(&mut self, session_id: [u8; 16], profile: &RemoteCaptureProfile)
    -> Result<(), String>;
    fn drain(&mut self) -> Vec<CapturedRemoteChunk>;
    fn drain_failures(&mut self) -> Vec<[u8; 16]>;
    fn stop(&mut self, session_id: [u8; 16]);
    fn stop_all(&mut self);
}

struct SourceProcess {
    child: Arc<Mutex<Child>>,
    stop: Arc<AtomicBool>,
    reader: std::thread::JoinHandle<()>,
}

pub(crate) struct ProductionRemoteCaptureSource {
    host: CaptureHost,
    sessions: BTreeMap<[u8; 16], SourceProcess>,
    tx: mpsc::SyncSender<CapturedRemoteChunk>,
    rx: mpsc::Receiver<CapturedRemoteChunk>,
    failure_tx: mpsc::SyncSender<[u8; 16]>,
    failure_rx: mpsc::Receiver<[u8; 16]>,
}

impl ProductionRemoteCaptureSource {
    pub(crate) fn new(host: CaptureHost) -> Self {
        let (tx, rx) = mpsc::sync_channel(MAX_REMOTE_CAPTURE_SESSIONS * 4);
        let (failure_tx, failure_rx) = mpsc::sync_channel(MAX_REMOTE_CAPTURE_SESSIONS);
        Self {
            host,
            sessions: BTreeMap::new(),
            tx,
            rx,
            failure_tx,
            failure_rx,
        }
    }

    pub(crate) fn availability(&self) -> RemoteCaptureAvailability {
        availability_of(&self.host.current())
    }

    pub(crate) fn start(
        &mut self,
        session_id: [u8; 16],
        profile: &RemoteCaptureProfile,
    ) -> Result<(), String> {
        if session_id == [0; 16]
            || self.sessions.contains_key(&session_id)
            || self.sessions.len() >= MAX_REMOTE_CAPTURE_SESSIONS
        {
            return Err("remote capture source capacity is exhausted".into());
        }
        if matches!(
            profile.kind(),
            RemoteCaptureKind::Camera | RemoteCaptureKind::Microphone
        ) && live_frame_bytes(profile).is_none()
        {
            return Err("the negotiated live frame exceeds the bounded fabric transport".into());
        }
        let mut command = source_command(profile, &self.host.current())?;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("start remote capture source: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "remote capture source has no media stream".to_string())?;
        let child = Arc::new(Mutex::new(child));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let tx = self.tx.clone();
        let failure_tx = self.failure_tx.clone();
        let worker_profile = profile.clone();
        let reader = std::thread::Builder::new()
            .name("tilo-remote-capture-source".into())
            .spawn(move || match worker_profile.kind() {
                RemoteCaptureKind::Camera | RemoteCaptureKind::Microphone => {
                    pump_live(stdout, session_id, &worker_profile, &worker_stop, &tx);
                }
                RemoteCaptureKind::DocumentScanner => {
                    pump_scanner(
                        stdout,
                        session_id,
                        &worker_profile,
                        &worker_stop,
                        &tx,
                        &failure_tx,
                    );
                }
            })
            .map_err(|error| {
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                }
                format!("start remote capture reader: {error}")
            })?;
        self.sessions.insert(
            session_id,
            SourceProcess {
                child,
                stop,
                reader,
            },
        );
        Ok(())
    }

    pub(crate) fn drain(&self) -> Vec<CapturedRemoteChunk> {
        self.rx.try_iter().take(MAX_DRAIN_PER_PUMP).collect()
    }

    pub(crate) fn drain_failures(&self) -> Vec<[u8; 16]> {
        self.failure_rx.try_iter().collect()
    }

    pub(crate) fn stop(&mut self, session_id: [u8; 16]) {
        if let Some(source) = self.sessions.remove(&session_id) {
            source.stop.store(true, Ordering::Release);
            if let Ok(mut child) = source.child.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
            let _ = source.reader.join();
        }
    }

    pub(crate) fn stop_all(&mut self) {
        let sessions = self.sessions.keys().copied().collect::<Vec<_>>();
        for session in sessions {
            self.stop(session);
        }
        while self.rx.try_recv().is_ok() {}
        while self.failure_rx.try_recv().is_ok() {}
    }
}

impl RemoteCaptureSource for ProductionRemoteCaptureSource {
    fn availability(&self) -> RemoteCaptureAvailability {
        self.availability()
    }

    fn start(
        &mut self,
        session_id: [u8; 16],
        profile: &RemoteCaptureProfile,
    ) -> Result<(), String> {
        self.start(session_id, profile)
    }

    fn drain(&mut self) -> Vec<CapturedRemoteChunk> {
        Self::drain(self)
    }

    fn drain_failures(&mut self) -> Vec<[u8; 16]> {
        Self::drain_failures(self)
    }

    fn stop(&mut self, session_id: [u8; 16]) {
        self.stop(session_id);
    }

    fn stop_all(&mut self) {
        self.stop_all();
    }
}

impl Drop for ProductionRemoteCaptureSource {
    fn drop(&mut self) {
        self.stop_all();
    }
}

fn availability_of(probe: &Probe) -> RemoteCaptureAvailability {
    RemoteCaptureAvailability {
        camera: probe.tool("gst-launch-1.0").is_some() && probe.physical_camera().is_some(),
        microphone: probe.tool("pw-record").is_some(),
        scanner: probe.tool("scanimage").is_some(),
    }
}

fn source_command(profile: &RemoteCaptureProfile, probe: &Probe) -> Result<Command, String> {
    let available = availability_of(probe);
    match profile {
        RemoteCaptureProfile::Camera {
            format: RemoteVideoFormat::Nv12,
            width,
            height,
            frames_per_second,
        } if available.camera => {
            let device = probe
                .physical_camera()
                .ok_or_else(|| "no local physical camera is available".to_string())?;
            // The camera through the OS media stack: raw NV12 frames of the
            // requested size and rate on stdout, nothing else (`-q`).
            let mut command = Command::new(
                probe
                    .tool("gst-launch-1.0")
                    .ok_or_else(|| "gst-launch-1.0 is unavailable".to_string())?,
            );
            command
                .arg("-q")
                .arg("v4l2src")
                .arg(format!("device={}", device.display()))
                .args(["!", "videoconvert", "!", "videoscale", "!", "videorate", "!"])
                .arg(format!(
                    "video/x-raw,format=NV12,width={width},height={height},framerate={frames_per_second}/1"
                ))
                .args(["!", "fdsink", "fd=1", "sync=false"]);
            Ok(command)
        }
        RemoteCaptureProfile::Microphone {
            format: RemoteAudioFormat::PcmS16Le,
            sample_rate,
            channels,
            ..
        } if available.microphone => {
            let mut command = Command::new(
                probe
                    .tool("pw-record")
                    .ok_or_else(|| "pw-record is unavailable".to_string())?,
            );
            command
                .arg("--format")
                .arg("s16")
                .arg("--rate")
                .arg(sample_rate.to_string())
                .arg("--channels")
                .arg(channels.to_string())
                .arg("-");
            Ok(command)
        }
        RemoteCaptureProfile::DocumentScanner { .. } if available.scanner => {
            let mut command = Command::new(
                probe
                    .tool("scanimage")
                    .ok_or_else(|| "scanimage is unavailable".to_string())?,
            );
            command.args(["--format=png"]);
            Ok(command)
        }
        _ => Err("the requested capture profile has no production source backend".into()),
    }
}

fn pump_live(
    mut source: impl Read,
    session_id: [u8; 16],
    profile: &RemoteCaptureProfile,
    stop: &AtomicBool,
    tx: &mpsc::SyncSender<CapturedRemoteChunk>,
) {
    let Some(frame_bytes) = live_frame_bytes(profile) else {
        return;
    };
    let mut payload = vec![0; frame_bytes];
    while !stop.load(Ordering::Acquire) && source.read_exact(&mut payload).is_ok() {
        let frame = CapturedRemoteChunk {
            session_id,
            offset: 0,
            total_bytes: u64::try_from(payload.len()).unwrap_or(u64::MAX),
            page_index: None,
            end_of_unit: true,
            payload: payload.clone(),
        };
        let _ = tx.try_send(frame);
    }
}

fn live_frame_bytes(profile: &RemoteCaptureProfile) -> Option<usize> {
    let bytes = match profile {
        RemoteCaptureProfile::Camera {
            format: RemoteVideoFormat::Nv12,
            width,
            height,
            ..
        } => usize::from(*width)
            .checked_mul(usize::from(*height))?
            .checked_mul(3)?
            .checked_div(2)?,
        RemoteCaptureProfile::Microphone {
            format: RemoteAudioFormat::PcmS16Le,
            sample_rate,
            channels,
            frame_millis,
        } => usize::try_from(*sample_rate)
            .ok()?
            .checked_mul(usize::from(*channels))?
            .checked_mul(2)?
            .checked_mul(usize::from(*frame_millis))?
            .checked_div(1_000)?,
        _ => return None,
    };
    (bytes > 0 && bytes <= MAX_REMOTE_CAPTURE_CHUNK_BYTES).then_some(bytes)
}

fn pump_scanner(
    mut source: impl Read,
    session_id: [u8; 16],
    profile: &RemoteCaptureProfile,
    stop: &AtomicBool,
    tx: &mpsc::SyncSender<CapturedRemoteChunk>,
    failures: &mpsc::SyncSender<[u8; 16]>,
) {
    let RemoteCaptureProfile::DocumentScanner {
        max_document_bytes, ..
    } = profile
    else {
        let _ = failures.try_send(session_id);
        return;
    };
    let Ok(mut spool) = unnamed_spool() else {
        let _ = failures.try_send(session_id);
        return;
    };
    let mut total = 0_u64;
    let mut buffer = vec![0; MAX_REMOTE_CAPTURE_CHUNK_BYTES];
    while !stop.load(Ordering::Acquire) {
        let Ok(read) = source.read(&mut buffer) else {
            let _ = failures.try_send(session_id);
            return;
        };
        if read == 0 {
            break;
        }
        total = match total.checked_add(u64::try_from(read).unwrap_or(u64::MAX)) {
            Some(total) if total <= *max_document_bytes => total,
            _ => {
                let _ = failures.try_send(session_id);
                return;
            }
        };
        #[expect(
            clippy::indexing_slicing,
            reason = "Read::read never reports more bytes than the buffer it was given"
        )]
        let chunk = &buffer[..read];
        if spool.write_all(chunk).is_err() {
            let _ = failures.try_send(session_id);
            return;
        }
    }
    if total == 0 || spool.seek(SeekFrom::Start(0)).is_err() {
        let _ = failures.try_send(session_id);
        return;
    }
    let mut offset = 0_u64;
    while offset < total && !stop.load(Ordering::Acquire) {
        let Ok(read) = spool.read(&mut buffer) else {
            let _ = failures.try_send(session_id);
            return;
        };
        if read == 0 {
            let _ = failures.try_send(session_id);
            return;
        }
        let next = offset.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        #[expect(
            clippy::indexing_slicing,
            reason = "Read::read never reports more bytes than the buffer it was given"
        )]
        let payload = buffer[..read].to_vec();
        let frame = CapturedRemoteChunk {
            session_id,
            offset,
            total_bytes: total,
            page_index: Some(0),
            end_of_unit: next == total,
            payload,
        };
        if tx.try_send(frame).is_err() {
            let _ = failures.try_send(session_id);
            return;
        }
        offset = next;
    }
}

#[cfg(target_os = "linux")]
fn unnamed_spool() -> std::io::Result<File> {
    let runtime = format!("/run/user/{}\0", rustix::process::geteuid().as_raw());
    let runtime = std::ffi::CStr::from_bytes_with_nul(runtime.as_bytes()).ok();
    for directory in runtime.into_iter().chain([c"/var/tmp"]) {
        // SAFETY: both directory paths are NUL terminated. O_TMPFILE returns
        // a fresh unnamed descriptor, transferred exactly once below.
        let fd = unsafe {
            libc::open(
                directory.as_ptr(),
                libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd >= 0 {
            // SAFETY: ownership of the fresh descriptor transfers once.
            return Ok(unsafe { File::from_raw_fd(fd) });
        }
    }
    // Filesystems used by containers do not universally support O_TMPFILE.
    // memfd is still anonymous, compositor-private, and bounded by the
    // negotiated scanner profile.
    // SAFETY: the static name is NUL terminated and a successful call returns
    // a fresh descriptor.
    let fd = unsafe { libc::memfd_create(c"tilo-remote-scan".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: ownership of the fresh descriptor transfers once.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_sources_bound_chunks_spool_consistently_and_report_saturation() {
        {
            let camera = RemoteCaptureProfile::Camera {
                format: RemoteVideoFormat::Nv12,
                width: 160,
                height: 120,
                frames_per_second: 15,
            };
            let microphone = RemoteCaptureProfile::Microphone {
                format: RemoteAudioFormat::PcmS16Le,
                sample_rate: 48_000,
                channels: 2,
                frame_millis: 20,
            };
            assert_eq!(live_frame_bytes(&camera), Some(28_800));
            assert_eq!(live_frame_bytes(&microphone), Some(3_840));
            assert!(
                live_frame_bytes(&RemoteCaptureProfile::Camera {
                    format: RemoteVideoFormat::Nv12,
                    width: 4096,
                    height: 2160,
                    frames_per_second: 30,
                })
                .is_none()
            );
        }

        {
            let profile = RemoteCaptureProfile::DocumentScanner {
                format: crate::shell::remote_capture::device::RemoteDocumentFormat::Png,
                max_pages: 1,
                max_document_bytes: 200_000,
            };
            let bytes = vec![7_u8; 130_000];
            let (tx, rx) = mpsc::sync_channel(4);
            let (failure_tx, failure_rx) = mpsc::sync_channel(1);
            pump_scanner(
                bytes.as_slice(),
                [3; 16],
                &profile,
                &AtomicBool::new(false),
                &tx,
                &failure_tx,
            );
            assert!(failure_rx.try_recv().is_err());
            let chunks = rx.try_iter().collect::<Vec<_>>();
            assert_eq!(chunks.len(), 2);
            assert_eq!(chunks[0].offset, 0);
            assert_eq!(chunks[1].offset, 65_536);
            assert!(chunks[1].end_of_unit);
            assert!(chunks.iter().all(|chunk| chunk.total_bytes == 130_000));
        }

        {
            let profile = RemoteCaptureProfile::DocumentScanner {
                format: crate::shell::remote_capture::device::RemoteDocumentFormat::Png,
                max_pages: 1,
                max_document_bytes: 200_000,
            };
            let bytes = vec![9_u8; 130_000];
            let (tx, _rx) = mpsc::sync_channel(1);
            let (failure_tx, failure_rx) = mpsc::sync_channel(1);
            pump_scanner(
                bytes.as_slice(),
                [4; 16],
                &profile,
                &AtomicBool::new(false),
                &tx,
                &failure_tx,
            );
            assert_eq!(failure_rx.try_recv(), Ok([4; 16]));
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn unnamed_spool() -> std::io::Result<File> {
    // tempfile returns an unlinked, owner-only descriptor and sets close-on-exec.
    tempfile::tempfile()
}
