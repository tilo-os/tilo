//! A source and the strict test sink negotiate over loopback, with a media
//! stand-in that sends a real transport stream through the muxer.

use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use miracast::modes::ModePreference;
use miracast::params::AudioCodec;
use miracast::sink::{SinkCapabilities, TestSink};
use miracast::source::accept_sink;
use miracast::ts::{PID_AUDIO, PID_VIDEO, RtpPacketizer, TsMuxer};
use miracast::{
    AudioPreference, Media, MediaError, MediaFactory, MediaPlan, SessionEnd, Source, SourceEvent,
    SourceOptions,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Sends a few muxed frames, like a stream that started.
struct SyntheticMedia {
    plans: Arc<Mutex<Vec<MediaPlan>>>,
    idr_requests: Arc<Mutex<u32>>,
}

struct Running {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    idr_requests: Arc<Mutex<u32>>,
}

impl Media for Running {
    fn request_idr(&mut self) {
        *self.idr_requests.lock().unwrap() += 1;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

impl MediaFactory for SyntheticMedia {
    fn start(&mut self, plan: &MediaPlan) -> Result<Box<dyn Media>, MediaError> {
        self.plans.lock().unwrap().push(plan.clone());
        let socket = UdpSocket::bind((plan.local_ip, plan.source_rtp_port))?;
        socket.connect(plan.sink_rtp)?;
        let lpcm = plan.audio == Some(AudioCodec::Lpcm);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            let mut muxer = TsMuxer::new(lpcm);
            let mut rtp = RtpPacketizer::new(7);
            let mut frame = 0_u64;
            while !stopping.load(std::sync::atomic::Ordering::Acquire) {
                let mut ts = Vec::new();
                let nal = if frame.is_multiple_of(30) { 0x65 } else { 0x41 };
                muxer.video(
                    &[0, 0, 0, 1, nal, 0x88, 0x80],
                    9000 + frame * 3000,
                    frame * 3000,
                    &mut ts,
                );
                muxer.lpcm(&[0; 1920], 9000 + frame * 3000, &mut ts);
                rtp.packetize(&ts, frame * 3000, |packet| {
                    let _ = socket.send(packet);
                });
                frame += 1;
                thread::sleep(Duration::from_millis(10));
            }
        });
        Ok(Box::new(Running {
            stop,
            thread: Some(thread),
            idr_requests: Arc::clone(&self.idr_requests),
        }))
    }
}

type Started = (
    SocketAddr,
    mpsc::Receiver<String>,
    Arc<Mutex<Vec<MediaPlan>>>,
    thread::JoinHandle<Result<SessionEnd, miracast::SourceError>>,
    mpsc::Receiver<miracast::StopHandle>,
);

fn start_source(options: SourceOptions) -> Started {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (events_tx, events_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let plans = Arc::new(Mutex::new(Vec::new()));
    let mut media = SyntheticMedia {
        plans: Arc::clone(&plans),
        idr_requests: Arc::new(Mutex::new(0)),
    };
    let thread = thread::spawn(move || {
        let stream = accept_sink(
            &listener,
            Instant::now() + TIMEOUT,
            &|peer| peer.ip().is_loopback(),
            &|| false,
        )?
        .expect("the sink connects");
        let source = Source::new(stream, options)?;
        stop_tx.send(source.stop_handle()).unwrap();
        source.run(&mut media, &mut |event| {
            let line = match event {
                SourceEvent::Negotiated { mode, audio } => {
                    format!("negotiated {} {audio:?}", mode.name())
                }
                SourceEvent::Playing(_) => "PLAY accepted; media stream started.".to_owned(),
                other => format!("{other:?}"),
            };
            events_tx.send(line).unwrap();
        })
    });
    (address, events_rx, plans, thread, stop_rx)
}

#[test]
fn a_sink_negotiates_plays_receives_the_stream_and_tears_down() {
    let options = SourceOptions {
        preference: ModePreference {
            max_size: Some((1920, 1080)),
            max_fps: 30,
        },
        ..SourceOptions::default()
    };
    let (address, events, plans, source, _stop) = start_source(options);
    let mut sink =
        TestSink::connect_and_play(address, &SinkCapabilities::default(), TIMEOUT).unwrap();
    assert_eq!(
        sink.negotiated().video_formats,
        "38 00 01 04 00000080 00000000 00000000 00 0000 0000 00 none none",
        "1080p30 at level 4 within the 30 fps limit"
    );
    assert_eq!(sink.negotiated().audio_codecs, "AAC 00000001 00");
    assert!(
        sink.negotiated()
            .presentation_url
            .starts_with(&format!("rtsp://{address}/wfd1.0")),
        "{:?}",
        sink.negotiated()
    );
    assert_eq!(
        events.recv_timeout(TIMEOUT).unwrap(),
        "negotiated 1920x1080p30 Some(Aac)"
    );
    assert_eq!(
        events.recv_timeout(TIMEOUT).unwrap(),
        "PLAY accepted; media stream started."
    );

    let streamed = sink
        .receive_until(TIMEOUT, |sink| {
            sink.ts.pes_count(PID_VIDEO) >= 10 && sink.ts.idr_seen
        })
        .unwrap();
    assert!(streamed, "video arrived: {:?}", sink.ts);
    assert_eq!(sink.bad_rtp_packets, 0);
    assert_eq!(sink.ts.sync_errors, 0);

    let plan = plans.lock().unwrap()[0].clone();
    assert_eq!(Some(plan.source_rtp_port), sink.negotiated().server_port);
    assert_eq!(plan.mode.name(), "1920x1080p30");

    sink.teardown(TIMEOUT).unwrap();
    assert_eq!(source.join().unwrap().unwrap(), SessionEnd::SinkTeardown);
}

#[test]
fn an_lpcm_sink_gets_wfd_lpcm_and_a_stopped_source_triggers_teardown() {
    let options = SourceOptions {
        audio: AudioPreference::PreferLpcm,
        keepalive: None,
        ..SourceOptions::default()
    };
    let (address, events, _plans, source, stop) = start_source(options);
    let capabilities = SinkCapabilities {
        video_formats: "00 00 01 01 00000021 00000000 00000000 00 0000 0000 00 none none"
            .to_owned(),
        audio_codecs: Some("LPCM 00000002 00".to_owned()),
    };
    let mut sink = TestSink::connect_and_play(address, &capabilities, TIMEOUT).unwrap();
    assert_eq!(sink.negotiated().audio_codecs, "LPCM 00000002 00");
    assert!(
        sink.negotiated()
            .video_formats
            .starts_with("28 00 01 01 00000020")
    );
    let streamed = sink
        .receive_until(TIMEOUT, |sink| sink.ts.pes_count(PID_AUDIO) >= 5)
        .unwrap();
    assert!(streamed, "audio arrived: {:?}", sink.ts);
    assert_eq!(
        sink.ts.pid_of(miracast::ts::STREAM_TYPE_WFD_LPCM),
        Some(PID_AUDIO)
    );
    while let Ok(event) = events.try_recv() {
        assert!(
            !event.contains("Keepalive"),
            "no keep-alive was configured: {event}"
        );
    }

    stop.recv_timeout(TIMEOUT).unwrap().stop();
    sink.follow_teardown_trigger(TIMEOUT).unwrap();
    assert_eq!(source.join().unwrap().unwrap(), SessionEnd::Stopped);
}

#[test]
fn keepalives_flow_while_playing() {
    let options = SourceOptions {
        keepalive: Some(Duration::from_millis(200)),
        ..SourceOptions::default()
    };
    let (address, _events, _plans, source, stop) = start_source(options);
    let mut sink =
        TestSink::connect_and_play(address, &SinkCapabilities::default(), TIMEOUT).unwrap();
    // The sink answers each M16 while it receives; a second unanswered one
    // would end the session with a timeout.
    sink.receive_until(Duration::from_millis(1500), |_| false)
        .unwrap();
    stop.recv_timeout(TIMEOUT).unwrap().stop();
    sink.follow_teardown_trigger(TIMEOUT).unwrap();
    assert_eq!(source.join().unwrap().unwrap(), SessionEnd::Stopped);
}

#[test]
fn a_sink_that_never_names_rtp_ports_fails_the_session() {
    use miracast::rtsp::{Message, MessageReader};
    use std::net::TcpStream;

    let (address, _events, _plans, source, _stop) = start_source(SourceOptions::default());
    let stream = TcpStream::connect(address).unwrap();
    let mut writer = stream.try_clone().unwrap();
    let mut reader = MessageReader::new(stream);
    let m1 = reader.read_message().unwrap().unwrap();
    Message::response_to(&m1, 200)
        .write_to(&mut writer)
        .unwrap();
    Message::request("OPTIONS", "*", 1)
        .write_to(&mut writer)
        .unwrap();
    let _m2_answer = reader.read_message().unwrap().unwrap();
    let m3 = reader.read_message().unwrap().unwrap();
    Message::response_to(&m3, 200)
        .with_body("wfd_audio_codecs: AAC 00000001 00\r\n")
        .write_to(&mut writer)
        .unwrap();
    let error = source.join().unwrap().expect_err("no RTP port");
    assert!(error.to_string().contains("no RTP port"), "{error}");
}
