//! The sandboxed helper process (`tilo-pdf-helper`).
//!
//! Started by [`crate::Engine`] with one end of a socket pair as its stdin.
//! Before it reads a single byte of any document it:
//!
//! 1. arranges to die with its parent and closes every inherited descriptor
//!    except the socket and stderr;
//! 2. loads pdfium;
//! 3. caps core dumps and descriptors;
//! 4. confines itself through `tilo-sandbox` — read-only access to the
//!    library, the system font directories and the pinned font catalog
//!    appearance text is set in, nothing else; no sockets it can bind or
//!    connect, no exec. On Linux that is Landlock plus the seccomp parser
//!    policy (which also enforces write-xor-execute memory); on macOS a
//!    Seatbelt profile.
//!
//! Then it announces `Ready` and serves requests until the socket closes.
//! If either confinement cannot be fully enforced it reports
//! `FailureKind::Sandbox` and exits without reading a document; see
//! [When the sandbox cannot be enforced](crate#when-the-sandbox-cannot-be-enforced).
//! A crash or hang here costs only this process; the client reports an
//! error and starts a fresh helper.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use tilo_sandbox::confine;

use crate::pdfium::{Engine, Library, font_directories};
use crate::proto::{Channel, FailureKind, PROTOCOL_VERSION, Request, Response};

/// Entry point of the helper binary. Returns the process exit code.
#[must_use]
pub fn run() -> i32 {
    // SAFETY: descriptor 0 is the socket the client handed us as stdin; we
    // take sole ownership of it here and nothing else in this process uses
    // stdin.
    let socket = unsafe { OwnedFd::from_raw_fd(0) };
    let mut channel = Channel::new(UnixStream::from(socket));
    match start(&mut channel) {
        Ok(engine) => serve(&mut channel, engine),
        Err((kind, message)) => {
            let _ = channel.send(&Response::Failed { kind, message });
            1
        }
    }
}

fn start(channel: &mut Channel) -> Result<Engine, (FailureKind, String)> {
    let engine = |message: String| (FailureKind::Engine, message);
    let sandbox = |message: String| (FailureKind::Sandbox, message);
    confine::detach().map_err(|e| engine(e.to_string()))?;
    background_priority();
    let library = Library::load().map_err(engine)?;
    let lib_path = library.path().to_path_buf();
    // The socket, stdio, and a handful of in-flight memory files.
    confine::limit_resources(64).map_err(|e| engine(e.to_string()))?;
    let catalog = tilo_font_catalog::locate().and_then(|root| root.canonicalize().ok());
    confine::confine(&policy(&lib_path, catalog.as_deref())).map_err(|e| sandbox(e.to_string()))?;
    if let Some(root) = catalog {
        crate::font::use_catalog(root);
    }
    channel
        .send(&Response::Ready {
            version: PROTOCOL_VERSION,
        })
        .map_err(|e| engine(e.to_string()))?;
    Ok(Engine::new(library))
}

/// Run below the apps it renders for: pages render ahead of a scroll while
/// the reader draws, and on a machine with one or two slow cores the window's
/// own thread must be scheduled first. Lowered before confinement, which
/// does not allow it later; a failure leaves the priority as it was.
fn background_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: plain integer syscalls on this (still single-threaded)
    // process; threads pdfium starts later inherit the value.
    unsafe {
        let current = libc::getpriority(libc::PRIO_PROCESS, 0);
        libc::setpriority(libc::PRIO_PROCESS, 0, current.saturating_add(10).min(19));
    }
}

/// Read-only access to the library, the system font directories and the
/// font catalog; nothing else.
fn policy(lib_path: &Path, catalog: Option<&Path>) -> confine::Policy {
    let mut read_only: Vec<PathBuf> = vec![lib_path.to_path_buf()];
    read_only.extend(font_directories().into_iter().filter(|d| d.is_dir()));
    // The image installs the catalog inside a system font directory, which
    // already covers it; a development run reads the checkout's copy.
    read_only.extend(catalog.map(Path::to_path_buf));
    confine::Policy {
        read_only,
        socket_pairs: false,
    }
}

/// Pixel buffers the client attached, mapped writable once.
type Buffers = std::collections::HashMap<u64, memmap2::MmapMut>;

/// Upper bound on attached pixel memory; the client releases buffers it
/// stops reusing, so this only stops a misbehaving client.
const MAX_BUFFER_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn serve(channel: &mut Channel, mut engine: Engine) -> i32 {
    let mut buffers = Buffers::new();
    loop {
        let request: Request = match channel.recv() {
            Ok(r) => r,
            // The client went away (or spoke garbage): nothing left to do.
            Err(_) => return 0,
        };
        if handle(channel, &mut engine, &mut buffers, request).is_err() {
            return 0;
        }
    }
}

fn failed(f: crate::pdfium::Failure) -> Response {
    Response::Failed {
        kind: f.kind,
        message: f.message,
    }
}

#[allow(clippy::too_many_lines, reason = "one arm per protocol request")]
fn handle(
    channel: &mut Channel,
    engine: &mut Engine,
    buffers: &mut Buffers,
    request: Request,
) -> io::Result<()> {
    match request {
        Request::Open {
            doc,
            len,
            kind,
            password,
        } => {
            let file = File::from(channel.recv_fd()?);
            let response = match engine.open(doc, &file, len, kind, password) {
                Ok(info) => Response::Opened(info),
                Err(e) => failed(e),
            };
            channel.send(&response)
        }
        Request::Close { doc } => {
            engine.close(doc);
            channel.send(&Response::Closed)
        }
        Request::Render {
            doc,
            page,
            options,
            buffer,
            buffer_len,
            attach,
        } => {
            if attach {
                let file = File::from(channel.recv_fd()?);
                let attached: u64 = buffers.values().map(|m| m.len() as u64).sum();
                match map_buffer(&file, buffer_len, attached) {
                    Ok(map) => {
                        buffers.insert(buffer, map);
                    }
                    Err(e) => return channel.send(&failed(e)),
                }
            }
            let response = match buffers.get_mut(&buffer) {
                Some(map) => match render(engine, doc, page as usize, &options, map) {
                    Ok((width, height)) => Response::Rendered { width, height },
                    Err(e) => failed(e),
                },
                None => Response::Failed {
                    kind: FailureKind::Invalid,
                    message: "unknown pixel buffer".into(),
                },
            };
            channel.send(&response)
        }
        Request::ReleaseBuffer { buffer } => {
            buffers.remove(&buffer);
            channel.send(&Response::Closed)
        }
        Request::Text { doc, page } => {
            let response = engine
                .text(doc, page as usize)
                .map_or_else(failed, Response::Text);
            channel.send(&response)
        }
        Request::Links { doc, page } => {
            let response = engine
                .links(doc, page as usize)
                .map_or_else(failed, Response::Links);
            channel.send(&response)
        }
        Request::Annotations { doc, page } => {
            let response = engine
                .annotations(doc, page as usize)
                .map_or_else(failed, Response::Annotations);
            channel.send(&response)
        }
        Request::FormFields { doc } => {
            let response = engine
                .form_fields(doc)
                .map_or_else(failed, Response::FormFields);
            channel.send(&response)
        }
        Request::Outline { doc } => {
            let response = engine.outline(doc).map_or_else(failed, Response::Outline);
            channel.send(&response)
        }
        Request::Export {
            doc,
            edits,
            imports,
        } => match engine.export(doc, &edits, &imports) {
            Ok(bytes) => match tilo_sandbox::shm::from_bytes("tilo-pdf-export", &bytes) {
                Ok(fd) => {
                    channel.send(&Response::Exported {
                        len: bytes.len() as u64,
                    })?;
                    channel.send_fd(fd.as_fd())
                }
                Err(e) => channel.send(&Response::Failed {
                    kind: FailureKind::Engine,
                    message: e.to_string(),
                }),
            },
            Err(e) => channel.send(&failed(e)),
        },
        Request::Probe { path } => channel.send(&Response::Probe(Engine::probe(&path))),
        Request::Abort => {
            if cfg!(debug_assertions) {
                std::process::abort();
            }
            channel.send(&Response::Failed {
                kind: FailureKind::Invalid,
                message: "not a development build".into(),
            })
        }
    }
}

fn map_buffer(
    file: &File,
    len: u64,
    attached: u64,
) -> Result<memmap2::MmapMut, crate::pdfium::Failure> {
    let failure = |kind, message: String| crate::pdfium::Failure { kind, message };
    let actual = file
        .metadata()
        .map_err(|e| failure(FailureKind::Invalid, e.to_string()))?
        .len();
    if actual < len || len == 0 {
        return Err(failure(
            FailureKind::Invalid,
            "pixel buffer size mismatch".into(),
        ));
    }
    if attached.saturating_add(len) > MAX_BUFFER_BYTES {
        return Err(failure(
            FailureKind::Limit,
            "too much attached pixel memory".into(),
        ));
    }
    tilo_sandbox::shm::map_writable(file).map_err(|e| failure(FailureKind::Engine, e.to_string()))
}

fn render(
    engine: &Engine,
    doc: u64,
    page: usize,
    options: &crate::model::RenderOptions,
    buffer: &mut [u8],
) -> Result<(u32, u32), crate::pdfium::Failure> {
    let (w, h) = engine.render_size(doc, page, options)?;
    let len = (w as usize).saturating_mul(h as usize).saturating_mul(4);
    let Some(target) = buffer.get_mut(..len) else {
        return Err(crate::pdfium::Failure {
            kind: FailureKind::Invalid,
            message: "pixel buffer too small".into(),
        });
    };
    engine.render_into(doc, page, options, target)?;
    Ok((w, h))
}
