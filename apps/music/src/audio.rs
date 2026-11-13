//! Audio output: every source Music plays goes through the shared media engine,
//! `GStreamer` on the desktop and the page's media element on the web. Spotify tracks
//! go through Spotify's own transport. Audible output needs the host's `GStreamer`
//! sink and AAC/Opus plugins: without a sink plugin `autoaudiosink` falls back to
//! `fakesink` and nothing is heard.

#[cfg(feature = "spotify-native")]
pub(crate) use tilo_spotify as spotify_audio;
use ui::lock::MutexRecover as _;

/// The audio handles: the media engine, and Spotify's native transport.
pub struct AudioEngine {
    #[cfg(feature = "spotify-native")]
    native: Option<spotify_audio::NativeAudio>,
    engine: Option<media::MediaEngine>,
    current_uri: Option<String>,
    volume: f32,
    want_play: bool,
    duration_known: bool,
    eos: bool,
    error: std::sync::Mutex<Option<String>>,
    /// What the last stopped source rendered, until the next one loads.
    rendered_before_stop: u64,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "spotify-native")]
            native: None,
            engine: None,
            current_uri: None,
            volume: 0.8,
            want_play: false,
            duration_known: false,
            eos: false,
            error: std::sync::Mutex::new(None),
            rendered_before_stop: 0,
        }
    }

    /// Start Spotify's own transport, never a URL resolver.
    pub fn load_spotify(&mut self, uri: &str, account: &str) -> Result<(), String> {
        self.stop();
        #[cfg(feature = "spotify-native")]
        {
            let native = spotify_audio::NativeAudio::start(
                uri.to_owned(),
                account.to_owned(),
                spotify_audio::Output::System,
            )?;
            native.volume(self.volume);
            self.current_uri = Some(uri.to_owned());
            self.native = Some(native);
            Ok(())
        }
        #[cfg(not(feature = "spotify-native"))]
        {
            let _ = (uri, account);
            Err("This build does not include native Spotify playback".into())
        }
    }

    pub fn spotify_startup_stage(&self) -> Option<&'static str> {
        #[cfg(feature = "spotify-native")]
        {
            self.native
                .as_ref()
                .map(spotify_audio::NativeAudio::startup_stage)
        }
        #[cfg(not(feature = "spotify-native"))]
        {
            None
        }
    }

    pub fn prepare_spotify_next(&self, uri: Option<&str>) {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            native.prepare_next(uri);
        }
        #[cfg(not(feature = "spotify-native"))]
        let _ = uri;
    }

    pub fn take_spotify_transition(&mut self) -> Option<String> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            let uri = native.take_transition()?;
            self.current_uri = Some(uri.clone());
            return Some(uri);
        }
        None
    }

    /// Point the engine at `uri`, an `http(s)` stream or a `file:` path, paused at the start.
    pub fn load(&mut self, uri: &str) {
        #[cfg(feature = "spotify-native")]
        {
            self.native = None;
        }
        self.current_uri = Some(uri.to_owned());
        self.want_play = false;
        self.duration_known = false;
        self.eos = false;
        *self.error.lock_recover() = None;
        self.rendered_before_stop = 0;
        if !Self::is_live() {
            return;
        }
        let volume = self.volume;
        let engine = self.engine.get_or_insert_with(|| {
            media::MediaEngine::with_config(media::EngineConfig::default().audio_only())
        });
        engine.open(media::Source::for_uri(uri));
        engine.set_volume(volume);
    }

    /// Poll while a loaded source has not rendered sound; a stopped engine repaints nothing.
    pub fn needs_poll(&self) -> bool {
        Self::is_live()
            && self.current_uri.is_some()
            && self.error.lock_recover().is_none()
            && self.frames_rendered() == 0
    }

    pub fn play(&mut self) {
        self.want_play = true;
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            native.play();
            return;
        }
        if let Some(engine) = &mut self.engine {
            engine.play();
        }
    }

    pub fn pause(&mut self) {
        self.want_play = false;
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            native.pause();
            return;
        }
        if let Some(engine) = &mut self.engine {
            engine.pause();
        }
    }

    /// Stop and release the source; the next load builds a fresh pipeline.
    pub fn stop(&mut self) {
        self.rendered_before_stop = self.frames_rendered();
        #[cfg(feature = "spotify-native")]
        {
            self.native = None;
        }
        self.current_uri = None;
        self.want_play = false;
        self.duration_known = false;
        self.eos = false;
        *self.error.lock_recover() = None;
        self.engine = None;
    }

    /// Seek to `ms`, held until the source is ready.
    pub fn seek_ms(&mut self, ms: u64) {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            native.seek(ms);
            return;
        }
        if self.current_uri.is_none() {
            return;
        }
        if let Some(engine) = &mut self.engine {
            engine.seek_ms(ms);
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            native.volume(self.volume);
            return;
        }
        if let Some(engine) = &mut self.engine {
            engine.set_volume(self.volume);
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn current_uri(&self) -> Option<&str> {
        self.current_uri.as_deref()
    }

    pub fn position_ms(&self) -> Option<u64> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.position();
        }
        self.current_uri.as_ref()?;
        self.engine.as_ref().map(media::MediaEngine::position_ms)
    }

    pub fn duration_ms(&self) -> Option<u64> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.duration_ms();
        }
        self.engine.as_ref()?.duration_ms()
    }

    pub fn has_stream(&self) -> bool {
        Self::is_live() && self.current_uri.is_some()
    }

    pub fn is_live() -> bool {
        cfg!(feature = "audio") || media::available(media::Feature::Playback)
    }

    /// Whether a loaded source is playing and has not failed; the now-playing indicator reads
    /// this, so it never follows a timer.
    pub fn is_playing_real(&self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.playing();
        }
        self.current_uri.is_some()
            && self.error.lock_recover().is_none()
            && self
                .engine
                .as_ref()
                .is_some_and(media::MediaEngine::is_playing)
    }

    /// Describe what plays to the platform player's own controls, where the app's MPRIS is
    /// not what the system hears.
    pub fn set_now_playing(&mut self, now: &media::NowPlaying) {
        #[cfg(not(feature = "audio"))]
        if let Some(engine) = &mut self.engine {
            engine.set_now_playing(now);
        }
        #[cfg(feature = "audio")]
        let _ = now;
    }

    /// What the platform player's controls asked since this was last taken.
    pub fn take_now_playing_actions(&mut self) -> Vec<media::NowPlayingAction> {
        #[cfg(not(feature = "audio"))]
        return self
            .engine
            .as_mut()
            .map(media::MediaEngine::take_now_playing_actions)
            .unwrap_or_default();
        #[cfg(feature = "audio")]
        Vec::new()
    }

    /// Asked to play, playing, and sound has reached the output.
    #[cfg(feature = "audio")]
    pub(crate) fn is_playing_confirmed(&self) -> bool {
        self.want_play
            && self.is_playing_real()
            && !self.reached_eos()
            && self.frames_rendered() > 0
    }

    /// Drain what the engine reported since the last frame; whether the source failed.
    pub fn pump(&mut self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.pump();
        }
        if self.current_uri.is_none() {
            return false;
        }
        if let Some(engine) = &mut self.engine {
            for event in engine.poll() {
                match event {
                    media::MediaEvent::DurationKnown { .. } => self.duration_known = true,
                    media::MediaEvent::Ended => self.eos = true,
                    media::MediaEvent::Error(reason) => {
                        tracing::warn!(target: "music::audio", %reason, "playback failed");
                        *self.error.lock_recover() = Some(reason);
                    }
                    _ => {}
                }
            }
        }
        self.error.lock_recover().is_some()
    }

    pub fn take_error(&self) -> Option<String> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.error();
        }
        self.error.lock_recover().take()
    }

    pub fn reached_eos(&self) -> bool {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.eos();
        }
        self.eos
    }

    /// How much decoded audio reached the output: non-zero proves sound rather than a pipeline
    /// that says it plays. A page's media element reports no buffers, so loaded metadata is
    /// its proof.
    pub fn frames_rendered(&self) -> u64 {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.frames();
        }
        if cfg!(feature = "audio") {
            self.engine.as_ref().map_or(
                self.rendered_before_stop,
                media::MediaEngine::audio_buffers_rendered,
            )
        } else {
            u64::from(self.duration_known)
        }
    }

    /// The sink the output plugged, once it has one; `fakesink` means nothing is heard.
    pub fn actual_audio_sink_name(&self) -> Option<String> {
        #[cfg(feature = "spotify-native")]
        if let Some(native) = &self.native {
            return native.sink();
        }
        #[cfg(feature = "audio")]
        {
            use gstreamer::prelude::*;
            let sink = self.engine.as_ref()?.audio_sink()?;
            // `autoaudiosink` is a bin that owns the concrete sink as its
            // child; a governed output's sink is the element itself.
            let element = match sink.dynamic_cast_ref::<gstreamer::ChildProxy>() {
                Some(proxy) if proxy.children_count() > 0 => proxy
                    .child_by_index(0)?
                    .downcast::<gstreamer::Element>()
                    .ok()?,
                _ => sink,
            };
            element.factory().map(|factory| factory.name().to_string())
        }
        #[cfg(not(feature = "audio"))]
        None
    }

    pub fn audio_sink_is_real(&self) -> Option<bool> {
        self.actual_audio_sink_name()
            .map(|name| !name.contains("fakesink"))
    }

    #[cfg(test)]
    pub fn diagnostics(&self) -> String {
        format!(
            "uri {:?}, engine {}, playing {}, frames {}, want_play {}, duration {:?}, eos {}, \
             error {:?}",
            self.current_uri,
            self.engine.is_some(),
            self.is_playing_real(),
            self.frames_rendered(),
            self.want_play,
            self.duration_ms(),
            self.eos,
            self.error.lock_recover(),
        )
    }
}

/// A device-local transfer for an offline copy: progress and a terminal result only.
pub struct OfflineDownload {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fetched: std::sync::Arc<std::sync::atomic::AtomicU64>,
    expected: std::sync::Arc<std::sync::atomic::AtomicU64>,
    result: std::sync::mpsc::Receiver<Result<(), String>>,
}

impl OfflineDownload {
    pub fn start(
        track_id: String,
        uri: String,
        cache_root: std::path::PathBuf,
    ) -> Result<Self, String> {
        #[cfg(feature = "live")]
        {
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let fetched = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let expected = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let (tx, result) = std::sync::mpsc::channel();
            let partial = cache_root.join("partials").join(format!(
                "{}-{}.part",
                std::process::id(),
                next_download_id()
            ));
            let parent = partial
                .parent()
                .ok_or_else(|| "offline cache path has no parent".to_string())?;
            crate::files::create_dir_all(parent).map_err(|error| error.to_string())?;
            let progress = (cancel.clone(), fetched.clone(), expected.clone());
            ui::rt::spawn_thread("tilo-music-offline-dl", move || {
                let (cancel, fetched, expected) = progress;
                let outcome =
                    download(&uri, &partial, &cancel, &fetched, &expected).and_then(|()| {
                        crate::storage::install_completed_media_file(
                            &cache_root,
                            &track_id,
                            &partial,
                            crate::storage::media_cache_quota(),
                        )
                        .map_err(|error| error.to_string())
                    });
                if outcome.is_err() {
                    crate::files::remove(&partial);
                }
                let _ = tx.send(outcome);
            })
            .map_err(|error| error.to_string())?;
            Ok(Self {
                cancel,
                fetched,
                expected,
                result,
            })
        }
        #[cfg(not(feature = "live"))]
        {
            let _ = (track_id, uri, cache_root);
            Err("this Music build cannot download offline media".into())
        }
    }

    pub fn cancel(&self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[must_use]
    pub fn progress(&self) -> Option<f32> {
        let expected = self.expected.load(std::sync::atomic::Ordering::Relaxed);
        (expected > 0).then(|| {
            (self.fetched.load(std::sync::atomic::Ordering::Relaxed) as f32 / expected as f32)
                .clamp(0.0, 1.0)
        })
    }

    pub fn take_result(&self) -> Option<Result<(), String>> {
        self.result.try_recv().ok()
    }
}

/// A process-unique id so concurrent downloads get distinct partial files.
#[cfg(feature = "live")]
fn next_download_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Fetch `uri` whole into `path` while publishing progress. The source declares a length
/// and the file must match it: a body short or long is not a track.
#[cfg(feature = "live")]
fn download(
    uri: &str,
    path: &std::path::Path,
    cancel: &std::sync::atomic::AtomicBool,
    fetched: &std::sync::atomic::AtomicU64,
    expected: &std::sync::atomic::AtomicU64,
) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::sync::atomic::Ordering::Relaxed;
    // The blocking client times out each read and write, so a stalled source fails.
    let client = tilo_net::http::blocking_client()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?;
    let mut response = client
        .get(uri)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| error.to_string())?;
    let length = response
        .content_length()
        .filter(|length| *length > 0)
        .ok_or("the source did not say how long the track is")?;
    expected.store(length, Relaxed);
    let mut file = crate::files::Growing::create(path).map_err(|error| error.to_string())?;
    let mut buffer = vec![0u8; 256 * 1024].into_boxed_slice();
    let mut written = 0u64;
    loop {
        if cancel.load(Relaxed) {
            return Err("offline download cancelled".into());
        }
        let read = response
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        written = written.saturating_add(read as u64);
        if written > length {
            return Err("the source sent more than the track's length".into());
        }
        let chunk = buffer.get(..read).ok_or("read past the buffer")?;
        file.write_all(chunk).map_err(|error| error.to_string())?;
        fetched.store(written, Relaxed);
    }
    if written < length {
        return Err("the source did not deliver a complete audio file".into());
    }
    Ok(())
}
