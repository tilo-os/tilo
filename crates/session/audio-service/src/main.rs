//! `tilo-audio`: the session's audio policy and MPRIS client, started by the
//! session.
//!
//! Alone, it reads one request per line on standard input, writes one event
//! per line to standard output and stops when standard input closes (see the
//! crate docs). With `--serve` it is the session's audio service: the same
//! policy, and the sound and media resources served over JSON lines on the
//! same pipes.

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
    // A session that dies without closing our standard input (killed
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
    if std::env::args().skip(1).any(|arg| arg == "--serve") {
        serve(runtime.handle().clone());
        return;
    }
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

/// The session's audio service: the policy on a thread of its own, the
/// sound and media resources served on the pipes until the session closes
/// them.
fn serve(runtime: tokio::runtime::Handle) {
    use std::sync::Arc;
    use tilo_audio_service::media::MediaService;
    use tilo_audio_service::sound::{SoundService, SystemGraph};
    use tilo_service::{Services, lines};

    let (incoming, requests, wake) = lines::incoming();
    let sound = match SoundService::new(Arc::new(SystemGraph::default()), Some(wake.clone())) {
        Ok(sound) => sound,
        Err(error) => {
            tracing::error!(%error, "the sound service did not start");
            return;
        }
    };
    let refresh = sound.refresher();
    let media = MediaService::start(runtime, Some(wake));
    let mpris = media.mpris().clone();
    let mut services = Services::default();
    services.add(Box::new(sound));
    services.add(Box::new(media));
    let (events, received) = mpsc::channel();
    let stop = events.clone();
    let policy = std::thread::Builder::new()
        .name("tilo-audio-policy".into())
        .spawn(move || {
            policy::serve(
                move |_| refresh(),
                move || mpris.pause_playing(),
                events,
                &received,
            );
        });
    let reader = std::thread::Builder::new()
        .name("tilo-audio-requests".into())
        .spawn(move || lines::read_lines(std::io::stdin().lock(), &incoming));
    if let Err(error) = reader {
        tracing::error!(%error, "the request reader did not start");
        return;
    }
    if let Err(error) = lines::serve(&mut services, &requests, std::io::stdout().lock()) {
        tracing::warn!(%error, "the session stopped reading");
    }
    let _ = stop.send(PolicyEvent::Stop);
    if let Ok(policy) = policy {
        let _ = policy.join();
    }
}
