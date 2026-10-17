//! Selected-profile capture and worker execution, independent of application UI.
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};

#[cfg(all(unix, feature = "native"))]
mod native;
#[cfg(all(unix, feature = "native"))]
pub use native::*;
#[cfg(not(all(unix, feature = "native")))]
mod unavailable;
#[cfg(not(all(unix, feature = "native")))]
pub use unavailable::*;

/// Resolve only the host-authorized private application state location.
pub type StatePath = Arc<dyn Fn() -> io::Result<PathBuf> + Send + Sync>;
/// Event-loop notification; native workers never edit application records.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Whether the launcher projection is available on this platform.
pub fn supported() -> bool {
    cfg!(all(target_os = "linux", feature = "native"))
}

pub enum Completion {
    Prepared(io::Result<Option<Prepared>>),
    Published(io::Result<()>),
}

/// Acquire and validate the entire selection off the view thread.
pub fn prepare(source: Source, person: [u8; 32], wake: Wake) -> mpsc::Receiver<Completion> {
    run(move || Completion::Prepared(source.prepare(&person)), wake)
}

/// Publish a receipt only after the host confirms all personal-record edits.
pub fn publish(prepared: Prepared, wake: Wake) -> mpsc::Receiver<Completion> {
    run(
        move || Completion::Published(prepared.capture.complete(&prepared.state)),
        wake,
    )
}

fn run(
    action: impl FnOnce() -> Completion + Send + 'static,
    wake: Wake,
) -> mpsc::Receiver<Completion> {
    let (sender, receiver) = mpsc::channel();
    // A failed spawn disconnects the receiver, which the controller treats as failure.
    let failed_wake = Arc::clone(&wake);
    if std::thread::Builder::new()
        .name("browser-continuity".into())
        .spawn(move || {
            let _ = sender.send(action());
            wake();
        })
        .is_err()
    {
        failed_wake();
    }
    receiver
}
