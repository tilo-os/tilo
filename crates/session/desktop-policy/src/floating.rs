//! Windows that float instead of taking a tab: dialogs.
//!
//! A dialog is part of the window it belongs to, not a window of its own:
//! it never takes a place in the strip and floats centred over its parent
//! while that is shown, hidden with it otherwise. A dialog naming no parent
//! floats centred on the display the person works on. Whether a window is
//! a dialog is judged from what the client says about it, never from its
//! name: a parent (`xdg_toplevel.set_parent`, an X11 window's
//! `WM_TRANSIENT_FOR`), or a fixed size (`min == max`) that fits well
//! inside the display, a dialog's shape rather than a document's.

use ui::kurbo::{Point, Rect};

use crate::Window;

/// Whether `window` floats rather than taking a tab, on a display of
/// `display` (width, height).
#[must_use]
pub fn floats(window: &Window, display: Option<(f64, f64)>) -> bool {
    window.parent.is_some() || has_dialog_size(window, display)
}

/// The client fixed its size (`min == max`, both set) to something no more
/// than two thirds of the display each way.
fn has_dialog_size(window: &Window, display: Option<(f64, f64)>) -> bool {
    let (Some(min), Some(max)) = (window.min_size, window.max_size) else {
        return false;
    };
    let fits = display.is_none_or(|(width, height)| {
        f64::from(min.0) * 3.0 <= width * 2.0 && f64::from(min.1) * 3.0 <= height * 2.0
    });
    min == max && min.0 > 0 && min.1 > 0 && fits
}

/// Where a window of `size` sits centred over `area`: its top-left corner,
/// kept inside `area` where it fits, at `area`'s top-left where it is
/// larger.
#[must_use]
pub fn centred(area: Rect, size: (f64, f64)) -> Point {
    let x = area.x0 + ((area.width() - size.0) / 2.0).max(0.0);
    let y = area.y0 + ((area.height() - size.1) / 2.0).max(0.0);
    Point::new(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> Window {
        Window {
            id: 1,
            ..Window::default()
        }
    }

    #[test]
    fn a_window_with_a_parent_floats() {
        let dialog = Window {
            parent: Some(7),
            ..window()
        };
        assert!(floats(&dialog, Some((1280.0, 800.0))));
        assert!(!floats(&window(), Some((1280.0, 800.0))));
    }

    #[test]
    fn a_small_fixed_size_floats_and_a_large_or_free_one_does_not() {
        let fixed = |w, h| Window {
            min_size: Some((w, h)),
            max_size: Some((w, h)),
            ..window()
        };
        let display = Some((1280.0, 800.0));
        assert!(floats(&fixed(400, 300), display));
        assert!(!floats(&fixed(1200, 300), display), "wider than two thirds");
        assert!(!floats(&fixed(400, 700), display), "taller than two thirds");
        let free = Window {
            min_size: Some((400, 300)),
            max_size: Some((800, 600)),
            ..window()
        };
        assert!(!floats(&free, display), "a size range is a document's");
        let min_only = Window {
            min_size: Some((400, 300)),
            ..window()
        };
        assert!(!floats(&min_only, display));
    }

    #[test]
    fn a_floating_window_is_centred_and_kept_inside_its_area() {
        let area = Rect::new(100.0, 50.0, 900.0, 650.0);
        assert_eq!(centred(area, (400.0, 200.0)), Point::new(300.0, 250.0));
        assert_eq!(centred(area, (1000.0, 1000.0)), Point::new(100.0, 50.0));
    }
}
