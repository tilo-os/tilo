//! `tilo-audio`: the session's audio policy and MPRIS client, started by the
//! compositor.
//!
//! It reads one request per line on standard input, writes one event per
//! line to standard output and stops when standard input closes (see the
//! crate docs).

use std::io::{BufRead as _, Write as _};
use std::sync::mpsc;

use tilo_audio_service::mpris::Mpris;
use tilo_audio_service::policy::{self, PolicyEvent};
use tilo_audio_service::{DeviceEvent, Event, Request};

fn emit(event: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", event.line()).and_then(|()| out.flush());
}

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
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("tilo-audio-mpris")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::error!(target: "tilo::audio_policy", %error, "no runtime for MPRIS");
            return;
        }
    };
    let mpris = Mpris::start(runtime.handle().clone(), |event| emit(&event));
    let (events, received) = mpsc::channel();
    let stop = events.clone();
    let requests = mpris.clone();
    let _stdin = std::thread::Builder::new()
        .name("tilo-audio-stdin".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if let Some(request) = Request::parse(&line) {
                    requests.handle(request);
                } else {
                    tracing::debug!(target: "tilo::audio_policy", "unreadable request");
                }
            }
            let _ = stop.send(PolicyEvent::Stop);
        });
    policy::serve(
        |event| {
            emit(&Event::Device {
                connected: event == DeviceEvent::Connected,
            });
        },
        move || mpris.pause_playing(),
        events,
        &received,
    );
}
