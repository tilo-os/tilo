//! A remote stream its source cuts off partway through: `YouTube` serves the
//! first segment of a URL its CDN will not serve whole and refuses the rest,
//! so the pipeline runs out of bytes about a minute into the song. Playback
//! has to pick the song up where it stopped, not take the cut for its end.

use super::stream_cut::{StreamCut, StreamCutAction, stream_cut_action};
use super::*;

#[test]
fn a_stream_cut_short_resumes_once_per_spot() {
    use StreamCutAction::{Accept, GiveUp, Resume};
    let song = 382_000;
    assert_eq!(
        stream_cut_action(StreamCut::Ended, 60_000, song, None),
        Resume(60_000),
        "an end of stream a minute into a six-minute song is a cut"
    );
    assert_eq!(
        stream_cut_action(StreamCut::Failed, 60_000, song, None),
        Resume(60_000),
        "so is a refused range there"
    );
    assert_eq!(
        stream_cut_action(StreamCut::Ended, song - 1_000, song, None),
        Accept,
        "an end of stream at the song's end is its end"
    );
    assert_eq!(
        stream_cut_action(StreamCut::Ended, 60_000, 0, None),
        Accept,
        "with no known length there is nothing to say it was cut"
    );
    assert_eq!(
        stream_cut_action(StreamCut::Failed, 0, song, None),
        Accept,
        "a stream that never played is a start failure, reported as one"
    );
    assert_eq!(
        stream_cut_action(StreamCut::Ended, 61_000, song, Some(60_000)),
        GiveUp,
        "cut again where it was resumed: the source will not serve it"
    );
    assert_eq!(
        stream_cut_action(StreamCut::Ended, 120_000, song, Some(60_000)),
        Resume(120_000),
        "a later cut, after real progress, is resumed again"
    );
}

/// 8 kHz mono 16-bit PCM: one second is this many bytes.
const BYTES_PER_SECOND: usize = 16_000;

/// A WAV of `seconds` of a 440 Hz tone.
fn tone_wav(seconds: usize) -> Vec<u8> {
    let data_len = u32::try_from(seconds * BYTES_PER_SECOND).unwrap();
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8_000u32.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..data_len / 2 {
        let phase = f64::from(i) * 440.0 * std::f64::consts::TAU / 8_000.0;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a unit sine scaled to 8000 is well inside i16"
        )]
        let sample = (phase.sin() * 8_000.0) as i16;
        wav.extend_from_slice(&sample.to_le_bytes());
    }
    wav
}

/// Serve `/full` as the whole song and `/cut` as what a CDN that stops
/// serving a URL partway through hands the player: the song's own header,
/// which declares its whole length, then only the first `cut_s` seconds.
/// Byte ranges are honoured, so a resume can seek. The server lives as long
/// as the test process.
fn serve_song(song: Vec<u8>, cut_s: usize) -> String {
    use std::io::{BufRead as _, BufReader, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cut = song[..44 + cut_s * BYTES_PER_SECOND].to_vec();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let mut start = None::<usize>;
            let mut end = None::<usize>;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(range) = line
                    .to_ascii_lowercase()
                    .strip_prefix("range: bytes=")
                    .map(|r| r.trim().to_owned())
                    && let Some((a, b)) = range.split_once('-')
                {
                    start = a.parse().ok();
                    end = b.parse().ok();
                }
            }
            let body = if request.contains(" /cut ") {
                &cut
            } else {
                &song
            };
            let first = start.unwrap_or(0).min(body.len());
            let last = end.map_or(body.len(), |e| (e + 1).min(body.len()));
            let slice = &body[first..last.max(first)];
            let head = if start.is_some() {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {first}-{}/{}\r\n",
                    last.max(first + 1) - 1,
                    body.len()
                )
            } else {
                "HTTP/1.1 200 OK\r\n".to_owned()
            };
            let _ = stream.write_all(
                format!(
                    "{head}Accept-Ranges: bytes\r\nContent-Type: audio/x-wav\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    slice.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(slice);
        }
    });
    base
}

fn resolved(url: String) -> crate::backend::ResolvedStream {
    crate::backend::ResolvedStream {
        video_id: "synthetic".into(),
        url,
        mime: "audio/x-wav".into(),
        bitrate: 128_000,
        itag: 0,
        size_bytes: 0,
        duration_ms: 0,
    }
}

/// The song ends a second in, as a cut stream does: the app resolves a
/// fresh URL and resumes at that second rather than moving to the next
/// song, plays on past the cut, and moves on only at the song's real end.
/// The real decode pipeline over local HTTP, scaled from a minute to a
/// second, and seeking over the rest once past the cut, so it runs in a few
/// seconds of real time.
#[test]
#[cfg(feature = "audio")]
#[serial_test::serial(audio_backend)]
fn a_song_cut_off_by_its_source_resumes_where_it_stopped() {
    // Longer than the end slack past the cut, so the cut is not the end.
    let song_s = 8;
    let cut_s = 1;
    let base = serve_song(tone_wav(song_s), cut_s);

    let mut app = Music::new();
    let doc = crate::seed::sample_document();
    app.reload(&doc);
    app.disconnect();
    app.lib.tracks.get_mut("tk_g1").unwrap().duration_s = u32::try_from(song_s).unwrap();
    app.pb.queue = vec!["tk_g1".into(), "tk_g2".into()];
    app.play_index(0);
    app.start_stream("tk_g1");
    let first = app.active_resolve_request.expect("the first song resolves");
    app.apply_resolved_stream("tk_g1", first, &resolved(format!("{base}/cut")));

    let deadline = tilo_test_env::hang_deadline();
    while app.stream_recovery.is_none() {
        app.tick();
        assert!(
            app.pb.current.as_deref() == Some("tk_g1") && app.playback_error.is_none(),
            "the cut ended the song instead of resuming it: current {:?}, error {:?}, {}",
            app.pb.current,
            app.playback_error,
            app.audio.diagnostics()
        );
        assert!(
            ui::time::now() < deadline,
            "the cut stream never ended: {}",
            app.audio.diagnostics()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let (track, resume_at) = app.stream_recovery.clone().unwrap();
    assert_eq!(track, "tk_g1");
    assert!(
        (800..=1_300).contains(&resume_at),
        "resumes where the cut stream stopped, not from the start: {resume_at} ms"
    );
    assert!(
        app.pb.playing,
        "the song is still playing while it reconnects"
    );
    assert_eq!(app.playhead_ms as u64, resume_at);
    assert!(
        app.toast
            .get()
            .is_some_and(|(text, _)| text.contains("Reconnecting")),
        "the cut is said, not silent: {:?}",
        app.toast.get()
    );
    let second = app
        .active_resolve_request
        .expect("a fresh URL is resolved for the same song");
    assert_ne!(second, first);

    app.apply_resolved_stream("tk_g1", second, &resolved(format!("{base}/full")));
    let mut past_cut = false;
    while app.pb.current.as_deref() == Some("tk_g1") {
        app.tick();
        if !past_cut
            && app
                .audio
                .position_ms()
                .is_some_and(|ms| ms > resume_at + 500)
            && app.audio.frames_rendered() > 0
        {
            past_cut = true;
            // Past the cut on the fresh URL: skip to the last half second.
            app.audio
                .seek_ms(u64::try_from(song_s).unwrap() * 1_000 - 500);
        }
        assert!(app.playback_error.is_none(), "{:?}", app.playback_error);
        if app.pb.current.as_deref() == Some("tk_g1") {
            assert_eq!(
                app.stream_recovery,
                Some(("tk_g1".to_owned(), resume_at)),
                "the resumed stream is not taken for another cut"
            );
        }
        assert!(
            ui::time::now() < deadline,
            "the resumed song never reached its end: {}",
            app.audio.diagnostics()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(past_cut, "the resumed stream played on past the cut");
    assert_eq!(
        app.pb.current.as_deref(),
        Some("tk_g2"),
        "the next song starts at the real end"
    );
    app.audio.stop();
}
