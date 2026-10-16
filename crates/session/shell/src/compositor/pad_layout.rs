//! Where a tablet pad's buttons, rings and strips physically sit.
//!
//! libinput numbers a pad's controls but says nothing about their places.
//! libwacom's tablet database does: each model's `.tablet` file lists which
//! buttons run down the left and right edges or along the top and bottom,
//! and which button switches a ring's or strip's mode. The pad map draws
//! that arrangement when the model is known and a plain list otherwise.

use std::path::{Path, PathBuf};

pub use crate::shell::pad_map::{PadControl, PadLayout, PadSide};

/// Where libwacom keeps its tablet database: the system copy, then local
/// overrides, which win.
pub const LIBWACOM_DIRS: [&str; 2] = ["/usr/share/libwacom", "/etc/libwacom"];

/// The layout of the pad with USB ids `vendor:product` and the given control
/// counts, from the first matching `.tablet` file in `dirs` (later
/// directories override earlier ones). `None` when no file describes the
/// model or its placement misses a button libinput reports.
pub fn find_layout(
    dirs: &[PathBuf],
    (vendor, product): (u32, u32),
    buttons: u32,
    rings: u32,
    strips: u32,
) -> Option<PadLayout> {
    dirs.iter().rev().find_map(|dir| {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "tablet"))
            .collect();
        files.sort();
        files.iter().find_map(|file| {
            let text = read_small(file)?;
            if !matches_device(&text, vendor, product) {
                return None;
            }
            parse_layout(&text, buttons, rings, strips)
        })
    })
}

/// A `.tablet` file's text; they are a few kilobytes, so anything much
/// larger is not one.
fn read_small(path: &Path) -> Option<String> {
    const LIMIT: u64 = 64 * 1024;
    String::from_utf8(tilo_files::native::bounded::read(path, LIMIT).ok()??).ok()
}

/// The `key=value` entries of `section`.
fn entries<'a>(text: &'a str, section: &'a str) -> impl Iterator<Item = (&'a str, &'a str)> {
    let mut current = "";
    text.lines().filter_map(move |line| {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            return None;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
            return None;
        }
        if current != section {
            return None;
        }
        let (key, value) = line.split_once('=')?;
        Some((key.trim(), value.trim()))
    })
}

fn value<'a>(text: &'a str, section: &'a str, key: &str) -> Option<&'a str> {
    entries(text, section).find_map(|(k, v)| (k == key).then_some(v))
}

/// Whether the file's `DeviceMatch` names a USB or Bluetooth device with
/// these ids.
fn matches_device(text: &str, vendor: u32, product: u32) -> bool {
    let Some(matches) = value(text, "Device", "DeviceMatch") else {
        return false;
    };
    matches.split(';').any(|entry| {
        let mut parts = entry.split('|');
        let bus = parts.next().unwrap_or_default();
        let id = |part: Option<&str>| part.and_then(|p| u32::from_str_radix(p, 16).ok());
        matches!(bus, "usb" | "bluetooth")
            && id(parts.next()) == Some(vendor)
            && id(parts.next()) == Some(product)
    })
}

/// A libwacom button letter; `A` is libinput's button 0.
fn button_of(letter: &str) -> Option<u32> {
    let mut chars = letter.trim().chars();
    let first = chars.next()?;
    if chars.next().is_some() || !first.is_ascii_uppercase() {
        return None;
    }
    u32::from(first).checked_sub(u32::from('A'))
}

/// The layout a `.tablet` file describes for a pad with these counts.
pub fn parse_layout(text: &str, buttons: u32, rings: u32, strips: u32) -> Option<PadLayout> {
    let mut layout = PadLayout::default();
    for (side, key) in [
        (PadSide::Left, "Left"),
        (PadSide::Right, "Right"),
        (PadSide::Top, "Top"),
        (PadSide::Bottom, "Bottom"),
    ] {
        if let Some(list) = value(text, "Buttons", key) {
            layout.side_mut(side).extend(
                list.split(';')
                    .filter_map(button_of)
                    .filter(|button| *button < buttons)
                    .map(PadControl::Button),
            );
        }
    }
    let placed = |layout: &PadLayout, button| layout.side_of(PadControl::Button(button)).is_some();
    if buttons == 0 || !(0..buttons).all(|button| placed(&layout, button)) {
        return None;
    }
    // A ring sits in the middle of the edge its mode button is on, with the
    // mode button at its centre; a strip heads its edge.
    for ring in 0..rings {
        let key = if ring == 0 { "Ring" } else { "Ring2" };
        let mode_button = value(text, "Buttons", key).and_then(button_of);
        place_dial(&mut layout, PadControl::Ring(ring), mode_button, ring, true);
    }
    for strip in 0..strips {
        let key = if strip == 0 {
            "Touchstrip"
        } else {
            "Touchstrip2"
        };
        let mode_button = value(text, "Buttons", key).and_then(button_of);
        place_dial(
            &mut layout,
            PadControl::Strip(strip),
            mode_button,
            strip,
            false,
        );
    }
    Some(layout)
}

fn place_dial(
    layout: &mut PadLayout,
    dial: PadControl,
    mode_button: Option<u32>,
    index: u32,
    centred: bool,
) {
    let fallback = if index.is_multiple_of(2) {
        PadSide::Left
    } else {
        PadSide::Right
    };
    let mode_button = mode_button.map(PadControl::Button);
    let side = mode_button
        .and_then(|button| layout.side_of(button))
        .unwrap_or(fallback);
    let controls = layout.side_mut(side);
    if let Some(button) = mode_button {
        controls.retain(|control| *control != button);
    }
    let at = if centred { controls.len() / 2 } else { 0 };
    let mut group = vec![dial];
    group.extend(mode_button);
    controls.splice(at..at, group);
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTUOS_PRO: &str = "\
[Device]
Name=Wacom Intuos Pro M
DeviceMatch=usb|056a|0357;bluetooth|056a|0360;
[Features]
NumRings=1
[Buttons]
Left=A;B;C;D;E;F;G;H;I
Ring=I
RingNumModes=4
";

    #[test]
    fn a_ring_sits_mid_edge_around_its_mode_button() {
        use PadControl::{Button, Ring};
        assert!(matches_device(INTUOS_PRO, 0x056a, 0x0357));
        assert!(matches_device(INTUOS_PRO, 0x056a, 0x0360));
        assert!(!matches_device(INTUOS_PRO, 0x056a, 0x0358));
        let layout = parse_layout(INTUOS_PRO, 9, 1, 0).expect("every button is placed");
        assert_eq!(
            layout.left,
            [
                Button(0),
                Button(1),
                Button(2),
                Button(3),
                Ring(0),
                Button(8),
                Button(4),
                Button(5),
                Button(6),
                Button(7),
            ]
        );
        assert!(layout.right.is_empty() && layout.top.is_empty());
    }

    #[test]
    fn a_pad_with_unplaced_buttons_has_no_layout() {
        assert_eq!(parse_layout(INTUOS_PRO, 10, 1, 0), None);
        assert_eq!(
            parse_layout("[Device]\nName=Pad\n", 4, 0, 0),
            None,
            "no [Buttons] section, no layout"
        );
    }

    #[test]
    fn strips_head_their_edges() {
        let layout = parse_layout("[Buttons]\nLeft=A;B\nRight=C;D\n", 4, 0, 2).expect("placed");
        assert_eq!(layout.left.first(), Some(&PadControl::Strip(0)));
        assert_eq!(layout.right.first(), Some(&PadControl::Strip(1)));
    }

    #[test]
    fn the_database_is_searched_by_usb_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("pro.tablet"), INTUOS_PRO).expect("write");
        std::fs::write(dir.path().join("readme.txt"), INTUOS_PRO).expect("write");
        let dirs = [dir.path().to_path_buf()];
        assert!(find_layout(&dirs, (0x056a, 0x0357), 9, 1, 0).is_some());
        assert!(find_layout(&dirs, (0x056a, 0x0001), 9, 1, 0).is_none());
    }
}
