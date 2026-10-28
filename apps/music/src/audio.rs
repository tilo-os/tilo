//! Audio output.
//!
//! Every source Music plays itself — a `YouTube` stream, a local or cached
//! file — goes through the shared media engine ([`media::MediaEngine`]), the
//! same playback layer as the video player and Tube: `GStreamer` on the
//! desktop, the page's media element on the web. It is built to play sound
//! only, so a stream that also carries a picture never has it decoded.
//! Spotify tracks play through Spotify's own native transport
//! ([`spotify_audio`]), which this type drives the same way.
//!
//! ## System dependency (host + OS image)
//!
//! Audible output needs the host `GStreamer` runtime, including an audio sink
//! (WASAPI on Windows) and AAC/Opus decoders. On Fedora, install:
//! - a `GStreamer` **audio sink** plugin — `gstreamer1-plugin-pipewire`
//!   (`pipewiresink`) and/or `gstreamer1-plugins-good` (`pulsesink`), plus a
//!   running PipeWire/Pulse server. Without one, `autoaudiosink` silently
//!   falls back to `fakesink` and the user hears nothing.
//! - an **AAC / Opus decoder** for the streams `YouTube` serves —
//!   `gstreamer1-libav` (`avdec_aac`, `avdec_opus`) and/or
//!   `gstreamer1-plugins-bad-free` (`faad`).
//! - `gstreamer1-plugins-good` for `souphttpsrc`, which streams the URL.

#[cfg(feature = "spotify-native")]
pub(crate) use tilo_spotify as spotify_audio;
use ui::lock::MutexRecover as _;

/// The UI-owned audio handle: the media engine for streams and files, the
/// native transport for Spotify. Nothing native is built until the first
/// source is loaded.
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
    /// What the last stopped source rendered: a stopped source keeps its
    /// evidence of having played until the next one loads.
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

    /// Start an independently authorized native Spotify transport, never a URL resolver.
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

    /// Safe, device-local diagnostics for the resource action door.
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

    /// Point the engine at `uri` (an `http(s)://` stream or a `file://`
    /// track), paused at the start.
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
        engine.open(source_for(uri));
        engine.set_volume(volume);
    }

    /// Poll while a loaded source has not rendered sound yet; a stopped
    /// engine keeps no idle window repainting.
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

    /// Stop and release the source: the pipeline is torn down, and the next
    /// load builds a fresh one.
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

    /// Seek to `ms`. The engine holds a seek asked before the source is
    /// ready and applies it once it is.
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

    /// Whether this build plays anything: `GStreamer` on the desktop, the
    /// page's media element on the web.
    pub fn is_live() -> bool {
        cfg!(feature = "audio") || media::available(media::Feature::Playback)
    }

    /// Whether the engine is playing a loaded source that has not failed.
    /// The now-playing indicator reads this so it only reflects genuine
    /// playback, never a fake timer.
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

    /// Describe what plays to the system's now-playing controls the
    /// platform's own player reaches (a page's Media Session). Where the
    /// app's MPRIS service is what the system hears, nothing more is said.
    pub fn set_now_playing(&mut self, now: &media::NowPlaying) {
        #[cfg(not(feature = "audio"))]
        if let Some(engine) = &mut self.engine {
            engine.set_now_playing(now);
        }
        #[cfg(feature = "audio")]
        let _ = now;
    }

    /// What the platform player's system controls asked since this was
    /// last taken: next and previous track, and the play, pause and seek
    /// it carried out already.
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

    /// Drain what the engine reported since the last frame. Returns whether
    /// the source failed; [`Self::take_error`] says why.
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

    /// How much decoded audio has reached the output since the source
    /// loaded. Non-zero proves sound is really coming out, not merely that
    /// the pipeline said it was playing. In a page, whose media element
    /// reports no buffers, its metadata having loaded is what proves the
    /// source decodes.
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

    /// Factory name of the concrete sink the output plugged (`pipewiresink`,
    /// `pulsesink`, `alsasink`), once it has one; `fakesink` means no real
    /// sink plugin is installed and nothing is heard.
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

    /// Whether the plugged sink is a real audio sink, not a `fakesink`.
    pub fn audio_sink_is_real(&self) -> Option<bool> {
        self.actual_audio_sink_name()
            .map(|name| !name.contains("fakesink"))
    }

    /// Where a source stands on its way to sound, for a test's failure
    /// message.
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

/// The engine source for `uri`: a `file://` URI is the local file it names,
/// anything else a stream the engine fetches. A page has no local files: the
/// media element fetches every URI itself.
fn source_for(uri: &str) -> media::Source {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = url::Url::parse(uri)
        .ok()
        .filter(|url| url.scheme() == "file")
        .and_then(|url| url.to_file_path().ok())
    {
        return media::Source::for_path(&path);
    }
    media::Source::Remote {
        url: uri.to_owned(),
    }
}

/// A device-local transfer for an offline cache request. It exposes only
/// aggregate progress and a terminal result; paths and bytes stay within the
/// storage/audio layers.
pub struct OfflineDownload {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fetched: std::sync::Arc<std::sync::atomic::AtomicU64>,
    expected: std::sync::Arc<std::sync::atomic::AtomicU64>,
    result: std::sync::mpsc::Receiver<Result<(), String>>,
}

impl OfflineDownload {
    /// Start downloading a resolved source into the device-local cache.
    ///
    /// # Errors
    ///
    /// Returns an error when this build cannot perform network transfers or
    /// when the local transfer thread cannot be created.
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

/// Fetch `uri` whole into `path`, publishing progress, until it ends, fails
/// or is cancelled. The source must declare its length, and the file must
/// come to exactly that length: a body that stops short or runs over is not
/// a track.
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
    // A blocking client applies its timeout to each connect, read and write,
    // so a stalled source fails instead of holding the download forever.
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
