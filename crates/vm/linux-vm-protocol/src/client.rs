//! The host side of one agent connection.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use crate::{
    AgentHello, Duplex, ExitStatus, GuestMessage, HostMessage, Launch, MAX_CHUNK_BYTES,
    PROTOCOL_MAJOR, ProtocolError, Signal, TerminalSize, recv, send,
};

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

pub struct LaunchIo<'a> {
    /// Copied to the app's stdin until it ends; `None` closes stdin at once.
    pub stdin: Option<Box<dyn Read + Send>>,
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
    pub started: &'a mut dyn FnMut(u32),
}

/// A handle that signals a running launch from another thread, and feeds a
/// terminal launch its keys and size.
pub struct LaunchSignals<S> {
    writer: Arc<Mutex<S>>,
}

impl<S> Clone for LaunchSignals<S> {
    fn clone(&self) -> Self {
        Self {
            writer: Arc::clone(&self.writer),
        }
    }
}

impl<S: Duplex> LaunchSignals<S> {
    pub fn send(&self, signal: Signal) -> Result<(), ProtocolError> {
        self.message(&HostMessage::Signal(signal))
    }

    pub fn input(&self, bytes: &[u8]) -> Result<(), ProtocolError> {
        for chunk in bytes.chunks(MAX_CHUNK_BYTES) {
            self.message(&HostMessage::Stdin(chunk.to_vec()))?;
        }
        Ok(())
    }

    pub fn resize(&self, size: TerminalSize) -> Result<(), ProtocolError> {
        self.message(&HostMessage::Resize(size))
    }

    fn message(&self, message: &HostMessage) -> Result<(), ProtocolError> {
        let mut writer = self.writer.lock().map_err(|_| ProtocolError::Closed)?;
        send(&mut *writer, message)
    }
}

/// What serves one connection a launched program made to the guest's agent
/// socket: the host's end of it, on a thread of its own, for as long as it
/// lasts, with the program that made it. The connection ends when the program
/// closes it or the launch ends.
pub type AgentServer = Arc<dyn Fn(AgentCaller, UnixStream) + Send + Sync>;

/// Who made a connection to the guest's agent socket, as the guest says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCaller {
    /// The executable the connecting process starts.
    pub program: String,
}

/// Start `launch` and pump its stdio until it exits. `on_signals` receives a
/// handle before the first byte is pumped, so a caller can forward its own
/// signals from the start.
pub fn launch<S: Duplex>(
    stream: S,
    launch: &Launch,
    io: LaunchIo<'_>,
    on_signals: impl FnOnce(LaunchSignals<S>),
) -> Result<ExitStatus, ProtocolError> {
    pump(stream, launch, io, None, on_signals)
}

/// [`launch`], with the guest's agent socket answering what the program
/// starts: each connection one of its processes makes there is handed to
/// `agents` as a stream of the host's, carried on this launch's connection,
/// and all of them end with the launch.
pub fn launch_serving_agents<S: Duplex>(
    stream: S,
    launch: &Launch,
    io: LaunchIo<'_>,
    agents: &AgentServer,
    on_signals: impl FnOnce(LaunchSignals<S>),
) -> Result<ExitStatus, ProtocolError> {
    pump(stream, launch, io, Some(agents), on_signals)
}

/// The host's ends of the connections a launch carries, by the guest's number.
type Carried = HashMap<u64, UnixStream>;

fn pump<S: Duplex>(
    mut stream: S,
    launch: &Launch,
    io: LaunchIo<'_>,
    agents: Option<&AgentServer>,
    on_signals: impl FnOnce(LaunchSignals<S>),
) -> Result<ExitStatus, ProtocolError> {
    launch.validate()?;
    handshake(&mut stream)?;
    let asked = match agents {
        Some(_) => HostMessage::LaunchServingAgents(launch.clone()),
        None => HostMessage::Launch(launch.clone()),
    };
    send(&mut stream, &asked)?;
    let writer = Arc::new(Mutex::new(stream.try_clone()?));
    let mut carried = Carried::new();
    let ended = pump_frames(&mut stream, &writer, io, agents, &mut carried, on_signals);
    // What the launch carried ends with it, whichever way it ended.
    for (_, connection) in carried.drain() {
        let _ = connection.shutdown(std::net::Shutdown::Both);
    }
    ended
}

fn pump_frames<S: Duplex>(
    stream: &mut S,
    writer: &Arc<Mutex<S>>,
    io: LaunchIo<'_>,
    agents: Option<&AgentServer>,
    carried: &mut Carried,
    on_signals: impl FnOnce(LaunchSignals<S>),
) -> Result<ExitStatus, ProtocolError> {
    on_signals(LaunchSignals {
        writer: Arc::clone(writer),
    });
    let LaunchIo {
        stdin,
        stdout,
        stderr,
        started,
    } = io;
    let mut stdin = Some(stdin);
    loop {
        match recv(stream)?.ok_or(ProtocolError::Closed)? {
            GuestMessage::Started { pid } => {
                started(pid);
                if let Some(input) = stdin.take() {
                    pump_stdin(input, Arc::clone(writer));
                }
            }
            GuestMessage::AgentOpened {
                connection,
                program,
            } => match agents {
                Some(agents) => {
                    carry(connection, AgentCaller { program }, agents, writer, carried)?;
                }
                None => {
                    let _ = send(
                        &mut *writer.lock().map_err(|_| ProtocolError::Closed)?,
                        &HostMessage::AgentClose { connection },
                    );
                }
            },
            GuestMessage::AgentBytes { connection, bytes } => {
                // A connection whose server stopped reading is closed: its
                // program reads the end.
                let written = carried
                    .get_mut(&connection)
                    .is_some_and(|ours| ours.write_all(&bytes).is_ok());
                if !written && let Some(ours) = carried.remove(&connection) {
                    let _ = ours.shutdown(std::net::Shutdown::Both);
                }
            }
            GuestMessage::AgentClosed { connection } => {
                if let Some(ours) = carried.remove(&connection) {
                    let _ = ours.shutdown(std::net::Shutdown::Write);
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

/// Hand the guest's connection `connection` to `agents` as a stream of the
/// host's, and carry what its server writes back to the guest.
fn carry<S: Duplex>(
    connection: u64,
    caller: AgentCaller,
    agents: &AgentServer,
    writer: &Arc<Mutex<S>>,
    carried: &mut Carried,
) -> Result<(), ProtocolError> {
    let (ours, theirs) = UnixStream::pair()?;
    let mut answers = ours.try_clone()?;
    carried.insert(connection, ours);
    let writer = Arc::clone(writer);
    std::thread::spawn(move || {
        let mut buffer = vec![0_u8; MAX_CHUNK_BYTES];
        loop {
            let read = match answers.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            let bytes = buffer.get(..read).unwrap_or_default().to_vec();
            let Ok(mut writer) = writer.lock() else {
                return;
            };
            if send(&mut *writer, &HostMessage::AgentBytes { connection, bytes }).is_err() {
                return;
            }
        }
        if let Ok(mut writer) = writer.lock() {
            let _ = send(&mut *writer, &HostMessage::AgentClose { connection });
        }
    });
    let agents = Arc::clone(agents);
    std::thread::spawn(move || agents(caller, theirs));
    Ok(())
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
    // Detached: a terminal's stdin may never end, and the launch returns as soon
    // as the app exits.
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

    tilo_test_env::scenario! {
        fn a_launch_pumps_stdio_and_reports_the_exit;

        phase stdin_reaches_the_guest_output_and_exit_come_back;
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
                stdio: Stdio::Pump,
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

        phase a_launch_serving_agents_carries_each_connection_to_the_hosts_server;
        // phase: a connection the program makes reaches the host's server and
        // its answers reach the program; the launch's end ends what it carried
        {
            let (host, mut guest) = UnixStream::pair().unwrap();
            let agent = std::thread::spawn(move || {
                let _ = recv::<_, HostMessage>(&mut guest).unwrap();
                send(&mut guest, &hello()).unwrap();
                let Some(HostMessage::LaunchServingAgents(launch)) = recv(&mut guest).unwrap()
                else {
                    panic!("expected a launch serving agents");
                };
                assert_eq!(launch.argv, ["agent"]);
                send(&mut guest, &GuestMessage::Started { pid: 42 }).unwrap();
                for connection in [1, 2] {
                    let program = "/usr/bin/tilo-runtime".to_owned();
                    send(&mut guest, &GuestMessage::AgentOpened { connection, program }).unwrap();
                }
                send(
                    &mut guest,
                    &GuestMessage::AgentBytes {
                        connection: 1,
                        bytes: b"ping\n".to_vec(),
                    },
                )
                .unwrap();
                let mut answered = Vec::new();
                loop {
                    match recv::<_, HostMessage>(&mut guest).unwrap().unwrap() {
                        HostMessage::CloseStdin => {}
                        HostMessage::AgentBytes { connection, bytes } => {
                            assert_eq!(connection, 1, "only the connection that asked is answered");
                            answered.extend(bytes);
                            if answered.ends_with(b"\n") {
                                break;
                            }
                        }
                        other => panic!("unexpected {other:?}"),
                    }
                }
                send(&mut guest, &GuestMessage::AgentClosed { connection: 1 }).unwrap();
                // The server of the first one ends its side once the program
                // closed its own.
                loop {
                    match recv::<_, HostMessage>(&mut guest).unwrap().unwrap() {
                        HostMessage::AgentClose { connection: 1 } => break,
                        HostMessage::CloseStdin => {}
                        other => panic!("unexpected {other:?}"),
                    }
                }
                send(
                    &mut guest,
                    &GuestMessage::Exited(ExitStatus {
                        code: Some(0),
                        signal: None,
                    }),
                )
                .unwrap();
                answered
            });
            let (ended, endings) = std::sync::mpsc::channel();
            let server: AgentServer = Arc::new(move |caller, mut theirs: UnixStream| {
                assert_eq!(caller.program, "/usr/bin/tilo-runtime");
                let mut byte = [0_u8; 1];
                while theirs.read(&mut byte).is_ok_and(|read| read == 1) {
                    if byte[0] == b'\n' {
                        let _ = theirs.write_all(b"pong\n");
                    }
                }
                let _ = ended.send(());
            });
            let request = Launch {
                argv: vec!["agent".into()],
                cwd: None,
                env: Vec::new(),
                stdio: Stdio::Pump,
                display: Display::None,
            };
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let status = launch_serving_agents(
                host,
                &request,
                LaunchIo {
                    stdin: None,
                    stdout: &mut out,
                    stderr: &mut err,
                    started: &mut |_| {},
                },
                &server,
                |_| {},
            )
            .unwrap();
            assert_eq!(status.code, Some(0));
            assert_eq!(agent.join().unwrap(), b"pong\n");
            // Both servers saw their connection end: the first when its
            // program closed it, the second when the launch ended.
            for _ in 0..2 {
                endings
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("every carried connection ends with the launch");
            }
        }

        phase an_agent_on_another_protocol_major_is_refused;
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
