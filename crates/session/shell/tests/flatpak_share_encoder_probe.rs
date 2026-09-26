//! Encoder selection and codec negotiation in
//! [`shell::collab::flatpak_share::SharedFlatpakSession`]. The media stack
//! picks hardware first, then royalty-free software, then Cisco's openh264;
//! the codec follows what the viewers can decode. Session start only — the
//! tracker/encode loop lives in `flatpak_share_e2e.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use media::StreamCodec;
use media::realtime::{CodecCapabilities, local_capabilities};
use shell::collab::encoder::EncoderKind;
use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::config::{EncoderChoice, FlatpakShareConfig};

const HOST_PEER: u64 = 0xAAAA;
const SURFACE: u64 = 42;

fn cfg(choice: EncoderChoice) -> FlatpakShareConfig {
    FlatpakShareConfig {
        max_bitrate_kbps: 8000,
        target_fps: 30,
        encoder: choice,
        hdr: None,
    }
}

fn start(choice: EncoderChoice, receivers: &[CodecCapabilities]) -> SharedFlatpakSession {
    SharedFlatpakSession::start_with_metrics(
        HOST_PEER,
        "org.gimp.GIMP",
        SURFACE,
        320,
        240,
        &cfg(choice),
        receivers,
        None,
    )
    .expect("session should start")
}

#[test]
fn encoder_negotiation_falls_back_safely_and_uses_hardware_av1_only_when_all_viewers_can() {
    // Phase `auto_starts_on_every_host`.
    // With no viewers yet, a share starts with this machine's best codec, and
    // the kind it reports is one the media stack can produce.
    {
        let session = start(EncoderChoice::Auto, &[]);
        eprintln!(
            "auto chose {} via {}",
            session.chosen_codec().wire_label(),
            session.chosen_encoder().as_str()
        );
    }

    // Phase `software_choice_never_reports_hardware`.
    // A software preference never touches hardware.
    {
        let session = start(EncoderChoice::Software, &[]);
        assert!(
            !session.chosen_encoder().is_hardware(),
            "{:?}",
            session.chosen_encoder()
        );
    }

    // Phase `a_missing_preferred_vendor_falls_back`.
    // Preferring a vendor that isn't here still starts the share.
    {
        for choice in [EncoderChoice::Vaapi, EncoderChoice::Nvenc] {
            let _session = start(choice, &[]);
        }
    }

    // Phase `hardware_av1_is_negotiated_only_while_every_viewer_takes_it`.
    // Where this machine encodes AV1 in hardware and a viewer decodes it in
    // hardware, AV1 is negotiated; a legacy viewer joining pins H.264 (when
    // an H.264 encoder exists) through `renegotiate`.
    {
        fn phase() {
            let local = local_capabilities();
            if !local.hardware_encode.contains(&StreamCodec::Av1) {
                eprintln!("no AV1 hardware encoder listed here; not run");
                return;
            }
            let hw_viewer = CodecCapabilities {
                hardware_decode: [StreamCodec::Av1, StreamCodec::H264].into(),
                ..CodecCapabilities::default()
            };
            let session = start(EncoderChoice::Auto, std::slice::from_ref(&hw_viewer));
            assert_eq!(session.chosen_codec(), StreamCodec::Av1);
            assert_eq!(session.chosen_encoder(), EncoderKind::VaApi);

            if local.can_encode(StreamCodec::H264) {
                let switched = session.renegotiate(&[hw_viewer, CodecCapabilities::legacy_peer()]);
                assert!(switched, "a legacy viewer forces a switch");
                assert_eq!(session.chosen_codec(), StreamCodec::H264);
                match session.stream_start_msg() {
                    shell::network::NeighbourMsg::SurfaceStreamStart { codec, .. } => {
                        assert_eq!(codec, "h264", "the new stream is announced as H.264");
                    }
                    other => panic!("expected SurfaceStreamStart, got {other:?}"),
                }
            }
        }
        phase();
    }
}
