use super::{input_capture_zones, output_geometry, select_fullscreen_output};
use crate::compositor::backend::{Output, OutputId};

/// A 2x 2560x1600 laptop panel with a 1x 2560x1440 monitor to its right:
/// logically the monitor starts at 1280, in the scene at 2560.
fn docked() -> [Output; 2] {
    [
        Output::new(OutputId(0), (2560, 1600), 2.0),
        Output {
            id: OutputId(1),
            position: (2560, 0),
            size: (2560, 1440),
            scale: 1.0,
            transform: settings_store::hardware::OutputTransform::Normal,
            color_profile: None,
            refresh_mhz: None,
            logical_position: (1280, 0),
            identity: crate::compositor::backend::OutputIdentity::default(),
        },
    ]
}

/// The shell lays out in the logical desktop and paints each display
/// into its scene rectangle at its own scale.
#[test]
fn a_dense_panel_is_a_logical_display_drawn_at_its_scale() {
    let geo = output_geometry(&docked());
    assert_eq!(
        (
            geo[0].x,
            geo[0].y,
            geo[0].width,
            geo[0].height,
            geo[0].scale
        ),
        (0.0, 0.0, 1280.0, 800.0, 2.0)
    );
    assert_eq!(
        geo[0].scene,
        vello::kurbo::Rect::new(0.0, 0.0, 2560.0, 1600.0)
    );
    assert_eq!(
        (geo[1].x, geo[1].width, geo[1].scale),
        (1280.0, 2560.0, 1.0)
    );
    assert_eq!(
        geo[1].scene,
        vello::kurbo::Rect::new(2560.0, 0.0, 5120.0, 1440.0)
    );
}

/// `InputCapture` zones are where a portal client's pointer barriers go:
/// the logical desktop the pointer moves in, gapless across a 2x|1x seam.
#[test]
fn input_capture_zones_are_logical() {
    let zones = input_capture_zones(&docked());
    assert_eq!(
        zones
            .iter()
            .map(|z| (z.x, z.y, z.width, z.height))
            .collect::<Vec<_>>(),
        vec![(0, 0, 1280, 800), (1280, 0, 2560, 1440)]
    );
}

#[test]
fn output_geometry_maps_outputs_to_displays_and_rehomes_fullscreen() {
    // Phase `single_output_maps_to_one_display`.
    // A single monitor maps to one display at the origin — the case every screenshot and test runs in.
    {
        let outs = [Output::new(OutputId(0), (3840, 2160), 1.0)];
        let geo = output_geometry(&outs);
        assert_eq!(geo.len(), 1);
        assert_eq!(
            (geo[0].x, geo[0].width, geo[0].height),
            (0.0, 3840.0, 2160.0)
        );
    }
    // Phase `side_by_side_outputs_stay_separate_displays`.
    // Two monitors side by side stay two displays, each with its own width —
    // what stops a full-width window spanning both (combined geometry here is 3520px, neither display is that wide).
    {
        let outs = [
            Output::new(OutputId(0), (1600, 900), 1.0),
            Output {
                id: OutputId(1),
                position: (1600, 0),
                size: (1920, 1080),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                refresh_mhz: None,
                logical_position: (1600, 0),
                identity: crate::compositor::backend::OutputIdentity::default(),
            },
        ];
        let geo = output_geometry(&outs);
        assert_eq!(geo.len(), 2);
        assert_eq!((geo[0].x, geo[0].width), (0.0, 1600.0));
        assert_eq!((geo[1].x, geo[1].width), (1600.0, 1920.0));
    }
    // Phase `zero_sized_outputs_are_dropped`.
    // A connector mid-modeset reports 0x0; a strip can't place a window on it, so it must not appear as a display.
    {
        let outs = [
            Output::new(OutputId(0), (1600, 900), 1.0),
            Output::new(OutputId(1), (0, 0), 1.0),
        ];
        assert_eq!(output_geometry(&outs).len(), 1);
    }
    // Phase `fullscreen_prefers_requested_output_then_rehomes_after_hotplug`.
    {
        let outputs = [
            Output::new(OutputId(0), (1600, 900), 1.0),
            Output {
                id: OutputId(1),
                position: (1600, 0),
                size: (1920, 1080),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                refresh_mhz: None,
                logical_position: (1600, 0),
                identity: crate::compositor::backend::OutputIdentity::default(),
            },
        ];
        let centre = vello::kurbo::Point::new(2000.0, 400.0);
        assert_eq!(
            select_fullscreen_output(Some(OutputId(1)), Some(centre), &outputs).map(|out| out.id),
            Some(OutputId(1)),
            "requested wl_output wins over the old tiled placement"
        );
        let remaining = &outputs[..1];
        assert_eq!(
            select_fullscreen_output(Some(OutputId(1)), Some(centre), remaining).map(|out| out.id),
            Some(OutputId(0)),
            "removed requested output deterministically rehomes fullscreen"
        );
        assert_eq!(
            select_fullscreen_output(None, Some(vello::kurbo::Point::new(300.0, 400.0)), &outputs)
                .map(|out| out.id),
            Some(OutputId(0)),
            "no requested output uses the window's owning display"
        );
    }
}
