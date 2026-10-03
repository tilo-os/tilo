//! Client-attached image description takes precedence over the
//! config-driven default in the flatpak share session's `stream_start_msg`.
//!
//! The compositor's Wayland dispatch calls
//! [`SharedFlatpakSession::set_color_override`] whenever the client commits
//! a `wp_color_management_surface_v1.set_image_description`; a subsequent
//! `stream_start_msg` must report the client's declared BT.2020 PQ
//! colorimetry even though the session was constructed with `hdr: None`.
//! The codec label and bit depth always describe the encoded bytes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::collab::colorimetry::{
    ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
};
use shell::collab::flatpak_share::SharedFlatpakSession;
use shell::collab::hdr_metadata::Hdr10Metadata;
use shell::config::{EncoderChoice, FlatpakShareConfig};
use shell::network::protocol::NeighbourMsg;

fn sdr_config() -> FlatpakShareConfig {
    FlatpakShareConfig {
        max_bitrate_kbps: 8000,
        target_fps: 30,
        encoder: EncoderChoice::Software,
        hdr: None,
    }
}

fn hdr10_pq() -> Colorimetry {
    Colorimetry {
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferCharacteristics::SmpteSt2084,
        matrix: MatrixCoefficients::Bt2020NonConstant,
        range: ColorRange::Limited,
    }
}

#[test]
fn stream_colour_state_follows_the_client_description_else_the_config() {
    // Phase `default_stream_start_reports_sdr_bt709`.
    {
        let session =
            SharedFlatpakSession::start(1, "app", 42, 64, 64, &sdr_config()).expect("session");
        match session.stream_start_msg() {
            NeighbourMsg::SurfaceStreamStart {
                codec,
                colorimetry,
                hdr_static_metadata,
                bit_depth,
                ..
            } => {
                assert_eq!(codec, session.chosen_codec().wire_label());
                assert_eq!(colorimetry, Some(Colorimetry::sdr_bt709()));
                assert!(hdr_static_metadata.is_none());
                assert_eq!(bit_depth, 8);
            }
            other => panic!("expected SurfaceStreamStart, got {other:?}"),
        }
    }

    // Phase `client_attached_image_description_wins_over_config`.
    {
        let session =
            SharedFlatpakSession::start(1, "app", 42, 64, 64, &sdr_config()).expect("session");
        // What the compositor's `wp_color_management_surface_v1` dispatch does
        // after a `commit`: push the client's declared colorimetry onto the session.
        session.set_color_override(hdr10_pq(), Some(Hdr10Metadata::reference_bt2020()));

        match session.stream_start_msg() {
            NeighbourMsg::SurfaceStreamStart {
                codec,
                colorimetry,
                hdr_static_metadata,
                bit_depth,
                ..
            } => {
                // The bytes are still the SDR encoder's: the label and depth say so,
                // while the colour signalling carries the client's intent.
                assert_eq!(codec, session.chosen_codec().wire_label());
                assert_eq!(bit_depth, 8);
                assert_eq!(colorimetry, Some(hdr10_pq()));
                assert_eq!(hdr_static_metadata, Some(Hdr10Metadata::reference_bt2020()));
            }
            other => panic!("expected SurfaceStreamStart, got {other:?}"),
        }
    }

    // Phase `unset_image_description_falls_back_to_config_default`.
    {
        let session =
            SharedFlatpakSession::start(1, "app", 42, 64, 64, &sdr_config()).expect("session");
        session.set_color_override(hdr10_pq(), None);
        // `unset_image_description` — the compositor calls `clear_color_override`.
        session.clear_color_override();

        match session.stream_start_msg() {
            NeighbourMsg::SurfaceStreamStart {
                codec,
                colorimetry,
                bit_depth,
                ..
            } => {
                assert_eq!(codec, session.chosen_codec().wire_label());
                assert_eq!(colorimetry, Some(Colorimetry::sdr_bt709()));
                assert_eq!(bit_depth, 8);
            }
            other => panic!("expected SurfaceStreamStart, got {other:?}"),
        }
    }

    // Phase `client_sdr_override_downgrades_hdr_config`.
    {
        // The workspace was configured for HDR and the client then attaches an SDR
        // description: client intent wins, so the session must not force PQ.
        let mut cfg = sdr_config();
        cfg.hdr = Some(shell::collab::hdr_metadata::HdrConfig::pq_reference());
        let session = SharedFlatpakSession::start(1, "app", 42, 64, 64, &cfg).expect("session");
        session.set_color_override(Colorimetry::sdr_bt709(), None);

        match session.stream_start_msg() {
            NeighbourMsg::SurfaceStreamStart {
                codec,
                colorimetry,
                bit_depth,
                ..
            } => {
                assert_eq!(codec, session.chosen_codec().wire_label());
                assert_eq!(colorimetry, Some(Colorimetry::sdr_bt709()));
                assert_eq!(bit_depth, 8);
            }
            other => panic!("expected SurfaceStreamStart, got {other:?}"),
        }
    }
}
