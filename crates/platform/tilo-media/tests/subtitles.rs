//! A text track a clip carries is drawn in the picture when the person selects it.
//!
//! `playbin` renders text streams itself (its flags include `text`, `gst_backend.rs`): the
//! engine's own sink receives frames that already carry the cue, so a selected subtitle needs no
//! overlay of Tilo's. This generates a clip with an embedded subtitle track at test time (no
//! binary media is committed), selects the track, and reads the pixels of a frame inside the cue
//! against a frame outside it.

#[cfg(feature = "decode")]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    reason = "a fixture walks a picture's pixels: they are far inside the arithmetic's range"
)]
mod real {
    use std::path::PathBuf;

    use gstreamer::prelude::*;
    use tilo_media::{EngineConfig, MediaEngine};

    use crate::common::{registry_policy_lock, shared_gst_registry};

    /// How bright a pixel is counted as the caption's white.
    const WHITE: u8 = 160;

    /// Lit pixels that mean the cue was drawn. The caption's glyph strokes at 320x180 come to a
    /// couple of dozen; a frame without the cue is black and lights none. Twenty is the floor a
    /// drawn cue clears with room for the anti-aliasing an overlay version may lose, and it
    /// cannot be met by an empty picture.
    const CUE_PIXELS: usize = 20;

    /// A 320x180 VP9 clip, five seconds of black, with one subtitle cue from 1s to 4s: the cue
    /// is the only white in the picture, so its pixels say the text was drawn.
    fn subtitle_fixture() -> PathBuf {
        let cache = std::env::var_os("TILO_TEST_CACHE_DIR")
            .filter(|dir| !dir.is_empty())
            .map_or_else(std::env::temp_dir, PathBuf::from)
            .join("tilo-media-test-fixtures");
        std::fs::create_dir_all(&cache).expect("create media fixture cache directory");
        let srt = cache.join("captioned-fixture.srt");
        let clip = cache.join("captioned-fixture-v1.mkv");
        if clip.is_file() {
            return clip;
        }
        shared_gst_registry();
        gstreamer::init().expect("initialize GStreamer for the captioned fixture");
        for element in ["vp9enc", "matroskamux", "subparse"] {
            assert!(
                gstreamer::ElementFactory::find(element).is_some(),
                "the decode-test stack must provide {element} so its captioned subject can be \
                 generated"
            );
        }
        std::fs::write(
            &srt,
            "1\n00:00:01,000 --> 00:00:04,000\nA caption in the picture\n",
        )
        .expect("write the fixture's subtitle file");
        let description = "videotestsrc pattern=black num-buffers=150 ! \
             video/x-raw,format=I420,width=320,height=180,framerate=30/1 ! \
             vp9enc deadline=1 cpu-used=8 row-mt=true lag-in-frames=0 ! queue ! mux.video_0 \
             filesrc name=captions ! subparse ! text/x-raw,format=utf8 ! queue ! mux.subtitle_0 \
             matroskamux name=mux ! filesink name=captioned";
        let pipeline = gstreamer::parse::launch(description)
            .expect("construct the captioned fixture pipeline")
            .downcast::<gstreamer::Pipeline>()
            .expect("the fixture graph is a pipeline");
        pipeline
            .by_name("captions")
            .expect("the fixture's subtitle file")
            .set_property("location", srt.to_string_lossy().as_ref());
        pipeline
            .by_name("captioned")
            .expect("the fixture's output file")
            .set_property("location", clip.to_string_lossy().as_ref());
        pipeline
            .set_state(gstreamer::State::Playing)
            .expect("start the captioned fixture encoder");
        let bus = pipeline.bus().expect("the fixture pipeline bus");
        let deadline = tilo_test_env::hang_deadline();
        let mut reached_eos = false;
        while std::time::Instant::now() < deadline {
            let Some(message) = bus.timed_pop(gstreamer::ClockTime::from_mseconds(500)) else {
                continue;
            };
            match message.view() {
                gstreamer::MessageView::Eos(_) => {
                    reached_eos = true;
                    break;
                }
                gstreamer::MessageView::Error(error) => panic!(
                    "the captioned fixture failed: {} ({:?})",
                    error.error(),
                    error.debug()
                ),
                _ => {}
            }
        }
        let _ = pipeline.set_state(gstreamer::State::Null);
        assert!(reached_eos, "the captioned fixture timed out before EOS");
        assert!(
            std::fs::metadata(&clip).is_ok_and(|metadata| metadata.len() > 512),
            "the captioned fixture is not a real encoded clip: {}",
            clip.display()
        );
        clip
    }

    /// The white pixels in the lower third of the picture, where a cue is drawn.
    fn caption_pixels(engine: &MediaEngine) -> usize {
        let Some(frame) = engine.latest_frame() else {
            panic!("no picture was decoded");
        };
        let top = (frame.height / 3) * 2;
        let mut white = 0;
        for y in top..frame.height {
            for x in 0..frame.width {
                let at = ((y * frame.width + x) * 4) as usize;
                let (r, g, b) = (frame.rgba[at], frame.rgba[at + 1], frame.rgba[at + 2]);
                if r > WHITE && g > WHITE && b > WHITE {
                    white += 1;
                }
            }
        }
        white
    }

    #[test]
    fn a_selected_text_track_is_drawn_in_the_picture() {
        let _registry = registry_policy_lock();
        let clip = subtitle_fixture();
        let mut engine = MediaEngine::with_config(EngineConfig::default().silent());
        assert!(
            engine.is_real_decode(),
            "these assertions require the real pipeline"
        );
        engine.open(tilo_media::Source::LocalFile(clip));
        // Preroll first: the demuxer exposes the streams it carries once the pipeline has
        // negotiated them, so the clip's text track is listed after its first picture.
        engine
            .seek_and_wait(0, tilo_test_env::hang_bound())
            .expect("the clip's first picture");
        tilo_test_env::wait_until("the clip's text track to be listed", || {
            engine.poll();
            !engine.subtitle_tracks().is_empty()
        });
        assert!(
            engine.select_subtitle_track(Some(0)),
            "the text track the clip carries is selectable: {:?}",
            engine.subtitle_tracks()
        );

        // Inside the cue: the caption's words are in the picture.
        engine
            .seek_and_wait(2_000, tilo_test_env::hang_bound())
            .expect("a picture inside the cue");
        let inside = caption_pixels(&engine);
        assert!(
            inside > CUE_PIXELS,
            "the cue's pixels are in the frame inside it: {inside} lit pixels"
        );

        // After it ended: the same picture without the cue.
        engine
            .seek_and_wait(4_500, tilo_test_env::hang_bound())
            .expect("a picture after the cue");
        let after = caption_pixels(&engine);
        assert!(
            after * 4 < inside,
            "the picture after the cue is black again: {after} lit pixels against {inside}"
        );
    }
}
