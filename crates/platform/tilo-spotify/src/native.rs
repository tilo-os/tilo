#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are assertions"
    )
)]
//! Native Spotify sessions stay on one worker. Only observations cross to UI;
//! streaming tokens never enter a resource, profile or collaborative document.

use gstreamer::{self as gst, prelude::*};
use librespot::{
    core::{SpotifyUri, authentication::Credentials, config::SessionConfig, session::Session},
    playback::{
        audio_backend::{Sink, SinkError, SinkResult},
        config::{Bitrate, PlayerConfig},
        convert::Converter,
        decoder::AudioPacket,
        mixer::NoOpVolume,
        player::{Player, PlayerEvent},
    },
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;

use crate::{STREAMING_ACCOUNT, STREAMING_USE};
/// Aggregate PCM observers for silent acceptance testing, never a fake output.
pub type PcmObserver = dyn Fn(&[u8]) + Send + Sync;
#[derive(Clone)]
pub struct CaptureOutput {
    pub on_pcm: Arc<PcmObserver>,
    pub on_rendered: Arc<dyn Fn() + Send + Sync>,
}
/// The same decoded PCM goes to a system sink or an observable clocked capture.
#[derive(Clone)]
pub enum Output {
    System,
    Capture(CaptureOutput),
}

fn build_audio_sink_bin(
    frames: &Arc<AtomicU64>,
    output: &Output,
) -> Option<(gst::Bin, gst::Element)> {
    let description = match output {
        Output::System => {
            "audioconvert ! audioresample ! autoaudiosink name=tilo_audiosink sync=true"
        }
        Output::Capture(_) => {
            "audioconvert ! audioresample ! appsink name=tilo_audiosink sync=true caps=audio/x-raw,format=S16LE,layout=interleaved emit-signals=true max-buffers=1 drop=true wait-on-eos=false"
        }
    };
    let bin = gst::parse::bin_from_description(description, true).ok()?;
    let sink = bin.by_name("tilo_audiosink")?;
    let capture = match output {
        Output::Capture(capture) => Some(capture.clone()),
        Output::System => None,
    };
    let observed = capture.clone();
    let count = frames.clone();
    sink.static_pad("sink")?
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            count.fetch_add(1, Ordering::Relaxed);
            if let Some(capture) = &observed
                && let Some(buffer) = info.buffer()
                && let Ok(bytes) = buffer.map_readable()
            {
                (capture.on_pcm)(bytes.as_slice());
            }
            gst::PadProbeReturn::Ok
        });
    if let Some(capture) = capture {
        sink.connect("new-sample", false, move |values| {
            let sample = values
                .first()
                .and_then(|value| value.get::<gst::Element>().ok())
                .and_then(|sink| sink.emit_by_name::<Option<gst::Sample>>("pull-sample", &[]));
            if sample.is_some() {
                (capture.on_rendered)();
            }
            Some(gst::FlowReturn::Ok.to_value())
        });
    }
    Some((bin, sink))
}

enum Command {
    Preload(String),
    Play,
    Pause,
    Seek(u32),
    Volume(f32),
}
#[derive(Default)]
struct Status {
    transition: Option<String>,
    duration: u64,
    position: u64,
    playing: bool,
    eos: bool,
    error: Option<String>,
    sink: Option<String>,
}

pub struct NativeAudio {
    commands: tokio::sync::mpsc::Sender<Command>,
    cancel: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    frames: Arc<AtomicU64>,
    started: Mutex<tilo_clock::Instant>,
    requested_play: AtomicBool,
    next: Arc<Mutex<Option<String>>>,
}
impl NativeAudio {
    /// Start native playback with an owned registration and a sealed grant.
    /// # Errors
    /// The runtime refused the worker. Session failures arrive through `error`.
    pub fn start(uri: String, account: String, output: Output) -> Result<Self, String> {
        let (commands, receiver) = tokio::sync::mpsc::channel(16);
        let status = Arc::new(Mutex::new(Status::default()));
        let frames = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let next = Arc::new(Mutex::new(None));
        let shared = Shared {
            status: status.clone(),
            frames: frames.clone(),
            cancel: cancel.clone(),
            next: next.clone(),
        };
        tilo_rt::spawn_thread("music-spotify-audio", move || {
            let result = connection().and_then(|connection| {
                tilo_rt::block_on(run(uri, account, output, receiver, &shared, connection))
                    .map_err(|_| "Spotify worker runtime is unavailable".to_string())
                    .and_then(std::convert::identity)
            });
            if let Err(error) = result {
                shared.update(|state| {
                    state.error = Some(error);
                    state.playing = false;
                });
            }
        })
        .map_err(|_| "Could not start Spotify audio worker".to_string())?;
        Ok(Self {
            commands,
            cancel,
            status,
            frames,
            started: Mutex::new(tilo_clock::now()),
            requested_play: AtomicBool::new(false),
            next,
        })
    }
    fn command(&self, command: Command) {
        if self.commands.try_send(command).is_err()
            && let Ok(mut state) = self.status.lock()
        {
            state.error = Some("Spotify audio command queue is unavailable".into());
        }
    }
    pub fn play(&self) {
        if !self.requested_play.swap(true, Ordering::AcqRel)
            && let Ok(mut started) = self.started.lock()
        {
            *started = tilo_clock::now();
        }
        self.command(Command::Play);
    }
    pub fn pause(&self) {
        self.requested_play.store(false, Ordering::Release);
        self.command(Command::Pause);
    }
    pub fn seek(&self, ms: u64) {
        self.command(Command::Seek(u32::try_from(ms).unwrap_or(u32::MAX)));
    }
    pub fn volume(&self, value: f32) {
        self.command(Command::Volume(value.clamp(0.0, 1.0)));
    }
    pub fn position(&self) -> Option<u64> {
        self.status.lock().ok().map(|state| state.position)
    }
    /// Preload only the next locally selected Spotify track. A mixed-source
    /// queue or shuffle boundary uses the app's ordinary source transition.
    pub fn prepare_next(&self, uri: Option<&str>) {
        let uri = uri
            .filter(|uri| {
                uri.strip_prefix("spotify:track:").is_some_and(|id| {
                    id.len() == 22 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
            })
            .map(str::to_owned);
        if let Ok(mut next) = self.next.lock()
            && *next != uri
        {
            next.clone_from(&uri);
            if let Some(uri) = uri {
                // Preloading is optional; a full channel must not interrupt
                // the currently playing song. The next identity is still held.
                let _ = self.commands.try_send(Command::Preload(uri));
            }
        }
    }
    pub fn take_transition(&self) -> Option<String> {
        self.status.lock().ok()?.transition.take()
    }
    pub fn playing(&self) -> bool {
        self.status.lock().is_ok_and(|state| state.playing) && self.frames() > 0
    }
    pub fn eos(&self) -> bool {
        self.status.lock().is_ok_and(|state| state.eos)
    }
    pub fn frames(&self) -> u64 {
        if self.status.lock().is_ok_and(|state| {
            state
                .sink
                .as_ref()
                .is_some_and(|sink| !sink.contains("fakesink"))
        }) {
            self.frames.load(Ordering::Relaxed)
        } else {
            0
        }
    }
    pub fn sink(&self) -> Option<String> {
        self.status.lock().ok()?.sink.clone()
    }
    pub fn error(&self) -> Option<String> {
        self.status.lock().ok()?.error.take()
    }
    pub fn pump(&self) -> bool {
        if self.requested_play.load(Ordering::Acquire)
            && self
                .started
                .lock()
                .is_ok_and(|started| started.elapsed() > Duration::from_secs(45))
            && self.frames() == 0
        {
            if let Ok(mut state) = self.status.lock() {
                state.error.get_or_insert_with(|| {
                    "Spotify did not deliver decoded audio within 45 seconds".into()
                });
            }
            self.cancel.store(true, Ordering::Release);
            return true;
        }
        self.status.lock().is_ok_and(|state| state.error.is_some())
    }
}
impl Drop for NativeAudio {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

struct Shared {
    status: Arc<Mutex<Status>>,
    frames: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    next: Arc<Mutex<Option<String>>>,
}
impl Shared {
    fn update(&self, change: impl FnOnce(&mut Status)) {
        if let Ok(mut state) = self.status.lock() {
            change(&mut state);
        }
    }
}

fn connection() -> Result<(SessionConfig, Credentials), String> {
    let registration = account_custody::ClientTable::defaults()
        .client_registration(account_custody::OAuthIssuer::Spotify)
        .ok_or_else(|| "Spotify streaming requires Tilo's own client registration".to_string())?;
    let token = account_custody::Custodian::live()
        .oauth_access_token(
            &accounts::AccountId::from(STREAMING_ACCOUNT.to_string()),
            STREAMING_USE,
            false,
        )
        .map_err(|_| "Authorize Spotify playback separately before playing".to_string())?;
    let config = SessionConfig {
        client_id: registration.client_id,
        autoplay: Some(false),
        ..SessionConfig::default()
    };
    Ok((config, Credentials::with_access_token(token.as_str())))
}

async fn run(
    uri: String,
    account: String,
    output: Output,
    mut commands: tokio::sync::mpsc::Receiver<Command>,
    shared: &Shared,
    connection: (SessionConfig, Credentials),
) -> Result<(), String> {
    let session = connect_session(connection, &account).await?;
    let _shutdown = SessionShutdown(session.clone());
    if shared.cancel.load(Ordering::Acquire) {
        session.shutdown();
        return Ok(());
    }
    let track =
        SpotifyUri::from_uri(&uri).map_err(|_| "Invalid Spotify track identity".to_string())?;
    let sink = PcmSink::new(&output, &shared.frames)?;
    if matches!(output, Output::Capture(_)) {
        shared.update(|state| state.sink = Some("appsink".into()));
    }
    let pipeline = sink.pipeline.clone();
    let source = sink.source.clone();
    let volume = sink.volume.clone();
    let actual_sink = sink.sink.clone();
    let config = PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        position_update_interval: Some(Duration::from_millis(250)),
        ..PlayerConfig::default()
    };
    let player = Player::new(config, session.clone(), Box::new(NoOpVolume), move || {
        Box::new(sink)
    });
    let mut events = player.get_player_event_channel();
    player.load(track, false, 0);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut pending_transition = None;
    let mut pending_started: Option<tilo_clock::Instant> = None;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Play) => player.play(), Some(Command::Pause) => player.pause(),
                Some(Command::Seek(ms)) => player.seek(ms),
                Some(Command::Volume(value)) => volume.set_property("volume", f64::from(value)),
                Some(Command::Preload(uri)) => {
                    if let Ok(uri) = SpotifyUri::from_uri(&uri) { player.preload(uri); }
                },
                None => break,
            },
            event = events.recv() => match event {
                Some(PlayerEvent::TrackChanged { audio_item }) => shared.update(|state| state.duration = u64::from(audio_item.duration_ms)),
                Some(PlayerEvent::Playing { position_ms, track_id, .. }) => {
                    let transition = if pending_transition.as_ref().is_some_and(|(uri, _)| *uri == track_id) {
                        pending_started = None;
                        pending_transition.take().map(|(_, uri)| uri)
                    } else { None };
                    shared.update(|state| { state.position = u64::from(position_ms); state.playing = true; if transition.is_some() { state.transition = transition; } });
                },
                Some(PlayerEvent::PositionChanged { position_ms, .. } | PlayerEvent::Seeked { position_ms, .. }) => shared.update(|state| { state.position = u64::from(position_ms); state.playing = true; }),
                Some(PlayerEvent::Paused { position_ms, .. }) => shared.update(|state| { state.position = u64::from(position_ms); state.playing = false; }),
                Some(PlayerEvent::EndOfTrack { .. }) => {
                    // librespot also emits EndOfTrack on decoder failure.
                    // A substantially truncated stream must not silently count
                    // as a completed song and advance the user's queue.
                    if shared.status.lock().is_ok_and(|state| state.duration.saturating_sub(state.position) > 2_000) {
                        shared.update(|state| state.error = Some("Spotify audio ended before the track was decoded".into())); break;
                    }
                    let next = shared.next.lock().ok().and_then(|mut next| next.take());
                    if let Some(next) = next {
                        let uri = SpotifyUri::from_uri(&next).map_err(|_| "Invalid Spotify queue track".to_string())?;
                        pending_transition = Some((uri.clone(), next));
                        pending_started = Some(tilo_clock::now());
                        player.load(uri, true, 0);
                        continue;
                    }
                    // EndOfTrack means the decoder is done; the clocked sink
                    // may still hold its tail. Let that PCM finish first.
                    source.end_of_stream().map_err(|_| "Spotify output could not finish the track".to_string())?;
                    drain_output(&pipeline, &shared.cancel).await?;
                    shared.update(|state| { state.eos = true; state.playing = false; }); break;
                },
                Some(PlayerEvent::Unavailable { .. }) => { shared.update(|state| state.error = Some("Spotify cannot play this track in your account or region".into())); break; },
                None => { shared.update(|state| state.error = Some("Spotify playback worker stopped".into())); break; },
                _ => {},
            },
            _ = tick.tick() => {
                if shared.cancel.load(Ordering::Acquire) { break; }
                if pending_started.is_some_and(|started| started.elapsed() > Duration::from_secs(30)) {
                    shared.update(|state| state.error = Some("Spotify did not start the next queued track".into())); break;
                }
                if let Some(message) = pipeline.bus().and_then(|bus| bus.pop_filtered(&[gst::MessageType::Error])) {
                    let _ = message; shared.update(|state| state.error = Some("Spotify audio output failed".into())); break;
                }
                let name = actual_sink.dynamic_cast_ref::<gst::ChildProxy>().and_then(|proxy| proxy.child_by_index(0)).and_then(|child| child.downcast::<gst::Element>().ok()).and_then(|element| element.factory()).map(|factory| factory.name().to_string());
                if name.as_ref().is_some_and(|name| name.contains("fakesink")) { shared.update(|state| state.error = Some("No real system audio sink is available".into())); break; }
                if name.is_some() { shared.update(|state| state.sink = name); }
            }
        }
    }
    let _ = pipeline.set_state(gst::State::Null);
    player.stop();
    session.shutdown();
    Ok(())
}

struct SessionShutdown(Session);
impl Drop for SessionShutdown {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

async fn connect_session(
    connection: (SessionConfig, Credentials),
    account: &str,
) -> Result<Session, String> {
    let (config, credentials) = connection;
    let session = Session::new(config, None); // No disk credential or audio cache.
    let result = tokio::time::timeout(Duration::from_secs(20), session.connect(credentials, false))
        .await
        .map_err(|_| "Spotify streaming connection timed out".to_string())
        .and_then(|result| result.map_err(|_| "Spotify rejected the native session. Check Premium and streaming authorization.".to_string()));
    if let Err(error) = result {
        session.shutdown();
        return Err(error);
    }
    if session.username() != account {
        session.shutdown();
        return Err("Catalogue and playback are signed in to different Spotify accounts".into());
    }
    Ok(session)
}

async fn drain_output(pipeline: &gst::Pipeline, cancel: &AtomicBool) -> Result<(), String> {
    let bus = pipeline.bus().ok_or("Spotify audio bus unavailable")?;
    for _ in 0..30 {
        if cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        if let Some(message) = bus.pop_filtered(&[gst::MessageType::Eos, gst::MessageType::Error]) {
            return match message.view() {
                gst::MessageView::Eos(_) => Ok(()),
                _ => Err("Spotify audio output failed at the end of the track".into()),
            };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("Spotify audio output did not finish the track".into())
}

struct PcmSink {
    pipeline: gst::Pipeline,
    source: gstreamer_app::AppSrc,
    volume: gst::Element,
    sink: gst::Element,
    samples: u64,
}
impl PcmSink {
    fn new(output: &Output, frames: &Arc<AtomicU64>) -> Result<Self, String> {
        gst::init().map_err(|_| "Could not initialize Spotify audio output".to_string())?;
        let pipeline = gst::Pipeline::new();
        let source = gstreamer_app::AppSrc::builder()
            .caps(
                &gst::Caps::builder("audio/x-raw")
                    .field("format", "F32LE")
                    .field("layout", "interleaved")
                    .field("rate", 44_100_i32)
                    .field("channels", 2_i32)
                    .build(),
            )
            .format(gst::Format::Time)
            .block(true)
            .max_bytes(352_800)
            .build();
        let volume = gst::ElementFactory::make("volume")
            .build()
            .map_err(|_| "Audio volume plugin is unavailable".to_string())?;
        let (bin, sink) = build_audio_sink_bin(frames, output)
            .ok_or_else(|| "System audio output is unavailable".to_string())?;
        pipeline
            .add_many([
                source.upcast_ref::<gst::Element>(),
                &volume,
                bin.upcast_ref(),
            ])
            .map_err(|_| "Could not assemble Spotify audio output".to_string())?;
        gst::Element::link_many([
            source.upcast_ref::<gst::Element>(),
            &volume,
            bin.upcast_ref(),
        ])
        .map_err(|_| "Could not connect Spotify audio output".to_string())?;
        Ok(Self {
            pipeline,
            source,
            volume,
            sink,
            samples: 0,
        })
    }
}
impl Sink for PcmSink {
    fn start(&mut self) -> SinkResult<()> {
        self.samples = 0;
        self.pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|_| SinkError::StateChange("Cannot start system audio output".into()))
    }
    fn stop(&mut self) -> SinkResult<()> {
        self.pipeline
            .set_state(gst::State::Null)
            .map(|_| ())
            .map_err(|_| SinkError::StateChange("Cannot stop system audio output".into()))
    }
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let AudioPacket::Samples(samples) = packet else {
            return Err(SinkError::InvalidParams(
                "Expected decoded PCM, not a protected/raw stream".into(),
            ));
        };
        let frames = u64::try_from(samples.len())
            .map_err(|_| SinkError::InvalidParams("PCM block too large".into()))?
            .checked_div(2)
            .unwrap_or(0);
        let bytes: Vec<u8> = converter
            .f64_to_f32(&samples)
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let mut buffer = gst::Buffer::from_mut_slice(bytes);
        if let Some(buffer) = buffer.get_mut() {
            buffer.set_pts(gst::ClockTime::from_nseconds(
                self.samples
                    .saturating_mul(1_000_000_000)
                    .checked_div(44_100)
                    .unwrap_or(0),
            ));
            buffer.set_duration(gst::ClockTime::from_nseconds(
                frames
                    .saturating_mul(1_000_000_000)
                    .checked_div(44_100)
                    .unwrap_or(0),
            ));
        }
        self.samples = self.samples.saturating_add(frames);
        self.source
            .push_buffer(buffer)
            .map(|_| ())
            .map_err(|_| SinkError::OnWrite("System audio output stopped accepting PCM".into()))
    }
}
impl Drop for PcmSink {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotify_decoded_pcm_reaches_the_shared_clocked_capture_sink() {
        let rendered = Arc::new(AtomicU64::new(0));
        let bytes = Arc::new(Mutex::new(Vec::<u8>::new()));
        let pcm = bytes.clone();
        let count = rendered.clone();
        let capture = CaptureOutput {
            on_pcm: Arc::new(move |data| pcm.lock().unwrap().extend_from_slice(data)),
            on_rendered: Arc::new(move || {
                count.fetch_add(1, Ordering::Relaxed);
            }),
        };
        let frames = Arc::new(AtomicU64::new(0));
        let mut sink = PcmSink::new(&Output::Capture(capture), &frames).unwrap();
        let mut converter = Converter::new(PlayerConfig::default().ditherer);
        sink.start().unwrap();
        for _ in 0..12 {
            sink.write(AudioPacket::Samples(vec![0.25; 2048]), &mut converter)
                .unwrap();
        }
        let deadline = tilo_clock::now() + Duration::from_secs(5);
        while rendered.load(Ordering::Relaxed) < 4 && tilo_clock::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            rendered.load(Ordering::Relaxed) >= 4,
            "clocked PCM must arrive"
        );
        assert!(bytes.lock().unwrap().len() >= 16_384);
        assert!(bytes.lock().unwrap().iter().any(|byte| *byte != 0));
        assert!(frames.load(Ordering::Relaxed) > 0);
        assert!(
            sink.write(AudioPacket::Raw(vec![1, 2, 3]), &mut converter)
                .is_err()
        );
        sink.source.end_of_stream().unwrap();
        tilo_rt::block_on(drain_output(&sink.pipeline, &AtomicBool::new(false)))
            .unwrap()
            .unwrap();
        sink.stop().unwrap();
    }

    #[test]
    fn spotify_preload_follows_the_latest_valid_local_queue_and_does_not_flood_commands() {
        let (commands, mut receiver) = tokio::sync::mpsc::channel(16);
        let native = NativeAudio {
            commands,
            cancel: Arc::new(AtomicBool::new(false)),
            status: Arc::new(Mutex::new(Status::default())),
            frames: Arc::new(AtomicU64::new(0)),
            started: Mutex::new(tilo_clock::now()),
            requested_play: AtomicBool::new(false),
            next: Arc::new(Mutex::new(None)),
        };
        let first = "spotify:track:1234567890123456789012";
        native.prepare_next(Some(first));
        assert!(matches!(receiver.try_recv().unwrap(), Command::Preload(uri) if uri == first));
        for _ in 0..100 {
            native.prepare_next(Some(first));
        }
        assert!(receiver.try_recv().is_err());
        native.prepare_next(Some("spotify:track:abcdefghijklmnopqrstuv"));
        assert!(
            matches!(receiver.try_recv().unwrap(), Command::Preload(uri) if uri.ends_with("abcdefghijklmnopqrstuv"))
        );
        native.prepare_next(Some("spotify:track:../wrong"));
        assert!(native.next.lock().unwrap().is_none());
        assert!(receiver.try_recv().is_err());
        native.prepare_next(Some(first));
        native.prepare_next(None);
        assert!(
            native.next.lock().unwrap().is_none(),
            "queue removal wins before end of track"
        );
    }

    #[test]
    fn spotify_audio_confirmation_rejects_an_unobserved_or_fake_system_sink() {
        let (commands, _receiver) = tokio::sync::mpsc::channel(1);
        let status = Arc::new(Mutex::new(Status {
            playing: true,
            ..Default::default()
        }));
        let native = NativeAudio {
            commands,
            status: status.clone(),
            frames: Arc::new(AtomicU64::new(10)),
            cancel: Arc::new(AtomicBool::new(false)),
            started: Mutex::new(tilo_clock::now()),
            requested_play: AtomicBool::new(false),
            next: Arc::new(Mutex::new(None)),
        };
        assert_eq!(native.frames(), 0);
        assert!(!native.playing());
        status.lock().unwrap().sink = Some("fakesink".into());
        assert_eq!(native.frames(), 0);
        status.lock().unwrap().sink = Some("pulsesink".into());
        assert_eq!(native.frames(), 10);
        assert!(native.playing());
        native.play();
        native.pause();
        assert!(
            native.pump(),
            "bounded commands must report refusal rather than fabricate success"
        );
        assert!(native.error().unwrap().contains("command queue"));
    }
}
