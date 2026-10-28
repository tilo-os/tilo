//! The source side of a Wi-Fi Display session: the RTSP exchange that
//! negotiates a stream with a sink (M1 to M7), keeps it alive (M16) and ends
//! it (M5 with a teardown trigger, M8), and the media lifecycle it drives.
//!
//! The source owns the exchange: once the TCP connection exists, it sends
//! `OPTIONS` (M1), answers the sink's `OPTIONS` (M2), asks for the sink's
//! capabilities (M3), sets the chosen formats (M4) and triggers the sink's
//! `SETUP` (M5). The sink then sends `SETUP` (M6) and `PLAY` (M7), and the
//! media starts.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt::{self, Write as _};
use std::io;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::modes::{H264Level, ModePreference, SinkVideo, VideoMode, choose_mode};
use crate::params::{self, AudioCodec, SinkAudio, names};
use crate::rtsp::{Message, MessageReader};

/// The request URI the source uses for its requests (M3, M4, M5).
const CONTROL_URI: &str = "rtsp://localhost/wfd1.0";
/// How long the session stays valid without a keep-alive, announced in the
/// `Session` header.
const SESSION_TIMEOUT_SECS: u64 = 30;
/// How often the session loop wakes to send keep-alives and notice a stop.
const POLL: Duration = Duration::from_millis(100);
/// How long a stopping session waits for the sink's `TEARDOWN`.
const TEARDOWN_GRACE: Duration = Duration::from_secs(2);

/// Which audio the source sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioPreference {
    /// Video only.
    Off,
    /// AAC when the sink decodes it, LPCM otherwise.
    Auto,
    /// LPCM when the sink decodes it, AAC otherwise. Microsoft's Wireless
    /// Display Adapter plays only LPCM.
    PreferLpcm,
}

/// How a source session behaves.
#[derive(Clone, Debug)]
pub struct SourceOptions {
    /// The largest frame and highest rate to send.
    pub preference: ModePreference,
    /// Which audio to send.
    pub audio: AudioPreference,
    /// How often to send the M16 keep-alive once playing; `None` sends none.
    /// Microsoft's adapter answers a keep-alive by tearing the session down.
    pub keepalive: Option<Duration>,
    /// The UDP port to send RTP from; the next free even port at or above it
    /// is used.
    pub rtp_port: u16,
    /// How long the sink may take from the connection to `PLAY`.
    pub negotiation_timeout: Duration,
}

impl Default for SourceOptions {
    fn default() -> Self {
        Self {
            preference: ModePreference {
                max_size: None,
                max_fps: 30,
            },
            audio: AudioPreference::Auto,
            keepalive: Some(Duration::from_secs(20)),
            rtp_port: 19_002,
            negotiation_timeout: Duration::from_secs(30),
        }
    }
}

/// Everything the media path needs once the sink sent `PLAY`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPlan {
    /// Where to send RTP.
    pub sink_rtp: SocketAddr,
    /// The sink's RTCP port, 0 for none.
    pub sink_rtcp_port: u16,
    /// The local address the session runs on.
    pub local_ip: IpAddr,
    /// The UDP port to send RTP from, as announced in the `SETUP` reply.
    pub source_rtp_port: u16,
    /// The negotiated video mode.
    pub mode: VideoMode,
    /// The H.264 level announced for it.
    pub level: H264Level,
    /// The negotiated audio, if any.
    pub audio: Option<AudioCodec>,
}

/// A running stream. Dropping it stops the stream.
pub trait Media: Send {
    /// The sink lost data and asks for an IDR frame.
    fn request_idr(&mut self) {}

    /// Why the stream stopped on its own (the capture ended, the encoder
    /// failed), checked while the session runs.
    fn failure(&mut self) -> Option<String> {
        None
    }
}

/// Why media could not start.
pub type MediaError = Box<dyn StdError + Send + Sync>;

/// Starts streams for a session.
pub trait MediaFactory {
    /// Starts sending what `plan` describes. Returns once the stream flows.
    ///
    /// # Errors
    ///
    /// The stream could not start.
    fn start(&mut self, plan: &MediaPlan) -> Result<Box<dyn Media>, MediaError>;
}

/// What a session reports as it goes.
#[derive(Debug)]
#[non_exhaustive]
pub enum SourceEvent<'a> {
    /// The sink's capabilities are known and the formats are chosen.
    Negotiated {
        /// The video mode.
        mode: VideoMode,
        /// The audio, if any.
        audio: Option<AudioCodec>,
    },
    /// The sink accepted `PLAY` and the media stream started.
    Playing(&'a MediaPlan),
    /// The sink paused the stream.
    Paused,
    /// The sink refused the M16 keep-alive; no more are sent.
    KeepaliveRefused(u16),
}

/// How a session ended without an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEnd {
    /// The sink sent `TEARDOWN`.
    SinkTeardown,
    /// The source was asked to stop.
    Stopped,
    /// The sink closed the connection.
    Closed,
}

/// Why a session failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum SourceError {
    /// The connection failed.
    Io(io::Error),
    /// The sink sent something the session cannot go on from.
    Protocol(String),
    /// The sink answered a request with an error status.
    Rejected {
        /// Which request.
        request: &'static str,
        /// The status it answered with.
        status: u16,
    },
    /// The media did not start.
    Media(MediaError),
    /// The sink went quiet.
    Timeout(&'static str),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "Wi-Fi Display connection failed: {error}"),
            Self::Protocol(detail) => write!(f, "Wi-Fi Display protocol error: {detail}"),
            Self::Rejected { request, status } => {
                write!(f, "the receiver refused {request} (RTSP {status})")
            }
            Self::Media(error) => write!(f, "the media stream did not start: {error}"),
            Self::Timeout(what) => write!(f, "the receiver did not answer: {what}"),
        }
    }
}

impl StdError for SourceError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Media(error) => Some(&**error),
            _ => None,
        }
    }
}

impl From<io::Error> for SourceError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Stops a running session from another thread.
#[derive(Clone, Debug)]
pub struct StopHandle(Arc<AtomicBool>);

impl StopHandle {
    /// Asks the session to end: a negotiated session triggers the sink's
    /// `TEARDOWN` first, briefly waiting for it.
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Options,
    GetParameters,
    SetParameters,
    TriggerSetup,
    TriggerTeardown,
    Keepalive,
}

impl Pending {
    fn name(self) -> &'static str {
        match self {
            Self::Options => "OPTIONS (M1)",
            Self::GetParameters => "GET_PARAMETER (M3)",
            Self::SetParameters => "SET_PARAMETER (M4)",
            Self::TriggerSetup => "the SETUP trigger (M5)",
            Self::TriggerTeardown => "the TEARDOWN trigger (M5)",
            Self::Keepalive => "the keep-alive (M16)",
        }
    }
}

/// The negotiated formats, known after M3.
#[derive(Clone, Copy, Debug)]
struct Formats {
    mode: VideoMode,
    level: H264Level,
    audio: Option<AudioCodec>,
    sink_rtp_port: u16,
    sink_rtcp_port: u16,
}

/// One source session on an established TCP connection.
pub struct Source {
    reader: MessageReader<TcpStream>,
    writer: TcpStream,
    options: SourceOptions,
    stop: Arc<AtomicBool>,
    local: SocketAddr,
    peer: SocketAddr,
    session_id: String,
    next_cseq: u32,
    pending: HashMap<u32, Pending>,
    options_answered: bool,
    sink_options_seen: bool,
    capabilities_requested: bool,
    formats: Option<Formats>,
    sending_port: Option<u16>,
    /// Holds the RTP port pair from `SETUP` until the media binds it, so no
    /// other process takes it in between.
    reserved: Vec<UdpSocket>,
    /// Keeps other sessions in this process off the pair for the whole
    /// session: the media binds it some time after the reservation ends.
    claim: Option<PortClaim>,
    set_up: bool,
    media: Option<Box<dyn Media>>,
    playing_since: Option<Instant>,
    keepalive: Option<Duration>,
    last_keepalive: Instant,
}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("local", &self.local)
            .field("peer", &self.peer)
            .field("set_up", &self.set_up)
            .field("playing", &self.media.is_some())
            .finish_non_exhaustive()
    }
}

impl Source {
    /// A session on `stream`, which reaches the sink: the connection the
    /// sink opened to the source's RTSP port, or one the source opened to
    /// the sink's.
    ///
    /// # Errors
    ///
    /// The socket could not be configured.
    pub fn new(stream: TcpStream, options: SourceOptions) -> io::Result<Self> {
        stream.set_read_timeout(Some(POLL))?;
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?;
        let peer = stream.peer_addr()?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: MessageReader::new(stream),
            writer,
            keepalive: options.keepalive,
            options,
            stop: Arc::new(AtomicBool::new(false)),
            local,
            peer,
            session_id: session_id(),
            next_cseq: 1,
            pending: HashMap::new(),
            options_answered: false,
            sink_options_seen: false,
            capabilities_requested: false,
            formats: None,
            sending_port: None,
            reserved: Vec::new(),
            claim: None,
            set_up: false,
            media: None,
            playing_since: None,
            last_keepalive: Instant::now(),
        })
    }

    /// The sink's address.
    #[must_use]
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// A handle that stops this session.
    #[must_use]
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle(Arc::clone(&self.stop))
    }

    /// Runs the session until the sink tears it down, closes the connection
    /// or the source is stopped. The media stops before this returns.
    ///
    /// # Errors
    ///
    /// The connection failed, the sink refused a request or went quiet, or
    /// the media did not start.
    pub fn run(
        mut self,
        media: &mut dyn MediaFactory,
        events: &mut dyn FnMut(SourceEvent<'_>),
    ) -> Result<SessionEnd, SourceError> {
        let result = self.drive(media, events);
        // Stop the stream before the connection closes.
        self.media = None;
        result
    }

    fn drive(
        &mut self,
        media: &mut dyn MediaFactory,
        events: &mut dyn FnMut(SourceEvent<'_>),
    ) -> Result<SessionEnd, SourceError> {
        let started = Instant::now();
        let mut teardown_deadline: Option<Instant> = None;
        self.send_request(
            Pending::Options,
            "OPTIONS",
            "*",
            &[("Require", "org.wfa.wfd1.0")],
            "",
        )?;
        loop {
            if self.stop.load(Ordering::Acquire) && teardown_deadline.is_none() {
                if !self.set_up {
                    return Ok(SessionEnd::Stopped);
                }
                self.send_request(
                    Pending::TriggerTeardown,
                    "SET_PARAMETER",
                    CONTROL_URI,
                    &[],
                    "wfd_trigger_method: TEARDOWN\r\n",
                )?;
                teardown_deadline = Some(Instant::now() + TEARDOWN_GRACE);
            }
            if teardown_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(SessionEnd::Stopped);
            }
            if self.playing_since.is_none()
                && teardown_deadline.is_none()
                && started.elapsed() > self.options.negotiation_timeout
            {
                return Err(SourceError::Timeout("negotiation did not reach PLAY"));
            }
            self.maybe_keepalive()?;
            if let Some(failure) = self.media.as_mut().and_then(|media| media.failure()) {
                return Err(SourceError::Media(failure.into()));
            }
            let message = match self.reader.read_message() {
                Ok(Some(message)) => message,
                Ok(None) => {
                    return Ok(if teardown_deadline.is_some() {
                        SessionEnd::Stopped
                    } else {
                        SessionEnd::Closed
                    });
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            if message.method().is_some() {
                if let Some(end) = self.handle_request(&message, media, events)? {
                    return Ok(if teardown_deadline.is_some() {
                        SessionEnd::Stopped
                    } else {
                        end
                    });
                }
            } else {
                self.handle_response(&message, events)?;
            }
        }
    }

    fn maybe_keepalive(&mut self) -> Result<(), SourceError> {
        let (Some(interval), Some(_)) = (self.keepalive, self.playing_since) else {
            return Ok(());
        };
        if self.last_keepalive.elapsed() < interval {
            return Ok(());
        }
        if self
            .pending
            .values()
            .any(|pending| *pending == Pending::Keepalive)
        {
            return Err(SourceError::Timeout("the keep-alive (M16) went unanswered"));
        }
        let session = self.session_header();
        let uri = self.presentation_uri();
        self.send_request(
            Pending::Keepalive,
            "GET_PARAMETER",
            &uri,
            &[("Session", session.as_str())],
            "",
        )?;
        self.last_keepalive = Instant::now();
        Ok(())
    }

    fn handle_response(
        &mut self,
        response: &Message,
        events: &mut dyn FnMut(SourceEvent<'_>),
    ) -> Result<(), SourceError> {
        let Some(pending) = response.cseq().and_then(|cseq| self.pending.remove(&cseq)) else {
            // An answer to nothing we asked: ignore it rather than fail.
            return Ok(());
        };
        let status = response.status().unwrap_or(0);
        if status != 200 {
            return match pending {
                Pending::Keepalive => {
                    self.keepalive = None;
                    events(SourceEvent::KeepaliveRefused(status));
                    Ok(())
                }
                Pending::TriggerTeardown => Ok(()),
                other => Err(SourceError::Rejected {
                    request: other.name(),
                    status,
                }),
            };
        }
        match pending {
            Pending::Options => {
                self.options_answered = true;
                self.maybe_request_capabilities()?;
            }
            Pending::GetParameters => {
                let formats = self.choose_formats(&response.body)?;
                events(SourceEvent::Negotiated {
                    mode: formats.mode,
                    audio: formats.audio,
                });
                self.formats = Some(formats);
                let body = self.set_parameters_body(&formats);
                self.send_request(
                    Pending::SetParameters,
                    "SET_PARAMETER",
                    CONTROL_URI,
                    &[],
                    &body,
                )?;
            }
            Pending::SetParameters => {
                self.send_request(
                    Pending::TriggerSetup,
                    "SET_PARAMETER",
                    CONTROL_URI,
                    &[],
                    "wfd_trigger_method: SETUP\r\n",
                )?;
            }
            Pending::TriggerSetup | Pending::TriggerTeardown | Pending::Keepalive => {}
        }
        Ok(())
    }

    fn maybe_request_capabilities(&mut self) -> Result<(), SourceError> {
        if self.capabilities_requested || !self.options_answered || !self.sink_options_seen {
            return Ok(());
        }
        self.capabilities_requested = true;
        let body = [
            names::CONTENT_PROTECTION,
            names::VIDEO_FORMATS,
            names::AUDIO_CODECS,
            names::CLIENT_RTP_PORTS,
        ]
        .iter()
        .fold(String::new(), |mut body, name| {
            let _ = write!(body, "{name}\r\n");
            body
        });
        self.send_request(
            Pending::GetParameters,
            "GET_PARAMETER",
            CONTROL_URI,
            &[],
            &body,
        )
    }

    fn choose_formats(&self, body: &str) -> Result<Formats, SourceError> {
        let values = params::parse(body);
        let (sink_rtp_port, sink_rtcp_port) = params::get(&values, names::CLIENT_RTP_PORTS)
            .and_then(params::parse_client_rtp_ports)
            .ok_or_else(|| {
                SourceError::Protocol("the receiver named no RTP port in M3".to_owned())
            })?;
        let sink_video: Option<SinkVideo> =
            params::get(&values, names::VIDEO_FORMATS).and_then(params::parse_sink_video);
        let mode = choose_mode(sink_video.as_ref(), &self.options.preference);
        let level = mode.minimum_level().unwrap_or(H264Level::L5_1);
        let sink_audio = params::get(&values, names::AUDIO_CODECS).map(params::parse_sink_audio);
        Ok(Formats {
            mode,
            level,
            audio: choose_audio(self.options.audio, sink_audio),
            sink_rtp_port,
            sink_rtcp_port,
        })
    }

    fn set_parameters_body(&self, formats: &Formats) -> String {
        format!(
            "{}: none\r\n{}: {}\r\n{}: {}\r\n{}: {}/streamid=0 none\r\n{}: {}\r\n",
            names::CONTENT_PROTECTION,
            names::VIDEO_FORMATS,
            params::format_video(&formats.mode, formats.level),
            names::AUDIO_CODECS,
            params::format_audio(formats.audio),
            names::PRESENTATION_URL,
            self.presentation_uri(),
            names::CLIENT_RTP_PORTS,
            params::format_client_rtp_ports(formats.sink_rtp_port, formats.sink_rtcp_port),
        )
    }

    fn handle_request(
        &mut self,
        request: &Message,
        media: &mut dyn MediaFactory,
        events: &mut dyn FnMut(SourceEvent<'_>),
    ) -> Result<Option<SessionEnd>, SourceError> {
        match request.method().unwrap_or_default() {
            "OPTIONS" => {
                self.respond(Message::response_to(request, 200).with_header(
                    "Public",
                    "org.wfa.wfd1.0, SETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER, SET_PARAMETER",
                ))?;
                self.sink_options_seen = true;
                self.maybe_request_capabilities()?;
            }
            "GET_PARAMETER" => {
                let body = self.answer_parameters(&request.body);
                let response = self.with_session(Message::response_to(request, 200));
                self.respond(response.with_body(body))?;
            }
            "SET_PARAMETER" => {
                if params::requested_names(&request.body)
                    .iter()
                    .any(|line| line.starts_with(names::IDR_REQUEST))
                    && let Some(stream) = self.media.as_mut()
                {
                    stream.request_idr();
                }
                let response = self.with_session(Message::response_to(request, 200));
                self.respond(response)?;
            }
            "SETUP" => self.setup(request)?,
            "PLAY" => self.play(request, media, events)?,
            "PAUSE" => {
                let response = self.with_session(Message::response_to(request, 200));
                self.respond(response)?;
                if self.media.take().is_some() {
                    self.playing_since = None;
                    events(SourceEvent::Paused);
                }
            }
            "TEARDOWN" => {
                self.media = None;
                let response = self
                    .with_session(Message::response_to(request, 200))
                    .with_header("Connection", "close");
                self.respond(response)?;
                return Ok(Some(SessionEnd::SinkTeardown));
            }
            _ => self.respond(Message::response_to(request, 405))?,
        }
        Ok(None)
    }

    fn setup(&mut self, request: &Message) -> Result<(), SourceError> {
        let Some(formats) = self.formats.as_mut() else {
            return self
                .respond(Message::response_to(request, 455))
                .map_err(Into::into);
        };
        if let Some((rtp, rtcp)) = request
            .header("Transport")
            .and_then(params::parse_transport_client_ports)
        {
            formats.sink_rtp_port = rtp;
            formats.sink_rtcp_port = rtcp;
        }
        let (rtp, rtcp) = (formats.sink_rtp_port, formats.sink_rtcp_port);
        let (source_port, reserved, claim) = free_rtp_port(
            self.local.ip(),
            self.options.rtp_port,
            self.peer.ip(),
            rtp,
            rtcp,
        )?;
        self.sending_port = Some(source_port);
        self.reserved = reserved;
        self.claim = Some(claim);
        let transport = if rtcp == 0 {
            format!("RTP/AVP/UDP;unicast;client_port={rtp};server_port={source_port}")
        } else {
            format!(
                "RTP/AVP/UDP;unicast;client_port={rtp}-{rtcp};server_port={source_port}-{}",
                source_port + 1
            )
        };
        self.set_up = true;
        let response = Message::response_to(request, 200)
            .with_header("Session", self.session_header())
            .with_header("Transport", transport);
        self.respond(response).map_err(Into::into)
    }

    fn play(
        &mut self,
        request: &Message,
        media: &mut dyn MediaFactory,
        events: &mut dyn FnMut(SourceEvent<'_>),
    ) -> Result<(), SourceError> {
        let (Some(formats), Some(source_rtp_port), true) =
            (self.formats, self.sending_port, self.set_up)
        else {
            return self
                .respond(Message::response_to(request, 455))
                .map_err(Into::into);
        };
        let response = self
            .with_session(Message::response_to(request, 200))
            .with_header("Range", "npt=now-");
        self.respond(response)?;
        if self.media.is_some() {
            return Ok(());
        }
        let plan = MediaPlan {
            sink_rtp: SocketAddr::new(self.peer.ip(), formats.sink_rtp_port),
            sink_rtcp_port: formats.sink_rtcp_port,
            local_ip: self.local.ip(),
            source_rtp_port,
            mode: formats.mode,
            level: formats.level,
            audio: formats.audio,
        };
        self.reserved.clear();
        let stream = media.start(&plan).map_err(SourceError::Media)?;
        self.media = Some(stream);
        self.playing_since = Some(Instant::now());
        self.last_keepalive = Instant::now();
        events(SourceEvent::Playing(&plan));
        Ok(())
    }

    fn answer_parameters(&self, body: &str) -> String {
        let mut answer = String::new();
        for name in params::requested_names(body) {
            let value = match name.to_ascii_lowercase().as_str() {
                "wfd_video_formats" => self
                    .formats
                    .map(|formats| params::format_video(&formats.mode, formats.level)),
                "wfd_audio_codecs" => self
                    .formats
                    .map(|formats| params::format_audio(formats.audio).to_owned()),
                "wfd_content_protection" => Some("none".to_owned()),
                _ => None,
            };
            if let Some(value) = value {
                let _ = write!(answer, "{name}: {value}\r\n");
            }
        }
        answer
    }

    fn presentation_uri(&self) -> String {
        format!("rtsp://{}/wfd1.0", self.local)
    }

    fn session_header(&self) -> String {
        format!("{};timeout={SESSION_TIMEOUT_SECS}", self.session_id)
    }

    fn with_session(&self, response: Message) -> Message {
        if self.set_up {
            response.with_header("Session", self.session_header())
        } else {
            response
        }
    }

    fn send_request(
        &mut self,
        pending: Pending,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<(), SourceError> {
        let cseq = self.next_cseq;
        self.next_cseq += 1;
        let mut request = Message::request(method, uri, cseq).with_body(body);
        for (name, value) in headers {
            request = request.with_header(name, *value);
        }
        request.write_to(&mut self.writer)?;
        self.pending.insert(cseq, pending);
        Ok(())
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "callers build the response in place"
    )]
    fn respond(&mut self, response: Message) -> io::Result<()> {
        response.write_to(&mut self.writer)
    }
}

/// The audio to send for `preference`, given what the sink listed (`None`
/// when it did not answer for `wfd_audio_codecs`, which sinks that predate
/// the parameter do; they take AAC).
#[must_use]
pub fn choose_audio(preference: AudioPreference, sink: Option<SinkAudio>) -> Option<AudioCodec> {
    let sink = sink.unwrap_or(SinkAudio {
        lpcm_48k: false,
        aac_48k: true,
    });
    match preference {
        AudioPreference::Off => None,
        AudioPreference::PreferLpcm if sink.lpcm_48k => Some(AudioCodec::Lpcm),
        _ if sink.aac_48k => Some(AudioCodec::Aac),
        _ if sink.lpcm_48k => Some(AudioCodec::Lpcm),
        _ => None,
    }
}

/// RTP ports sessions of this process hold.
static CLAIMED: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

/// A session's claim on an RTP port pair, released when dropped.
#[derive(Debug)]
struct PortClaim(u16);

impl Drop for PortClaim {
    fn drop(&mut self) {
        if let Ok(mut claimed) = CLAIMED.lock() {
            claimed.retain(|port| *port != self.0);
        }
    }
}

/// The first even port at or above `preferred` that is free on `local`, not
/// claimed by another session of this process, and not one of the sink's
/// when the sink is on this host; with sockets that hold it and the next
/// port until dropped, and the claim.
fn free_rtp_port(
    local: IpAddr,
    preferred: u16,
    sink: IpAddr,
    sink_rtp: u16,
    sink_rtcp: u16,
) -> io::Result<(u16, Vec<UdpSocket>, PortClaim)> {
    let mut claimed = CLAIMED
        .lock()
        .map_err(|_| io::Error::other("the RTP port claims are poisoned"))?;
    let start = preferred.max(1024) & !1;
    let collides = |port: u16| {
        local == sink
            && [port, port + 1].iter().any(|candidate| {
                *candidate == sink_rtp || (sink_rtcp != 0 && *candidate == sink_rtcp)
            })
    };
    (start..u16::MAX - 1)
        .step_by(2)
        .take(512)
        .filter(|port| !collides(*port) && !claimed.contains(port))
        .find_map(|port| {
            let rtp = UdpSocket::bind((local, port)).ok()?;
            let rtcp = UdpSocket::bind((local, port + 1)).ok()?;
            Some((port, vec![rtp, rtcp]))
        })
        .map(|(port, sockets)| {
            claimed.push(port);
            (port, sockets, PortClaim(port))
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::AddrInUse, "no free RTP port pair"))
}

/// A session identifier: decimal, as sinks expect, and different per session.
fn session_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    format!("{}", 10_000_000 + hasher.finish() % 90_000_000)
}

/// Waits for a sink to connect to `listener` until `deadline`, accepting
/// only peers `accept` admits (the Wi-Fi Direct group's subnet, say) and
/// giving up early when `cancelled` says so.
///
/// # Errors
///
/// Accepting failed.
pub fn accept_sink(
    listener: &TcpListener,
    deadline: Instant,
    accept: &dyn Fn(&SocketAddr) -> bool,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Option<TcpStream>> {
    listener.set_nonblocking(true)?;
    let result = loop {
        if cancelled() || Instant::now() >= deadline {
            break Ok(None);
        }
        match listener.accept() {
            Ok((stream, peer)) if accept(&peer) => {
                stream.set_nonblocking(false)?;
                break Ok(Some(stream));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => break Err(error),
        }
    };
    listener.set_nonblocking(false)?;
    result
}

/// Gets the RTSP connection to a sink, however it chooses to make it.
///
/// Most sinks connect to the source's RTSP port; some wait for the source to
/// connect to theirs. This waits `passive_grace` for an incoming connection
/// (from a peer `accept` admits), then alternates between trying the sink's
/// port, when `active` knows it, and listening again, until `deadline` or
/// until `cancelled` says so.
///
/// # Errors
///
/// Accepting failed.
pub fn establish(
    listener: &TcpListener,
    accept: &dyn Fn(&SocketAddr) -> bool,
    active: &dyn Fn() -> Option<SocketAddr>,
    passive_grace: Duration,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Option<TcpStream>> {
    let grace = (Instant::now() + passive_grace).min(deadline);
    if let Some(stream) = accept_sink(listener, grace, accept, cancelled)? {
        return Ok(Some(stream));
    }
    while Instant::now() < deadline && !cancelled() {
        if let Some(target) = active()
            && let Ok(stream) = TcpStream::connect_timeout(&target, Duration::from_secs(2))
        {
            return Ok(Some(stream));
        }
        let listen_until = (Instant::now() + Duration::from_secs(1)).min(deadline);
        if let Some(stream) = accept_sink(listener, listen_until, accept, cancelled)? {
            return Ok(Some(stream));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_follows_the_preference_and_what_the_sink_decodes() {
        let both = Some(SinkAudio {
            lpcm_48k: true,
            aac_48k: true,
        });
        let lpcm_only = Some(SinkAudio {
            lpcm_48k: true,
            aac_48k: false,
        });
        let nothing = Some(SinkAudio::default());
        assert_eq!(
            choose_audio(AudioPreference::Auto, both),
            Some(AudioCodec::Aac)
        );
        assert_eq!(
            choose_audio(AudioPreference::PreferLpcm, both),
            Some(AudioCodec::Lpcm)
        );
        assert_eq!(
            choose_audio(AudioPreference::Auto, lpcm_only),
            Some(AudioCodec::Lpcm)
        );
        assert_eq!(choose_audio(AudioPreference::Auto, nothing), None);
        assert_eq!(
            choose_audio(AudioPreference::Auto, None),
            Some(AudioCodec::Aac)
        );
        assert_eq!(choose_audio(AudioPreference::Off, both), None);
    }

    #[test]
    fn the_rtp_port_avoids_a_sink_on_the_same_host() {
        let local = IpAddr::from([127, 0, 0, 1]);
        let (port, held, claim) = free_rtp_port(local, 19_002, local, 19_002, 0).expect("port");
        assert!(port >= 19_004 && port.is_multiple_of(2));
        // While held, the next session gets another pair, and still while
        // claimed once the sockets are gone.
        let (next, _, _) = free_rtp_port(local, 19_002, local, 19_002, 0).expect("port");
        assert_ne!(next, port);
        drop(held);
        let (after, _, _) = free_rtp_port(local, 19_002, local, 19_002, 0).expect("port");
        assert_ne!(after, port);
        drop(claim);
        let other = IpAddr::from([192, 168, 49, 1]);
        assert_eq!(
            free_rtp_port(local, 19_001, other, 19_002, 0)
                .expect("port")
                .0
                % 2,
            0
        );
    }
}
