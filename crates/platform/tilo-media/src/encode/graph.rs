//! Frames and samples pushed into an encode graph, and the file finished.
//!
//! The graph is a `gst-launch` description whose raw inputs are `appsrc`s:
//! `tilo_src` takes RGBA frames, and `tilo_audio`, when the graph has one,
//! takes interleaved S16LE samples timed with them. Its sink writes the
//! file. Which encoders and muxer the description names is the caller's
//! choice (see [`super::Selection`]); this only feeds it and drains it.

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

/// A running encode graph.
pub struct Graph {
    pipeline: gst::Pipeline,
    video: gst_app::AppSrc,
    audio: Option<gst_app::AppSrc>,
    fps: f64,
}

/// The start of frame `index` at `fps`. Cumulative boundaries avoid
/// truncating 1/30 or 1/60 second once per frame, which otherwise shortens
/// the file and drifts against the sound.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a frame count far inside f64's exact range, rounded to whole nanoseconds"
)]
pub fn frame_time(index: u64, fps: f64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds((index as f64 * 1_000_000_000.0 / fps).round() as u64)
}

impl Graph {
    /// Build `desc` and start it. `with_audio` says the graph has a
    /// `tilo_audio` input to feed.
    ///
    /// # Errors
    /// Why the graph could not be built or started.
    pub fn start(desc: &str, with_audio: bool, fps: f64) -> Result<Self, String> {
        gst::init().map_err(|e| format!("GStreamer init failed: {e}"))?;
        let pipeline = gst::parse::launch(desc)
            .map_err(|e| format!("build export pipeline: {e}"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "export pipeline is not a Pipeline".to_string())?;
        let input = |name: &str| {
            pipeline
                .by_name(name)
                .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
                .inspect(|src| src.set_format(gst::Format::Time))
        };
        let video = input("tilo_src").ok_or_else(|| "export appsrc not found".to_string())?;
        let audio = if with_audio {
            Some(input("tilo_audio").ok_or_else(|| "export audio appsrc not found".to_string())?)
        } else {
            None
        };
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("start export pipeline: {e}"))?;
        Ok(Self {
            pipeline,
            video,
            audio,
            fps,
        })
    }

    /// Push frame `index` and the sound that plays during it (when the
    /// graph has sound), both timed to the frame.
    ///
    /// # Errors
    /// Why the graph refused them.
    pub fn push(&self, index: u64, rgba: &[u8], sound: Option<&[u8]>) -> Result<(), String> {
        let pts = frame_time(index, self.fps);
        let duration = frame_time(index.saturating_add(1), self.fps).saturating_sub(pts);
        let buffer = |bytes: &[u8]| {
            let mut buffer = gst::Buffer::from_slice(bytes.to_vec());
            if let Some(b) = buffer.get_mut() {
                b.set_pts(pts);
                b.set_duration(duration);
            }
            buffer
        };
        self.video
            .push_buffer(buffer(rgba))
            .map_err(|e| format!("push export frame {index}: {e:?}"))?;
        if let (Some(audio), Some(sound)) = (&self.audio, sound) {
            audio
                .push_buffer(buffer(sound))
                .map_err(|e| format!("push export audio {index}: {e:?}"))?;
        }
        Ok(())
    }

    /// End the inputs and wait for the muxer to finish the file.
    ///
    /// `tick` is asked about every 200 ms whether to keep waiting, told
    /// whether the graph said anything since; an `Err` stops the wait with
    /// that error (a cancel, or a flush that has gone quiet too long).
    ///
    /// # Errors
    /// The graph's own error, or `tick`'s.
    pub fn finish(self, mut tick: impl FnMut(bool) -> Result<(), String>) -> Result<(), String> {
        self.video
            .end_of_stream()
            .map_err(|e| format!("end export stream: {e:?}"))?;
        if let Some(audio) = &self.audio {
            audio
                .end_of_stream()
                .map_err(|e| format!("end export audio stream: {e:?}"))?;
        }
        let bus = self
            .pipeline
            .bus()
            .ok_or_else(|| "export pipeline bus".to_string())?;
        loop {
            // Short waits rather than one long one, so a cancel arriving
            // while the muxer flushes is still noticed promptly.
            let msg = bus.timed_pop(gst::ClockTime::from_mseconds(200));
            tick(msg.is_some())?;
            match msg.as_deref().map(gst::MessageRef::view) {
                Some(gst::MessageView::Eos(_)) => return Ok(()),
                Some(gst::MessageView::Error(e)) => {
                    return Err(format!(
                        "export pipeline error: {} ({})",
                        e.error(),
                        e.debug().unwrap_or_default()
                    ));
                }
                _ => {}
            }
        }
    }
}

impl Drop for Graph {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_boundaries_do_not_accumulate_timestamp_roundoff() {
        for fps in [24_u64, 30, 60] {
            #[expect(clippy::cast_precision_loss, reason = "small exact integers")]
            let rate = fps as f64;
            assert_eq!(
                frame_time(3600 * fps, rate),
                gst::ClockTime::from_seconds(3600),
            );
        }
    }
}
