//! Governed decoder controls must not block the executor serving their output lane.
use gstreamer::{self as gst, prelude::*};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use ui::lock::MutexRecover as _;

pub(super) struct Pipeline {
    pub element: gst::Element,
    pub output: Arc<media::audio_output::gst::Bridge>,
    pub frames: Arc<AtomicU64>,
    pub eos: Arc<std::sync::atomic::AtomicBool>,
    pub failed: Arc<std::sync::atomic::AtomicBool>,
    pub error: Arc<Mutex<Option<String>>>,
}

#[derive(Clone, PartialEq)]
struct Intent {
    version: u64,
    source: u64,
    uri: Option<String>,
    playing: bool,
    volume: f32,
    seek: Option<(u64, u64)>,
    closed: bool,
}

#[cfg(test)]
struct SeekResult {
    source: u64,
    id: u64,
    result: Result<(), String>,
}

struct Shared {
    intent: Mutex<Intent>,
    applied: AtomicU64,
    applied_version: AtomicU64,
    #[cfg(test)]
    seek_result: Mutex<Option<SeekResult>>,
}

pub(super) struct Controls {
    shared: Arc<Shared>,
    wake: mpsc::SyncSender<()>,
}

impl Controls {
    pub fn spawn(pipeline: Pipeline) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            intent: Mutex::new(Intent {
                version: 0,
                source: 0,
                uri: None,
                playing: false,
                volume: 0.8,
                seek: None,
                closed: false,
            }),
            applied: AtomicU64::new(0),
            applied_version: AtomicU64::new(0),
            #[cfg(test)]
            seek_result: Mutex::new(None),
        });
        // The mailbox carries one wakeup, not a growing list of stale commands.
        let (wake, receive) = mpsc::sync_channel(1);
        let worker = shared.clone();
        ui::rt::spawn_thread("music-audio-controls", move || {
            run(&pipeline, &worker, &receive);
            pipeline.output.close();
            let _ = pipeline.element.set_state(gst::State::Null);
        })
        .map_err(|error| format!("cannot start audio controls: {error}"))?;
        Ok(Self { shared, wake })
    }

    fn update(&self, change: impl FnOnce(&mut Intent)) {
        let changed = {
            let mut intent = self.shared.intent.lock_recover();
            let previous = intent.clone();
            change(&mut intent);
            if *intent == previous {
                false
            } else {
                intent.version = intent.version.wrapping_add(1);
                true
            }
        };
        if changed {
            // Full means a worker wakeup is already pending. Latest intent wins.
            let _ = self.wake.try_send(());
        }
    }

    pub fn source(&self, uri: Option<&str>, volume: f32) {
        self.update(|intent| {
            intent.source = intent.source.wrapping_add(1);
            intent.uri = uri.map(str::to_owned);
            intent.playing = false;
            intent.volume = volume;
            intent.seek = None;
        });
    }

    pub fn playing(&self, playing: bool) {
        self.update(|intent| intent.playing = playing);
    }
    pub fn volume(&self, volume: f32) {
        self.update(|intent| intent.volume = volume);
    }

    pub fn seek(&self, ms: u64) -> bool {
        let mut accepted = false;
        self.update(|intent| {
            if intent.uri.is_some() && !intent.closed {
                let previous = intent.seek.map_or(0, |(id, _)| id);
                intent.seek = Some((previous.wrapping_add(1), ms));
                accepted = true;
            }
        });
        accepted
    }

    pub fn ready(&self) -> bool {
        let intent = self.shared.intent.lock_recover();
        intent.source == self.shared.applied.load(Ordering::Acquire)
            && intent.version == self.shared.applied_version.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn seek_result(&self) -> Option<Result<(), String>> {
        let current = {
            let intent = self.shared.intent.lock_recover();
            intent.seek.map(|(id, _)| (intent.source, id))
        };
        self.shared
            .seek_result
            .lock_recover()
            .as_ref()
            .filter(|result| current == Some((result.source, result.id)))
            .map(|result| result.result.clone())
    }
}

impl Drop for Controls {
    fn drop(&mut self) {
        self.update(|intent| intent.closed = true);
    }
}

fn run(pipeline: &Pipeline, shared: &Shared, receive: &mpsc::Receiver<()>) {
    let mut source = 0;
    let mut last_seek = None;
    while receive.recv().is_ok() {
        let intent = shared.intent.lock_recover().clone();
        if intent.closed {
            break;
        }
        let outcome = apply(pipeline, &intent, &mut source, &mut last_seek);
        #[cfg(test)]
        if let Some((id, result)) = &outcome.seek {
            *shared.seek_result.lock_recover() = Some(SeekResult {
                source: intent.source,
                id: *id,
                result: result.clone(),
            });
        }
        if shared.intent.lock_recover().source == intent.source
            && let Err(error) = outcome.result
        {
            pipeline.failed.store(true, Ordering::Release);
            *pipeline.error.lock_recover() = Some(error);
        }
        // Publish completion only after its observation and failure are visible.
        shared.applied.store(intent.source, Ordering::Release);
        shared
            .applied_version
            .store(intent.version, Ordering::Release);
    }
}

struct Outcome {
    result: Result<(), String>,
    #[cfg(test)]
    seek: Option<(u64, Result<(), String>)>,
}

fn apply(
    pipeline: &Pipeline,
    intent: &Intent,
    source: &mut u64,
    last_seek: &mut Option<u64>,
) -> Outcome {
    #[cfg(test)]
    let mut seek = None;
    let result = (|| {
        if *source != intent.source {
            pipeline
                .element
                .set_state(gst::State::Null)
                .map_err(|error| error.to_string())?;
            pipeline.output.reset();
            if let Some(bus) = pipeline.element.bus() {
                while bus.pop().is_some() {}
            }
            pipeline.frames.store(0, Ordering::Release);
            pipeline.eos.store(false, Ordering::Release);
            pipeline.failed.store(false, Ordering::Release);
            *pipeline.error.lock_recover() = None;
            *source = intent.source;
            *last_seek = None;
            if let Some(uri) = &intent.uri {
                pipeline.element.set_property("uri", uri);
            }
        }
        if intent.uri.is_none() {
            return Ok(());
        }
        pipeline.element.set_property("mute", false);
        pipeline
            .element
            .set_property("volume", f64::from(intent.volume));
        if intent.playing {
            pipeline.output.pause(false);
        }
        pipeline
            .element
            .set_state(if intent.playing {
                gst::State::Playing
            } else {
                gst::State::Paused
            })
            .map_err(|error| error.to_string())?;
        // Finish the decoded buffer before suspending host consumption.
        if !intent.playing {
            pipeline.output.pause(true);
        }
        if let Some((id, ms)) = intent.seek
            && *last_seek != Some(id)
        {
            let result = seek_pipeline(pipeline, ms);
            *last_seek = Some(id);
            #[cfg(test)]
            {
                seek = Some((id, result.clone()));
            }
            result?;
        }
        Ok(())
    })();
    Outcome {
        result,
        #[cfg(test)]
        seek,
    }
}

fn seek_pipeline(pipeline: &Pipeline, ms: u64) -> Result<(), String> {
    let (result, state, _) = pipeline.element.state(gst::ClockTime::from_seconds(5));
    if result.is_err() || !matches!(state, gst::State::Paused | gst::State::Playing) {
        return Err("audio source did not become seekable within five seconds".into());
    }
    let mut seeking = gst::query::Seeking::new(gst::Format::Time);
    if pipeline.element.query(&mut seeking) && !seeking.result().0 {
        return Err("this audio source does not support seeking".into());
    }
    let target = pipeline
        .element
        .query_duration::<gst::ClockTime>()
        .filter(|duration| duration.mseconds() > 0)
        .map_or(ms, |duration| {
            ms.min(duration.mseconds().saturating_sub(250))
        });
    pipeline.output.flush();
    pipeline
        .element
        .seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::from_mseconds(target),
        )
        .map_err(|error| format!("audio source rejected the seek: {error}"))
}
