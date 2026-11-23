//! `tilolang-process`: the process the session starts as a tilolang host, for one workspace and author.
//!
//! It holds its api (its process link, `TILO_SVC_PROCESS_FD`) and its storage
//! (`TILO_TILOLANG_PROCESS_STORAGE`) and nothing else. It waits on the API, its instances' windows,
//! its own wake and a stop, and runs a frame of every instance when one of them has something for
//! it. It ends when the session closes its api and its last window is gone, or when it is asked to
//! stop: both drop the instances.
//!
//! Its Windows body does not exist yet: a session hands the host its process link by passing a
//! descriptor, which needs named pipes and handle duplication. Until then the host refuses in
//! words where it would take that link.

#[cfg(unix)]
mod host;

/// Run the host until the session ends it.
#[cfg(unix)]
fn main() -> std::process::ExitCode {
    host::main()
}

/// The tilolang host is not started on Windows yet: the process link a session hands it is a
/// descriptor, and nothing here receives one yet.
#[cfg(windows)]
fn main() -> std::process::ExitCode {
    eprintln!("tilolang-process: the tilolang host does not run on Windows yet");
    std::process::ExitCode::from(2)
}
