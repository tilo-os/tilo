//! `tilo-audio`: the session's audio policy, started by the compositor.
//!
//! It writes one line per audio device arriving or leaving to standard
//! output and stops when standard input closes (see the crate docs).

use std::io::{Read as _, Write as _};
use std::sync::mpsc;

use tilo_audio_service::policy::{self, PolicyEvent};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // A compositor that dies without closing our standard input (killed
    // outright) still takes the helper with it.
    #[cfg(target_os = "linux")]
    // SAFETY: prctl(PR_SET_PDEATHSIG) only sets this process's own
    // parent-death signal; it reads and writes no memory of ours.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    let (events, received) = mpsc::channel();
    let stop = events.clone();
    let _stdin = std::thread::Builder::new()
        .name("tilo-audio-stdin".into())
        .spawn(move || {
            let mut sink = [0_u8; 64];
            let mut stdin = std::io::stdin().lock();
            while stdin.read(&mut sink).is_ok_and(|read| read > 0) {}
            let _ = stop.send(PolicyEvent::Stop);
        });
    policy::serve(
        |event| {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", event.line()).and_then(|()| out.flush());
        },
        events,
        &received,
    );
}
