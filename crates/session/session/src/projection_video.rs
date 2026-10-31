//! The desktop as a wireless display's picture.
//!
//! A receiver of the network protocols (`Chromecast`, `FCast`) is shown the
//! desktop the projector service serves as video, and the frames it serves
//! are these: the compositor's own composition of every display, drawn at
//! the profile the service asks for, H.264 encoded and handed to the
//! projector's video socket as one Annex B packet per frame.
//!
//! Nothing is captured until a session to such a receiver is live, and a
//! locked session never is: a lock screen is not something to send to a
//! television. Frames are rendered on the session's loop, at most as often as
//! the profile's frame rate, and encoded on a worker of their own — one frame
//! ahead at most, so an encoder that falls behind drops the newest frame
//! instead of holding up the desktop. An encoder that cannot keep up lowers
//! the profile itself (smaller and cheaper), and climbs back once frames are
//! taken again, so a slow link degrades instead of stalling.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use tilo_compositor::Compositor;
use tilo_projector::{CaptureProfile, DisplayStreamPolicy, VideoPacket};
use tilo_window_share::config::EncoderChoice;
use tilo_window_share::encoder::{Encoder, EncoderInit, FrameInput, build};

/// What a live projection streams: where the projector reads video, the
/// options the person chose and the profile the service last asked for.
pub(crate) struct VideoStream {
    pub socket: PathBuf,
    pub policy: DisplayStreamPolicy,
    /// The ceiling the service reported for this receiver; `None` until it
    /// says anything, when the person's own options are used.
    pub asked: Option<CaptureProfile>,
}

/// One frame of the desktop, ready to be encoded.
struct RawFrame {
    width: u32,
    height: u32,
    presentation_time_us: u64,
    rgba: Vec<u8>,
    fps: u32,
    bitrate_kbps: u32,
}

/// Encodes frames and writes them to the projector's video socket, off the
/// session's loop.
struct EncoderWorker {
    tx: SyncSender<RawFrame>,
}

impl EncoderWorker {
    /// Starts the encoder thread. Returns `None` — logging why — if the OS
    /// refuses to spawn it, in which case the desktop is not streamed rather
    /// than the session stopping.
    fn spawn(socket_path: &Path) -> Option<Self> {
        let (tx, rx) = mpsc::sync_channel::<RawFrame>(1);
        let socket_path = socket_path.to_owned();
        let spawned = std::thread::Builder::new()
            .name("tilo-projection-encoder".to_owned())
            .spawn(move || {
                let mut stream: Option<UnixStream> = None;
                let mut encoder: Option<Box<dyn Encoder>> = None;
                let mut reported_unavailable = false;
                let mut encoder_profile = None;
                while let Ok(frame) = rx.recv() {
                    if stream.is_none()
                        && let Ok(connected) = UnixStream::connect(&socket_path)
                    {
                        stream = Some(connected);
                        // A new connection must begin with fresh SPS/PPS.
                        encoder = None;
                        encoder_profile = None;
                    }
                    let Some(active_stream) = stream.as_mut() else {
                        continue;
                    };
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
                                // An image without H.264 hardware encode and
                                // without Cisco's openh264 has no H.264
                                // encoder at all; say so once.
                                if !reported_unavailable {
                                    tracing::warn!(
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
                    if packet.write_to(active_stream).is_err() {
                        stream = None;
                        encoder = None;
                        encoder_profile = None;
                    }
                }
            });
        match spawned {
            Ok(_handle) => Some(Self { tx }),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not start the projection encoder; the desktop is not streamed"
                );
                None
            }
        }
    }

    fn submit(&self, frame: RawFrame) -> bool {
        match self.tx.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
}

/// The desktop's frames for the projector: what is sent while a projection
/// is live, and the profile it is sent at.
pub(crate) struct ProjectionVideo {
    worker: Option<EncoderWorker>,
    /// The socket the worker writes to.
    socket: Option<PathBuf>,
    started: Instant,
    last_frame: Option<Instant>,
    /// The profile frames are rendered and encoded at now: it drops at once
    /// when the service asks for less, and climbs back as frames are taken.
    profile: CaptureProfile,
    accepted_since_pressure: u32,
}

impl ProjectionVideo {
    /// A desktop that streams to nobody until a projection is live.
    pub(crate) fn new() -> Self {
        Self {
            worker: None,
            socket: None,
            started: Instant::now(),
            last_frame: None,
            profile: DisplayStreamPolicy::default().ceiling(None),
            accepted_since_pressure: 0,
        }
    }

    /// Send the desktop to `stream`'s receiver, if a projection is live: the
    /// next frame is drawn once the profile's frame rate allows it.
    pub(crate) fn follow(&mut self, compositor: &mut Compositor, stream: Option<&VideoStream>) {
        let Some(stream) = stream else {
            return;
        };
        // A lock screen is never shown to a receiver: the session stays
        // connected, and its last frame is what the receiver keeps showing.
        if compositor.locked() {
            return;
        }
        let asked = stream.asked.unwrap_or_else(|| stream.policy.ceiling(None));
        self.profile = clamp_profile(self.profile, asked);
        let profile = self.profile;
        let interval = Duration::from_secs_f64(1.0 / f64::from(profile.fps.max(1)));
        let now = Instant::now();
        if self
            .last_frame
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
        {
            return;
        }
        let picture =
            match crate::capture::video(compositor, (profile.max_width, profile.max_height)) {
                Ok(picture) => picture,
                Err(error) => {
                    tracing::debug!(%error, "no frame of the desktop for the projection");
                    return;
                }
            };
        self.last_frame = Some(now);
        let presentation_time_us =
            u64::try_from(now.saturating_duration_since(self.started).as_micros())
                .unwrap_or(u64::MAX);
        let Some(worker) = self.worker(&stream.socket) else {
            return;
        };
        let accepted = worker.submit(RawFrame {
            width: picture.width(),
            height: picture.height(),
            presentation_time_us,
            rgba: picture.into_raw(),
            fps: profile.fps,
            bitrate_kbps: profile.bitrate_kbps,
        });
        if accepted {
            self.accepted_since_pressure = self.accepted_since_pressure.saturating_add(1);
            if self.accepted_since_pressure >= profile.fps.saturating_mul(10) {
                self.profile = raise_profile(self.profile, asked);
                self.accepted_since_pressure = 0;
            }
        } else {
            // The encoder is behind: the next frames are cheaper ones.
            self.profile = lower_profile(profile);
            self.accepted_since_pressure = 0;
        }
    }

    /// The worker writing to `socket`, started (or replaced, when the
    /// socket moved) as needed.
    fn worker(&mut self, socket: &Path) -> Option<&EncoderWorker> {
        if self.socket.as_deref() != Some(socket) {
            self.worker = EncoderWorker::spawn(socket);
            self.socket = Some(socket.to_owned());
        }
        self.worker.as_ref()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    use tilo_compositor::{OutputConfig, StackPolicy};
    use tilo_projector::{DisplayResolutionLimit, VideoPacket};

    /// What the sheet's 720p choice on a 30 Hz television resolves to.
    fn television() -> DisplayStreamPolicy {
        DisplayStreamPolicy {
            adaptive: true,
            resolution_limit: DisplayResolutionLimit::P720,
            max_fps: 30,
        }
    }

    /// A desktop with one 1080p display, drawn once as a session's pass
    /// draws it.
    fn desktop() -> Compositor {
        let mut compositor =
            Compositor::headless(Box::new(StackPolicy)).expect("an in-process compositor");
        compositor
            .add_output(&OutputConfig::new("HEADLESS-1", 1920, 1080))
            .expect("a display");
        compositor.render().expect("a first frame");
        compositor
    }

    /// Whether this machine can encode H.264 at all: without hardware encode
    /// and without Cisco's openh264 there is nothing to send with.
    fn has_h264_encoder() -> bool {
        media::realtime::local_capabilities().can_encode(media::StreamCodec::H264)
    }

    #[test]
    fn a_live_projection_sends_the_desktop_at_the_profile_the_person_chose() {
        if !has_h264_encoder() {
            eprintln!("no H.264 encoder on this machine: nothing could encode the frames");
            return;
        }
        let dir = tempfile::tempdir().expect("a directory for the projector's socket");
        let socket = dir.path().join("video.sock");
        let listener = UnixListener::bind(&socket).expect("the projector's video socket");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking accept");

        let mut compositor = desktop();
        let stream = VideoStream {
            socket,
            policy: television(),
            asked: None,
        };
        let mut video = ProjectionVideo::new();
        // One pass of the session's loop, as `Session::step` makes it.
        video.follow(&mut compositor, Some(&stream));

        let mut connection = tilo_test_env::wait_for(
            "the desktop to connect to the projector's video socket",
            || listener.accept().ok().map(|(connection, _)| connection),
        );
        connection
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .expect("a read timeout");
        let packet = VideoPacket::read_from(&mut connection).expect("an encoded frame");
        assert_eq!(
            (packet.width, packet.height),
            (1280, 720),
            "the television is sent the 720p the person chose, not the screen's 1080p"
        );
        assert!(
            packet.keyframe,
            "the first frame a receiver gets stands on its own"
        );
        assert!(
            packet.annex_b.starts_with(&[0, 0, 0, 1]),
            "the frame is one Annex B access unit"
        );
    }

    #[test]
    fn nothing_is_captured_while_no_projection_is_live() {
        let dir = tempfile::tempdir().expect("a directory for the projector's socket");
        let socket = dir.path().join("video.sock");
        let listener = UnixListener::bind(&socket).expect("the projector's video socket");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking accept");

        let mut compositor = desktop();
        let mut video = ProjectionVideo::new();
        for _ in 0..4 {
            video.follow(&mut compositor, None);
        }

        assert!(
            video.worker.is_none(),
            "no encoder is started, so the desktop is neither copied nor encoded"
        );
        assert!(
            video.last_frame.is_none(),
            "no frame of the desktop is drawn for a receiver"
        );
        assert!(
            listener.accept().is_err(),
            "nothing connects to the projector's video socket"
        );
    }

    #[test]
    fn an_encoder_that_falls_behind_lowers_the_profile_and_climbs_back() {
        let ceiling = CaptureProfile::new(2560, 1440, 60, 15_000);
        let lowered = lower_profile(ceiling);
        assert_eq!((lowered.max_width, lowered.max_height), (1920, 1080));
        assert_eq!(lowered.fps, ceiling.fps, "the rate is the person's");
        assert!(lowered.bitrate_kbps < ceiling.bitrate_kbps);
        assert_eq!(raise_profile(lowered, ceiling), ceiling);

        // The person's own limit always wins, whatever the link allows.
        let limit = CaptureProfile::new(1280, 720, 30, 5_000);
        assert_eq!(clamp_profile(ceiling, limit), limit);

        // A link that never recovers settles on a picture that still reads,
        // instead of shrinking to nothing.
        let settled = (0..10).fold(ceiling, |value, _| lower_profile(value));
        assert_eq!(
            (settled.max_width, settled.max_height, settled.bitrate_kbps),
            (640, 360, 2_500)
        );
    }
}
