//! Cell presence is anchored to shared row/column identities, never pixels
//! or sheet names. Each receiver resolves the range against its own layout.
use super::{Affine, Id, PresenceCtx, PresenceKey, Rect, Scene, SheetsApp, TextRenderer, Theme};
use serde::{Deserialize, Serialize};
use ui::paint::PaintCtx;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CellScope {
    pub sheet: String,
    start: (String, String),
    end: (String, String),
}

impl CellScope {
    pub(super) fn corners(
        &self,
        layout: &crate::workbook::Layout,
    ) -> Option<(super::Addr, super::Addr)> {
        Some((
            layout.addr(&Id::from_raw(&self.start.0), &Id::from_raw(&self.start.1))?,
            layout.addr(&Id::from_raw(&self.end.0), &Id::from_raw(&self.end.1))?,
        ))
    }

    pub(super) fn from_key(key: &PresenceKey) -> Option<Self> {
        let PresenceKey::Resource { app, id, kind } = key else {
            return None;
        };
        if app != "sheets" || kind != "cell-selection" || id.len() > 4096 {
            return None;
        }
        serde_json::from_str(id).ok()
    }
}

impl SheetsApp {
    pub(super) fn cell_presence_key(&self) -> Option<PresenceKey> {
        let id = self.active_sheet_id()?;
        let layout = self.workbook.sheet(&id)?.layout();
        let (start, end) = self.grid.selection_bounds();
        let (top, left) = layout.ids(start)?;
        let (bottom, right) = layout.ids(end)?;
        let scope = CellScope {
            sheet: id.as_str().to_owned(),
            start: (top.as_str().to_owned(), left.as_str().to_owned()),
            end: (bottom.as_str().to_owned(), right.as_str().to_owned()),
        };
        Some(PresenceKey::resource(
            "sheets",
            serde_json::to_string(&scope).ok()?,
            "cell-selection",
        ))
    }

    pub(super) fn draw_cell_presence(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(sheet_id) = self.active_sheet_id() else {
            return;
        };
        let Some(sheet) = self.workbook.sheet(&sheet_id) else {
            return;
        };
        let layout = sheet.layout();
        let body = self.grid.body_rect();
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &body,
        );
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        for (key, peers) in &presence.here {
            let Some(scope) =
                CellScope::from_key(key).filter(|scope| scope.sheet == sheet_id.as_str())
            else {
                continue;
            };
            let Some((start, end)) = scope.corners(&layout) else {
                continue;
            };
            let a = self
                .grid
                .cell_screen_rect(start.0.min(end.0), start.1.min(end.1));
            let b = self
                .grid
                .cell_screen_rect(start.0.max(end.0), start.1.max(end.1));
            let rect = Rect::new(a.x0, a.y0, b.x1, b.y1);
            if rect.intersect(body).is_zero_area() {
                continue;
            }
            for peer in peers
                .iter()
                .filter(|peer| !presence.is_own_identity(&peer.0))
            {
                presence.draw_peer_outline_labeled_within(
                    &mut ctx,
                    peer,
                    &presence.display_name(peer),
                    rect,
                    0.0,
                    body,
                );
            }
        }
        scene.pop_layer();
    }
}
