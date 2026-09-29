//! Proof that a seek lands on the frame that was asked for.
//!
//! The fixture's keyframes sit **8.333 seconds apart** (0, 8.333, 16.667, 25,
//! 33.333, 41.667, 50 — verified with `ffprobe -skip_frame nokey`). That makes
//! the difference between the two seek modes impossible to mistake for noise:
//!
//! * `SeekMode::Keyframe` — the old behaviour — cannot land closer than the
//!   nearest keyframe, so a request for 12.500s comes back **seconds** away;
//! * `SeekMode::Accurate` decodes forward from that keyframe and presents the
//!   picture at 12.500s, within one 33ms frame interval.
//!
//! Every frame between two keyframes was simply unreachable before: 249 of
//! every 250 pictures in this clip could not be displayed at all.
//!
//! # About the timebase
//!
//! Container edit lists and decoder preroll can make the first decoded PTS
//! non-zero. Every assertion below measures that base from the clip itself
//! ([`common::stream_pts_base_ms`]) rather than assuming a timestamp origin,
//! so the tests calibrate against the stream's timeline and genuine drift
//! still fails.

mod common;

#[cfg(feature = "decode")]
mod real {
    use super::common::*;
    use tilo_media::{EngineConfig, MediaEngine, SeekMode};

    /// A time deep inside a GOP: 12.500s sits 4.167s past the keyframe at
    /// 8.333s and 4.167s before the one at 16.667s, so no keyframe seek can
    /// come near it.
    const MID_GOP_MS: u64 = 12_500;

    fn engine() -> MediaEngine {
        // RGBA acceptance so frame content can be compared pixel by pixel.
        let mut e = MediaEngine::with_config(EngineConfig::default().silent());
        assert!(
            e.is_real_decode(),
            "these assertions require the real pipeline"
        );
        e.open(fixture_source());
        e
    }

    #[test]
    fn an_accurate_seek_lands_on_the_requested_frame_not_the_keyframe_before_it() {
        let mut e = engine();
        let base = stream_pts_base_ms(&mut e);
        let frame = decode_frame_at(&mut e, MID_GOP_MS, "accurate seek");
        println!("decoder: {}", e.hw_report().describe());

        let expected = MID_GOP_MS + base;
        let delta = Drift::between(frame.pts_ms, expected);
        println!(
            "requested {MID_GOP_MS}ms (stream base {base}ms → expect {expected}ms), \
             got pts {}ms, delta {delta}ms",
            frame.pts_ms
        );
        assert!(
            delta.ms <= FRAME_MS,
            "an accurate seek to {MID_GOP_MS}ms returned the picture at {}ms, \
             {delta}ms from the expected {expected}ms — more than one {FRAME_MS}ms \
             frame. Decoder: {}",
            frame.pts_ms,
            e.hw_report().describe()
        );

        // And prove the target really is deep inside a GOP, so landing on it
        // could not have happened by snapping to a keyframe.
        let into_gop = (frame.pts_ms - base) % FIXTURE_GOP_MS;
        assert!(
            into_gop > FRAME_MS * 10 && into_gop < FIXTURE_GOP_MS - FRAME_MS * 10,
            "the returned frame is {into_gop}ms into its GOP; this test must land \
             far from any keyframe to prove anything"
        );
    }

    #[test]
    fn a_keyframe_seek_lands_seconds_away_which_is_what_accurate_seeking_fixes() {
        // The coarse mode, asked directly — `seek_and_wait` always seeks
        // accurately, so it cannot be used to measure the keyframe path.
        let mut coarse = engine();
        let base = stream_pts_base_ms(&mut coarse);
        assert!(
            coarse.seek_with(MID_GOP_MS, SeekMode::Keyframe)
                || coarse.seek_with(MID_GOP_MS, SeekMode::Keyframe),
            "the pipeline refused a keyframe seek even after prerolling"
        );
        let mut landed = None;
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            coarse.poll();
            if let Some(f) = coarse.latest()
                && f.pts_ms != base
            {
                landed = Some(f.pts_ms);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let keyframe_pts = landed.expect("the keyframe seek produced no new picture");
        let expected = MID_GOP_MS + base;
        let keyframe_err = Drift::between(keyframe_pts, expected).ms;
        println!(
            "keyframe seek to {MID_GOP_MS}ms landed at {keyframe_pts}ms ({keyframe_err}ms off)"
        );

        // The accurate path on the same request.
        let mut exact = engine();
        let f = decode_frame_at(&mut exact, MID_GOP_MS, "accurate comparison");
        let exact_err = Drift::between(f.pts_ms, expected).ms;
        println!(
            "accurate seek to {MID_GOP_MS}ms landed at {}ms ({exact_err}ms off)",
            f.pts_ms
        );

        assert!(
            exact_err <= FRAME_MS,
            "the accurate seek should be within a frame, was {exact_err}ms off"
        );
        assert!(
            keyframe_err > FRAME_MS * 10,
            "the keyframe seek landed {keyframe_err}ms away — close enough that this \
             comparison proves nothing. With an {FIXTURE_GOP_MS}ms GOP it should be \
             seconds off; check the fixture still has the expected keyframe spacing."
        );
        assert!(
            exact_err < keyframe_err,
            "accurate seeking must beat keyframe snapping: accurate was {exact_err}ms off, \
             keyframe {keyframe_err}ms off. If these are equal the ACCURATE flag is not \
             taking effect."
        );
    }

    /// Two different times must yield two genuinely different pictures.
    ///
    /// Timestamps alone prove nothing: a pipeline that re-labelled a cached
    /// buffer would satisfy every check above. Pixels are the evidence.
    ///
    /// The deterministic test pattern moves every frame. The invariant is
    /// therefore "the picture genuinely changed", not merely "the timestamp
    /// changed" — a stale frame differs in exactly zero bytes. A floor well
    /// above sampling noise keeps that from being vacuous, and the far-apart
    /// comparison below adds a case with greater visual displacement.
    #[test]
    fn stepping_forward_changes_the_picture_not_just_the_timestamp() {
        let mut e = engine();
        let a = decode_frame_at(&mut e, MID_GOP_MS, "frame A");
        let b = decode_frame_at(&mut e, MID_GOP_MS + 500, "frame B");

        assert_ne!(
            a.pts_ms, b.pts_ms,
            "different times must yield different timestamps"
        );

        let pa = a
            .to_rgba()
            .expect("RGBA acceptance yields a paintable buffer");
        let pb = b
            .to_rgba()
            .expect("RGBA acceptance yields a paintable buffer");
        assert_eq!(pa.len(), pb.len());
        let differing = pa.iter().zip(pb.iter()).filter(|(x, y)| x != y).count();
        let pct = percent_of(differing, pa.len());
        println!(
            "500ms apart: {differing} of {} bytes differ ({pct:.3}%)",
            pa.len()
        );
        assert!(
            differing > 4096,
            "frames 500ms apart differ in only {differing} bytes — a re-decode \
             changes thousands of pixels even on near-static content, and a stale \
             frame would differ in none. The decoder is not seeking."
        );

        // Two seconds moves the deterministic pattern much farther.
        let far = decode_frame_at(&mut e, MID_GOP_MS + 2_000, "frame C");
        let pc = far.to_rgba().expect("paintable");
        let far_diff = pa.iter().zip(pc.iter()).filter(|(x, y)| x != y).count();
        let far_pct = percent_of(far_diff, pa.len());
        println!(
            "2s apart: {far_diff} of {} bytes differ ({far_pct:.3}%)",
            pa.len()
        );
        assert!(
            far_diff > differing,
            "pictures 2s apart ({far_diff} bytes) should differ more than ones \
             500ms apart ({differing} bytes); if they do not, the seek target is \
             not driving what gets decoded"
        );
    }

    /// Seeking backwards must work as well as forwards: an editor scrubs both
    /// ways, and a pipeline that only decodes forward would stall on a nudge
    /// back.
    #[test]
    fn seeking_backwards_across_a_keyframe_boundary_still_lands_exactly() {
        let mut e = engine();
        let base = stream_pts_base_ms(&mut e);
        // Forward past the keyframe at 16.667s, then back across it.
        let _ = decode_frame_at(&mut e, 20_000, "forward");
        let back = decode_frame_at(&mut e, MID_GOP_MS, "backward across the keyframe");
        let delta = Drift::between(back.pts_ms, MID_GOP_MS + base);
        assert!(
            delta.ms <= FRAME_MS,
            "seeking back to {MID_GOP_MS}ms landed at {}ms ({delta}ms off)",
            back.pts_ms
        );
    }

    /// A single frame step must resolve to the adjacent picture, not round to
    /// the same one. This is the finest granularity an editor asks for.
    #[test]
    fn a_single_frame_step_resolves_to_the_next_picture() {
        let mut e = engine();
        let a = decode_frame_at(&mut e, MID_GOP_MS, "step from");
        let b = decode_frame_at(&mut e, MID_GOP_MS + FRAME_MS + 1, "step to");
        let step = Drift::between(b.pts_ms, a.pts_ms);
        println!("one-frame step: {}ms → {}ms ({step}ms)", a.pts_ms, b.pts_ms);
        assert!(
            !step.early && step.ms > 0 && step.ms <= FRAME_MS * 2,
            "a one-frame step moved {step}ms; it must advance by about one \
             {FRAME_MS}ms frame, not stand still and not jump"
        );
    }

    /// Scrubbing must not make a sound. The old preview drove the pipeline
    /// through PLAYING on every drawn frame to force a picture out; the
    /// accurate path reads the preroll while the pipeline stays paused.
    #[test]
    fn scrubbing_never_starts_playback() {
        let mut e = engine();
        for t in [4_000u64, 12_500, 30_000, 12_500, 4_000] {
            let _ = decode_frame_at(&mut e, t, "scrub step");
            assert!(
                !e.is_playing(),
                "scrubbing to {t}ms put the pipeline into playback; that is the \
                 audio blip this path removes"
            );
        }
    }
}

/// The synthesized fallback has no keyframes, but a seek must still move the
/// playhead exactly where it was told. Asserting that keeps the hermetic build
/// checking something real.
#[cfg(not(feature = "decode"))]
mod hermetic {
    use tilo_media::MediaEngine;

    #[test]
    fn the_synthesized_backend_seeks_to_the_exact_requested_time() {
        let mut e = MediaEngine::new_synthesized();
        e.open(tilo_media::Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        e.seek_ms(12_500);
        assert_eq!(
            e.position_ms(),
            12_500,
            "no snapping in the fallback either"
        );
        let f = e
            .latest_frame()
            .expect("a frame is rendered for the new position");
        assert_eq!(f.pts_ms, 12_500);
    }
}
