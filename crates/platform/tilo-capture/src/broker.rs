//! Capture for an app its sandbox confines (macOS Seatbelt), brokered by the
//! host that launched it.
//!
//! A confined app can reach no camera or microphone: macOS gates both
//! through TCC, which the sandbox profile does not grant. The host, which
//! holds the TCC permission, captures on the app's behalf instead. The app
//! inherits one control socket ([`CAPTURE_FD_ENV`]); for each capture it
//! creates a socket pair and sends one end with its request, and the host
//! answers on that end — granted or refused — and then streams records to it
//! until either side closes it or the grant goes. The app names a device and
//! a format, never a device handle.
//!
//! [`crate::VideoCapture::open_camera`] and
//! [`crate::AudioCapture::open_microphone`] take this path by themselves in
//! such an app, so its code is the same wherever it runs.

use std::collections::VecDeque;
use std::io::{self, IoSlice, IoSliceMut, Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rustix::io::Errno;
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};

use crate::{
    AudioCapture, AudioSource, CameraRequest, CaptureError, CaptureState, DeviceKind, Frame,
    MAX_BUFFERED_SAMPLES, MicrophoneRequest, VideoCapture, VideoSource, Wake, valid_rgba_frame,
};
use tilo_lock::MutexRecover as _;

/// The inherited control socket, as a descriptor number.
pub const CAPTURE_FD_ENV: &str = "TILO_SVC_CAPTURE_FD";

const MAX_REQUEST: usize = 512;
const MAX_REASON: u32 = 4096;
const MAX_AUDIO_SAMPLES: u32 = 1 << 20;
/// How long a stream waits for a frame before checking its grant again.
const IDLE: Duration = Duration::from_millis(10);

/// What an app asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Camera { width: u32, height: u32 },
    Microphone { sample_rate: u32 },
}

impl Request {
    #[must_use]
    pub fn device(&self) -> DeviceKind {
        match self {
            Self::Camera { .. } => DeviceKind::Camera,
            Self::Microphone { .. } => DeviceKind::Microphone,
        }
    }

    fn encode(&self) -> String {
        match self {
            Self::Camera { width, height } => format!("camera {width} {height}"),
            Self::Microphone { sample_rate } => format!("microphone {sample_rate}"),
        }
    }

    fn decode(text: &str) -> Option<Self> {
        let mut words = text.split(' ');
        let request = match (words.next()?, words.next()?.parse().ok()?) {
            ("camera", width) => Self::Camera {
                width,
                height: words.next()?.parse().ok()?,
            },
            ("microphone", sample_rate) => Self::Microphone { sample_rate },
            _ => return None,
        };
        words.next().is_none().then_some(request)
    }
}

/// One record on a capture stream.
#[derive(Clone, Debug, PartialEq)]
enum Record {
    Granted,
    Refused(String),
    Video(Frame),
    Audio { sample_rate: u32, samples: Vec<f32> },
    Failed(String),
    Ended(String),
}

fn header(tag: u8, a: u32, b: u32, len: usize) -> io::Result<[u8; 13]> {
    let len = u32::try_from(len).map_err(|_| invalid("a capture record is too large"))?;
    let mut out = [0_u8; 13];
    out[0] = tag;
    out[1..5].copy_from_slice(&a.to_le_bytes());
    out[5..9].copy_from_slice(&b.to_le_bytes());
    out[9..13].copy_from_slice(&len.to_le_bytes());
    Ok(out)
}

fn write_record(mut stream: &UnixStream, record: &Record) -> io::Result<()> {
    let (tag, a, b, payload): (u8, u32, u32, std::borrow::Cow<'_, [u8]>) = match record {
        Record::Granted => (1, 0, 0, (&[][..]).into()),
        Record::Refused(reason) => (2, 0, 0, reason.as_bytes().into()),
        Record::Failed(reason) => (5, 0, 0, reason.as_bytes().into()),
        Record::Ended(reason) => (6, 0, 0, reason.as_bytes().into()),
        Record::Video(frame) => (3, frame.width, frame.height, frame.rgba.as_slice().into()),
        Record::Audio {
            sample_rate,
            samples,
        } => (
            4,
            *sample_rate,
            0,
            samples
                .iter()
                .flat_map(|s| s.to_le_bytes())
                .collect::<Vec<u8>>()
                .into(),
        ),
    };
    stream.write_all(&header(tag, a, b, payload.len())?)?;
    stream.write_all(&payload)
}

/// The next record; `None` at the end of the stream.
fn read_record(stream: &mut UnixStream) -> io::Result<Option<Record>> {
    let mut head = [0_u8; 13];
    match stream.read_exact(&mut head) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let [tag, a0, a1, a2, a3, b0, b1, b2, b3, l0, l1, l2, l3] = head;
    let (a, b, len) = (
        u32::from_le_bytes([a0, a1, a2, a3]),
        u32::from_le_bytes([b0, b1, b2, b3]),
        u32::from_le_bytes([l0, l1, l2, l3]),
    );
    let limit = match tag {
        3 => u32::try_from(crate::MAX_FRAME_PIXELS * 4).unwrap_or(u32::MAX),
        4 => MAX_AUDIO_SAMPLES * 4,
        _ => MAX_REASON,
    };
    if len > limit {
        return Err(invalid("a capture record is larger than allowed"));
    }
    let mut payload = vec![0_u8; usize::try_from(len).map_err(|_| invalid("too large"))?];
    stream.read_exact(&mut payload)?;
    let text = || String::from_utf8_lossy(&payload).into_owned();
    Ok(Some(match tag {
        1 => Record::Granted,
        2 => Record::Refused(text()),
        3 if valid_rgba_frame(a, b, payload.len()) => Record::Video(Frame {
            width: a,
            height: b,
            rgba: payload,
        }),
        4 if payload.len() % 4 == 0 => Record::Audio {
            sample_rate: a,
            samples: payload
                .chunks_exact(4)
                .map(|c| <[u8; 4]>::try_from(c).map_or(0.0, f32::from_le_bytes))
                .collect(),
        },
        5 => Record::Failed(text()),
        6 => Record::Ended(text()),
        _ => return Err(invalid("not a capture record")),
    }))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

fn send_with_fd(socket: &UnixDatagram, bytes: &[u8], fd: &OwnedFd) -> io::Result<()> {
    let fds = [fd.as_fd()];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(invalid("control buffer"));
    }
    rustix::net::sendmsg(
        socket,
        &[IoSlice::new(bytes)],
        &mut control,
        SendFlags::empty(),
    )?;
    Ok(())
}

/// Receive one message and the descriptor attached to it, if any; `None`
/// when the peer closed the socket.
fn recv_with_fd(socket: &UnixDatagram) -> io::Result<Option<(Vec<u8>, Option<OwnedFd>)>> {
    let mut bytes = vec![0_u8; MAX_REQUEST];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    #[cfg(target_os = "linux")]
    let flags = RecvFlags::CMSG_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = RecvFlags::empty();
    let received = match rustix::net::recvmsg(
        socket,
        &mut [IoSliceMut::new(&mut bytes)],
        &mut control,
        flags,
    ) {
        Ok(received) => received,
        // A datagram peer that closed reads as a reset on macOS.
        Err(Errno::CONNRESET | Errno::NOTCONN) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    // A descriptor the kernel attached is owned from here on; any other
    // control message is ignored.
    let mut fd = None;
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(mut fds) = message
            && fd.is_none()
        {
            fd = fds.next();
        }
    }
    if received.bytes == 0 && fd.is_none() {
        return Ok(None);
    }
    bytes.truncate(received.bytes);
    Ok(Some((bytes, fd)))
}

/// Whether a device may be captured for the app a control socket serves.
pub trait Policy: Send + Sync {
    /// Whether a capture of `device` may start now, asking the person where
    /// nobody has decided yet.
    ///
    /// # Errors
    ///
    /// Why it may not; the app is told this.
    fn admit(&self, device: DeviceKind) -> Result<(), String>;
    /// Whether a running capture of `device` may go on.
    fn allows(&self, device: DeviceKind) -> bool;
}

/// Where the host's own captures come from.
pub trait Sources: Send + Sync {
    /// # Errors
    ///
    /// The camera cannot start.
    fn camera(&self, request: &CameraRequest) -> Result<VideoCapture, CaptureError>;
    /// # Errors
    ///
    /// The microphone cannot start.
    fn microphone(&self, request: &MicrophoneRequest) -> Result<AudioCapture, CaptureError>;
}

/// The host's own devices, through this crate's native backends.
pub struct SystemSources;

impl Sources for SystemSources {
    fn camera(&self, request: &CameraRequest) -> Result<VideoCapture, CaptureError> {
        VideoCapture::open_camera(request)
    }
    fn microphone(&self, request: &MicrophoneRequest) -> Result<AudioCapture, CaptureError> {
        AudioCapture::open_microphone(request)
    }
}

/// Serve one app's capture requests on `control` until it closes it: each
/// admitted by `policy` is captured from `sources` on a thread of its own
/// and streamed until the app hangs up or `policy` stops allowing it.
///
/// # Errors
///
/// Receiving on `control` failed.
pub fn serve(
    control: &UnixDatagram,
    policy: &Arc<dyn Policy>,
    sources: &Arc<dyn Sources>,
) -> io::Result<()> {
    while let Some((bytes, fd)) = recv_with_fd(control)? {
        let Some(fd) = fd else { continue };
        let stream = UnixStream::from(fd);
        let request = std::str::from_utf8(&bytes).ok().and_then(Request::decode);
        let Some(request) = request else {
            let _ = write_record(&stream, &Record::Refused("not a capture request".into()));
            continue;
        };
        if let Err(reason) = policy.admit(request.device()) {
            let _ = write_record(&stream, &Record::Refused(reason));
            continue;
        }
        let (policy, sources) = (Arc::clone(policy), Arc::clone(sources));
        std::thread::Builder::new()
            .name("tilo-capture-stream".into())
            .spawn(move || stream_to(&stream, &request, &*policy, &*sources))?;
    }
    Ok(())
}

fn stream_to(stream: &UnixStream, request: &Request, policy: &dyn Policy, sources: &dyn Sources) {
    let device = request.device();
    let ended = match request {
        Request::Camera { width, height } => sources
            .camera(&CameraRequest {
                device: None,
                width: *width,
                height: *height,
            })
            .map(|mut capture| {
                pump(stream, policy, device, || {
                    Ok(match capture.state() {
                        CaptureState::Failed(error) => Some(Record::Failed(error.to_string())),
                        CaptureState::Ended => Some(Record::Ended("the camera stopped".into())),
                        _ => capture.take_latest().map(Record::Video),
                    })
                });
            }),
        Request::Microphone { sample_rate } => sources
            .microphone(&MicrophoneRequest {
                device: None,
                sample_rate: *sample_rate,
            })
            .map(|mut capture| {
                pump(stream, policy, device, || {
                    Ok(match capture.state() {
                        CaptureState::Failed(error) => Some(Record::Failed(error.to_string())),
                        CaptureState::Ended => Some(Record::Ended("the microphone stopped".into())),
                        _ => {
                            let samples = capture.read();
                            (!samples.is_empty()).then(|| Record::Audio {
                                sample_rate: capture.sample_rate(),
                                samples,
                            })
                        }
                    })
                });
            }),
    };
    if let Err(error) = ended {
        let _ = write_record(stream, &Record::Refused(error.to_string()));
    }
}

/// Grant, then send what `next` produces until the app hangs up, the
/// capture ends or its grant goes.
fn pump(
    stream: &UnixStream,
    policy: &dyn Policy,
    device: DeviceKind,
    mut next: impl FnMut() -> io::Result<Option<Record>>,
) {
    if write_record(stream, &Record::Granted).is_err() {
        return;
    }
    loop {
        if !policy.allows(device) {
            let _ = write_record(stream, &Record::Ended("access was revoked".into()));
            return;
        }
        match next() {
            Ok(Some(record)) => {
                let last = matches!(record, Record::Failed(_) | Record::Ended(_));
                if write_record(stream, &record).is_err() || last {
                    return;
                }
            }
            Ok(None) => std::thread::sleep(IDLE),
            Err(_) => return,
        }
    }
}

/// The broker of an app whose host passed it one ([`CAPTURE_FD_ENV`]). The
/// descriptor is taken once, by the first call.
pub(crate) fn from_env() -> Option<Arc<Client>> {
    static CLIENT: OnceLock<Option<Arc<Client>>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let fd: i32 = std::env::var(CAPTURE_FD_ENV).ok()?.parse().ok()?;
            // SAFETY: fstat only reports whether `fd` names an open
            // descriptor.
            let open = unsafe {
                let mut stat: libc::stat = std::mem::zeroed();
                libc::fstat(fd, &raw mut stat) == 0
            };
            // SAFETY: the host passed this descriptor for this purpose alone,
            // and nothing else in the process claims it.
            open.then(|| {
                Arc::new(Client::new(UnixDatagram::from(unsafe {
                    OwnedFd::from_raw_fd(fd)
                })))
            })
        })
        .clone()
}

/// An app's end of the control socket.
pub struct Client {
    control: UnixDatagram,
}

/// What a brokered capture has received so far.
struct Received {
    state: CaptureState,
    latest: Option<Frame>,
    samples: VecDeque<f32>,
    sample_rate: u32,
    /// Told when `state` changes.
    wake: Option<Wake>,
}

/// A running brokered capture: what the stream delivered, and the stream,
/// which is shut down when the capture is dropped.
struct Brokered {
    received: Arc<Mutex<Received>>,
    stream: UnixStream,
}

impl Drop for Brokered {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl Client {
    #[must_use]
    pub fn new(control: UnixDatagram) -> Self {
        Self { control }
    }

    /// Ask the host for `request` and receive its stream in the background.
    fn start(&self, request: &Request) -> Result<Brokered, CaptureError> {
        let failed =
            |error: io::Error| CaptureError::Failed(format!("the capture broker: {error}"));
        let (ours, theirs) = UnixStream::pair().map_err(failed)?;
        send_with_fd(
            &self.control,
            request.encode().as_bytes(),
            &OwnedFd::from(theirs),
        )
        .map_err(failed)?;
        let received = Arc::new(Mutex::new(Received {
            state: CaptureState::Starting,
            latest: None,
            samples: VecDeque::new(),
            sample_rate: 0,
            wake: None,
        }));
        let mut reader = ours.try_clone().map_err(failed)?;
        let shared = Arc::clone(&received);
        std::thread::Builder::new()
            .name("tilo-capture-client".into())
            .spawn(move || receive(&mut reader, &shared))
            .map_err(failed)?;
        Ok(Brokered {
            received,
            stream: ours,
        })
    }

    /// A camera through the host.
    ///
    /// # Errors
    ///
    /// The request could not be sent; a refusal arrives as
    /// [`CaptureState::Failed`].
    pub fn camera(&self, request: &CameraRequest) -> Result<VideoCapture, CaptureError> {
        let inner = self.start(&Request::Camera {
            width: request.width,
            height: request.height,
        })?;
        Ok(VideoCapture {
            inner: Box::new(inner),
        })
    }

    /// A microphone through the host.
    ///
    /// # Errors
    ///
    /// As [`Self::camera`].
    pub fn microphone(&self, request: &MicrophoneRequest) -> Result<AudioCapture, CaptureError> {
        let inner = self.start(&Request::Microphone {
            sample_rate: request.sample_rate,
        })?;
        Ok(AudioCapture {
            inner: Box::new(inner),
        })
    }
}

fn receive(stream: &mut UnixStream, shared: &Mutex<Received>) {
    loop {
        let record = read_record(stream);
        let (state, done) = match record {
            Ok(Some(Record::Granted)) => (Some(CaptureState::Live), false),
            Ok(Some(Record::Refused(_))) => (
                Some(CaptureState::Failed(CaptureError::PermissionDenied)),
                true,
            ),
            Ok(Some(Record::Video(frame))) => {
                shared.lock_recover().latest = Some(frame);
                (None, false)
            }
            Ok(Some(Record::Audio {
                sample_rate,
                samples,
            })) => {
                let mut received = shared.lock_recover();
                received.sample_rate = sample_rate;
                received.samples.extend(samples);
                let excess = received.samples.len().saturating_sub(MAX_BUFFERED_SAMPLES);
                received.samples.drain(..excess);
                drop(received);
                (None, false)
            }
            Ok(Some(Record::Failed(reason))) => (
                Some(CaptureState::Failed(CaptureError::Failed(reason))),
                true,
            ),
            Ok(Some(Record::Ended(_)) | None) => (Some(CaptureState::Ended), true),
            Err(error) => (
                Some(CaptureState::Failed(CaptureError::Failed(
                    error.to_string(),
                ))),
                true,
            ),
        };
        if let Some(state) = state {
            let mut received = shared.lock_recover();
            received.state = state;
            let wake = received.wake.clone();
            drop(received);
            if let Some(wake) = wake {
                wake();
            }
        }
        if done {
            return;
        }
    }
}

impl Brokered {
    fn received(&self) -> std::sync::MutexGuard<'_, Received> {
        self.received.lock_recover()
    }
}

impl VideoSource for Brokered {
    fn state(&mut self) -> CaptureState {
        self.received().state.clone()
    }
    fn take_latest(&mut self) -> Option<Frame> {
        self.received().latest.take()
    }
    fn notify(&mut self, wake: Wake) -> bool {
        self.received().wake = Some(wake);
        true
    }
}

impl AudioSource for Brokered {
    fn state(&mut self) -> CaptureState {
        self.received().state.clone()
    }
    fn read(&mut self) -> Vec<f32> {
        self.received().samples.drain(..).collect()
    }
    fn sample_rate(&self) -> u32 {
        self.received().sample_rate
    }
}

/// Synthetic devices, for tests of code that brokers or consumes capture.
#[cfg(any(test, feature = "test-support"))]
pub mod synthetic {
    use super::{
        AudioCapture, AudioSource, CameraRequest, CaptureError, CaptureState, Frame,
        MicrophoneRequest, Sources, VideoCapture, VideoSource,
    };

    struct Pattern {
        frame: u8,
    }

    impl VideoSource for Pattern {
        fn state(&mut self) -> CaptureState {
            CaptureState::Live
        }
        fn take_latest(&mut self) -> Option<Frame> {
            self.frame = self.frame.wrapping_add(1);
            Some(Frame {
                width: 4,
                height: 2,
                rgba: vec![self.frame; 4 * 2 * 4],
            })
        }
    }

    struct Tone;

    impl AudioSource for Tone {
        fn state(&mut self) -> CaptureState {
            CaptureState::Live
        }
        fn read(&mut self) -> Vec<f32> {
            vec![0.25; 480]
        }
        fn sample_rate(&self) -> u32 {
            48_000
        }
    }

    /// A 4×2 camera whose every pixel byte counts its frames, and a
    /// microphone of a constant 0.25.
    pub struct Devices;

    impl Sources for Devices {
        fn camera(&self, _: &CameraRequest) -> Result<VideoCapture, CaptureError> {
            Ok(VideoCapture {
                inner: Box::new(Pattern { frame: 0 }),
            })
        }
        fn microphone(&self, _: &MicrophoneRequest) -> Result<AudioCapture, CaptureError> {
            Ok(AudioCapture {
                inner: Box::new(Tone),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{Decision, DeviceGrants, GrantPolicy};

    #[track_caller]
    fn until<T>(probe: impl FnMut() -> Option<T>) -> T {
        tilo_test_env::wait_for("the broker", probe)
    }

    struct Host {
        client: Client,
        grants: Arc<DeviceGrants>,
        _dir: tempfile::TempDir,
    }

    fn host(declared: Vec<DeviceKind>, consent: bool) -> Host {
        host_of(declared, consent, Arc::new(synthetic::Devices))
    }

    fn host_of(declared: Vec<DeviceKind>, consent: bool, sources: Arc<dyn Sources>) -> Host {
        let dir = tempfile::tempdir().unwrap();
        let grants = Arc::new(DeviceGrants::open(&dir.path().join("grants")).unwrap());
        let (app, runtime) = UnixDatagram::pair().unwrap();
        let policy: Arc<dyn Policy> = Arc::new(GrantPolicy {
            grants: Arc::clone(&grants),
            room: "r".into(),
            app: "so.tilo.video".into(),
            declared,
            consent: move |_| Ok(consent),
        });
        std::thread::spawn(move || serve(&runtime, &policy, &sources));
        Host {
            client: Client::new(app),
            grants,
            _dir: dir,
        }
    }

    const CAMERA: CameraRequest = CameraRequest {
        device: None,
        width: 640,
        height: 480,
    };

    #[test]
    fn requests_survive_the_wire_and_nothing_else_parses() {
        for request in [
            Request::Camera {
                width: 640,
                height: 480,
            },
            Request::Microphone {
                sample_rate: 48_000,
            },
        ] {
            assert_eq!(Request::decode(&request.encode()), Some(request));
        }
        for bad in [
            "",
            "camera",
            "camera 1",
            "screen 1 2",
            "microphone 1 2",
            "camera x y",
        ] {
            assert_eq!(Request::decode(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_granted_camera_and_microphone_stream_to_the_app() {
        let host = host(vec![DeviceKind::Camera, DeviceKind::Microphone], true);
        let mut camera = host.client.camera(&CAMERA).unwrap();
        let frame = until(|| camera.take_latest());
        assert_eq!((frame.width, frame.height, frame.rgba.len()), (4, 2, 32));
        assert_eq!(camera.state(), CaptureState::Live);
        let later = until(|| {
            camera
                .take_latest()
                .filter(|next| next.rgba[0] != frame.rgba[0])
        });
        assert_ne!(later.rgba, frame.rgba, "frames keep coming");

        let mut microphone = host
            .client
            .microphone(&MicrophoneRequest::default())
            .unwrap();
        let samples = until(|| Some(microphone.read()).filter(|s| !s.is_empty()));
        assert!(samples.iter().all(|s| (*s - 0.25).abs() < f32::EPSILON));
        assert_eq!(microphone.sample_rate(), 48_000);
        assert_eq!(
            host.grants
                .decision("r", "so.tilo.video", DeviceKind::Camera),
            Some(Decision::Allow)
        );
    }

    #[test]
    fn an_undeclared_or_denied_device_is_refused_and_nothing_streams() {
        let undeclared = host(vec![DeviceKind::Microphone], true);
        let mut camera = undeclared.client.camera(&CAMERA).unwrap();
        until(|| Some(camera.state()).filter(|s| *s != CaptureState::Starting));
        assert_eq!(
            camera.state(),
            CaptureState::Failed(CaptureError::PermissionDenied)
        );
        assert!(camera.take_latest().is_none());
        assert_eq!(
            undeclared
                .grants
                .decision("r", "so.tilo.video", DeviceKind::Camera),
            None,
            "an undeclared device was recorded"
        );

        let denied = host(vec![DeviceKind::Camera], false);
        let mut camera = denied.client.camera(&CAMERA).unwrap();
        until(|| Some(camera.state()).filter(|s| *s != CaptureState::Starting));
        assert_eq!(
            camera.state(),
            CaptureState::Failed(CaptureError::PermissionDenied)
        );
        assert_eq!(
            denied
                .grants
                .decision("r", "so.tilo.video", DeviceKind::Camera),
            Some(Decision::Deny)
        );
    }

    /// While the person has not answered, nothing changes and nothing
    /// wakes the app; their answer, either way, wakes it once, and the
    /// state it then reads is that answer.
    #[test]
    fn the_persons_answer_wakes_an_app_that_sleeps_until_then() {
        for allow in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let grants = Arc::new(DeviceGrants::open(&dir.path().join("grants")).unwrap());
            let (asked_tx, asked_rx) = std::sync::mpsc::channel::<()>();
            let (answer_tx, answer_rx) = std::sync::mpsc::channel::<bool>();
            let prompt = Mutex::new((asked_tx, answer_rx));
            let (app, runtime) = UnixDatagram::pair().unwrap();
            let policy: Arc<dyn Policy> = Arc::new(GrantPolicy {
                grants,
                room: "r".into(),
                app: "so.tilo.camera".into(),
                declared: vec![DeviceKind::Camera],
                consent: move |_| {
                    let prompt = prompt.lock_recover();
                    let _ = prompt.0.send(());
                    Ok(prompt.1.recv().unwrap_or(false))
                },
            });
            let sources: Arc<dyn Sources> = Arc::new(synthetic::Devices);
            std::thread::spawn(move || serve(&runtime, &policy, &sources));
            let client = Client::new(app);

            let mut camera = client.camera(&CAMERA).unwrap();
            let (woke_tx, woke_rx) = std::sync::mpsc::channel::<()>();
            let woke_tx = Mutex::new(woke_tx);
            assert!(
                camera.notify(move || {
                    let _ = woke_tx.lock_recover().send(());
                }),
                "the broker says when its state changes"
            );
            tilo_test_env::recv("the host to ask the person", &asked_rx);
            // The prompt is held open until the answer below is sent.
            assert_eq!(camera.state(), CaptureState::Starting);
            assert!(
                woke_rx.try_recv().is_err(),
                "nothing wakes the app while the prompt is open"
            );

            answer_tx.send(allow).unwrap();
            tilo_test_env::recv("the answer to wake the app", &woke_rx);
            let expected = if allow {
                CaptureState::Live
            } else {
                CaptureState::Failed(CaptureError::PermissionDenied)
            };
            assert_eq!(camera.state(), expected, "allow: {allow}");
        }
    }

    #[test]
    fn revoking_ends_a_running_capture() {
        let host = host(vec![DeviceKind::Camera], true);
        let mut camera = host.client.camera(&CAMERA).unwrap();
        until(|| camera.take_latest());
        assert!(
            host.grants
                .revoke("r", "so.tilo.video", DeviceKind::Camera)
                .unwrap()
        );
        until(|| Some(camera.state()).filter(|s| *s == CaptureState::Ended));
    }

    /// The host's real default camera and microphone through the broker.
    /// Opt-in, since it turns the camera on and may ask for the TCC
    /// permission: `TILO_CAPTURE_DEVICE_TEST=1 cargo test -p tilo-capture
    /// --features camera,microphone`.
    #[cfg(all(native_camera, native_microphone))]
    #[test]
    fn the_default_devices_stream_through_the_broker_when_asked() {
        if std::env::var_os("TILO_CAPTURE_DEVICE_TEST").is_none() {
            return;
        }
        let host = host_of(
            vec![DeviceKind::Camera, DeviceKind::Microphone],
            true,
            Arc::new(SystemSources),
        );
        let mut camera = host.client.camera(&CAMERA).unwrap();
        for _ in 0..3 {
            let frame = until(|| {
                if let CaptureState::Failed(error) = camera.state() {
                    panic!("the camera failed: {error}");
                }
                camera.take_latest()
            });
            assert!(valid_rgba_frame(
                frame.width,
                frame.height,
                frame.rgba.len()
            ));
            assert_eq!((frame.width, frame.height), (CAMERA.width, CAMERA.height));
        }
        let mut microphone = host
            .client
            .microphone(&MicrophoneRequest::default())
            .unwrap();
        let mut samples = 0;
        while samples < 4800 {
            if let CaptureState::Failed(error) = microphone.state() {
                panic!("the microphone failed: {error}");
            }
            samples += until(|| Some(microphone.read().len()).filter(|n| *n > 0));
        }
        assert!(microphone.sample_rate() > 0);
    }

    #[test]
    fn records_are_bounded_and_checked() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_all(&header(3, 2, 2, 3).unwrap()).unwrap();
        a.write_all(&[0; 3]).unwrap();
        assert!(read_record(&mut b).is_err(), "a frame of the wrong size");
        a.write_all(&header(2, 0, 0, MAX_REASON as usize + 1).unwrap())
            .unwrap();
        assert!(read_record(&mut b).is_err(), "an oversized reason");
    }
}
