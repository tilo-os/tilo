//! A strict minimal sink, for testing sources: it connects to a source's
//! RTSP port, negotiates as a TV would (M1 to M7, checking each message the
//! source sends), then receives the RTP stream and reads the transport
//! stream inside it.

use std::fmt::Write as _;
use std::io;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use crate::params::{self, names};
use crate::rtsp::{Message, MessageReader};
use crate::ts::{PACKET_SIZE, RTP_PAYLOAD_MP2T, TsStats};

/// What the sink announces in its M3 answer.
#[derive(Clone, Debug)]
pub struct SinkCapabilities {
    /// `wfd_video_formats`
    pub video_formats: String,
    /// `wfd_audio_codecs`, or nothing to leave the parameter out.
    pub audio_codecs: Option<String>,
}

impl Default for SinkCapabilities {
    /// A 1080p TV: CEA 640x480p60, 720p30/60 and 1080p30/60 at up to level
    /// 4.2 in constrained baseline; AAC and LPCM.
    fn default() -> Self {
        Self {
            video_formats: "00 00 01 1f 000001e1 00000000 00000000 00 0000 0000 00 none none"
                .to_owned(),
            audio_codecs: Some("LPCM 00000002 00, AAC 00000001 00".to_owned()),
        }
    }
}

/// What the source chose in M4.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negotiated {
    /// `wfd_video_formats` as the source set it.
    pub video_formats: String,
    /// `wfd_audio_codecs` as the source set it.
    pub audio_codecs: String,
    /// The URL the sink set the session up on.
    pub presentation_url: String,
    /// The source's RTP port from the `SETUP` reply.
    pub server_port: Option<u16>,
}

/// A sink with a negotiated, playing session.
#[derive(Debug)]
pub struct TestSink {
    reader: MessageReader<TcpStream>,
    writer: TcpStream,
    rtp: UdpSocket,
    cseq: u32,
    session: String,
    negotiated: Negotiated,
    /// RTP packets received.
    pub rtp_packets: u64,
    /// RTP packets that were not MPEG-TS over RTP.
    pub bad_rtp_packets: u64,
    /// What the received transport stream contained.
    pub ts: TsStats,
}

fn protocol(detail: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, detail)
}

impl TestSink {
    /// Connects to the source at `source`, negotiates with `capabilities`
    /// and sends `PLAY`. Every step must happen before `timeout` passes.
    ///
    /// # Errors
    ///
    /// The source broke the protocol, refused a request or was too slow.
    pub fn connect_and_play(
        source: SocketAddr,
        capabilities: &SinkCapabilities,
        timeout: Duration,
    ) -> io::Result<Self> {
        let stream = TcpStream::connect_timeout(&source, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        let local_ip = stream.local_addr()?.ip();
        let rtp = UdpSocket::bind((local_ip, 0))?;
        let rtp_port = rtp.local_addr()?.port();
        let writer = stream.try_clone()?;
        let mut sink = Self {
            reader: MessageReader::new(stream),
            writer,
            rtp,
            cseq: 0,
            session: String::new(),
            negotiated: Negotiated {
                video_formats: String::new(),
                audio_codecs: String::new(),
                presentation_url: String::new(),
                server_port: None,
            },
            rtp_packets: 0,
            bad_rtp_packets: 0,
            ts: TsStats::default(),
        };
        let deadline = Instant::now() + timeout;

        // M1: the source's OPTIONS must require WFD.
        let m1 = sink.expect_request("OPTIONS", deadline)?;
        if m1.header("Require") != Some("org.wfa.wfd1.0") {
            return Err(protocol(format!(
                "M1 does not require org.wfa.wfd1.0: {m1:?}"
            )));
        }
        sink.reply(
            &m1,
            "Public",
            "org.wfa.wfd1.0, SET_PARAMETER, GET_PARAMETER",
        )?;

        // M2: ours; its answer must list the methods a source serves.
        let m2 = sink.request("OPTIONS", "*", &[("Require", "org.wfa.wfd1.0")])?;
        let public = sink.expect_response(m2, deadline)?;
        let methods = public.header("Public").unwrap_or_default().to_owned();
        for method in ["org.wfa.wfd1.0", "SETUP", "PLAY", "TEARDOWN"] {
            if !methods.contains(method) {
                return Err(protocol(format!("M2 answer lacks {method}: {methods:?}")));
            }
        }

        // M3: the source asks for our capabilities.
        let m3 = sink.expect_request("GET_PARAMETER", deadline)?;
        let asked = params::requested_names(&m3.body);
        let mut answer = String::new();
        for name in &asked {
            let value = match name.as_str() {
                names::VIDEO_FORMATS => Some(capabilities.video_formats.clone()),
                names::AUDIO_CODECS => capabilities.audio_codecs.clone(),
                names::CLIENT_RTP_PORTS => Some(params::format_client_rtp_ports(rtp_port, 0)),
                names::CONTENT_PROTECTION => Some("none".to_owned()),
                _ => None,
            };
            if let Some(value) = value {
                let _ = write!(answer, "{name}: {value}\r\n");
            }
        }
        if !asked.iter().any(|name| name == names::CLIENT_RTP_PORTS) {
            return Err(protocol(format!(
                "M3 does not ask for the RTP ports: {asked:?}"
            )));
        }
        sink.send(Message::response_to(&m3, 200).with_body(answer))?;

        // M4: the chosen formats.
        let m4 = sink.expect_request("SET_PARAMETER", deadline)?;
        let chosen = params::parse(&m4.body);
        let get = |name: &str| params::get(&chosen, name).map(str::to_owned);
        sink.negotiated.video_formats = get(names::VIDEO_FORMATS)
            .ok_or_else(|| protocol("M4 sets no wfd_video_formats".to_owned()))?;
        sink.negotiated.audio_codecs = get(names::AUDIO_CODECS).unwrap_or_default();
        let url = get(names::PRESENTATION_URL)
            .ok_or_else(|| protocol("M4 sets no wfd_presentation_URL".to_owned()))?;
        url.split_whitespace()
            .next()
            .unwrap_or_default()
            .clone_into(&mut sink.negotiated.presentation_url);
        let ports =
            get(names::CLIENT_RTP_PORTS).and_then(|value| params::parse_client_rtp_ports(&value));
        if ports.map(|(rtp, _)| rtp) != Some(rtp_port) {
            return Err(protocol(format!(
                "M4 names RTP ports {ports:?}, not {rtp_port}"
            )));
        }
        sink.send(Message::response_to(&m4, 200))?;

        // M5: the SETUP trigger.
        let m5 = sink.expect_request("SET_PARAMETER", deadline)?;
        if params::get(&params::parse(&m5.body), names::TRIGGER_METHOD) != Some("SETUP") {
            return Err(protocol(format!("expected the SETUP trigger, got {m5:?}")));
        }
        sink.send(Message::response_to(&m5, 200))?;

        // M6: SETUP on the presentation URL.
        let transport = format!("RTP/AVP/UDP;unicast;client_port={rtp_port}");
        let url = sink.negotiated.presentation_url.clone();
        let m6 = sink.request("SETUP", &url, &[("Transport", transport.as_str())])?;
        let setup = sink.expect_response(m6, deadline)?;
        setup
            .session_id()
            .ok_or_else(|| protocol("the SETUP answer has no Session".to_owned()))?
            .clone_into(&mut sink.session);
        sink.negotiated.server_port = setup.header("Transport").and_then(|transport| {
            transport
                .split(';')
                .find_map(|part| part.trim().strip_prefix("server_port="))
                .and_then(|ports| ports.split('-').next())
                .and_then(|port| port.parse().ok())
        });

        // M7: PLAY.
        let session = sink.session.clone();
        let m7 = sink.request("PLAY", &url, &[("Session", session.as_str())])?;
        sink.expect_response(m7, deadline)?;
        Ok(sink)
    }

    /// What the source chose.
    #[must_use]
    pub fn negotiated(&self) -> &Negotiated {
        &self.negotiated
    }

    /// Receives RTP until `done` holds for the stream so far or `timeout`
    /// passes, answering the source's keep-alives meanwhile. Returns whether
    /// `done` held.
    ///
    /// # Errors
    ///
    /// The control connection failed.
    pub fn receive_until(
        &mut self,
        timeout: Duration,
        done: impl Fn(&Self) -> bool,
    ) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        self.rtp.set_read_timeout(Some(Duration::from_millis(50)))?;
        self.reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(1)))?;
        let mut buffer = vec![0_u8; 2048];
        while Instant::now() < deadline {
            if done(self) {
                return Ok(true);
            }
            match self.rtp.recv(&mut buffer) {
                Ok(length) => self.rtp_packet(&buffer[..length]),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error),
            }
            self.answer_control()?;
        }
        Ok(done(self))
    }

    fn rtp_packet(&mut self, packet: &[u8]) {
        self.rtp_packets += 1;
        let valid = packet.len() >= 12
            && packet[0] >> 6 == 2
            && packet[1] & 0x7f == RTP_PAYLOAD_MP2T
            && (packet.len() - 12).is_multiple_of(PACKET_SIZE);
        if valid {
            self.ts.read(&packet[12..]);
        } else {
            self.bad_rtp_packets += 1;
        }
    }

    fn answer_control(&mut self) -> io::Result<()> {
        match self.reader.read_message() {
            Ok(Some(message)) if message.method().is_some() => {
                self.send(Message::response_to(&message, 200))
            }
            Ok(Some(_)) => Ok(()),
            // The source ended the session: no more media is coming.
            Ok(None) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the source closed the session",
            )),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Sends `TEARDOWN` and waits for its answer.
    ///
    /// # Errors
    ///
    /// The source refused or did not answer.
    pub fn teardown(mut self, timeout: Duration) -> io::Result<()> {
        self.reader.get_ref().set_read_timeout(Some(timeout))?;
        let url = self.negotiated.presentation_url.clone();
        let session = self.session.clone();
        let cseq = self.request("TEARDOWN", &url, &[("Session", session.as_str())])?;
        self.expect_response(cseq, Instant::now() + timeout)?;
        Ok(())
    }

    /// Waits for the source's `TEARDOWN` trigger (M5), answers it and sends
    /// `TEARDOWN` (M8), as a sink does when the source ends the session.
    ///
    /// # Errors
    ///
    /// No trigger came or the source refused the teardown.
    pub fn follow_teardown_trigger(mut self, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        self.reader.get_ref().set_read_timeout(Some(timeout))?;
        loop {
            let trigger = self.expect_request("SET_PARAMETER", deadline)?;
            let method = params::get(&params::parse(&trigger.body), names::TRIGGER_METHOD)
                .map(str::to_owned);
            self.send(Message::response_to(&trigger, 200))?;
            if method.as_deref() == Some("TEARDOWN") {
                break;
            }
        }
        let url = self.negotiated.presentation_url.clone();
        let session = self.session.clone();
        let cseq = self.request("TEARDOWN", &url, &[("Session", session.as_str())])?;
        self.expect_response(cseq, deadline)?;
        Ok(())
    }

    fn request(&mut self, method: &str, uri: &str, headers: &[(&str, &str)]) -> io::Result<u32> {
        self.cseq += 1;
        let mut message = Message::request(method, uri, self.cseq);
        for (name, value) in headers {
            message = message.with_header(name, *value);
        }
        self.send(message)?;
        Ok(self.cseq)
    }

    fn reply(&mut self, request: &Message, name: &str, value: &str) -> io::Result<()> {
        self.send(Message::response_to(request, 200).with_header(name, value))
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "callers build the message in place"
    )]
    fn send(&mut self, message: Message) -> io::Result<()> {
        message.write_to(&mut self.writer)
    }

    fn next(&mut self, deadline: Instant) -> io::Result<Message> {
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.reader
            .read_message()?
            .ok_or_else(|| io::ErrorKind::UnexpectedEof.into())
    }

    /// The next request, which must be `method`. Keep-alives (an empty
    /// `GET_PARAMETER`) in between are answered.
    fn expect_request(&mut self, method: &str, deadline: Instant) -> io::Result<Message> {
        loop {
            let message = self.next(deadline)?;
            match message.method() {
                Some("GET_PARAMETER")
                    if method != "GET_PARAMETER" && message.body.trim().is_empty() =>
                {
                    self.send(Message::response_to(&message, 200))?;
                }
                Some(seen) if seen == method => return Ok(message),
                _ => return Err(protocol(format!("expected {method}, got {message:?}"))),
            }
        }
    }

    /// The answer to the request with `cseq`, which must be 200.
    fn expect_response(&mut self, cseq: u32, deadline: Instant) -> io::Result<Message> {
        loop {
            let message = self.next(deadline)?;
            if message.method().is_some() {
                // A source keep-alive or trigger crossing our request.
                self.send(Message::response_to(&message, 200))?;
                continue;
            }
            if message.cseq() != Some(cseq) {
                return Err(protocol(format!(
                    "answer to CSeq {cseq} expected, got {message:?}"
                )));
            }
            if message.status() != Some(200) {
                return Err(protocol(format!("request {cseq} refused: {message:?}")));
            }
            return Ok(message);
        }
    }
}
