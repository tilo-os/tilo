//! Whole-desktop capture and H.264 handoff to `tilo-projector`.
//!
//! Capture is dormant until the Win+P controller starts a connection. Scenes
//! are rendered at a receiver-friendly size on the compositor thread, while
//! H.264 encoding (the codec projection receivers take: a hardware encoder
//! first, Cisco's openh264 through the system's `GStreamer` plugin otherwise)
//! and socket I/O happen on a one-frame bounded worker. If
//! encoding falls behind, the newest repaint replaces future work rather than
//! back-pressuring input and animation.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use tilo_projector::{CaptureProfile, DisplayStreamPolicy, VideoPacket};
use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

use crate::collab::encoder::{Encoder, EncoderInit, FrameInput, build};
use crate::config::EncoderChoice;

const VIDEO_SOCKET_ENV: &str = "TILO_PROJECTOR_VIDEO_SOCKET";

struct RawFrame {
    width: u32,
    height: u32,
    presentation_time_us: u64,
    rgba: Vec<u8>,
    fps: u32,
    bitrate_kbps: u32,
    reset_encoder: bool,
}

struct EncoderWorker {
    tx: SyncSender<RawFrame>,
    packets: Receiver<VideoPacket>,
}

impl EncoderWorker {
    /// Starts the encoder thread. Returns `None` — logging why — if the OS refuses to spawn
    /// it, in which case capture stays unavailable rather than panicking the compositor.
    fn spawn(socket_path: Option<PathBuf>) -> Option<Self> {
        let (tx, rx) = mpsc::sync_channel::<RawFrame>(1);
        let (packet_tx, packets) = mpsc::sync_channel::<VideoPacket>(1);
        let spawned = std::thread::Builder::new()
            .name("tilo-projection-encoder".to_owned())
            .spawn(move || {
                let mut stream: Option<UnixStream> = None;
                let mut encoder: Option<Box<dyn Encoder>> = None;
                let mut reported_unavailable = false;
                let mut encoder_profile = None;
                while let Ok(frame) = rx.recv() {
                    if frame.reset_encoder {
                        encoder = None;
                        encoder_profile = None;
                    }
                    if stream.is_none()
                        && let Some(socket_path) = socket_path.as_ref()
                        && let Ok(connected) = UnixStream::connect(socket_path)
                    {
                        stream = Some(connected);
                        // A new broker connection must begin with fresh SPS/PPS.
                        encoder = None;
                    }
                    let profile = (frame.width, frame.height, frame.fps, frame.bitrate_kbps);
                    if encoder_profile != Some(profile) {
                        encoder = match build(
                            EncoderInit::sdr_h264(
                                frame.width,
                                frame.height,
                                frame.fps,
                                frame.bitrate_kbps,
                                1,
                            ),
                            EncoderChoice::Auto,
                        ) {
                            Ok(built) => Some(built),
                            Err(error) => {
                                // A stock image without H.264 hardware encode
                                // and without Cisco's openh264 has no H.264
                                // encoder at all; say so once.
                                if !reported_unavailable {
                                    tracing::warn!(
                                        target: "shell::compositor::projection",
                                        %error,
                                        "no H.264 encoder for projection; install the additional codecs"
                                    );
                                    reported_unavailable = true;
                                }
                                None
                            }
                        };
                        encoder_profile = encoder.as_ref().map(|_| profile);
                    }
                    let Some(active_encoder) = encoder.as_mut() else {
                        continue;
                    };
                    let mut annex_b = Vec::new();
                    let Ok(stats) =
                        active_encoder.encode(FrameInput::Rgba(&frame.rgba), &mut annex_b)
                    else {
                        continue;
                    };
                    let packet = VideoPacket {
                        width: frame.width,
                        height: frame.height,
                        presentation_time_us: frame.presentation_time_us,
                        keyframe: stats.keyframe,
                        annex_b,
                    };
                    let _ = packet_tx.try_send(packet.clone());
                    // The branch above either just set `stream` to `Some` or `continue`d, so
                    // this is always populated; handled rather than asserted regardless.
                    if stream
                        .as_mut()
                        .is_some_and(|active_stream| packet.write_to(active_stream).is_err())
                    {
                        stream = None;
                        encoder = None;
                        encoder_profile = None;
                    }
                }
            });
        match spawned {
            Ok(_handle) => Some(Self { tx, packets }),
            Err(e) => {
                tracing::warn!(
                    target: "shell::compositor::projection",
                    error = %e,
                    "could not start projection encoder worker; capture stays unavailable"
                );
                None
            }
        }
    }

    fn take_packet(&self) -> Option<VideoPacket> {
        self.packets.try_recv().ok()
    }

    fn submit(&self, frame: RawFrame) -> bool {
        match self.tx.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
}

/// Lazily allocated projection capture tap.
pub struct ProjectionCapture {
    worker: Option<EncoderWorker>,
    renderer: Option<OffscreenRenderer>,
    renderer_size: (u32, u32),
    started: Instant,
    last_frame: Option<Instant>,
    profile_path: Option<PathBuf>,
    profile: CaptureProfile,
    hardware_profile: CaptureProfile,
    accepted_since_pressure: u32,
    last_profile_check: Option<Instant>,
    personal_profile: Option<CaptureProfile>,
    reset_encoder: bool,
}

impl Default for ProjectionCapture {
    fn default() -> Self {
        Self::new(default_socket_path())
    }
}

impl ProjectionCapture {
    fn new(socket_path: Option<PathBuf>) -> Self {
        let profile = DisplayStreamPolicy::default().ceiling(None);
        Self {
            worker: EncoderWorker::spawn(socket_path),
            renderer: None,
            renderer_size: (0, 0),
            started: Instant::now(),
            last_frame: None,
            profile_path: default_profile_path(),
            profile,
            hardware_profile: profile,
            accepted_since_pressure: 0,
            last_profile_check: None,
            personal_profile: None,
            reset_encoder: false,
        }
    }

    pub fn set_personal_profile(&mut self, profile: Option<CaptureProfile>) {
        self.personal_profile = profile;
    }

    pub fn request_keyframe(&mut self) {
        self.reset_encoder = true;
    }

    pub fn take_packet(&self) -> Option<VideoPacket> {
        self.worker.as_ref().and_then(EncoderWorker::take_packet)
    }

    /// Render and queue a desktop scene. Returns `true` when a frame was
    /// accepted for encoding, `false` when throttled or capture is unavailable.
    pub fn submit_scene(&mut self, scene: &Scene, source_size: (u32, u32)) -> bool {
        let Some(worker) = self.worker.as_ref() else {
            return false;
        };
        let now = Instant::now();
        if self
            .last_profile_check
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_secs(1))
        {
            if let Some(path) = self.profile_path.as_ref()
                && let Ok(bytes) = std::fs::read(path)
                && let Ok(profile) = serde_json::from_slice::<CaptureProfile>(&bytes)
            {
                self.profile = profile;
                self.hardware_profile = clamp_profile(self.hardware_profile, profile);
            }
            self.last_profile_check = Some(now);
        }
        let ceiling = self.personal_profile.map_or(self.profile, |personal| {
            clamp_profile(self.profile, personal)
        });
        let effective_profile = clamp_profile(self.hardware_profile, ceiling);
        let frame_interval = Duration::from_secs_f64(1.0 / f64::from(effective_profile.fps.max(1)));
        if self
            .last_frame
            .is_some_and(|last| now.saturating_duration_since(last) < frame_interval)
        {
            return false;
        }
        let Some((width, height, scale)) = target_size(source_size, effective_profile) else {
            return false;
        };
        if self.renderer_size != (width, height) {
            self.renderer = OffscreenRenderer::try_new(width, height).ok();
            self.renderer_size = (width, height);
        }
        let Some(renderer) = self.renderer.as_mut() else {
            return false;
        };
        let mut scaled = Scene::new();
        scaled.append(scene, Some(Affine::scale(scale)));
        let rgba = renderer.render(&scaled, Color::from_rgb8(0x1A, 0x1B, 0x26));
        let accepted = worker.submit(RawFrame {
            width,
            height,
            presentation_time_us: u64::try_from(
                now.saturating_duration_since(self.started).as_micros(),
            )
            .unwrap_or(u64::MAX),
            rgba,
            fps: effective_profile.fps,
            bitrate_kbps: effective_profile.bitrate_kbps,
            reset_encoder: std::mem::take(&mut self.reset_encoder),
        });
        self.last_frame = Some(now);
        if accepted {
            self.accepted_since_pressure = self.accepted_since_pressure.saturating_add(1);
            if self.accepted_since_pressure >= effective_profile.fps.saturating_mul(10) {
                self.hardware_profile = raise_profile(self.hardware_profile, self.profile);
                self.accepted_since_pressure = 0;
            }
        } else {
            self.hardware_profile = lower_profile(effective_profile);
            self.accepted_since_pressure = 0;
        }
        accepted
    }
}

fn clamp_profile(value: CaptureProfile, ceiling: CaptureProfile) -> CaptureProfile {
    CaptureProfile::new(
        value.max_width.min(ceiling.max_width),
        value.max_height.min(ceiling.max_height),
        value.fps.min(ceiling.fps),
        value.bitrate_kbps.min(ceiling.bitrate_kbps),
    )
}

fn lower_profile(value: CaptureProfile) -> CaptureProfile {
    CaptureProfile::new(
        ((value.max_width.saturating_mul(3) / 4).max(640)) & !1,
        ((value.max_height.saturating_mul(3) / 4).max(360)) & !1,
        value.fps,
        (value.bitrate_kbps.saturating_mul(3) / 4).max(2_500),
    )
}

fn raise_profile(value: CaptureProfile, ceiling: CaptureProfile) -> CaptureProfile {
    CaptureProfile::new(
        (value.max_width.saturating_mul(4) / 3).min(ceiling.max_width) & !1,
        (value.max_height.saturating_mul(4) / 3).min(ceiling.max_height) & !1,
        ceiling.fps,
        (value.bitrate_kbps.saturating_mul(4) / 3).min(ceiling.bitrate_kbps),
    )
}

fn default_profile_path() -> Option<PathBuf> {
    std::env::var_os("TILO_PROJECTOR_PROFILE")
        .map(PathBuf::from)
        .or_else(|| {
            xdg_paths::xdg::runtime_dir().map(|dir| dir.join("tilo-projector/capture-profile.json"))
        })
}

fn default_socket_path() -> Option<PathBuf> {
    std::env::var_os(VIDEO_SOCKET_ENV)
        .map(PathBuf::from)
        .or_else(|| xdg_paths::xdg::runtime_dir().map(|dir| dir.join("tilo-projector/video.sock")))
}

fn target_size(source: (u32, u32), profile: CaptureProfile) -> Option<(u32, u32, f64)> {
    if source.0 == 0 || source.1 == 0 {
        return None;
    }
    let scale = (f64::from(profile.max_width) / f64::from(source.0))
        .min(f64::from(profile.max_height) / f64::from(source.1))
        .min(1.0);
    Some((
        even_u32(f64::from(source.0) * scale),
        even_u32(f64::from(source.1) * scale),
        scale,
    ))
}

/// Round `value` down to the nearest even `u32`, floored at 2 — the encoder's minimum coded
/// dimension in either axis.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=f64::from(u32::MAX) immediately above, so it always fits a non-negative u32"
)]
fn even_u32(value: f64) -> u32 {
    ((value.floor().clamp(0.0, f64::from(u32::MAX)) as u32).max(2)) & !1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn projection_target_size_and_encoder_pressure_scaling() {
        // Phase `target_size_preserves_aspect_and_encoder_evenness`.
        {
            let smooth = CaptureProfile::new(1280, 720, 60, 6_000);
            assert_eq!(
                target_size((1920, 1080), smooth),
                Some((1280, 720, 2.0 / 3.0))
            );
            let (width, height, _) = target_size((1365, 768), smooth).expect("target");
            assert_eq!(width % 2, 0);
            assert_eq!(height % 2, 0);
            assert!(width <= smooth.max_width && height <= smooth.max_height);
            assert_eq!(
                target_size((3840, 2160), CaptureProfile::new(3840, 2160, 30, 20_000)),
                Some((3840, 2160, 1.0))
            );
        }
        // Phase `encoder_pressure_scales_continuously_and_recovers_only_to_the_ceiling`.
        {
            let ceiling = CaptureProfile::new(2560, 1440, 60, 15_000);
            let lowered = lower_profile(ceiling);
            assert_eq!((lowered.max_width, lowered.max_height), (1920, 1080));
            assert!(lowered.bitrate_kbps < ceiling.bitrate_kbps);
            assert_eq!(raise_profile(lowered, ceiling), ceiling);

            let user_limit = CaptureProfile::new(1280, 720, 30, 5_000);
            assert_eq!(clamp_profile(ceiling, user_limit), user_limit);
        }
    }

    /// Needs an H.264 encoder: hardware, or Cisco's openh264 plugin. A stock
    /// image has neither, and says so rather than passing.
    #[test]
    fn encoder_worker_sends_a_real_h264_packet_to_a_fake_broker() {
        if !media::realtime::local_capabilities().can_encode(media::StreamCodec::H264) {
            eprintln!("no H.264 encoder on this machine; projection encode not run");
            return;
        }
        let dir = tempfile::tempdir().expect("temp runtime dir");
        let socket = dir.path().join("video.sock");
        let listener = UnixListener::bind(&socket).expect("fake broker socket");
        let worker = EncoderWorker::spawn(Some(socket)).expect("encoder worker should start");
        // Large enough for every hardware encoder's minimum size.
        assert!(worker.submit(RawFrame {
            width: 320,
            height: 240,
            presentation_time_us: 12_345,
            rgba: vec![0x44; 320 * 240 * 4],
            fps: 30,
            bitrate_kbps: 2_000,
            reset_encoder: false,
        }));
        let (mut stream, _) = listener.accept().expect("encoder connection");
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .expect("read timeout");
        let packet = VideoPacket::read_from(&mut stream).expect("encoded video packet");
        assert_eq!((packet.width, packet.height), (320, 240));
        assert_eq!(packet.presentation_time_us, 12_345);
        assert!(packet.keyframe);
        assert!(packet.annex_b.windows(4).any(|bytes| bytes == [0, 0, 0, 1]));
    }
}
