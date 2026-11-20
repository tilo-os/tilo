//! Windows that float instead of taking a tab: dialogs.
//!
//! A dialog is part of the window it belongs to, not a window of its own:
//! it never takes a place in the strip and floats centred over its parent
//! while that is shown, hidden with it otherwise. Whether a window is
//! a dialog is judged from what the client says about it, never from its
//! name or its size: a parent (`xdg_toplevel.set_parent`, an X11 window's
//! `WM_TRANSIENT_FOR`). A program's own top-level window takes a tab
//! whatever size it keeps: a fixed size is how an X11 settings window opens
//! under Proton as much as how a calculator's main window does.

use ui::kurbo::{Point, Rect};

use crate::Window;

/// Whether `window` floats rather than taking a tab: it names the window it
/// belongs to.
#[must_use]
pub fn floats(window: &Window) -> bool {
    window.parent.is_some()
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
        assert!(floats(&dialog));
        assert!(!floats(&window()));
    }

    /// A program's first window keeps a size on many programs, and is no
    /// dialog for it: a fixed size behind the strip is a tab like any other.
    #[test]
    fn a_window_keeping_one_size_takes_a_tab_without_a_parent() {
        let fixed = Window {
            min_size: Some((400, 300)),
            max_size: Some((400, 300)),
            ..window()
        };
        assert!(!floats(&fixed));
        let child = Window {
            parent: Some(7),
            min_size: Some((400, 300)),
            max_size: Some((400, 300)),
            ..window()
        };
        assert!(floats(&child), "its parent decides, not its size");
    }

    #[test]
    fn a_floating_window_is_centred_and_kept_inside_its_area() {
        let area = Rect::new(100.0, 50.0, 900.0, 650.0);
        assert_eq!(centred(area, (400.0, 200.0)), Point::new(300.0, 250.0));
        assert_eq!(centred(area, (1000.0, 1000.0)), Point::new(100.0, 50.0));
    }
}
