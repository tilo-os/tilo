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
use std::sync::{Arc, Mutex};

use tilo_actions::{DoorError, ResourceSpec};
use tilo_audio_service::media::MediaService;
use tilo_audio_service::mpris::Mpris;
use tilo_audio_service::policy::{self, PolicyEvent};
use tilo_audio_service::sound::{SoundService, SystemGraph, destinations};
use tilo_audio_service::{DeviceEvent, Event, Request};
use tilo_service::{Service, Services, Started, Wake, lines};

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
    let Some(runtime) = mpris_runtime() else {
        return;
    };
    if std::env::args().skip(1).any(|arg| arg == "--serve") {
        serve();
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
        || {},
        move || mpris.pause_playing(),
        events,
        &received,
    );
}

/// The runtime the MPRIS client is polled on.
fn mpris_runtime() -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("tilo-audio-mpris")
        .enable_all()
        .build()
    {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            tracing::error!(target: "tilo::audio_policy", %error, "no runtime for MPRIS");
            None
        }
    }
}

/// The sound service as what plays reads it, and as the session invokes it.
struct SharedSound(Arc<Mutex<SoundService>>);

impl Service for SharedSound {
    fn seated(&mut self, person: &str, earlier: &[String]) {
        if let Ok(mut sound) = self.0.lock() {
            sound.seated(person, earlier);
        }
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        self.0
            .lock()
            .map(|sound| sound.resources())
            .unwrap_or_default()
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        self.0.lock().ok().and_then(|sound| sound.read(resource))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Started, DoorError> {
        let mut sound = self.0.lock().map_err(|_| DoorError::Unavailable {
            code: "no_sound".to_owned(),
            reason: "the sound did not answer".to_owned(),
        })?;
        sound.invoke(resource, action, arguments)
    }

    fn poll(&mut self) -> bool {
        self.0.lock().is_ok_and(|mut sound| sound.poll())
    }

    fn settle(&mut self) {
        if let Ok(mut sound) = self.0.lock() {
            sound.settle();
        }
    }
}

/// The sound service over the graph this machine has, waking `wake` for
/// every change it sees.
fn start_sound(wake: Wake) -> Option<SharedSound> {
    match SoundService::new(Arc::new(SystemGraph::default()), Some(wake)) {
        Ok(sound) => Some(SharedSound(Arc::new(Mutex::new(sound)))),
        Err(error) => {
            tracing::error!(%error, "the sound service did not start");
            None
        }
    }
}

/// The session's audio service: the policy on a thread of its own, the
/// sound and media resources served on the pipes until the session closes
/// them.
fn serve() {
    let Some(runtime) = mpris_runtime() else {
        return;
    };
    let (incoming, requests, wake) = lines::incoming();
    let Some(SharedSound(sound)) = start_sound(wake.clone()) else {
        return;
    };
    let Ok(opened) = sound.lock() else {
        tracing::error!("the sound service did not start");
        return;
    };
    let refresh = opened.refresher();
    drop(opened);
    // What plays offers the graph's outputs as its destinations: the sound
    // service owns them, so the card names the nodes it routes to.
    let outputs = {
        let graph = Arc::clone(&sound);
        Box::new(move || {
            graph
                .lock()
                .map_or_else(|_| Vec::new(), |sound| destinations(sound.sound()))
        })
    };
    let media = MediaService::start(runtime.handle().clone(), Some(wake), outputs);
    let mpris = media.mpris().clone();
    let mut services = Services::default();
    services.add(Box::new(SharedSound(sound)));
    services.add(Box::new(media));
    let (events, received) = mpsc::channel();
    let stop = events.clone();
    // An output arriving or leaving is the person's to hear, as far as their
    // system-sound settings and the quiet state allow.
    let sounds = tilo_audio_service::sounds::SystemSounds::new();
    let heard = sounds.clone();
    let policy = std::thread::Builder::new()
        .name("tilo-audio-policy".into())
        .spawn(move || {
            policy::serve(
                move |event| {
                    heard.device(event);
                },
                refresh,
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
    // The last player goes with the policy that played it.
    drop(sounds);
}
