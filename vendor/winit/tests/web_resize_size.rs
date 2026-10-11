//! The web canvas surface is sized from the ResizeObserver entry. A device
//! box reported in CSS pixels left an 800 CSS px canvas at devicePixelRatio 2
//! with an 800 px backing store and a 400 px logical layout.
#[path = "../src/platform_impl/web/web_sys/resize_size.rs"]
mod resize_size;

use resize_size::physical_size;

#[test]
fn device_box_in_device_pixels_is_used_exactly() {
    assert_eq!(
        physical_size((800.0, 600.0), (800.0, 600.0), 1.0),
        (800, 600)
    );
    assert_eq!(
        physical_size((1600.0, 1200.0), (800.0, 600.0), 2.0),
        (1600, 1200)
    );
    // Fractional scale: the browser's edge snapping wins over rounding.
    assert_eq!(
        physical_size((1001.0, 751.0), (800.4, 600.4), 1.25),
        (1001, 751)
    );
}

#[test]
fn device_box_in_css_pixels_falls_back_to_css_times_scale() {
    assert_eq!(
        physical_size((800.0, 600.0), (800.0, 600.0), 2.0),
        (1600, 1200)
    );
    assert_eq!(
        physical_size((800.0, 600.0), (800.0, 600.0), 1.5),
        (1200, 900)
    );
}

#[test]
fn scale_change_resizes_the_backing_store_without_a_css_resize() {
    let css = (800.0, 600.0);
    assert_eq!(physical_size((800.0, 600.0), css, 1.0), (800, 600));
    // Moving to a 2x monitor or zooming to 200%, with and without a device box
    // that followed the change.
    assert_eq!(physical_size((1600.0, 1200.0), css, 2.0), (1600, 1200));
    assert_eq!(physical_size((800.0, 600.0), css, 2.0), (1600, 1200));
    assert_eq!(physical_size((800.0, 600.0), css, 1.0), (800, 600));
}

#[test]
fn ancestor_transform_does_not_change_the_backing_store() {
    // An ancestor `transform: scale(0.5)` leaves the content box at 800 CSS px.
    // A device box that applied the transform must not halve the resolution.
    let css = (800.0, 600.0);
    assert_eq!(physical_size((800.0, 600.0), css, 2.0), (1600, 1200));
    assert_eq!(physical_size((400.0, 300.0), css, 1.0), (800, 600));
}

#[test]
fn detached_canvas_is_empty() {
    assert_eq!(physical_size((0.0, 0.0), (0.0, 0.0), 2.0), (0, 0));
}
