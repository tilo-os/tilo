//! The sandboxed helper process (`tilo-pdf-helper`).
//!
//! Started by [`crate::Engine`] with one end of a socket pair as its stdin.
//! Before it reads a single byte of any document it:
//!
//! 1. arranges to die with its parent and closes every inherited descriptor
//!    except the socket and stderr;
//! 2. loads pdfium (the only file it maps besides system fonts);
//! 3. caps core dumps and descriptors;
//! 4. confines itself with Landlock — read-only access to the library and
//!    the system font directories, nothing else, and no TCP;
//! 5. installs the seccomp parser policy — no sockets of any family, no
//!    connect/bind, no exec, write-xor-execute memory.
//!
//! Then it announces `Ready` and serves requests until the socket closes.
//! A crash or hang here costs only this process; the client reports an
//! error and starts a fresh helper.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

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
        Err(message) => {
            let _ = channel.send(&Response::Failed {
                kind: FailureKind::Engine,
                message,
            });
            1
        }
    }
}

fn start(channel: &mut Channel) -> Result<Engine, String> {
    die_with_parent()?;
    close_inherited_descriptors()?;
    let library = Library::load()?;
    let lib_path = library.path().to_path_buf();
    limit_resources()?;
    confine_filesystem(&lib_path)?;
    confine_syscalls()?;
    channel
        .send(&Response::Ready {
            version: PROTOCOL_VERSION,
        })
        .map_err(|e| e.to_string())?;
    Ok(Engine::new(library))
}

fn die_with_parent() -> Result<(), String> {
    // SAFETY: prctl with integer arguments.
    let rc = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
    if rc != 0 {
        return Err(format!("PR_SET_PDEATHSIG: {}", io::Error::last_os_error()));
    }
    // The parent may have exited before the call took effect.
    // SAFETY: getppid has no failure mode.
    if unsafe { libc::getppid() } == 1 {
        return Err("the client exited".to_string());
    }
    Ok(())
}

fn close_inherited_descriptors() -> Result<(), String> {
    // Keep 0 (the socket), 1 (null) and 2 (diagnostics).
    // SAFETY: close_range on descriptors this process does not use.
    let rc = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
    if rc != 0 {
        return Err(format!("close_range: {}", io::Error::last_os_error()));
    }
    Ok(())
}

fn limit_resources() -> Result<(), String> {
    for (resource, value) in [
        (libc::RLIMIT_CORE, 0),
        // The socket, stdio, and a handful of in-flight memfds.
        (libc::RLIMIT_NOFILE, 64),
    ] {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: setrlimit with a valid struct.
        if unsafe { libc::setrlimit(resource, &raw const limit) } != 0 {
            return Err(format!("setrlimit: {}", io::Error::last_os_error()));
        }
    }
    Ok(())
}

fn confine_filesystem(lib_path: &std::path::Path) -> Result<(), String> {
    use sandbox_core::landlock::{
        FS_ABI, LandlockOutcome, NetRestriction, RulesetSpec, access_plan, outcome_from_status,
        restrict_self,
    };
    let mut read_only: Vec<PathBuf> = vec![lib_path.to_path_buf()];
    read_only.extend(font_directories().into_iter().filter(|d| d.is_dir()));
    let spec = RulesetSpec::application(
        access_plan(&read_only, &[], FS_ABI),
        NetRestriction::DenyAll,
    );
    let status = restrict_self(&spec, |_, _| {}).map_err(|e| format!("landlock: {e}"))?;
    match outcome_from_status(&status.ruleset) {
        LandlockOutcome::FullyEnforced => Ok(()),
        other => Err(other.describe().to_string()),
    }
}

fn confine_syscalls() -> Result<(), String> {
    let program = sandbox_core::seccomp::parser_program().map_err(|e| e.to_string())?;
    sandbox_core::seccomp::install(&program).map_err(|e| e.to_string())
}

fn serve(channel: &mut Channel, mut engine: Engine) -> i32 {
    loop {
        let request: Request = match channel.recv() {
            Ok(r) => r,
            // The client went away (or spoke garbage): nothing left to do.
            Err(_) => return 0,
        };
        if handle(channel, &mut engine, request).is_err() {
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

fn handle(channel: &mut Channel, engine: &mut Engine, request: Request) -> io::Result<()> {
    match request {
        Request::Open {
            doc,
            kind,
            password,
        } => {
            let file = File::from(channel.recv_fd()?);
            let response = match engine.open(doc, &file, kind, password) {
                Ok(info) => Response::Opened(info),
                Err(e) => failed(e),
            };
            channel.send(&response)
        }
        Request::Close { doc } => {
            engine.close(doc);
            channel.send(&Response::Closed)
        }
        Request::Render { doc, page, options } => {
            let page = page as usize;
            match render(engine, doc, page, &options) {
                Ok((width, height, fd)) => {
                    channel.send(&Response::Rendered { width, height })?;
                    channel.send_fd(fd.as_fd())
                }
                Err(e) => channel.send(&failed(e)),
            }
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
            Ok(bytes) => match crate::shm::from_bytes("tilo-pdf-export", &bytes) {
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
    }
}

fn render(
    engine: &Engine,
    doc: u64,
    page: usize,
    options: &crate::model::RenderOptions,
) -> Result<(u32, u32, OwnedFd), crate::pdfium::Failure> {
    let engine_err = |e: io::Error| crate::pdfium::Failure {
        kind: FailureKind::Engine,
        message: e.to_string(),
    };
    let (w, h) = engine.render_size(doc, page, options)?;
    let len = u64::from(w).saturating_mul(u64::from(h)).saturating_mul(4);
    let file = crate::shm::create("tilo-pdf-page", len).map_err(engine_err)?;
    {
        let mut map = crate::shm::map_writable(&file).map_err(engine_err)?;
        engine.render_into(doc, page, options, &mut map)?;
    }
    crate::shm::seal(&file).map_err(engine_err)?;
    Ok((w, h, file.into()))
}
