//! Real VNC transport: an RFB client over a plain TCP socket.
//!
//! The protocol codec lives in [`crate::rfb`]; this module drives it. A
//! background thread owns the socket: it runs the handshake, authenticates,
//! then loops reading real `FramebufferUpdate`s from the server and applying
//! them into a shared [`Framebuffer`], while draining queued input the other
//! way. The UI thread reads the framebuffer each frame and paints it.
//!
//! On any failure the shared [`SessionStatus`] carries the real reason; the
//! UI never falls back to a synthetic desktop.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use ui::time::Duration;

use crate::rfb::{self, Framebuffer, PixelFormat, ServerInit};
use crate::session::SessionStatus;
use ui::lock::MutexRecover as _;

/// Bound input queued while a VNC server is busy with a framebuffer read. The
/// producer uses non-blocking `try_send`, so pointer motion can never stall
/// the UI thread or consume unbounded memory.
const VNC_INPUT_QUEUE_CAPACITY: usize = 256;

/// Everything needed to open a VNC session.
#[derive(Clone, Debug)]
pub struct VncConfig {
    pub host: String,
    pub port: u16,
    /// Password for `VNC Authentication`. `None` uses the `None` security
    /// type (fails if the server insists on a password).
    pub password: Option<String>,
}

/// An input event to inject into the remote desktop.
#[derive(Clone, Copy, Debug)]
pub enum VncInput {
    Pointer { button_mask: u8, x: u16, y: u16 },
    Key { down: bool, key: u32 },
}

/// A live VNC session handle held by the UI.
pub struct VncSession {
    status: Arc<Mutex<SessionStatus>>,
    /// The latest immutable framebuffer snapshot. `Framebuffer` shares its
    /// pixel storage copy-on-write with the worker, so the UI never clones a
    /// full desktop merely to see whether a generation changed.
    frame: Arc<Mutex<Option<Framebuffer>>>,
    input_tx: SyncSender<VncInput>,
    last_seen_gen: Option<u64>,
    _worker: std::thread::JoinHandle<()>,
}

impl VncSession {
    /// Start connecting; returns immediately with status `Connecting`.
    ///
    /// # Errors
    ///
    /// Returns an error if the operating system refuses to spawn the
    /// background worker thread.
    pub fn connect(cfg: VncConfig) -> io::Result<VncSession> {
        let status = Arc::new(Mutex::new(SessionStatus::Connecting));
        let frame: Arc<Mutex<Option<Framebuffer>>> = Arc::new(Mutex::new(None));
        let (input_tx, input_rx) = sync_channel::<VncInput>(VNC_INPUT_QUEUE_CAPACITY);

        let status_worker = status.clone();
        let frame_worker = frame.clone();
        let worker = std::thread::Builder::new()
            .name("remote-vnc".into())
            .spawn(move || {
                if let Err(e) = run_client(&cfg, &status_worker, &frame_worker, &input_rx) {
                    let mut s = status_worker.lock_recover();
                    if !matches!(*s, SessionStatus::Failed(_)) {
                        // A clean server close reads as EOF; anything else is
                        // an honest failure.
                        if e.kind() == io::ErrorKind::UnexpectedEof
                            && *s == SessionStatus::Connected
                        {
                            *s = SessionStatus::Closed;
                        } else {
                            *s = SessionStatus::Failed(friendly(&e));
                        }
                    }
                }
            })?;

        Ok(VncSession {
            status,
            frame,
            input_tx,
            last_seen_gen: None,
            _worker: worker,
        })
    }

    pub fn status(&self) -> SessionStatus {
        self.status.lock_recover().clone()
    }

    /// Return the framebuffer if it has a newer generation than last taken.
    /// This clone is shallow: the pixel allocation remains shared until the
    /// worker receives the next update, when `Framebuffer` performs COW.
    pub fn take_frame(&mut self) -> Option<Framebuffer> {
        let fb = {
            let guard = self.frame.lock_recover();
            guard.clone()?
        };
        if self.last_seen_gen == Some(fb.generation) {
            None
        } else {
            self.last_seen_gen = Some(fb.generation);
            Some(fb)
        }
    }

    /// The remote framebuffer size, once known.
    pub fn size(&self) -> Option<(u16, u16)> {
        self.frame
            .lock_recover()
            .as_ref()
            .map(|fb| (fb.width, fb.height))
    }

    /// Queue input without blocking the UI thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the bounded queue is full or the VNC worker has
    /// already stopped.
    pub fn send_input(&self, input: VncInput) -> Result<(), String> {
        self.input_tx.try_send(input).map_err(|error| match error {
            TrySendError::Full(_) => {
                "VNC input is busy; wait for the remote desktop to catch up".into()
            }
            TrySendError::Disconnected(_) => "VNC session input channel is closed".into(),
        })
    }
}

fn friendly(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::ConnectionRefused => "Connection refused".into(),
        io::ErrorKind::TimedOut => "Connection timed out".into(),
        _ => e.to_string(),
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
    status: &Arc<Mutex<SessionStatus>>,
    frame: &Arc<Mutex<Option<Framebuffer>>>,
    input_rx: &Receiver<VncInput>,
) -> io::Result<()> {
    let mut stream = TcpStream::connect((cfg.host.as_str(), cfg.port))?;
    stream.set_nodelay(true).ok();

    // 1. ProtocolVersion exchange.
    let mut ver = [0u8; 12];
    stream.read_exact(&mut ver)?;
    let (maj, min) = rfb::parse_protocol_version(&ver)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad ProtocolVersion"))?;
    stream.write_all(&rfb::client_protocol_version(maj, min))?;

    // 2. Security negotiation.
    negotiate_security(&mut stream, min, cfg.password.as_deref())?;

    // 3. ClientInit (request a shared desktop).
    stream.write_all(&[1u8])?;

    // 4. ServerInit → framebuffer geometry + name.
    let server_init = ServerInit::read(&mut stream)?;

    // 5. Ask the server to speak our simple pixel format + encodings.
    let fmt = PixelFormat::rgb888();
    stream.write_all(&rfb::encode_set_pixel_format(&fmt))?;
    stream.write_all(&rfb::encode_set_encodings(&[
        rfb::encoding::RAW,
        rfb::encoding::COPY_RECT,
        rfb::encoding::RRE,
    ]))?;

    let mut fb = Framebuffer::new(server_init.width, server_init.height);
    // 6. Kick off with a full (non-incremental) update request.
    request_update(&mut stream, false, &fb)?;

    *status.lock_recover() = SessionStatus::Connected;
    *frame.lock_recover() = Some(fb.clone());

    let mut last_request = ui::time::now();
    loop {
        // Drain any queued input first.
        loop {
            match input_rx.try_recv() {
                Ok(input) => write_input(&mut stream, input)?,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(()), // UI dropped us
            }
        }

        // Read the next message type with a short timeout so input stays
        // responsive when the server is idle. Only the type byte is read
        // under a timeout; the body is read blocking so framing is intact.
        stream.set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut ty = [0u8; 1];
        match stream.read_exact(&mut ty) {
            Ok(()) => {}
            Err(e) if is_timeout(&e) => {
                // Idle: keep pixels fresh with a throttled incremental request.
                if last_request.elapsed() >= Duration::from_millis(500) {
                    stream.set_read_timeout(None)?;
                    request_update(&mut stream, true, &fb)?;
                    last_request = ui::time::now();
                }
                continue;
            }
            Err(e) => return Err(e),
        }

        stream.set_read_timeout(None)?;
        match ty[0] {
            0 => {
                // FramebufferUpdate — decode straight from the socket.
                rfb::decode_framebuffer_update(&mut stream, &mut fb, &fmt)?;
                *frame.lock_recover() = Some(fb.clone());
                request_update(&mut stream, true, &fb)?;
                last_request = ui::time::now();
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

/// Negotiate the security handshake. Supports the 3.7+ type list plus the
/// legacy 3.3 fixed-type form, and both `None` and `VNC Authentication`.
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
        // 3.3: the server dictates a single security type as a u32.
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf)?;
        u32::from_be_bytes(buf) as u8
    };

    match chosen {
        rfb::security::NONE => {
            // 3.8 sends a SecurityResult even for None.
            if minor >= 8 {
                read_security_result(stream)?;
            }
        }
        rfb::security::VNC_AUTH => {
            let mut challenge = [0u8; 16];
            stream.read_exact(&mut challenge)?;
            let pw = password.unwrap_or("");
            stream.write_all(&rfb::vnc_auth_response(&challenge, pw))?;
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
        // Failed — a reason string may follow (3.8).
        let reason = read_reason(stream).unwrap_or_else(|_| "authentication failed".into());
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason));
    }
    Ok(())
}

fn read_reason(stream: &mut TcpStream) -> io::Result<String> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let mut buf = vec![0u8; u32::from_be_bytes(len) as usize];
    stream.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn read_set_colour_map(stream: &mut TcpStream) -> io::Result<()> {
    // 3 pad + first-colour(u16) + number-of-colours(u16) + n*6 bytes.
    let mut head = [0u8; 5];
    stream.read_exact(&mut head)?;
    let n = u16::from_be_bytes([head[3], head[4]]) as usize;
    let mut skip = vec![0u8; n * 6];
    stream.read_exact(&mut skip)
}

fn read_server_cut_text(stream: &mut TcpStream) -> io::Result<()> {
    // 3 pad + length(u32) + text.
    let mut head = [0u8; 7];
    stream.read_exact(&mut head)?;
    let len = u32::from_be_bytes([head[3], head[4], head[5], head[6]]) as usize;
    let mut skip = vec![0u8; len];
    stream.read_exact(&mut skip)
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    use tilo_test_env::hang_bound;

    /// A minimal RFB 3.8 server that offers `None` security and serves one
    /// 4x2 Raw framebuffer of known pixels. Real socket, real bytes — the
    /// client's handshake + decode run end-to-end against it. The server keeps
    /// the connection open until the returned sender is dropped.
    fn spawn_loopback_server(pixels: Vec<[u8; 4]>) -> (u16, std::sync::mpsc::Sender<()>) {
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

            // ClientInit.
            let mut ci = [0u8; 1];
            s.read_exact(&mut ci).unwrap();

            // ServerInit: 4x2, our rgb888 format, name "loopback".
            let mut si = Vec::new();
            si.extend_from_slice(&4u16.to_be_bytes());
            si.extend_from_slice(&2u16.to_be_bytes());
            si.extend_from_slice(&PixelFormat::rgb888().to_bytes());
            si.extend_from_slice(&8u32.to_be_bytes());
            si.extend_from_slice(b"loopback");
            s.write_all(&si).unwrap();

            // Read SetPixelFormat (20), SetEncodings (var), FBUpdateRequest (10).
            let mut spf = [0u8; 20];
            s.read_exact(&mut spf).unwrap();
            let mut se_head = [0u8; 4];
            s.read_exact(&mut se_head).unwrap();
            let n_enc = u16::from_be_bytes([se_head[2], se_head[3]]) as usize;
            let mut se_body = vec![0u8; n_enc * 4];
            s.read_exact(&mut se_body).unwrap();
            let mut req = [0u8; 10];
            s.read_exact(&mut req).unwrap();

            // One FramebufferUpdate: 4x2 Raw of the given pixels (as BGRX).
            // Layout: type(1) + pad(1) + rectangle-count(u16).
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

            // Keep the socket open until the client has read the update.
            let _ = open.recv();
        });
        (port, keep_open)
    }

    #[test]
    fn vnc_client_reads_real_framebuffer_and_reports_refusal_honestly() {
        eprintln!("phase real_vnc_round_trip_reads_framebuffer");
        {
            // 4x2 checkerboard of primaries.
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

            let mut vnc = VncSession::connect(VncConfig {
                host: "127.0.0.1".into(),
                port,
                password: None,
            })
            .unwrap();

            let mut got: Option<Framebuffer> = None;
            let deadline = ui::time::now() + hang_bound();
            while ui::time::now() < deadline {
                if let SessionStatus::Failed(e) = vnc.status() {
                    panic!("vnc connect failed: {e}")
                }
                if let Some(fb) = vnc.take_frame()
                    && fb.data.iter().any(|&b| b != 0)
                {
                    got = Some(fb);
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }

            let fb = got.expect("never received a framebuffer");
            assert_eq!((fb.width, fb.height), (4, 2));
            // Verify the exact pixels landed where they should.
            for (i, expected) in pixels.iter().enumerate() {
                let got_px = &fb.data[i * 4..i * 4 + 4];
                assert_eq!(got_px, &expected[..], "pixel {i} mismatch");
            }
        }

        eprintln!("phase refused_connection_is_honest_failure");
        {
            // Nothing is listening on this port → an honest Failed, not a frame.
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener); // free the port

            let vnc = VncSession::connect(VncConfig {
                host: "127.0.0.1".into(),
                port,
                password: None,
            })
            .unwrap();
            let mut failed = false;
            let deadline = ui::time::now() + hang_bound();
            while ui::time::now() < deadline {
                if let SessionStatus::Failed(_) = vnc.status() {
                    failed = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(failed, "a refused connection must surface as Failed");
        }
    }
}
