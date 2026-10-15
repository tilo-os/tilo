//! Transient toplevels shown as sheets on their parent's tile.
//!
//! A toplevel with an `xdg_toplevel.set_parent` — GIMP's "Export As", a
//! "Save changes?" prompt, a Qt message box — is part of its parent's
//! window, not a window of its own. It never becomes a tab and it never
//! floats over other tiles: it is drawn centred on the tile of its root
//! (the first ancestor that *is* a tab), stacked in the order sheets
//! attached. A modal sheet (`xdg_dialog_v1.set_modal`) dims the tile and
//! swallows pointer input aimed anywhere outside a sheet; the topmost
//! mapped sheet owns the tile's keyboard focus either way.
//!
//! Geometry lives in the root's *image* coordinates (the pixels the tile
//! image is drawn from), so paint and the control socket's rects agree;
//! input arrives surface-local and logical and is carried into it.

use vello::kurbo::Rect;

use super::{ForeignFrameStore, SurfaceId};

/// One sheet and the toplevel it was attached to. `parent` may itself be a
/// sheet; the tile it is drawn on is [`ForeignFrameStore::sheet_root`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetLink {
    pub surface: SurfaceId,
    pub parent: SurfaceId,
    pub modal: bool,
}

/// Where a point on a tile goes once sheets are taken into account.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PointRoute {
    /// The tile's own toplevel, at the point as given.
    Root,
    /// A sheet or a subsurface, at the given surface-local point.
    Surface { surface: SurfaceId, x: f64, y: f64 },
    /// A modal sheet is up and the point is outside every sheet: nobody.
    Blocked,
}

/// Longest parent chain followed before assuming a cycle; a real dialog
/// stack is two or three deep.
const MAX_SHEET_DEPTH: usize = 32;

impl ForeignFrameStore {
    fn sheet_links(&self) -> Vec<SheetLink> {
        self.sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Show `surface` as a sheet over `parent`. Re-attaching moves it to
    /// the top of the stack. Refused (`false`) when it would make a cycle.
    pub fn attach_sheet(&self, surface: SurfaceId, parent: SurfaceId, modal: bool) -> bool {
        if surface == parent || self.sheet_ancestors(parent).contains(&surface) {
            return false;
        }
        let mut links = self
            .sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        links.retain(|link| link.surface != surface);
        links.push(SheetLink {
            surface,
            parent,
            modal,
        });
        drop(links);
        true
    }

    /// Stop treating `surface` as a sheet, returning the link it had.
    pub fn detach_sheet(&self, surface: SurfaceId) -> Option<SheetLink> {
        let mut links = self
            .sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = links.iter().position(|link| link.surface == surface)?;
        let link = links.remove(index);
        drop(links);
        Some(link)
    }

    /// Record the modal hint for a sheet; `false` when `surface` is not one.
    pub fn set_sheet_modal(&self, surface: SurfaceId, modal: bool) -> bool {
        let mut links = self
            .sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(link) = links.iter_mut().find(|link| link.surface == surface) else {
            return false;
        };
        link.modal = modal;
        drop(links);
        true
    }

    pub fn sheet_link(&self, surface: SurfaceId) -> Option<SheetLink> {
        self.sheets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|link| link.surface == surface)
            .copied()
    }

    pub fn is_sheet(&self, surface: SurfaceId) -> bool {
        self.sheet_link(surface).is_some()
    }

    /// `surface` followed by each sheet parent above it, ending at the root.
    fn sheet_ancestors(&self, surface: SurfaceId) -> Vec<SurfaceId> {
        let links = self.sheet_links();
        let mut chain = vec![surface];
        let mut current = surface;
        while chain.len() <= MAX_SHEET_DEPTH {
            let Some(link) = links.iter().find(|link| link.surface == current) else {
                break;
            };
            current = link.parent;
            chain.push(current);
        }
        chain
    }

    /// The tile a sheet is drawn on: its first ancestor that is not a
    /// sheet. A surface that is not a sheet is its own root.
    pub fn sheet_root(&self, surface: SurfaceId) -> SurfaceId {
        self.sheet_ancestors(surface)
            .last()
            .copied()
            .unwrap_or(surface)
    }

    /// Every sheet drawn on `root`'s tile, bottom to top.
    pub fn sheets_over(&self, root: SurfaceId) -> Vec<SheetLink> {
        self.sheet_links()
            .into_iter()
            .filter(|link| self.sheet_root(link.surface) == root && link.surface != root)
            .collect()
    }

    /// Sheets whose parent is exactly `parent`.
    pub fn sheet_children(&self, parent: SurfaceId) -> Vec<SurfaceId> {
        self.sheet_links()
            .into_iter()
            .filter(|link| link.parent == parent)
            .map(|link| link.surface)
            .collect()
    }

    /// Size of `root`'s frame, or of its tab when nothing has been drawn.
    fn root_extent(&self, root: SurfaceId) -> Option<(f64, f64)> {
        if let Some((w, h)) = self.frame_size(root) {
            return Some((f64::from(w), f64::from(h)));
        }
        self.placement(root)
            .map(|rect| (rect.width(), rect.height()))
    }

    /// Where `sheet` sits in `root`'s frame: centred, and pinned to the
    /// top-left corner when it is larger than the tile. `None` until the
    /// sheet has drawn a frame.
    pub fn sheet_rect_in_root(&self, root: SurfaceId, sheet: SurfaceId) -> Option<Rect> {
        let (root_w, root_h) = self.root_extent(root)?;
        let (w, h) = self.frame_size(sheet)?;
        let (w, h) = (f64::from(w), f64::from(h));
        let x = ((root_w - w) / 2.0).max(0.0).floor();
        let y = ((root_h - h) / 2.0).max(0.0).floor();
        Some(Rect::new(x, y, x + w, y + h))
    }

    /// Each drawn sheet over `root` with its rect on the desktop — the rect
    /// the tile paints it into, for the control socket and for tests.
    pub fn sheet_rects(&self, root: SurfaceId) -> Vec<(SheetLink, Rect)> {
        let Some(tile) = self.placement(root) else {
            return Vec::new();
        };
        let scale = self
            .frame_size(root)
            .map_or(1.0, |(w, _)| tile.width() / f64::from(w.max(1)));
        self.sheets_over(root)
            .into_iter()
            .filter_map(|link| {
                let local = self.sheet_rect_in_root(root, link.surface)?;
                Some((
                    link,
                    Rect::new(
                        tile.x0 + local.x0 * scale,
                        tile.y0 + local.y0 * scale,
                        tile.x0 + local.x1 * scale,
                        tile.y0 + local.y1 * scale,
                    ),
                ))
            })
            .collect()
    }

    /// Resolve a point on `root`'s surface (surface-local logical pixels)
    /// to the surface that receives it.
    pub fn route_point(&self, root: SurfaceId, x: f64, y: f64) -> PointRoute {
        let sheets = self.sheets_over(root);
        // Sheets sit in the root's image pixels, each drawn pixel for pixel
        // from its own image; their surfaces are reached back through it.
        let at = if sheets.is_empty() {
            (x, y)
        } else {
            self.layout(root)
                .map_or((x, y), |layout| layout.to_image((x, y)))
        };
        for link in sheets.iter().rev() {
            let Some(rect) = self.sheet_rect_in_root(root, link.surface) else {
                continue;
            };
            if !rect.contains(vello::kurbo::Point::new(at.0, at.1)) || !self.is_mapped(link.surface)
            {
                continue;
            }
            let offset = (at.0 - rect.x0, at.1 - rect.y0);
            let (local_x, local_y) = self
                .layout(link.surface)
                .map_or(offset, |layout| layout.to_surface(offset));
            return self.child_at(link.surface, local_x, local_y).map_or(
                PointRoute::Surface {
                    surface: link.surface,
                    x: local_x,
                    y: local_y,
                },
                |(surface, x, y)| PointRoute::Surface { surface, x, y },
            );
        }
        if sheets.iter().any(|link| link.modal) {
            return PointRoute::Blocked;
        }
        self.child_at(root, x, y)
            .map_or(PointRoute::Root, |(surface, x, y)| PointRoute::Surface {
                surface,
                x,
                y,
            })
    }

    /// The surface that should hold the keyboard when `surface`'s tile is
    /// focused: its topmost mapped sheet, or the tile's own toplevel.
    pub fn keyboard_target(&self, surface: SurfaceId) -> SurfaceId {
        let root = self.sheet_root(surface);
        self.sheets_over(root)
            .iter()
            .rev()
            .find(|link| self.is_mapped(link.surface))
            .map_or(root, |link| link.surface)
    }

    /// Sum of the frame counters of every sheet over `root`, so a tile
    /// repaints when one of its sheets draws.
    pub(super) fn sheets_seq(&self, root: SurfaceId) -> u64 {
        self.sheets_over(root).into_iter().fold(0_u64, |seq, link| {
            seq.wrapping_add(self.surface_and_children_seq(link.surface))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(store: &ForeignFrameStore, id: SurfaceId, w: u32, h: u32) {
        let len = usize::try_from(w.saturating_mul(h).saturating_mul(4)).unwrap_or(0);
        store.publish_rgba(id, &vec![255; len], w, h);
    }

    #[test]
    fn a_sheet_is_centred_on_its_root_tile() {
        let store = ForeignFrameStore::default();
        frame(&store, 1, 800, 600);
        frame(&store, 2, 200, 100);
        store.note_placement(1, Rect::new(100.0, 50.0, 900.0, 650.0));
        assert!(store.attach_sheet(2, 1, false));
        assert_eq!(
            store.sheet_rect_in_root(1, 2),
            Some(Rect::new(300.0, 250.0, 500.0, 350.0))
        );
        let rects = store.sheet_rects(1);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].1, Rect::new(400.0, 300.0, 600.0, 400.0));
    }

    #[test]
    fn nested_sheets_share_the_root_tile_and_stack_in_attach_order() {
        let store = ForeignFrameStore::default();
        assert!(store.attach_sheet(2, 1, true));
        assert!(store.attach_sheet(3, 2, false));
        assert_eq!(store.sheet_root(3), 1);
        assert_eq!(
            store
                .sheets_over(1)
                .iter()
                .map(|link| link.surface)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(store.sheet_children(2), vec![3]);
        assert!(!store.attach_sheet(1, 3, false), "a cycle is refused");
    }

    #[test]
    fn a_modal_sheet_blocks_the_parent_but_not_itself() {
        let store = ForeignFrameStore::default();
        frame(&store, 1, 800, 600);
        frame(&store, 2, 200, 100);
        assert!(store.attach_sheet(2, 1, false));
        assert_eq!(store.route_point(1, 10.0, 10.0), PointRoute::Root);
        assert!(store.set_sheet_modal(2, true));
        assert_eq!(store.route_point(1, 10.0, 10.0), PointRoute::Blocked);
        assert_eq!(
            store.route_point(1, 310.0, 260.0),
            PointRoute::Surface {
                surface: 2,
                x: 10.0,
                y: 10.0
            }
        );
    }

    /// Input arrives in logical pixels; at 2x the sheet is found at twice
    /// the offset in the image and reached in its own logical pixels.
    #[test]
    fn a_sheet_is_reached_in_logical_pixels_at_scale_two() {
        let store = ForeignFrameStore::default();
        let two = super::super::SurfaceScale {
            buffer_scale: 2,
            ..Default::default()
        };
        frame(&store, 1, 1600, 1200);
        store.note_surface_scale(1, two);
        frame(&store, 2, 400, 200);
        store.note_surface_scale(2, two);
        assert!(store.attach_sheet(2, 1, true));
        assert_eq!(
            store.sheet_rect_in_root(1, 2),
            Some(Rect::new(600.0, 500.0, 1000.0, 700.0))
        );
        assert_eq!(
            store.route_point(1, 310.0, 260.0),
            PointRoute::Surface {
                surface: 2,
                x: 10.0,
                y: 10.0
            }
        );
        assert_eq!(store.route_point(1, 290.0, 260.0), PointRoute::Blocked);
    }

    #[test]
    fn the_keyboard_goes_to_the_topmost_drawn_sheet() {
        let store = ForeignFrameStore::default();
        frame(&store, 1, 800, 600);
        assert!(store.attach_sheet(2, 1, true));
        assert_eq!(
            store.keyboard_target(1),
            1,
            "a sheet with nothing drawn cannot take the keyboard yet"
        );
        frame(&store, 2, 200, 100);
        assert_eq!(store.keyboard_target(1), 2);
        assert_eq!(store.keyboard_target(2), 2);
        store.detach_sheet(2);
        assert_eq!(store.keyboard_target(1), 1);
    }

    #[test]
    fn a_sheet_drawing_moves_its_tiles_frame_counter() {
        let store = ForeignFrameStore::default();
        frame(&store, 1, 800, 600);
        assert!(store.attach_sheet(2, 1, false));
        let before = store.frame_seq(1);
        frame(&store, 2, 200, 100);
        assert_ne!(store.frame_seq(1), before);
    }
}
