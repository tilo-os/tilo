//! Tiled rendering of zoomed pages, as the PDF engine renders them: the
//! shared tile geometry ([`tilo_page_raster::tiles`]) and its engine terms.

pub use tilo_page_raster::tiles::*;

/// The engine's name for a tile's pixel region.
pub fn engine_rect(region: PixelRect) -> tilo_pdf::PixelRect {
    tilo_pdf::PixelRect {
        x: region.x,
        y: region.y,
        width: region.width,
        height: region.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_levels_and_regions_match_the_engine() {
        for scale in [0.3_f64, 1.0, 1.7, 2.0, 5.5, 16.0] {
            let snapped = tilo_pdf::TileGrid::snap_scale(scale as f32);
            assert!((level_scale(level_for(scale)) - snapped).abs() < 1e-4);
        }
        assert_eq!(tilo_pdf::TileGrid::default().tile, TILE_PX);
        for level in [-4, 0, 7, 12] {
            let size = (612.0, 792.0);
            let options = tilo_pdf::RenderOptions {
                scale: level_scale(level),
                ..tilo_pdf::RenderOptions::default()
            };
            let page = tilo_pdf::PageSize {
                width: 612.0,
                height: 792.0,
            };
            assert_eq!(page_pixels(size, level), options.page_pixels(page));
            let (pw, ph) = page_pixels(size, level);
            let all = tilo_pdf::PixelRect {
                x: 0,
                y: 0,
                width: pw,
                height: ph,
            };
            let engine = tilo_pdf::TileGrid::default().tiles(page, &options, all);
            let shared = tiles_meeting(
                (pw, ph),
                PixelRect {
                    x: 0,
                    y: 0,
                    width: pw,
                    height: ph,
                },
            );
            assert_eq!(engine.len(), shared.len());
            for (e, s) in engine.iter().zip(&shared) {
                assert_eq!(
                    (e.col, e.row, e.region),
                    (s.col, s.row, engine_rect(s.region))
                );
                assert_eq!(tile_region(size, level, s.col, s.row), Some(s.region));
            }
        }
    }
}
