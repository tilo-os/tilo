//! Where a member's pointer lands: a pointer travels over a tab's tile, in
//! that tile's own logical pixels, because every member's windows have their
//! own size and their strips their own scroll. A surface shows the tiles it
//! has, and the pointer is drawn where the tile of the tab it is over stands
//! there.

/// A tile a surface shows: the tab it shows, and where it stands on the
/// surface, in logical pixels from its corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    /// The tab, as the workspace numbers it.
    pub tab: u64,
    /// Its corner on the surface.
    pub x: f64,
    /// Its corner on the surface.
    pub y: f64,
    /// How wide it stands.
    pub width: f64,
    /// How tall it stands.
    pub height: f64,
}

impl Tile {
    /// The tile of `tab` standing at `(x, y)`, `width` by `height`.
    #[must_use]
    pub const fn new(tab: u64, x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            tab,
            x,
            y,
            width,
            height,
        }
    }

    /// Whether `(x, y)` of the surface is over this tile.
    #[must_use]
    pub fn contains(&self, (x, y): (f64, f64)) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    /// Where `(x, y)` of this tile stands on the surface.
    #[must_use]
    pub fn point(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (self.x + x, self.y + y)
    }
}

/// Where one member's pointer is drawn on a surface: its publisher, the tab
/// it is over, and the point on the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// The member's device.
    pub publisher: [u8; 32],
    /// The tab it is over.
    pub tab: u64,
    /// The point on the surface.
    pub x: f64,
    /// The point on the surface.
    pub y: f64,
}

/// The tile `(x, y)` of a surface is over: the tab it shows and the point in
/// that tile's own logical pixels, as this device says its pointer. `None`
/// over no tile, which says nothing of this device's pointer.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    reason = "the presence wire carries tile offsets as f32"
)]
pub fn anchor(tiles: &[Tile], (x, y): (f64, f64)) -> Option<(u64, f32, f32)> {
    let over = tiles.iter().find(|tile| tile.contains((x, y)))?;
    Some((over.tab, (x - over.x) as f32, (y - over.y) as f32))
}

/// The tile of `tab`, when the surface shows it.
#[must_use]
pub fn tile(tiles: &[Tile], tab: u64) -> Option<Tile> {
    tiles.iter().copied().find(|tile| tile.tab == tab)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_is_said_in_the_pixels_of_the_tile_it_is_over() {
        let tiles = [
            Tile::new(3, 100.0, 50.0, 400.0, 300.0),
            Tile::new(4, 500.0, 0.0, 100.0, 100.0),
        ];
        assert_eq!(anchor(&tiles, (150.0, 80.0)), Some((3, 50.0, 30.0)));
        assert_eq!(anchor(&tiles, (550.0, 10.0)), Some((4, 50.0, 10.0)));
        assert_eq!(anchor(&tiles, (0.0, 0.0)), None, "over no tile");
        assert_eq!(
            anchor(&tiles, (500.0, 0.0)).map(|(tab, _, _)| tab),
            Some(4),
            "an edge belongs to the tile it opens"
        );
        assert_eq!(tile(&tiles, 4), Some(tiles[1]));
        assert_eq!(tile(&tiles, 5), None);
    }

    #[test]
    fn a_point_on_a_tile_lands_where_the_tile_stands() {
        let over = Tile::new(7, 20.0, 30.0, 100.0, 100.0);
        assert_eq!(over.point((5.0, 6.0)), (25.0, 36.0));
    }
}
