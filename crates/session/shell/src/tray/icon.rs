//! Turning what an item says about its icon into pixels.
//!
//! An item sends either pixels (`IconPixmap`, ARGB32 in network byte
//! order, possibly several sizes) or a name (`IconName`) to look up in the
//! icon theme, optionally under its own `IconThemePath`. PNG theme icons
//! are decoded here; an item whose icon cannot be found or decoded still
//! gets a letter badge, so it is never an invisible, unclickable gap.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The edge the bar draws tray icons at; the pixmap nearest above it wins.
pub const PREFERRED_PX: i32 = 22;

/// Largest pixmap edge accepted from a client, so a hostile item cannot
/// make the shell allocate an arbitrary buffer.
const MAX_EDGE: i32 = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayIcon {
    /// Straight-alpha RGBA, row-major.
    Pixels {
        width: u32,
        height: u32,
        rgba: Arc<Vec<u8>>,
    },
    /// No usable image: the first letter of the item's name.
    Letter(char),
}

impl TrayIcon {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Pixels { .. } => "pixels",
            Self::Letter(_) => "letter",
        }
    }

    /// The badge for an item called `name`.
    pub fn letter_for(name: &str) -> Self {
        Self::Letter(
            name.chars()
                .find(|c| c.is_alphanumeric())
                .map_or('?', |c| c.to_uppercase().next().unwrap_or(c)),
        )
    }
}

/// Pick the pixmap closest to (and preferably not below) [`PREFERRED_PX`]
/// and convert it from ARGB32 big-endian to RGBA.
pub fn from_pixmaps(pixmaps: &[(i32, i32, Vec<u8>)]) -> Option<TrayIcon> {
    let valid = pixmaps.iter().filter(|(w, h, data)| {
        (1..=MAX_EDGE).contains(w)
            && (1..=MAX_EDGE).contains(h)
            && usize::try_from(w.saturating_mul(*h).saturating_mul(4)).ok() == Some(data.len())
    });
    let (width, height, pixels) = valid.min_by_key(|(edge, _, _)| {
        let edge = *edge;
        // Prefer the smallest at-or-above the target, then the largest below it.
        if edge >= PREFERRED_PX {
            edge.saturating_sub(PREFERRED_PX)
        } else {
            PREFERRED_PX.saturating_sub(edge).saturating_add(MAX_EDGE)
        }
    })?;
    let rgba = pixels
        .chunks_exact(4)
        .flat_map(|argb| match argb {
            &[alpha, red, green, blue] => [red, green, blue, alpha],
            _ => [0, 0, 0, 0],
        })
        .collect();
    Some(TrayIcon::Pixels {
        width: u32::try_from(*width).ok()?,
        height: u32::try_from(*height).ok()?,
        rgba: Arc::new(rgba),
    })
}

/// Directories icon themes live under, most specific first.
fn icon_roots(theme_path: Option<&str>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(path) = theme_path.filter(|path| !path.is_empty()) {
        roots.push(PathBuf::from(path));
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data_home).join("icons"));
    } else if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/icons"));
    }
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    for dir in data_dirs.split(':').filter(|dir| !dir.is_empty()) {
        roots.push(Path::new(dir).join("icons"));
    }
    roots
}

/// Candidate files for icon `name`, best first: sized PNGs in hicolor
/// (and directly under an item's own theme path), then pixmaps.
pub fn candidates(name: &str, theme_path: Option<&str>) -> Vec<PathBuf> {
    if name.is_empty() || name.contains("..") {
        return Vec::new();
    }
    if Path::new(name).is_absolute() {
        return vec![PathBuf::from(name)];
    }
    let file = format!("{name}.png");
    let sizes = [
        "22x22", "24x24", "32x32", "48x48", "64x64", "16x16", "128x128", "256x256",
    ];
    let categories = ["apps", "status", "panel", "devices"];
    let mut out = Vec::new();
    for root in icon_roots(theme_path) {
        out.push(root.join(&file));
        for size in sizes {
            for category in categories {
                out.push(root.join("hicolor").join(size).join(category).join(&file));
            }
        }
    }
    out.push(PathBuf::from("/usr/share/pixmaps").join(&file));
    out
}

/// Look `name` up and decode the first PNG found.
pub fn from_name(name: &str, theme_path: Option<&str>) -> Option<TrayIcon> {
    candidates(name, theme_path)
        .into_iter()
        .filter(|path| path.is_file())
        .find_map(|path| decode_png(&path))
}

fn decode_png(path: &Path) -> Option<TrayIcon> {
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?;
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return None;
    }
    Some(TrayIcon::Pixels {
        width,
        height,
        rgba: Arc::new(rgba.into_raw()),
    })
}

/// The icon to show: pixels when the item sent usable ones, else its
/// named theme icon, else a letter.
pub fn resolve(
    pixmaps: &[(i32, i32, Vec<u8>)],
    name: Option<&str>,
    theme_path: Option<&str>,
    title: &str,
) -> TrayIcon {
    from_pixmaps(pixmaps)
        .or_else(|| name.and_then(|name| from_name(name, theme_path)))
        .unwrap_or_else(|| TrayIcon::letter_for(title))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pixmap_nearest_the_bar_size_is_converted_to_rgba() {
        let small = (16, 16, vec![0xff; 16 * 16 * 4]);
        let mut argb = Vec::new();
        for _ in 0..(24 * 24) {
            argb.extend_from_slice(&[0x80, 0x10, 0x20, 0x30]);
        }
        let right = (24, 24, argb);
        let big = (64, 64, vec![0; 64 * 64 * 4]);
        let icon = from_pixmaps(&[small, right, big]).expect("an icon");
        let TrayIcon::Pixels {
            width,
            height,
            rgba,
        } = icon
        else {
            panic!("pixels expected");
        };
        assert_eq!((width, height), (24, 24));
        assert_eq!(rgba.get(0..4), Some(&[0x10, 0x20, 0x30, 0x80][..]));
    }

    #[test]
    fn a_malformed_pixmap_is_ignored() {
        assert_eq!(from_pixmaps(&[(4, 4, vec![0; 3])]), None);
        assert_eq!(from_pixmaps(&[(-1, 4, Vec::new())]), None);
    }

    #[test]
    fn an_item_with_no_image_gets_its_initial() {
        assert_eq!(
            resolve(&[], Some("no-such-icon-anywhere"), None, "keepassxc"),
            TrayIcon::Letter('K')
        );
        assert_eq!(TrayIcon::letter_for("  "), TrayIcon::Letter('?'));
    }

    #[test]
    fn an_item_theme_path_is_searched_first() {
        let found = candidates("steam_tray_mono", Some("/opt/steam/icons"));
        assert_eq!(
            found.first(),
            Some(&PathBuf::from("/opt/steam/icons/steam_tray_mono.png"))
        );
        assert!(candidates("../etc/passwd", None).is_empty());
    }
}
