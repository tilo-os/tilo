//! `tilo-audio`: the session's audio, started by the session.
//!
//! Alone, it runs the audio policy: it writes one line per audio device
//! arriving or leaving to standard output and stops when standard input
//! closes (see the crate docs). With `--serve` it is the session's audio
//! service: the same policy, and the sound resource served over JSON lines
//! on the same pipes.

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
    // A session that dies without closing our standard input (killed
    // outright) still takes the helper with it.
    #[cfg(target_os = "linux")]
    // SAFETY: prctl(PR_SET_PDEATHSIG) only sets this process's own
    // parent-death signal; it reads and writes no memory of ours.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
    }
    if std::env::args().skip(1).any(|arg| arg == "--serve") {
        serve();
        return;
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

/// The session's audio service: the policy on a thread of its own, the
/// sound resource served on the pipes until the session closes them.
fn serve() {
    use std::sync::Arc;
    use tilo_audio_service::sound::{SoundService, SystemGraph};
    use tilo_service::lines;

    let (incoming, requests, wake) = lines::incoming();
    let mut sound = match SoundService::new(Arc::new(SystemGraph::default()), Some(wake)) {
        Ok(sound) => sound,
        Err(error) => {
            tracing::error!(%error, "the sound service did not start");
            return;
        }
    };
    let refresh = sound.refresher();
    let (events, received) = mpsc::channel();
    let stop = events.clone();
    let policy = std::thread::Builder::new()
        .name("tilo-audio-policy".into())
        .spawn(move || policy::serve(move |_| refresh(), events, &received));
    let reader = std::thread::Builder::new()
        .name("tilo-audio-requests".into())
        .spawn(move || lines::read_lines(std::io::stdin().lock(), &incoming));
    if let Err(error) = reader {
        tracing::error!(%error, "the request reader did not start");
        return;
    }
    if let Err(error) = lines::serve(&mut sound, &requests, std::io::stdout().lock()) {
        tracing::warn!(%error, "the session stopped reading");
    }
    let _ = stop.send(PolicyEvent::Stop);
    if let Ok(policy) = policy {
        let _ = policy.join();
    }
}
