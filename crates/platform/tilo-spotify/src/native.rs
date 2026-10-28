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
    stage: &'static str,
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
        shared.update(|state| state.stage = "credentials");
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
    pub fn duration_ms(&self) -> Option<u64> {
        self.status
            .lock()
            .ok()
            .and_then(|state| (state.duration > 0).then_some(state.duration))
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
    /// Non-secret startup observations; contains no account or grant data.
    pub fn startup_stage(&self) -> &'static str {
        self.status.lock().map_or("unavailable", |state| {
            if state.error.is_some() {
                "failed"
            } else if state.eos {
                "ended"
            } else if self.frames.load(Ordering::Relaxed) > 0
                && state
                    .sink
                    .as_ref()
                    .is_some_and(|sink| !sink.contains("fakesink"))
            {
                if state.playing { "playing" } else { "paused" }
            } else {
                state.stage
            }
        })
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

fn premium_confirmed(session: &Session) -> Result<bool, String> {
    if session.is_invalid() {
        return Err(
            "Spotify streaming session ended. Check Premium eligibility and sign in again.".into(),
        );
    }
    match session.get_user_attribute("type").as_deref() {
        Some("premium") => Ok(true),
        None => Ok(false),
        Some(_) => Err("Spotify's native session did not confirm Premium eligibility".into()),
    }
}

async fn verified_sink(
    session: &Session,
    cancel: &AtomicBool,
    output: &Output,
    frames: &Arc<AtomicU64>,
    timeout: Duration,
) -> Result<Option<PcmSink>, String> {
    let confirmed = tokio::time::timeout(timeout, async {
        loop {
            if cancel.load(Ordering::Acquire) {
                return Ok::<bool, String>(false);
            }
            if premium_confirmed(session)? {
                return Ok(true);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| {
        "Spotify's native session did not confirm Premium eligibility; no audio started".to_string()
    })??;
    if !confirmed {
        return Ok(None);
    }
    PcmSink::new(output, frames).map(Some)
}

async fn authorized_sink(
    session: &Session,
    shared: &Shared,
    output: &Output,
) -> Result<Option<PcmSink>, String> {
    shared.update(|state| state.stage = "premium");
    let sink = verified_sink(
        session,
        &shared.cancel,
        output,
        &shared.frames,
        Duration::from_secs(10),
    )
    .await?;
    if sink.is_some() && matches!(output, Output::Capture(_)) {
        shared.update(|state| state.sink = Some("appsink".into()));
    }
    Ok(sink)
}

async fn run(
    uri: String,
    account: String,
    output: Output,
    mut commands: tokio::sync::mpsc::Receiver<Command>,
    shared: &Shared,
    connection: (SessionConfig, Credentials),
) -> Result<(), String> {
    shared.update(|state| state.stage = "session");
    let session = connect_session(connection, &account).await?;
    let _shutdown = SessionShutdown(session.clone());
    if shared.cancel.load(Ordering::Acquire) {
        session.shutdown();
        return Ok(());
    }
    let track = playback_metadata(&session, &uri).await?;
    let Some(sink) = authorized_sink(&session, shared, &output).await? else {
        return Ok(());
    };
    let pipeline = sink.pipeline.clone();
    let source = sink.source.clone();
    let volume = sink.volume.clone();
    let actual_sink = sink.sink.clone();
    let governed_output = sink.governed_output.clone();
    shared.update(|state| state.stage = "decoding");
    let player = Player::new(
        playback_config(),
        session.clone(),
        Box::new(NoOpVolume),
        move || Box::new(sink),
    );
    let mut events = player.get_player_event_channel();
    player.load(track, false, 0);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut pending_transition = None;
    let mut pending_started: Option<tilo_clock::Instant> = None;
    loop {
        if !confirm_playback_session(&session, shared) {
            break;
        }
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Play) => { if let Some(output) = &governed_output { output.pause(false); } player.play(); },
                Some(Command::Pause) => { player.pause(); if let Some(output) = &governed_output { output.pause(true); } },
                Some(Command::Seek(ms)) => { if let Some(output) = &governed_output { output.flush(); } player.seek(ms); },
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
                    // EndOfTrack means the decoder is done; the clocked sink
                    // may still hold its tail. Let that PCM finish first.
                    source.end_of_stream().map_err(|_| "Spotify output could not finish the track".to_string())?;
                    drain_output(&pipeline, &shared.cancel).await?;
                    drain_governed_output(governed_output.as_deref(), &shared.cancel).await?;
                    let next = shared.next.lock().ok().and_then(|mut next| next.take());
                    if let Some(next) = next {
                        let uri = SpotifyUri::from_uri(&next).map_err(|_| "Invalid Spotify queue track".to_string())?;
                        pending_transition = Some((uri.clone(), next));
                        pending_started = Some(tilo_clock::now());
                        player.load(uri, true, 0);
                        continue;
                    }
                    shared.update(|state| { state.eos = true; state.playing = false; }); break;
                },
                Some(PlayerEvent::Unavailable { .. }) => { shared.update(|state| state.error = Some("Spotify could not load this track; check playback authorization and availability".into())); break; },
                None => { shared.update(|state| state.error = Some("Spotify playback worker stopped".into())); break; },
                _ => {},
            },
            _ = tick.tick() => {
                if shared.cancel.load(Ordering::Acquire) { break; }
                observe_governed_output(governed_output.as_deref(), shared)?;
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
    if let Some(output) = &governed_output {
        output.close();
    }
    player.stop();
    session.shutdown();
    Ok(())
}

/// Runs native playback's own code with no account, network or audio device:
/// parses a track identity and builds the session and player configuration a
/// session starts from. Proves that the library carrying librespot (Music's
/// engine library in a thin build) loads and runs; the report holds no
/// account or device data.
///
/// # Errors
/// librespot rejected a well-formed track identity.
pub fn self_check() -> Result<String, String> {
    let uri = SpotifyUri::from_uri("spotify:track:4uLU6hMCjMI75M1A2tKUQC")
        .map_err(|error| format!("librespot rejected a track identity: {error}"))?;
    let session = SessionConfig::default();
    let player = playback_config();
    Ok(format!(
        "librespot {} parsed {uri}; a session starts with autoplay {:?} and plays at {:?}",
        librespot::core::version::SEMVER,
        session.autoplay,
        player.bitrate,
    ))
}

fn playback_config() -> PlayerConfig {
    PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        position_update_interval: Some(Duration::from_millis(250)),
        ..PlayerConfig::default()
    }
}

fn confirm_playback_session(session: &Session, shared: &Shared) -> bool {
    if matches!(premium_confirmed(session), Ok(true)) {
        return true;
    }
    shared.update(|state| {
        state.error =
            Some("Spotify's native session no longer confirms Premium eligibility".into());
        state.playing = false;
    });
    false
}

fn observe_governed_output(
    output: Option<&tilo_media::audio_output::gst::Bridge>,
    shared: &Shared,
) -> Result<(), String> {
    let Some(output) = output else {
        return Ok(());
    };
    if let Some(error) = output.error() {
        return Err(error);
    }
    let observed = output.observation();
    if observed.ready {
        shared
            .frames
            .store(observed.rendered_frames, Ordering::Release);
        shared.update(|state| state.sink = Some("tilo-governed-pipewire".into()));
    }
    Ok(())
}

async fn drain_governed_output(
    output: Option<&tilo_media::audio_output::gst::Bridge>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let Some(output) = output else {
        return Ok(());
    };
    for _ in 0..30 {
        let observed = output.refresh()?;
        if observed.queued_frames == 0 || cancel.load(Ordering::Acquire) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err("Governed Spotify output did not drain".into())
}

struct SessionShutdown(Session);
impl Drop for SessionShutdown {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

async fn playback_metadata(session: &Session, uri: &str) -> Result<SpotifyUri, String> {
    let track =
        SpotifyUri::from_uri(uri).map_err(|_| "Invalid Spotify track identity".to_string())?;
    tokio::time::timeout(
        Duration::from_secs(15),
        session.spclient().get_track_metadata(&track),
    )
    .await
    .map_err(|_| "Spotify playback metadata request timed out".to_string())?
    .map_err(|error| playback_metadata_error(&error))?;
    Ok(track)
}

fn playback_metadata_error(error: &librespot::core::Error) -> String {
    // Only the typed status and transport-allowlisted fixed reason are public.
    // Other sources may contain credentials and must never reach app state.
    if let Some(source) = error.error.downcast_ref() {
        let source: &librespot::core::http_client::HttpClientError = source;
        let reason = source
            .reason()
            .map_or(String::new(), |reason| format!(", {reason}"));
        return format!(
            "Spotify refused the playback metadata request (HTTP {}{reason})",
            source.status().as_u16()
        );
    }
    if let Some(source) = error.error.downcast_ref::<std::io::Error>() {
        return format!(
            "Spotify playback metadata transport failed ({:?}, OS error {:?})",
            source.kind(),
            source.raw_os_error()
        );
    }
    format!(
        "Spotify playback metadata request failed ({:?})",
        error.kind
    )
}

async fn connect_session(
    connection: (SessionConfig, Credentials),
    account: &str,
) -> Result<Session, String> {
    let (config, credentials) = connection;
    let session = Session::new(config, None); // No disk credential or audio cache.
    session.login5().set_external_token_provider(Arc::new(|| {
        Box::pin(async {
            tokio::task::spawn_blocking(custody_streaming_bearer)
                .await
                .map_err(|_| {
                    librespot::core::Error::unavailable("Spotify credential worker stopped")
                })?
        })
    }));
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

fn custody_streaming_bearer() -> Result<librespot::core::token::Token, librespot::core::Error> {
    let (token, expires) = account_custody::Custodian::live()
        .oauth_access_token_with_expiry(
            &accounts::AccountId::from(STREAMING_ACCOUNT.to_string()),
            STREAMING_USE,
            false,
        )
        .map_err(|_| {
            librespot::core::Error::unauthenticated("Spotify playback authorization is unavailable")
        })?;
    let lifetime = expires
        .and_then(|expiry| expiry.checked_sub(tilo_clock::unix_now()))
        .ok_or_else(|| {
            librespot::core::Error::unauthenticated("Spotify did not provide a valid bearer expiry")
        })?;
    Ok(librespot::core::token::Token {
        access_token: token.as_str().to_owned(),
        expires_in: Duration::from_secs(lifetime),
        token_type: "Bearer".into(),
        scopes: Vec::new(),
        timestamp: tilo_clock::SystemTime::now(),
    })
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
    rendered: Arc<AtomicU64>,
    governed_output: Option<Arc<tilo_media::audio_output::gst::Bridge>>,
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
        let (bin, sink, governed_output) = if matches!(output, Output::System)
            && tilo_media::audio_output::is_governed()
        {
            let (bin, bridge) =
                tilo_media::audio_output::gst::sink_bin("tilo_audiosink", "tilo_governed_gain")?;
            let sink = bin
                .by_name("tilo_audiosink")
                .ok_or("Governed audio sink is unavailable")?;
            (bin, sink, Some(bridge))
        } else {
            let (bin, sink) =
                build_audio_sink_bin(frames, output).ok_or("System audio output is unavailable")?;
            (bin, sink, None)
        };
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
            rendered: frames.clone(),
            governed_output,
        })
    }
}
impl Sink for PcmSink {
    fn start(&mut self) -> SinkResult<()> {
        self.samples = 0;
        self.rendered.store(0, Ordering::Release);
        self.pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|_| SinkError::StateChange("Cannot start system audio output".into()))
    }
    fn stop(&mut self) -> SinkResult<()> {
        let result = self
            .pipeline
            .set_state(gst::State::Null)
            .map(|_| ())
            .map_err(|_| SinkError::StateChange("Cannot stop system audio output".into()));
        if let Some(output) = &self.governed_output {
            output.reset();
        }
        self.rendered.store(0, Ordering::Release);
        result
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
        if let Some(output) = &self.governed_output {
            output.close();
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spotify_metadata_refusal_classifies_only_bounded_provider_codes() {
        use librespot::core::http_client::HttpClientError;
        let status = "403".parse().unwrap();
        for (body, expected) in [
            (
                br#"{"error":"insufficient_scope","error_description":"fixture-private-token"}"#
                    .as_slice(),
                "insufficient_scope",
            ),
            (
                br#"{"error":{"reason":"PREMIUM_REQUIRED","message":"fixture-private-token"}}"#
                    .as_slice(),
                "PREMIUM_REQUIRED",
            ),
            (
                br#"{"error":"fixture-private-token","access_token":"fixture-private-token"}"#
                    .as_slice(),
                "unclassified-json",
            ),
            (
                br#"{"error":{"message":"fixture-private-token"}}"#.as_slice(),
                "unclassified-json",
            ),
            (
                b"<html>fixture-private-token</html>".as_slice(),
                "non-json-body",
            ),
            (b"".as_slice(), "empty-body"),
        ] {
            let source = HttpClientError::from_status_body(status, body);
            assert_eq!(source.reason(), Some(expected));
            let error: librespot::core::Error = source.into();
            assert_eq!(
                error.kind,
                librespot::core::error::ErrorKind::PermissionDenied
            );
            let public = playback_metadata_error(&error);
            assert!(public.contains(expected));
            assert!(public.contains("HTTP 403"));
            assert!(!format!("{error:?} {error} {public}").contains("fixture-private-token"));
        }
        let oversized = vec![b'x'; 8193];
        let error = HttpClientError::from_status_body(status, &oversized);
        assert_eq!(error.reason(), Some("body-limit-exceeded"));
    }

    #[test]
    fn playback_metadata_failure_preserves_provider_status_without_secret_sources() {
        use librespot::core::http_client::HttpClientError;

        for status in ["401", "402", "403"] {
            let error = HttpClientError::StatusCode(status.parse().unwrap()).into();
            assert_eq!(
                playback_metadata_error(&error),
                format!("Spotify refused the playback metadata request (HTTP {status})")
            );
        }
        let error = librespot::core::Error::permission_denied(
            "private bearer and request URL must not enter collaborative state",
        );
        assert_eq!(
            playback_metadata_error(&error),
            "Spotify playback metadata request failed (PermissionDenied)"
        );
        let error = std::io::Error::from_raw_os_error(13).into();
        assert_eq!(
            playback_metadata_error(&error),
            "Spotify playback metadata transport failed (PermissionDenied, OS error Some(13))"
        );
    }

    fn eligibility_output() -> Output {
        Output::Capture(CaptureOutput {
            on_pcm: Arc::new(|_| {}),
            on_rendered: Arc::new(|| {}),
        })
    }

    #[test]
    fn native_output_waits_for_confirmed_premium_metadata() {
        tilo_rt::block_on(async {
            let session = Session::new(SessionConfig::default(), None);
            let cancel = AtomicBool::new(false);
            let frames = Arc::new(AtomicU64::new(0));
            let output = eligibility_output();
            let unknown = verified_sink(
                &session,
                &cancel,
                &output,
                &frames,
                Duration::from_millis(20),
            )
            .await;
            assert!(unknown.err().unwrap().contains("did not confirm Premium"));
            assert_eq!(frames.load(Ordering::Acquire), 0);
            let updates = session.clone();
            let received = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                updates.set_user_attribute("type", "premium");
            });
            let mut sink = verified_sink(
                &session,
                &cancel,
                &output,
                &frames,
                tilo_test_env::hang_bound(),
            )
            .await
            .unwrap()
            .unwrap();
            received.await.unwrap();
            assert_eq!(sink.pipeline.current_state(), gst::State::Null);
            assert_eq!(frames.load(Ordering::Acquire), 0);
            sink.start().unwrap();
            sink.write(
                AudioPacket::Samples(vec![0.0; 2048]),
                &mut Converter::new(None),
            )
            .unwrap();
            sink.source.end_of_stream().unwrap();
            drain_output(&sink.pipeline, &cancel).await.unwrap();
            assert!(
                frames.load(Ordering::Acquire) > 0,
                "real clocked PCM follows confirmed Premium"
            );
            session.set_user_attribute("type", "free");
            assert!(
                premium_confirmed(&session).is_err(),
                "active playback must reject a withdrawn subscription"
            );
            session.shutdown();
        })
        .unwrap();
    }

    #[test]
    fn native_eligibility_wait_is_cancellable_without_output() {
        tilo_rt::block_on(async {
            let session = Session::new(SessionConfig::default(), None);
            let cancel = Arc::new(AtomicBool::new(false));
            let frames = Arc::new(AtomicU64::new(0));
            let cancelled = cancel.clone();
            let update = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                cancelled.store(true, Ordering::Release);
            });
            let sink = verified_sink(
                &session,
                &cancel,
                &eligibility_output(),
                &frames,
                tilo_test_env::hang_bound(),
            )
            .await
            .unwrap();
            update.await.unwrap();
            assert!(sink.is_none());
            assert_eq!(frames.load(Ordering::Acquire), 0);
            session.shutdown();
        })
        .unwrap();
    }

    #[test]
    fn native_output_rejects_non_premium_and_unknown_product_types() {
        tilo_rt::block_on(async {
            for product in ["free", "open", "", "premium-like"] {
                let session = Session::new(SessionConfig::default(), None);
                session.set_user_attribute("type", product);
                let frames = Arc::new(AtomicU64::new(0));
                let result = verified_sink(
                    &session,
                    &AtomicBool::new(false),
                    &eligibility_output(),
                    &frames,
                    tilo_test_env::hang_bound(),
                )
                .await;
                assert!(result.is_err(), "{product:?} must not authorize audio");
                assert!(session.is_invalid());
                assert_eq!(frames.load(Ordering::Acquire), 0);
            }
        })
        .unwrap();
    }

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
        tilo_test_env::wait_until("clocked PCM to arrive", || {
            rendered.load(Ordering::Relaxed) >= 4
        });
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
    fn spotify_interrupted_pcm_sessions_release_output_before_the_next_start() {
        for _ in 0..3 {
            let rendered = Arc::new(AtomicU64::new(0));
            let count = rendered.clone();
            let frames = Arc::new(AtomicU64::new(0));
            let capture = CaptureOutput {
                on_pcm: Arc::new(|_| {}),
                on_rendered: Arc::new(move || {
                    count.fetch_add(1, Ordering::Relaxed);
                }),
            };
            let mut sink = PcmSink::new(&Output::Capture(capture), &frames).unwrap();
            let pipeline = sink.pipeline.clone();
            sink.start().unwrap();
            let mut converter = Converter::new(PlayerConfig::default().ditherer);
            for _ in 0..8 {
                sink.write(AudioPacket::Samples(vec![0.25; 2048]), &mut converter)
                    .unwrap();
            }
            tilo_test_env::wait_until("each new output to deliver clocked PCM", || {
                rendered.load(Ordering::Relaxed) > 0
            });
            // An interruption must not wait for EOS that will never be sent.
            tilo_rt::block_on(drain_output(&pipeline, &AtomicBool::new(true)))
                .unwrap()
                .unwrap();
            drop(sink);
            assert_eq!(
                pipeline.current_state(),
                gst::State::Null,
                "the old output releases before another session starts"
            );
        }
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
    #[test]
    fn external_oauth_bearer_never_uses_stored_credential_login5() {
        use librespot::core::{Error, error::ErrorKind, token::Token};
        use std::time::SystemTime;
        tilo_rt::block_on(async {
            let session = Session::new(
                SessionConfig {
                    client_id: "fixture-own-oauth-client".into(),
                    ..Default::default()
                },
                None,
            );
            session
                .login5()
                .set_external_token_provider(std::sync::Arc::new(|| {
                    Box::pin(async {
                        Ok(Token {
                            access_token: "fixture-approved-bearer".into(),
                            expires_in: Duration::from_secs(60),
                            token_type: "Bearer".into(),
                            scopes: vec!["streaming".into()],
                            timestamp: SystemTime::now(),
                        })
                    })
                }));
            let token = session.login5().auth_token().await.unwrap();
            assert_eq!(token.access_token, "fixture-approved-bearer");
            assert_eq!(session.client_id(), "fixture-own-oauth-client");
            // This disconnected session has no AP stored credentials. A fallback
            // to login5 would fail, and must never hide provider refusal/expiry.
            session
                .login5()
                .set_external_token_provider(std::sync::Arc::new(|| {
                    Box::pin(async { Err(Error::invalid_argument("fixture-provider-refusal")) })
                }));
            let error = session.login5().auth_token().await.unwrap_err();
            assert_eq!(error.kind, ErrorKind::InvalidArgument);
            assert!(error.to_string().contains("fixture-provider-refusal"));
            session
                .login5()
                .set_external_token_provider(std::sync::Arc::new(|| {
                    Box::pin(async {
                        Ok(Token {
                            access_token: "fixture-expired-bearer".into(),
                            expires_in: Duration::ZERO,
                            token_type: "Bearer".into(),
                            scopes: Vec::new(),
                            timestamp: SystemTime::now(),
                        })
                    })
                }));
            assert_eq!(
                session.login5().auth_token().await.unwrap_err().kind,
                ErrorKind::Unauthenticated
            );
        })
        .unwrap();
    }
}
