//! Integration tests for the runtime encoder probe in
//! [`shell::collab::flatpak_share::SharedFlatpakSession`], which walks
//! `NVENC → VA-API → openh264` in priority order and picks the
//! highest-priority backend that comes up.
//!
//! Exercises the **session-start probe** only, not the tracker or encode
//! loop — those live in `flatpak_share_e2e.rs`.

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

/// On a stripped host the Auto choice must resolve to openh264 — the
/// software fallback is always available.
#[test]
fn auto_resolves_to_openh264_on_stripped_host() {
    let session = SharedFlatpakSession::start(
        HOST_PEER,
        "org.gimp.GIMP",
        SURFACE,
        64,
        64,
        cfg(EncoderChoice::Auto),
    )
    .expect("session should start");
    // Without the `vaapi` / `nvenc` cargo features the probe walks
    // past both hardware backends and lands on Openh264.
    #[cfg(not(feature = "vaapi"))]
    #[cfg(not(feature = "nvenc"))]
    assert_eq!(session.chosen_encoder(), EncoderKind::Openh264);
    // On feature-on builds, a hardware backend may legitimately win.
    // The test just asserts a valid encoder was chosen.
    let kind = session.chosen_encoder();
    assert!(matches!(
        kind,
        EncoderKind::Openh264 | EncoderKind::VaApi | EncoderKind::NvEnc
    ));
    eprintln!("auto probe chose: {}", kind.as_str());
}

/// Explicit openh264 skips the hardware probes — no `/dev/dri` open,
/// no NVIDIA init.
#[test]
fn explicit_openh264_never_probes_hardware() {
    let session = SharedFlatpakSession::start(
        HOST_PEER,
        "org.gimp.GIMP",
        SURFACE,
        64,
        64,
        cfg(EncoderChoice::Openh264),
    )
    .expect("session should start");
    assert_eq!(session.chosen_encoder(), EncoderKind::Openh264);
}

/// Requesting VA-API on a host that doesn't have it falls back to
/// openh264 (best-effort mode). The reason is recorded in
/// `chosen_encoder` — the OSD surfaces "vaapi requested → fell back to
/// software" without the session refusing to start.
#[test]
fn requested_vaapi_falls_back_when_hardware_absent() {
    let session = SharedFlatpakSession::start(
        HOST_PEER,
        "org.gimp.GIMP",
        SURFACE,
        64,
        64,
        cfg(EncoderChoice::Vaapi),
    )
    .expect("session should fall back rather than fail");
    #[cfg(not(feature = "vaapi"))]
    {
        assert_eq!(
            session.chosen_encoder(),
            EncoderKind::Openh264,
            "no vaapi feature: probe must fall back to openh264"
        );
    }
    // On a vaapi-feature build with a real `/dev/dri`, the probe may
    // succeed and pick VA-API instead — both outcomes are valid here.
    let _ = session.chosen_encoder();
}

/// Explicit NVENC on a non-NVIDIA host falls back — same pattern as
/// the VA-API case above.
#[test]
fn requested_nvenc_falls_back_when_hardware_absent() {
    let session = SharedFlatpakSession::start(
        HOST_PEER,
        "org.gimp.GIMP",
        SURFACE,
        64,
        64,
        cfg(EncoderChoice::Nvenc),
    )
    .expect("session should fall back rather than fail");
    #[cfg(not(feature = "nvenc"))]
    {
        assert_eq!(
            session.chosen_encoder(),
            EncoderKind::Openh264,
            "no nvenc feature: probe must fall back to openh264"
        );
    }
    let _ = session.chosen_encoder();
}

/// On a real VA-API host with a wired encoder, Auto picks VaApi.
/// `#[ignore]`d because CI runners don't ship `/dev/dri`; run with
/// `--include-ignored` on a workstation. Self-skips when the host
/// doesn't have a functional H.264 VA-API path (e.g. the render node
/// exists but the driver doesn't expose H.264 encode).
#[test]
#[ignore = "requires --features vaapi + a real /dev/dri render node; run with --include-ignored on a workstation"]
fn auto_probe_picks_vaapi_when_hardware_present() {
    #[cfg(not(feature = "vaapi"))]
    {
        eprintln!(
            "skipping vaapi hardware assertion — build has no `vaapi` \
             feature; rerun with --features vaapi to actually exercise \
             the probe path"
        );
        return;
    }
    #[cfg(feature = "vaapi")]
    {
        use shell::collab::encoder::{Codec, probe as encoder_probe};
        // Self-skip if the underlying probe reports unavailable — some
        // hosts have a render node but no H.264 encode entrypoint
        // (older iGPUs, discrete GPUs without an encode block, etc.).
        let (chosen_by_probe, _outcomes) = encoder_probe(Codec::H264, Some(EncoderChoice::Vaapi));
        if chosen_by_probe != EncoderKind::VaApi {
            eprintln!(
                "skipping vaapi hardware assertion — probe reports \
                 vaapi unavailable on this host (probe chose {})",
                chosen_by_probe.as_str()
            );
            return;
        }
        let session = SharedFlatpakSession::start(
            HOST_PEER,
            "org.gimp.GIMP",
            SURFACE,
            320,
            240,
            cfg(EncoderChoice::Auto),
        )
        .expect("session should start");
        assert_eq!(
            session.chosen_encoder(),
            EncoderKind::VaApi,
            "auto probe must pick vaapi when the render node exposes a working h264_vaapi path"
        );
    }
}
