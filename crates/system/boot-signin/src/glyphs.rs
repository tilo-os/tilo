//! What the boot screen's font can draw. Plymouth draws text in one face
//! with no fallback, so a character the face lacks shows as an empty box.
//! The asset generator writes the face's characters beside the theme
//! (`glyphs.txt`, one `first-last` hexadecimal range a line), and the names
//! the screen is told are chosen from what it can draw.

use std::path::Path;

/// Where the theme carries it in the initramfs.
pub const GLYPHS: &str = "/usr/share/plymouth/themes/tilo/glyphs.txt";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// `None` when not known: every character is taken as drawable.
    ranges: Option<Vec<(u32, u32)>>,
}

impl Glyphs {
    /// Nothing known about the face, as on a console, where the terminal
    /// draws the text.
    pub fn any() -> Self {
        Self { ranges: None }
    }

    /// The theme's list; an unreadable one is taken as knowing nothing.
    pub fn read(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| Self::parse(&text))
            .unwrap_or_else(Self::any)
    }

    /// `None` when a line is not a range.
    pub fn parse(text: &str) -> Option<Self> {
        let mut ranges = Vec::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let (first, last) = line.split_once('-')?;
            let first = u32::from_str_radix(first, 16).ok()?;
            let last = u32::from_str_radix(last, 16).ok()?;
            if first > last {
                return None;
            }
            ranges.push((first, last));
        }
        Some(Self {
            ranges: Some(ranges),
        })
    }

    /// Whether every character of `text` is in the face.
    pub fn draws(&self, text: &str) -> bool {
        let Some(ranges) = &self.ranges else {
            return true;
        };
        text.chars().all(|c| {
            let c = u32::from(c);
            ranges.iter().any(|&(first, last)| first <= c && c <= last)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_says_what_the_face_draws() {
        let glyphs = Glyphs::parse("0020-007e\n00c0-00ff\n\n").expect("ranges");
        assert!(glyphs.draws("Ada Lovelace"));
        assert!(glyphs.draws("Zoë"));
        assert!(!glyphs.draws("Ĉiu"));
        assert!(!glyphs.draws("\u{674e}"));
    }

    #[test]
    fn an_unreadable_list_knows_nothing() {
        assert_eq!(Glyphs::parse("0020-007e\nlatin\n"), None);
        assert_eq!(Glyphs::parse("007e-0020\n"), None);
        assert!(Glyphs::read(Path::new("/nonexistent/glyphs.txt")).draws("\u{674e}"));
    }
}
