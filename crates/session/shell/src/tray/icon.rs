//! Turning what an item says about its icon into pixels.
//!
//! An item sends either pixels (`IconPixmap`, ARGB32 in network byte
//! order, possibly several sizes) or a name (`IconName`) to look up in the
//! icon theme, optionally under its own `IconThemePath`, through
//! [`crate::app_icons`]; an item whose icon cannot be found or decoded still
//! gets a letter badge, so it is never an invisible, unclickable gap.

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

/// Look `name` up in the icon theme (under the item's own `theme_path`
/// first) and rasterise it at twice the bar size, crisp on a high-density output.
pub fn from_name(name: &str, theme_path: Option<&str>) -> Option<TrayIcon> {
    let edge = u32::try_from(PREFERRED_PX.saturating_mul(2)).ok()?;
    let raster = crate::app_icons::tray_raster(name, theme_path, edge)?;
    Some(TrayIcon::Pixels {
        width: raster.width,
        height: raster.height,
        rgba: raster.rgba,
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
}
