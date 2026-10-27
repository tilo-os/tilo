//! The GStreamer media path end to end: a live test pattern and tone are
//! encoded, muxed and sent over RTP to the strict test sink, which must see
//! H.264 with an IDR frame and the negotiated audio.
//!
//! These tests need GStreamer with an H.264 encoder and, for AAC, an AAC
//! encoder. They fail rather than skip without them: a test that skips where
//! the encoders are missing proves nothing where they matter.

#![cfg(feature = "gstreamer")]

use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use miracast::headless::HeadlessScreen;
use miracast::media::{AudioInput, GstMediaConfig, GstMediaFactory, VideoInput};
use miracast::modes::ModePreference;
use miracast::sink::{SinkCapabilities, TestSink};
use miracast::source::accept_sink;
use miracast::ts::{
    PID_AUDIO, PID_VIDEO, STREAM_TYPE_AAC_ADTS, STREAM_TYPE_H264, STREAM_TYPE_WFD_LPCM,
};
use miracast::{AudioPreference, SessionEnd, Source, SourceEvent, SourceOptions};

const TIMEOUT: Duration = Duration::from_secs(30);

type SourceThread = thread::JoinHandle<Result<SessionEnd, miracast::SourceError>>;

fn stream_to_sink(
    audio: AudioPreference,
    capabilities: &SinkCapabilities,
) -> (TestSink, SourceThread) {
    stream_from(VideoInput::TestPattern, audio, capabilities)
}

fn stream_from(
    video: VideoInput,
    audio: AudioPreference,
    capabilities: &SinkCapabilities,
) -> (TestSink, SourceThread) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (playing_tx, playing_rx) = mpsc::channel();
    let source = thread::spawn(move || {
        let mut media =
            GstMediaFactory::new(GstMediaConfig::new(video, AudioInput::TestTone, 2_000)).unwrap();
        let stream = accept_sink(&listener, Instant::now() + TIMEOUT, &|_| true, &|| false)
            .unwrap()
            .expect("the sink connects");
        let options = SourceOptions {
            preference: ModePreference {
                max_size: Some((1280, 720)),
                max_fps: 30,
            },
            audio,
            negotiation_timeout: TIMEOUT,
            ..SourceOptions::default()
        };
        Source::new(stream, options)
            .unwrap()
            .run(&mut media, &mut |event| {
                if let SourceEvent::Playing(plan) = event {
                    println!(
                        "PLAY accepted; media stream started. ({})",
                        plan.mode.name()
                    );
                    playing_tx.send(()).unwrap();
                }
            })
    });
    let sink = TestSink::connect_and_play(address, capabilities, TIMEOUT).unwrap();
    if playing_rx.recv_timeout(TIMEOUT).is_err() {
        let ended = source.join().expect("source thread");
        panic!("the stream did not start: {ended:?}");
    }
    (sink, source)
}

fn tear_down(sink: TestSink, source: SourceThread) {
    sink.teardown(TIMEOUT).unwrap();
    assert_eq!(source.join().unwrap().unwrap(), SessionEnd::SinkTeardown);
}

#[test]
fn h264_and_aac_reach_the_sink_in_mpeg_ts() {
    let (mut sink, source) = stream_to_sink(AudioPreference::Auto, &SinkCapabilities::default());
    assert_eq!(sink.negotiated().audio_codecs, "AAC 00000001 00");
    let streamed = sink
        .receive_until(TIMEOUT, |sink| {
            sink.ts.idr_seen
                && sink.ts.pes_count(PID_VIDEO) >= 15
                && sink.ts.pes_count(PID_AUDIO) >= 5
        })
        .unwrap();
    assert!(streamed, "media arrived: {:?}", sink.ts);
    assert_eq!(sink.ts.pid_of(STREAM_TYPE_H264), Some(PID_VIDEO));
    assert_eq!(sink.ts.pid_of(STREAM_TYPE_AAC_ADTS), Some(PID_AUDIO));
    assert_eq!((sink.bad_rtp_packets, sink.ts.sync_errors), (0, 0));
    tear_down(sink, source);
}

#[test]
fn h264_and_wfd_lpcm_reach_a_microsoft_style_sink() {
    let capabilities = SinkCapabilities {
        audio_codecs: Some("LPCM 00000002 00".to_owned()),
        ..SinkCapabilities::default()
    };
    let (mut sink, source) = stream_to_sink(AudioPreference::PreferLpcm, &capabilities);
    assert_eq!(sink.negotiated().audio_codecs, "LPCM 00000002 00");
    let streamed = sink
        .receive_until(TIMEOUT, |sink| {
            sink.ts.idr_seen
                && sink.ts.pes_count(PID_VIDEO) >= 15
                && sink.ts.pes_count(PID_AUDIO) >= 20
        })
        .unwrap();
    assert!(streamed, "media arrived: {:?}", sink.ts);
    assert_eq!(sink.ts.pid_of(STREAM_TYPE_WFD_LPCM), Some(PID_AUDIO));
    assert!(sink.ts.pcr_packets > 0);
    assert_eq!((sink.bad_rtp_packets, sink.ts.sync_errors), (0, 0));
    tear_down(sink, source);
}

#[test]
fn video_only_sessions_carry_no_audio_stream() {
    let (mut sink, source) = stream_to_sink(AudioPreference::Off, &SinkCapabilities::default());
    assert_eq!(sink.negotiated().audio_codecs, "none");
    let streamed = sink
        .receive_until(TIMEOUT, |sink| {
            sink.ts.idr_seen && sink.ts.pes_count(PID_VIDEO) >= 10
        })
        .unwrap();
    assert!(streamed, "video arrived: {:?}", sink.ts);
    assert_eq!(sink.ts.streams, vec![(PID_VIDEO, STREAM_TYPE_H264)]);
    tear_down(sink, source);
}

#[test]
fn a_pipewire_screen_node_is_captured_through_pipewiresrc() {
    // The headless screen needs PipeWire and WirePlumber. Where they are
    // missing the test says so and passes, unless the environment promises
    // them, as CI that runs it does.
    let missing: Vec<&str> = ["pipewire", "wireplumber", "pw-dump"]
        .into_iter()
        .filter(|program| !on_path(program))
        .collect();
    if !missing.is_empty() {
        assert!(
            std::env::var_os("MIRACAST_REQUIRE_SCREEN_CAPTURE").is_none(),
            "MIRACAST_REQUIRE_SCREEN_CAPTURE is set but {missing:?} are missing"
        );
        eprintln!(
            "skipping: {missing:?} not installed (set MIRACAST_REQUIRE_SCREEN_CAPTURE=1 to fail instead)"
        );
        return;
    }
    let screen = HeadlessScreen::start(1280, 720, 30).unwrap();
    let video = VideoInput::PipeWire {
        remote: screen.connect().unwrap(),
        node: screen.node(),
    };
    let (mut sink, source) = stream_from(video, AudioPreference::Off, &SinkCapabilities::default());
    let streamed = sink
        .receive_until(TIMEOUT, |sink| {
            sink.ts.idr_seen && sink.ts.pes_count(PID_VIDEO) >= 15
        })
        .unwrap();
    assert!(streamed, "the captured screen arrived: {:?}", sink.ts);
    tear_down(sink, source);
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}
