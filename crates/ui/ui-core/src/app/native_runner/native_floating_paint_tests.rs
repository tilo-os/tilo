use super::native_floating_window_clip;
use vello::kurbo::Rect;

tilo_test_env::scenario! {
    fn native_floating_content_stays_inside_its_window;

    phase content_ground_is_clipped_to_the_submitted_native_window;
    {
        let window = Rect::new(-16.0, 120.0, 544.0, 480.0);
        assert_eq!(
            native_floating_window_clip(window, Rect::new(0.0, 120.0, 520.0, 480.0)),
            Some(Rect::new(0.0, 120.0, 520.0, 480.0))
        );
        assert_eq!(
            native_floating_window_clip(window, Rect::new(-32.0, 100.0, 560.0, 500.0)),
            Some(window),
            "a crossing presentation update cannot paint beyond this buffer"
        );
        assert_eq!(
            native_floating_window_clip(window, Rect::new(600.0, 120.0, 700.0, 220.0)),
            None
        );
    }

    phase malformed_content_never_becomes_an_opaque_shell_spill;
    {
        let window = Rect::new(0.0, 0.0, 560.0, 360.0);
        assert_eq!(
            native_floating_window_clip(window, Rect::new(0.0, 0.0, f64::NAN, 360.0)),
            None
        );
        assert_eq!(
            native_floating_window_clip(window, Rect::new(0.0, 0.0, 0.0, 360.0)),
            None
        );
    }
}
