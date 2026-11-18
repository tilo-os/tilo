//! One endpoint of a served run: a loopback address the run's own processes
//! ask at, each request carried to the host and its answer written back.

use std::io::{self, BufReader, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::fd::AsRawFd as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use linux_vm_protocol::served::{Asked, FromRun};

use super::http::{self, Answer, Malformed};
use super::{Answered, Shared};

/// How long an asker is given to send its request's head, and each piece of
/// its body.
const REQUEST_PATIENCE: Duration = Duration::from_secs(30);

/// How often an asker that waits for its answer is looked at, to tell the
/// host when it left.
const LOOK: Duration = Duration::from_millis(250);

/// The most requests one run has open at once.
const MAX_OPEN: usize = 64;

pub(super) struct Endpoint {
    pub(super) name: String,
    listener: TcpListener,
    port: u16,
}

impl Endpoint {
    /// Open `name` on a loopback port of the kernel's choosing.
    pub(super) fn open(name: &str) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            name: name.to_owned(),
            listener,
            port,
        })
    }

    /// What the program is told in the endpoint's place.
    pub(super) fn address(&self) -> String {
        format!("http://{}:{}", Ipv4Addr::LOCALHOST, self.port)
    }

    /// Answer every caller until the launcher ends, each on a thread of its
    /// own.
    pub(super) fn serve(self, run: &Arc<Shared>) {
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else {
                continue;
            };
            let run = Arc::clone(run);
            let (name, port) = (self.name.clone(), self.port);
            std::thread::spawn(move || {
                let open = run.open.fetch_add(1, Ordering::SeqCst);
                answer_one(&run, &name, port, stream, open >= MAX_OPEN);
                run.open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
}

/// Whether the asker closed its end: a read that would not wait finds the
/// end of the stream, or the connection broken.
fn gone(stream: &TcpStream) -> bool {
    let mut byte = [0_u8; 1];
    // SAFETY: `recv` writes at most one byte into `byte`, which lives for
    // the call, on a descriptor this function borrows.
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            byte.as_mut_ptr().cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    read == 0 || (read < 0 && io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock)
}

/// Read one request from `stream` and carry it to the host, then write the
/// host's answer back. A caller that is not of the run gets nothing.
fn answer_one(run: &Shared, endpoint: &str, port: u16, stream: TcpStream, full: bool) {
    let of_the_run = stream.peer_addr().is_ok_and(|caller| {
        caller.ip().is_loopback() && run.processes.of_the_run(run.launcher, caller.port(), port)
    });
    if !of_the_run {
        run.tell(&FromRun::Turned {
            endpoint: endpoint.to_owned(),
        });
        return;
    }
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    if full {
        let _ = http::refuse(&mut writer, 503, "this run has too many requests open");
        return;
    }
    let _ = stream.set_read_timeout(Some(REQUEST_PATIENCE));
    let mut reader = BufReader::new(stream);
    let (head, length) = match http::read_head(&mut reader)
        .and_then(|head| head.body().map(|length| (head, length)))
    {
        Ok(read) => read,
        Err(Malformed::Bad(why)) => {
            let _ = http::refuse(&mut writer, 400, why);
            return;
        }
        Err(Malformed::Io(error)) => {
            eprintln!(
                "tilo-linux-vm-agent serve-run: a request at {endpoint} was not read: {error}"
            );
            return;
        }
        Err(Malformed::Closed) => return,
    };
    if head.expects_continue() && writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").is_err() {
        return;
    }
    let (id, answers) = run.asking();
    let asked = FromRun::Asked(Asked {
        id,
        endpoint: endpoint.to_owned(),
        method: head.method.clone(),
        path: head.target.clone(),
        headers: head.carried(),
    });
    let carried = run.tell(&asked)
        && http::read_body(&mut reader, length, &mut |bytes, last| {
            let told = run.tell(&FromRun::AskedBody {
                id,
                bytes: bytes.to_vec(),
                last,
            });
            if told {
                Ok(())
            } else {
                Err(io::Error::other("the run's host is gone"))
            }
        })
        .is_ok();
    if carried {
        write_answer(run, id, &answers, reader.get_ref(), &mut writer);
    } else {
        // The asker left in the middle of its request, or the host did.
        run.tell(&FromRun::AskedGone { id });
        let _ = http::refuse(&mut writer, 502, "this run's host is not answering");
    }
    run.forget(id);
}

/// Write the host's answer to request `id` as it comes, and tell the host
/// when the asker left before it was whole.
fn write_answer(
    run: &Shared,
    id: u64,
    answers: &Receiver<Answered>,
    stream: &TcpStream,
    writer: &mut TcpStream,
) {
    let mut begun: Option<Answer> = None;
    loop {
        let answered = match answers.recv_timeout(LOOK) {
            Ok(answered) => answered,
            Err(RecvTimeoutError::Timeout) => {
                if run.alone() {
                    if begun.is_none() {
                        let _ = http::refuse(writer, 502, "this run's host is not answering");
                    }
                    return;
                }
                if gone(stream) {
                    run.tell(&FromRun::AskedGone { id });
                    return;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let written = match answered {
            Answered::Head { status, headers } if begun.is_none() => {
                Answer::begin(writer, status, &headers).map(|answer| {
                    begun = Some(answer);
                    false
                })
            }
            Answered::Body { bytes, last } => {
                let Some(answer) = &begun else {
                    // A body before its head: not an answer.
                    return;
                };
                answer
                    .body(writer, &bytes)
                    .and_then(|()| if last { answer.end(writer) } else { Ok(()) })
                    .map(|()| last)
            }
            // A second head: not an answer.
            Answered::Head { .. } => return,
        };
        match written {
            Ok(false) => {}
            Ok(true) => return,
            Err(_) => {
                run.tell(&FromRun::AskedGone { id });
                return;
            }
        }
    }
}
