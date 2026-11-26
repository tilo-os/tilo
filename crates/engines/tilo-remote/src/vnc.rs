//! A VNC client: an RFB session over a plain TCP socket, its framebuffer and its input.
//!
//! The session connects on a thread of its own and keeps the newest framebuffer; every update
//! is handed to whoever asked for it, in this device's own pixels, and the person's pointer and
//! keys are sent back through a bounded queue so a busy server never stalls the window.
//!
//! A refused connection, a refused sign-in and a server that hangs up each say what happened in
//! words: a session that ended is never reported as one that failed, and neither as one still
//! running.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::input::VncInput;
use crate::rfb::{self, Framebuffer, PixelFormat, ServerInit};

/// Bound input queued while a VNC server is busy with a framebuffer read. The producer uses
/// non-blocking `try_send`, so pointer motion can never stall the window or consume unbounded
/// memory.
const VNC_INPUT_QUEUE_CAPACITY: usize = 256;
/// How long the read waits for a message before looking at the input queue and the update clock.
const READ_TIMEOUT: Duration = Duration::from_millis(100);
/// How often an idle session asks the server for changes, so another program's pointer on the
/// remote desktop shows without an input from here.
const UPDATE_EVERY: Duration = Duration::from_millis(500);

/// How a VNC session stands, as the window says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VncStatus {
    /// The handshake or the sign-in is in flight.
    Connecting,
    /// The framebuffer is live, `width`×`height`.
    Connected { width: u16, height: u16 },
    /// The server closed the session.
    Closed,
    /// The connection or the sign-in failed, in words a person reads.
    Failed(String),
}

/// One saved VNC machine and what it signs in with.
#[derive(Clone, Debug)]
pub struct VncConfig {
    pub host: String,
    pub port: u16,
    /// The password for `VNC Authentication`; `None` uses the `None` security type.
    pub password: Option<String>,
}

/// A live VNC session: its status, its size, and the queue its input goes through.
pub struct VncSession {
    status: Arc<Mutex<VncStatus>>,
    size: Arc<Mutex<Option<(u16, u16)>>>,
    input_tx: SyncSender<VncInput>,
    _worker: std::thread::JoinHandle<()>,
}

impl VncSession {
    /// Start connecting; answers at once, with a session whose status is `Connecting`. `said` is
    /// told each way the session stands, and `saw` each framebuffer as it changes.
    ///
    /// # Errors
    ///
    /// Why the worker thread could not start. A connection that fails is told to `said`.
    pub fn start(
        cfg: VncConfig,
        said: Box<dyn Fn(VncStatus) + Send + Sync>,
        saw: Box<dyn Fn(Framebuffer) + Send + Sync>,
    ) -> Result<Self, String> {
        let status = Arc::new(Mutex::new(VncStatus::Connecting));
        let size: Arc<Mutex<Option<(u16, u16)>>> = Arc::default();
        let (input_tx, input_rx) = sync_channel::<VncInput>(VNC_INPUT_QUEUE_CAPACITY);
        let status_worker = Arc::clone(&status);
        let size_worker = Arc::clone(&size);
        let worker = std::thread::Builder::new()
            .name("remote-vnc".into())
            .spawn(move || {
                let ended =
                    run_client(&cfg, &status_worker, &size_worker, &input_rx, &*saw, &*said);
                let final_status = {
                    let now = status_worker
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    match (ended, now) {
                        (Ok(()), _) => VncStatus::Closed,
                        (Err(error), VncStatus::Failed(_)) => VncStatus::Failed(error.to_string()),
                        (Err(error), _) if error.kind() == io::ErrorKind::UnexpectedEof => {
                            VncStatus::Closed
                        }
                        (Err(error), _) => VncStatus::Failed(friendly(&error)),
                    }
                };
                *status_worker.lock().unwrap_or_else(PoisonError::into_inner) =
                    final_status.clone();
                said(final_status);
            })
            .map_err(|error| format!("could not start the VNC session: {error}"))?;
        Ok(Self {
            status,
            size,
            input_tx,
            _worker: worker,
        })
    }

    /// How the session stands now.
    #[must_use]
    pub fn status(&self) -> VncStatus {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The remote framebuffer's size, once the server said it.
    #[must_use]
    pub fn size(&self) -> Option<(u16, u16)> {
        *self.size.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue input without blocking the window.
    ///
    /// # Errors
    ///
    /// The session's queue is full or the session is closed.
    pub fn input(&self, input: VncInput) -> Result<(), String> {
        self.input_tx.try_send(input).map_err(|error| match error {
            TrySendError::Full(_) => {
                "VNC input is busy; wait for the remote desktop to catch up".to_owned()
            }
            TrySendError::Disconnected(_) => "VNC session input channel is closed".to_owned(),
        })
    }
}

/// `error` in words a person reads.
fn friendly(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::ConnectionRefused => "Connection refused".to_owned(),
        io::ErrorKind::TimedOut => "Connection timed out".to_owned(),
        _ => error.to_string(),
    }
}

fn write_input(stream: &mut TcpStream, input: VncInput) -> io::Result<()> {
    match input {
        VncInput::Pointer { button_mask, x, y } => {
            stream.write_all(&rfb::encode_pointer_event(button_mask, x, y))
        }
        VncInput::Key { down, key } => stream.write_all(&rfb::encode_key_event(down, key)),
    }
}

/// The full client body: handshake, then the read/apply/input loop.
fn run_client(
    cfg: &VncConfig,
    status: &Arc<Mutex<VncStatus>>,
    size: &Arc<Mutex<Option<(u16, u16)>>>,
    input_rx: &Receiver<VncInput>,
    saw: &(dyn Fn(Framebuffer) + Send + Sync),
    said: &(dyn Fn(VncStatus) + Send + Sync),
) -> io::Result<()> {
    let mut stream = TcpStream::connect((cfg.host.as_str(), cfg.port))?;
    stream.set_nodelay(true).ok();

    let mut ver = [0u8; 12];
    stream.read_exact(&mut ver)?;
    let (maj, min) = rfb::parse_protocol_version(&ver)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad ProtocolVersion"))?;
    stream.write_all(&rfb::client_protocol_version(maj, min))?;

    negotiate_security(&mut stream, min, cfg.password.as_deref())?;

    stream.write_all(&[1u8])?;

    let server_init = ServerInit::read(&mut stream)?;

    let fmt = PixelFormat::rgb888();
    stream.write_all(&rfb::encode_set_pixel_format(&fmt))?;
    stream.write_all(&rfb::encode_set_encodings(&[
        rfb::encoding::RAW,
        rfb::encoding::COPY_RECT,
        rfb::encoding::RRE,
    ]))?;

    let mut fb = Framebuffer::new(server_init.width, server_init.height);
    request_update(&mut stream, false, &fb)?;

    let connected = VncStatus::Connected {
        width: server_init.width,
        height: server_init.height,
    };
    *status.lock().unwrap_or_else(PoisonError::into_inner) = connected.clone();
    said(connected);
    *size.lock().unwrap_or_else(PoisonError::into_inner) = Some((fb.width, fb.height));
    saw(fb.clone());

    let mut last_request = Instant::now();
    loop {
        loop {
            match input_rx.try_recv() {
                Ok(input) => write_input(&mut stream, input)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()), // the window dropped it
            }
        }

        // Read the next message type with a short timeout so input stays responsive when the
        // server is idle. Only the type byte is read under a timeout; the body is read blocking
        // so framing is intact.
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        let mut ty = [0u8; 1];
        match stream.read_exact(&mut ty) {
            Ok(()) => {}
            Err(error) if is_timeout(&error) => {
                if last_request.elapsed() >= UPDATE_EVERY {
                    stream.set_read_timeout(None)?;
                    request_update(&mut stream, true, &fb)?;
                    last_request = Instant::now();
                }
                continue;
            }
            Err(error) => return Err(error),
        }

        stream.set_read_timeout(None)?;
        match ty[0] {
            0 => {
                rfb::decode_framebuffer_update(&mut stream, &mut fb, &fmt)?;
                saw(fb.clone());
                request_update(&mut stream, true, &fb)?;
                last_request = Instant::now();
            }
            1 => read_set_colour_map(&mut stream)?,
            2 => {} // Bell — nothing to render.
            3 => read_server_cut_text(&mut stream)?,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unknown server message {other}"),
                ));
            }
        }
    }
}

fn request_update(stream: &mut TcpStream, incremental: bool, fb: &Framebuffer) -> io::Result<()> {
    stream.write_all(&rfb::encode_framebuffer_update_request(
        incremental,
        0,
        0,
        fb.width,
        fb.height,
    ))
}

/// Negotiate the security handshake. Supports the 3.7+ type list plus the 3.3 single type, the
/// `None` type and `VNC Authentication`.
fn negotiate_security(stream: &mut TcpStream, minor: u8, password: Option<&str>) -> io::Result<()> {
    let chosen = if minor >= 7 {
        let mut count = [0u8; 1];
        stream.read_exact(&mut count)?;
        if count[0] == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                read_reason(stream)?,
            ));
        }
        let mut types = vec![0u8; count[0] as usize];
        stream.read_exact(&mut types)?;
        let chosen = rfb::choose_security(&types).ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "no supported VNC security type")
        })?;
        stream.write_all(&[chosen])?;
        chosen
    } else {
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf)?;
        u32::from_be_bytes(buf) as u8
    };

    match chosen {
        rfb::security::NONE => {
            if minor >= 8 {
                read_security_result(stream)?;
            }
        }
        rfb::security::VNC_AUTH => {
            let mut challenge = [0u8; 16];
            stream.read_exact(&mut challenge)?;
            let password = password.unwrap_or("");
            stream.write_all(&rfb::vnc_auth_response(&challenge, password))?;
            read_security_result(stream)?;
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unhandled security type {other}"),
            ));
        }
    }
    Ok(())
}

fn read_security_result(stream: &mut TcpStream) -> io::Result<()> {
    let mut buf = [0u8; 4];
    stream.read_exact(&mut buf)?;
    if u32::from_be_bytes(buf) != 0 {
        let reason = read_reason(stream).unwrap_or_else(|_| "authentication failed".to_owned());
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason));
    }
    Ok(())
}

fn read_reason(stream: &mut TcpStream) -> io::Result<String> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = usize::try_from(u32::from_be_bytes(len)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "RFB server reason exceeds the 16 KiB limit",
        )
    })?;
    if len > rfb::MAX_SERVER_NAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("RFB server reason length {len} exceeds the 16 KiB limit"),
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn discard_exact(stream: &mut TcpStream, len: u64) -> io::Result<()> {
    let copied = io::copy(&mut Read::by_ref(stream).take(len), &mut io::sink())?;
    if copied < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "RFB server closed the stream mid-message",
        ));
    }
    Ok(())
}

fn read_set_colour_map(stream: &mut TcpStream) -> io::Result<()> {
    let mut head = [0u8; 5];
    stream.read_exact(&mut head)?;
    let n = u64::from(u16::from_be_bytes([head[3], head[4]]));
    discard_exact(stream, n.saturating_mul(6))
}

fn read_server_cut_text(stream: &mut TcpStream) -> io::Result<()> {
    let mut head = [0u8; 7];
    stream.read_exact(&mut head)?;
    let len = u64::from(u32::from_be_bytes([head[3], head[4], head[5], head[6]]));
    discard_exact(stream, len)
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A minimal RFB 3.8 server that offers `None` security, optionally sends leading messages
    /// before the framebuffer update, and serves one raw update.
    fn spawn_loopback_server_with_prefix(
        prefix: Vec<u8>,
        pixels: Vec<[u8; 4]>,
    ) -> (u16, std::sync::mpsc::Sender<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (keep_open, open) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();

            s.write_all(b"RFB 003.008\n").unwrap();
            let mut ver = [0u8; 12];
            s.read_exact(&mut ver).unwrap();

            // Security: offer only None.
            s.write_all(&[1u8, rfb::security::NONE]).unwrap();
            let mut chosen = [0u8; 1];
            s.read_exact(&mut chosen).unwrap();
            assert_eq!(chosen[0], rfb::security::NONE);
            s.write_all(&0u32.to_be_bytes()).unwrap(); // SecurityResult OK

            let mut ci = [0u8; 1];
            s.read_exact(&mut ci).unwrap();

            let mut si = Vec::new();
            si.extend_from_slice(&4u16.to_be_bytes());
            si.extend_from_slice(&2u16.to_be_bytes());
            si.extend_from_slice(&PixelFormat::rgb888().to_bytes());
            si.extend_from_slice(&8u32.to_be_bytes());
            si.extend_from_slice(b"loopback");
            s.write_all(&si).unwrap();

            let mut spf = [0u8; 20];
            s.read_exact(&mut spf).unwrap();
            let mut se_head = [0u8; 4];
            s.read_exact(&mut se_head).unwrap();
            let n_enc = u16::from_be_bytes([se_head[2], se_head[3]]) as usize;
            let mut se_body = vec![0u8; n_enc * 4];
            s.read_exact(&mut se_body).unwrap();
            let mut req = [0u8; 10];
            s.read_exact(&mut req).unwrap();

            if !prefix.is_empty() {
                s.write_all(&prefix).unwrap();
            }

            let mut msg = vec![0u8, 0, 0, 1]; // type=0, pad=0, count=1
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&0u16.to_be_bytes());
            msg.extend_from_slice(&4u16.to_be_bytes());
            msg.extend_from_slice(&2u16.to_be_bytes());
            msg.extend_from_slice(&rfb::encoding::RAW.to_be_bytes());
            for px in &pixels {
                // rgb888 wire order is [B, G, R, x].
                msg.extend_from_slice(&[px[2], px[1], px[0], 0]);
            }
            s.write_all(&msg).unwrap();

            let _ = open.recv();
        });
        (port, keep_open)
    }

    fn spawn_loopback_server(pixels: Vec<[u8; 4]>) -> (u16, std::sync::mpsc::Sender<()>) {
        spawn_loopback_server_with_prefix(Vec::new(), pixels)
    }

    /// A window's side of a VNC session: its statuses and the frames it saw.
    #[derive(Clone, Default)]
    struct Heard {
        statuses: Arc<Mutex<Vec<VncStatus>>>,
        frames: Arc<Mutex<Vec<Framebuffer>>>,
    }

    type Said = Box<dyn Fn(VncStatus) + Send + Sync>;
    type Saw = Box<dyn Fn(Framebuffer) + Send + Sync>;

    impl Heard {
        fn callbacks(&self) -> (Said, Saw) {
            let statuses = Arc::clone(&self.statuses);
            let frames = Arc::clone(&self.frames);
            (
                Box::new(move |status| {
                    statuses
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(status);
                }),
                Box::new(move |frame| {
                    frames
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(frame);
                }),
            )
        }

        fn frames(&self) -> Vec<Framebuffer> {
            self.frames
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn last_status(&self) -> VncStatus {
            self.statuses
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .last()
                .cloned()
                .unwrap_or(VncStatus::Connecting)
        }
    }

    tilo_test_env::scenario! {
        fn vnc_client_reads_real_framebuffer_and_reports_refusal_honestly;

        phase real_vnc_round_trip_reads_framebuffer;
        {
            let pixels = vec![
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [255, 255, 255, 255],
                [10, 20, 30, 255],
                [40, 50, 60, 255],
                [70, 80, 90, 255],
                [100, 110, 120, 255],
            ];
            let (port, _hold) = spawn_loopback_server(pixels.clone());
            let heard = Heard::default();
            let (said, saw) = heard.callbacks();
            let _vnc = VncSession::start(
                VncConfig {
                    host: "127.0.0.1".into(),
                    port,
                    password: None,
                },
                said,
                saw,
            )
            .unwrap();

            let fb = tilo_test_env::wait_for("a framebuffer", || {
                if let VncStatus::Failed(error) = heard.last_status() {
                    panic!("vnc connect failed: {error}")
                }
                heard
                    .frames()
                    .into_iter()
                    .find(|fb| fb.data.iter().any(|&b| b != 0))
            });
            assert_eq!((fb.width, fb.height), (4, 2));
            for (i, expected) in pixels.iter().enumerate() {
                let got = &fb.data[i * 4..i * 4 + 4];
                assert_eq!(got, &expected[..], "pixel {i} mismatch");
            }
            assert_eq!(
                heard.last_status(),
                VncStatus::Connected {
                    width: 4,
                    height: 2
                }
            );
        }

        phase refused_connection_is_honest_failure;
        {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener); // free the port
            let heard = Heard::default();
            let (said, saw) = heard.callbacks();
            let _vnc = VncSession::start(
                VncConfig {
                    host: "127.0.0.1".into(),
                    port,
                    password: None,
                },
                said,
                saw,
            )
            .unwrap();
            let status = tilo_test_env::wait_for("the refused connection to surface as Failed", || {
                match heard.last_status() {
                    VncStatus::Failed(error) => Some(VncStatus::Failed(error)),
                    _ => None,
                }
            });
            assert_eq!(status, VncStatus::Failed("Connection refused".to_owned()));
        }
    }

    #[test]
    fn read_reason_refuses_reason_over_the_server_name_bound() {
        let too_long = rfb::MAX_SERVER_NAME_BYTES + 1;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let header = u32::try_from(too_long).unwrap().to_be_bytes();
            let _ = s.write_all(&header);
            let _ = s.write_all(&vec![b'x'; too_long]);
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let error = read_reason(&mut client).expect_err("reason over 16 KiB must be refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        drop(client);
        server.join().unwrap();

        let exact = rfb::MAX_SERVER_NAME_BYTES;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(&u32::try_from(exact).unwrap().to_be_bytes())
                .unwrap();
            s.write_all(&vec![b'a'; exact]).unwrap();
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let reason = read_reason(&mut client).unwrap();
        assert_eq!(reason.len(), exact);
        assert!(reason.bytes().all(|b| b == b'a'));
        server.join().unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            s.write_all(b"RFB 003.008\n").unwrap();
            let mut ver = [0u8; 12];
            s.read_exact(&mut ver).unwrap();
            let _ = s.write_all(&[0u8]);
            let _ = s.write_all(&u32::try_from(too_long).unwrap().to_be_bytes());
            let _ = s.write_all(&vec![b'x'; too_long]);
        });
        let heard = Heard::default();
        let (said, saw) = heard.callbacks();
        let _vnc = VncSession::start(
            VncConfig {
                host: "127.0.0.1".into(),
                port,
                password: None,
            },
            said,
            saw,
        )
        .unwrap();
        let failure = tilo_test_env::wait_for("oversized reason refusal", || {
            if let VncStatus::Failed(message) = heard.last_status() {
                Some(message)
            } else {
                None
            }
        });
        assert!(
            failure.contains("16 KiB"),
            "expected size refusal, got: {failure}"
        );
        server.join().unwrap();
    }

    #[test]
    fn discarded_colour_map_and_cut_text_leave_the_following_update_intact() {
        let mut prefix = Vec::new();
        // SetColourMapEntries: type=1, pad=0, first_colour=0, n_colours=3, 3*6 payload bytes.
        prefix.push(1u8);
        prefix.push(0u8);
        prefix.extend_from_slice(&0u16.to_be_bytes());
        prefix.extend_from_slice(&3u16.to_be_bytes());
        prefix.extend_from_slice(&[0xAB; 18]);
        // ServerCutText: type=3, 3 pad bytes, len=256, 256 payload bytes.
        prefix.push(3u8);
        prefix.extend_from_slice(&[0u8; 3]);
        prefix.extend_from_slice(&256u32.to_be_bytes());
        prefix.extend_from_slice(&[b'c'; 256]);

        let pixels = vec![
            [11, 22, 33, 255],
            [44, 55, 66, 255],
            [77, 88, 99, 255],
            [111, 122, 133, 255],
            [144, 155, 166, 255],
            [177, 188, 199, 255],
            [200, 210, 220, 255],
            [230, 240, 250, 255],
        ];
        let (port, _hold) = spawn_loopback_server_with_prefix(prefix, pixels.clone());
        let heard = Heard::default();
        let (said, saw) = heard.callbacks();
        let _vnc = VncSession::start(
            VncConfig {
                host: "127.0.0.1".into(),
                port,
                password: None,
            },
            said,
            saw,
        )
        .unwrap();

        let fb = tilo_test_env::wait_for("framebuffer after discarded messages", || {
            if let VncStatus::Failed(error) = heard.last_status() {
                panic!("vnc connect failed: {error}")
            }
            heard
                .frames()
                .into_iter()
                .find(|fb| fb.data.iter().any(|&b| b != 0))
        });
        assert_eq!((fb.width, fb.height), (4, 2));
        for (i, expected) in pixels.iter().enumerate() {
            let got = &fb.data[i * 4..i * 4 + 4];
            assert_eq!(got, &expected[..], "pixel {i} mismatch");
        }
    }
}
