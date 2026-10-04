//! The host side of one agent connection.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use crate::{
    AgentHello, Duplex, ExitStatus, GuestMessage, HostMessage, Launch, MAX_CHUNK_BYTES,
    PROTOCOL_MAJOR, ProtocolError, Signal, recv, send,
};

/// Exchange hellos; refuses an agent speaking another protocol major.
pub fn handshake<S: Read + Write>(stream: &mut S) -> Result<AgentHello, ProtocolError> {
    send(
        stream,
        &HostMessage::Hello {
            protocol_major: PROTOCOL_MAJOR,
        },
    )?;
    match recv(stream)?.ok_or(ProtocolError::Closed)? {
        GuestMessage::Hello(hello) if hello.protocol_major == PROTOCOL_MAJOR => Ok(hello),
        GuestMessage::Hello(hello) => Err(ProtocolError::Version {
            expected: PROTOCOL_MAJOR,
            actual: hello.protocol_major,
        }),
        GuestMessage::Error { message } => Err(ProtocolError::Refused(message)),
        _ => Err(ProtocolError::Unexpected),
    }
}

/// One request and its single answer, on a fresh connection.
pub fn request<S: Read + Write>(
    stream: &mut S,
    message: &HostMessage,
) -> Result<GuestMessage, ProtocolError> {
    handshake(stream)?;
    send(stream, message)?;
    match recv(stream)?.ok_or(ProtocolError::Closed)? {
        GuestMessage::Error { message } => Err(ProtocolError::Refused(message)),
        answer => Ok(answer),
    }
}

/// Where a launch's output goes on the host.
pub struct LaunchIo<'a> {
    /// Copied to the app's stdin until it ends; `None` closes stdin at once.
    pub stdin: Option<Box<dyn Read + Send>>,
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
    /// Called once with the guest pid when the app started.
    pub started: &'a mut dyn FnMut(u32),
}

/// A handle that can signal a running launch from another thread.
#[derive(Clone)]
pub struct LaunchSignals<S> {
    writer: Arc<Mutex<S>>,
}

impl<S: Duplex> LaunchSignals<S> {
    pub fn send(&self, signal: Signal) -> Result<(), ProtocolError> {
        let mut writer = self.writer.lock().map_err(|_| ProtocolError::Closed)?;
        send(&mut *writer, &HostMessage::Signal(signal))
    }
}

/// Start `launch` and relay its stdio until it exits.
///
/// `on_signals` receives a handle before the first byte is relayed, so a
/// caller can forward its own Ctrl-C.
pub fn launch<S: Duplex>(
    mut stream: S,
    launch: &Launch,
    io: LaunchIo<'_>,
    on_signals: impl FnOnce(LaunchSignals<S>),
) -> Result<ExitStatus, ProtocolError> {
    launch.validate()?;
    handshake(&mut stream)?;
    send(&mut stream, &HostMessage::Launch(launch.clone()))?;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    on_signals(LaunchSignals {
        writer: Arc::clone(&writer),
    });
    let LaunchIo {
        stdin,
        stdout,
        stderr,
        started,
    } = io;
    let mut stdin = Some(stdin);
    loop {
        match recv(&mut stream)?.ok_or(ProtocolError::Closed)? {
            GuestMessage::Started { pid } => {
                started(pid);
                if let Some(input) = stdin.take() {
                    pump_stdin(input, Arc::clone(&writer));
                }
            }
            GuestMessage::Stdout(bytes) => {
                stdout.write_all(&bytes)?;
                stdout.flush()?;
            }
            GuestMessage::Stderr(bytes) => {
                stderr.write_all(&bytes)?;
                stderr.flush()?;
            }
            GuestMessage::Exited(status) => {
                let _ = stream.shutdown_both();
                return Ok(status);
            }
            GuestMessage::Error { message } => return Err(ProtocolError::Refused(message)),
            _ => return Err(ProtocolError::Unexpected),
        }
    }
}

fn pump_stdin<S: Duplex>(input: Option<Box<dyn Read + Send>>, writer: Arc<Mutex<S>>) {
    let close = |writer: &Arc<Mutex<S>>| {
        if let Ok(mut writer) = writer.lock() {
            let _ = send(&mut *writer, &HostMessage::CloseStdin);
        }
    };
    let Some(mut input) = input else {
        close(&writer);
        return;
    };
    // Detached: a terminal's stdin may never end, and the launch returns as
    // soon as the app exits; the thread ends with the next failed write.
    std::thread::spawn(move || {
        let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
        loop {
            let read = match input.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let chunk = buffer.get(..read).unwrap_or_default().to_vec();
            let Ok(mut stream) = writer.lock() else {
                return;
            };
            if send(&mut *stream, &HostMessage::Stdin(chunk)).is_err() {
                return;
            }
        }
        close(&writer);
    });
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;
    use crate::{Display, GuestOs, Stdio};

    fn hello() -> GuestMessage {
        GuestMessage::Hello(AgentHello {
            protocol_major: PROTOCOL_MAJOR,
            os: GuestOs::Linux,
            boot_id: "boot".into(),
            agent_version: "test".into(),
        })
    }

    #[test]
    fn a_launch_relays_stdio_and_reports_the_exit() {
        // phase: stdin reaches the guest, output and exit come back
        {
            let (host, mut guest) = UnixStream::pair().unwrap();
            let agent = std::thread::spawn(move || {
                assert!(matches!(
                    recv::<_, HostMessage>(&mut guest).unwrap(),
                    Some(HostMessage::Hello { .. })
                ));
                send(&mut guest, &hello()).unwrap();
                let Some(HostMessage::Launch(launch)) = recv(&mut guest).unwrap() else {
                    panic!("expected a launch");
                };
                assert_eq!(launch.argv, ["cat"]);
                send(&mut guest, &GuestMessage::Started { pid: 42 }).unwrap();
                let mut input = Vec::new();
                loop {
                    match recv::<_, HostMessage>(&mut guest).unwrap().unwrap() {
                        HostMessage::Stdin(bytes) => input.extend(bytes),
                        HostMessage::CloseStdin => break,
                        other => panic!("unexpected {other:?}"),
                    }
                }
                send(&mut guest, &GuestMessage::Stdout(input)).unwrap();
                send(&mut guest, &GuestMessage::Stderr(b"warn".to_vec())).unwrap();
                send(
                    &mut guest,
                    &GuestMessage::Exited(ExitStatus {
                        code: Some(7),
                        signal: None,
                    }),
                )
                .unwrap();
            });
            let request = Launch {
                argv: vec!["cat".into()],
                cwd: None,
                env: Vec::new(),
                stdio: Stdio::Relay,
                display: Display::None,
            };
            let (mut out, mut err, mut pid) = (Vec::new(), Vec::new(), 0);
            let status = launch(
                host,
                &request,
                LaunchIo {
                    stdin: Some(Box::new(std::io::Cursor::new(b"hello".to_vec()))),
                    stdout: &mut out,
                    stderr: &mut err,
                    started: &mut |started| pid = started,
                },
                |_| {},
            )
            .unwrap();
            agent.join().unwrap();
            assert_eq!(status.code, Some(7));
            assert_eq!(
                (out.as_slice(), err.as_slice(), pid),
                (&b"hello"[..], &b"warn"[..], 42)
            );
        }
        // phase: an agent on another protocol major is refused
        {
            let (mut host, mut guest) = UnixStream::pair().unwrap();
            let agent = std::thread::spawn(move || {
                let _ = recv::<_, HostMessage>(&mut guest).unwrap();
                send(
                    &mut guest,
                    &GuestMessage::Hello(AgentHello {
                        protocol_major: PROTOCOL_MAJOR.wrapping_add(1),
                        os: GuestOs::Linux,
                        boot_id: String::new(),
                        agent_version: String::new(),
                    }),
                )
                .unwrap();
            });
            assert!(matches!(
                request(&mut host, &HostMessage::Health),
                Err(ProtocolError::Version { .. })
            ));
            agent.join().unwrap();
        }
    }
}
