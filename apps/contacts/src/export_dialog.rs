//! Contact export-selection dialog.
//!
//! Owns modal state, responsive geometry, drawing and pointer/wheel routing.
//! The root app only handles the resulting request to open the native Save
//! portal, keeping host I/O out of this reusable interaction surface.

use std::collections::HashSet;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::app::{chip, draw_checkbox, menu_panel, toolbar_button};
use crate::model::{ContactId, ContactStore, SortKey};
use crate::vcard::VCardVersion;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ExportRequest {
    pub(crate) ids: Vec<ContactId>,
    pub(crate) version: VCardVersion,
}

pub(crate) struct ExportDialog {
    selection: HashSet<ContactId>,
    scroll: f64,
    open: bool,
}

#[derive(Debug)]
pub(crate) struct Geometry {
    pub(crate) panel: Rect,
    pub(crate) select_all: Rect,
    pub(crate) select_none: Rect,
    pub(crate) list_body: Rect,
    pub(crate) rows: Vec<(ContactId, Rect, Rect)>,
    pub(crate) export_v3: Rect,
    pub(crate) export_v4: Rect,
    pub(crate) cancel: Rect,
    content_h: f64,
}

impl ExportDialog {
    pub(crate) fn new() -> Self {
        Self {
            selection: HashSet::new(),
            scroll: 0.0,
            open: false,
        }
    }

    pub(crate) fn open(&mut self, selected: impl IntoIterator<Item = ContactId>) {
        self.selection = selected.into_iter().collect();
        self.scroll = 0.0;
        self.open = true;
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    #[cfg(test)]
    pub(crate) fn selection(&self) -> &HashSet<ContactId> {
        &self.selection
    }

    pub(crate) fn geometry(&self, window: Rect, store: &ContactStore, sort: SortKey) -> Geometry {
        let width = 460.0_f64.min((window.width() - 32.0).max(280.0));
        let height = (window.height() * 0.8)
            .clamp(320.0, 560.0)
            .min(window.height() - 40.0);
        let panel = Rect::from_center_size(window.center(), (width, height));
        let head_y = panel.y0 + 18.0;
        let select_none = Rect::new(panel.x1 - 112.0, head_y, panel.x1 - 16.0, head_y + 28.0);
        let select_all = Rect::new(
            select_none.x0 - 104.0,
            head_y,
            select_none.x0 - 8.0,
            head_y + 28.0,
        );
        let list_body = Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 64.0,
            panel.x1 - 16.0,
            panel.y1 - 64.0,
        );
        let ids = store.filtered_sorted("", None, sort);
        let row_h = 40.0;
        let rows = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let y = list_body.y0 - self.scroll + row_h * index as f64;
                let row = Rect::new(list_body.x0, y, list_body.x1, y + row_h);
                let checkbox = Rect::new(
                    row.x0 + 4.0,
                    row.center().y - 10.0,
                    row.x0 + 24.0,
                    row.center().y + 10.0,
                );
                (*id, row, checkbox)
            })
            .collect();
        let by = panel.y1 - 44.0;
        let export_v4 = Rect::new(panel.x1 - 136.0, by, panel.x1 - 16.0, by + 32.0);
        let export_v3 = Rect::new(export_v4.x0 - 128.0, by, export_v4.x0 - 8.0, by + 32.0);
        let cancel = Rect::new(panel.x0 + 16.0, by, panel.x0 + 104.0, by + 32.0);
        Geometry {
            panel,
            select_all,
            select_none,
            list_body,
            rows,
            export_v3,
            export_v4,
            cancel,
            content_h: ids.len() as f64 * row_h,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        window: Rect,
        store: &ContactStore,
        sort: SortKey,
        pointer: Point,
    ) {
        let colors = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            transform,
            colors.surfaces.overlay,
            None,
            &window,
        );
        let geometry = self.geometry(window, store, sort);
        menu_panel(scene, transform, theme, geometry.panel);
        text.draw_text_weighted(
            scene,
            "Export contacts",
            16.0,
            colors.text_primary,
            transform * Affine::translate((geometry.panel.x0 + 18.0, geometry.panel.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );
        text.draw_text(
            scene,
            &format!("{} selected", self.selection.len()),
            12.0,
            colors.text_dim,
            transform * Affine::translate((geometry.panel.x0 + 18.0, geometry.panel.y0 + 40.0)),
            None,
        );
        chip(
            scene,
            transform,
            text,
            theme,
            geometry.select_all,
            "Select all",
            false,
        );
        chip(
            scene,
            transform,
            text,
            theme,
            geometry.select_none,
            "Select none",
            false,
        );

        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &geometry.list_body,
        );
        for (id, row, checkbox) in &geometry.rows {
            if row.y1 < geometry.list_body.y0 || row.y0 > geometry.list_body.y1 {
                continue;
            }
            let Some(contact) = store.get(*id) else {
                continue;
            };
            if row.contains(pointer) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.surfaces.raised,
                    None,
                    &RoundedRect::from_rect(*row, f64::from(theme.tokens.radii.md)),
                );
            }
            draw_checkbox(
                scene,
                transform,
                theme,
                *checkbox,
                self.selection.contains(id),
            );
            text.draw_text(
                scene,
                &contact.display_name(),
                14.0,
                colors.text_primary,
                transform * Affine::translate((checkbox.x1 + 12.0, row.center().y - 9.0)),
                Some((row.x1 - checkbox.x1 - 20.0) as f32),
            );
        }
        scene.pop_layer();

        toolbar_button(
            scene,
            transform,
            text,
            theme,
            geometry.cancel,
            "Cancel",
            false,
            geometry.cancel.contains(pointer),
            false,
        );
        toolbar_button(
            scene,
            transform,
            text,
            theme,
            geometry.export_v3,
            "Export 3.0",
            false,
            geometry.export_v3.contains(pointer),
            false,
        );
        toolbar_button(
            scene,
            transform,
            text,
            theme,
            geometry.export_v4,
            "Export 4.0",
            true,
            geometry.export_v4.contains(pointer),
            false,
        );
    }

    pub(crate) fn handle_event(
        &mut self,
        event: &WindowEvent,
        pointer: Point,
        window: Rect,
        store: &ContactStore,
        sort: SortKey,
    ) -> (bool, Option<ExportRequest>) {
        if let WindowEvent::MouseWheel { delta, .. } = event {
            let delta = match delta {
                MouseScrollDelta::LineDelta(_, y) => f64::from(-y * 48.0),
                MouseScrollDelta::PixelDelta(point) => -point.y,
            };
            let geometry = self.geometry(window, store, sort);
            self.scroll = (self.scroll + delta).clamp(
                0.0,
                (geometry.content_h - geometry.list_body.height()).max(0.0),
            );
            return (true, None);
        }
        if !matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            return (false, None);
        }
        let geometry = self.geometry(window, store, sort);
        if geometry.select_all.contains(pointer) {
            self.selection = store.filtered_sorted("", None, sort).into_iter().collect();
        } else if geometry.select_none.contains(pointer) {
            self.selection.clear();
        } else if geometry.list_body.contains(pointer) {
            if let Some((id, _, _)) = geometry
                .rows
                .iter()
                .find(|(_, row, _)| row.contains(pointer))
                && !self.selection.remove(id)
            {
                self.selection.insert(*id);
            }
        } else if geometry.cancel.contains(pointer) || !geometry.panel.contains(pointer) {
            self.close();
        } else if geometry.export_v3.contains(pointer) || geometry.export_v4.contains(pointer) {
            let version = if geometry.export_v4.contains(pointer) {
                VCardVersion::V4
            } else {
                VCardVersion::V3
            };
            let ids = store
                .filtered_sorted("", None, sort)
                .into_iter()
                .filter(|id| self.selection.contains(id))
                .collect();
            self.close();
            return (true, Some(ExportRequest { ids, version }));
        }
        (true, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::sample_contacts;

    fn store() -> ContactStore {
        let mut store = ContactStore::new();
        for contact in sample_contacts() {
            store.upsert(contact);
        }
        store
    }

    #[test]
    fn narrow_geometry_stays_inside_the_window() {
        let dialog = ExportDialog::new();
        let window = Rect::new(0.0, 0.0, 320.0, 500.0);
        let geometry = dialog.geometry(window, &store(), SortKey::Name);
        assert!(window.contains(geometry.panel.origin()));
        assert!(geometry.panel.x1 <= window.x1);
    }

    #[test]
    fn opening_replaces_selection_and_resets_visibility() {
        let store = store();
        let ids = store.filtered_sorted("", None, SortKey::Name);
        let mut dialog = ExportDialog::new();
        dialog.open(ids.iter().take(2).copied());
        assert!(dialog.is_open());
        assert_eq!(dialog.selection().len(), 2);
        dialog.close();
        assert!(!dialog.is_open());
    }

    #[test]
    fn row_selection_and_version_choice_emit_one_export_request() {
        let store = store();
        let ids = store.filtered_sorted("", None, SortKey::Name);
        let window = Rect::new(0.0, 0.0, 960.0, 700.0);
        let mut dialog = ExportDialog::new();
        dialog.open(ids.iter().copied());
        let press = WindowEvent::MouseInput {
            // winit intentionally exposes no constructor for synthetic ids.
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };

        let geometry = dialog.geometry(window, &store, SortKey::Name);
        dialog.handle_event(
            &press,
            geometry.select_none.center(),
            window,
            &store,
            SortKey::Name,
        );
        let geometry = dialog.geometry(window, &store, SortKey::Name);
        let first = geometry.rows[0];
        dialog.handle_event(&press, first.1.center(), window, &store, SortKey::Name);
        let geometry = dialog.geometry(window, &store, SortKey::Name);
        let (_, request) = dialog.handle_event(
            &press,
            geometry.export_v4.center(),
            window,
            &store,
            SortKey::Name,
        );

        assert_eq!(
            request,
            Some(ExportRequest {
                ids: vec![first.0],
                version: VCardVersion::V4,
            })
        );
        assert!(!dialog.is_open());
    }
}
