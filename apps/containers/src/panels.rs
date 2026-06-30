//! Panel rendering + interaction for [`ContainersApp`](crate::app::ContainersApp).
//!
//! Split out of `app.rs` to keep the widget-trait wiring legible. Everything
//! here is an inherent method on `ContainersApp`: the `draw_*` methods paint a
//! region and record [`Hit`] regions; `on_click` / `on_key` resolve input
//! against those regions and mutate the CRDT view state.

use automerge::AutoCommit;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use vello::Scene;
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};

use crate::app::{ContainersApp, Hit, HitRegion, ViewState};
use crate::doc::{CreateField, DetailTab, Section, ViewDoc};
use crate::draw::{self, Icon};
use crate::model::{human_bytes, Container, ContainerStatus, Stats};
use crate::podman::Action;

/// Sizes used across the panels — kept here so the layout reads coherently.
const PAD: f64 = 16.0;
const ICON_BTN: f64 = 32.0;

impl ContainersApp {
    fn push_hit(&mut self, rect: Rect, hit: Hit) {
        self.hits.push(HitRegion { rect, hit });
    }

    fn hovered(&self, rect: Rect) -> bool {
        rect.contains(self.cursor)
    }

    // =================================================================
    // Navigation rail
    // =================================================================

    pub(crate) fn draw_rail(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        view: &ViewState,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_round(scene, rail, 0.0, c.surfaces.raised);
        draw::divider(scene, rail.x1 - 0.5, rail.x1 - 0.5, rail.y1, c.border_standard);
        // re-draw the right border as a vertical line.
        let border = Rect::new(rail.x1 - 1.0, rail.y0, rail.x1, rail.y1);
        scene.fill(Fill::NonZero, Affine::IDENTITY, c.border_standard, None, &border);

        // App wordmark.
        let title_y = rail.y0 + PAD + 8.0;
        draw::icon(
            scene,
            Icon::Box,
            Rect::new(rail.x0 + PAD, title_y, rail.x0 + PAD + 20.0, title_y + 20.0),
            c.accent[5],
            1.8,
        );
        draw::text(
            scene,
            tr,
            "Containers",
            theme.tokens.typography.scale.lg,
            c.text_primary,
            Point::new(rail.x0 + PAD + 28.0, title_y),
            TextWeight::Bold,
        );

        // Section items.
        let items = [
            (Section::Containers, Icon::Box, self.engine.containers.len()),
            (Section::Images, Icon::Layers, self.engine.images.len()),
            (Section::Distrobox, Icon::Terminal, self.engine.distroboxes.len()),
        ];
        let mut y = rail.y0 + 64.0;
        for (section, icon, count) in items {
            let item = Rect::new(rail.x0 + 8.0, y, rail.x1 - 8.0, y + 40.0);
            let active = view.section == section;
            if active {
                draw::fill_round(scene, item, theme.tokens.radii.md as f64, draw::with_alpha(c.accent[5], 46));
            } else if self.hovered(item) {
                draw::fill_round(scene, item, theme.tokens.radii.md as f64, c.surface_white_5);
            }
            let fg = if active { c.accent[6] } else { c.text_dim };
            draw::icon(
                scene,
                icon,
                Rect::new(item.x0 + 10.0, item.center().y - 9.0, item.x0 + 28.0, item.center().y + 9.0),
                fg,
                1.6,
            );
            draw::text(
                scene,
                tr,
                section.label(),
                theme.tokens.typography.scale.base,
                if active { c.text_primary } else { c.text_dim },
                Point::new(item.x0 + 38.0, item.center().y - 8.0),
                if active { TextWeight::Semibold } else { TextWeight::Medium },
            );
            // count chip on the right
            let count_str = count.to_string();
            let (cw, _) = tr.measure(&count_str, theme.tokens.typography.scale.xs, None);
            let chip = Rect::new(item.x1 - 14.0 - cw as f64, item.center().y - 8.0, item.x1 - 6.0, item.center().y + 8.0);
            draw::fill_round(scene, chip, 8.0, c.surface_white_10);
            draw::text(
                scene,
                tr,
                &count_str,
                theme.tokens.typography.scale.xs,
                c.text_dim,
                Point::new(chip.x0 + 4.0, chip.center().y - 5.0),
                TextWeight::Medium,
            );
            self.push_hit(item, Hit::Section(section));
            y += 46.0;
        }

        // Engine status footer.
        let foot_y = rail.y1 - 76.0;
        draw::divider(scene, rail.x0 + PAD, rail.x1 - PAD, foot_y - 12.0, c.border_standard);
        let dot_color = if self.engine.live { c.semantic.success } else { c.semantic.warning };
        let dot = vello::kurbo::Circle::new(Point::new(rail.x0 + PAD + 4.0, foot_y + 4.0), 4.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, dot_color, None, &dot);
        draw::text(
            scene,
            tr,
            if self.engine.live { "Connected" } else { "Offline" },
            theme.tokens.typography.scale.sm,
            c.text_primary,
            Point::new(rail.x0 + PAD + 16.0, foot_y - 3.0),
            TextWeight::Medium,
        );
        draw::text_clip(
            scene,
            tr,
            &self.engine.connection,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(rail.x0 + PAD, foot_y + 18.0),
            rail.width() - PAD * 2.0,
            TextWeight::Regular,
        );
        let summary = format!(
            "{} running · {} stopped",
            self.engine.running_count(),
            self.engine.stopped_count()
        );
        draw::text(
            scene,
            tr,
            &summary,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(rail.x0 + PAD, foot_y + 36.0),
            TextWeight::Regular,
        );
    }

    // =================================================================
    // Shared header (title + search + primary action)
    // =================================================================

    /// Draws the content header and returns the y of the content body start.
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        title: &str,
        view: &ViewState,
        action_label: Option<(&str, Hit)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let header = Rect::new(content.x0, content.y0, content.x1, content.y0 + crate::app::HEADER_HEIGHT);
        scene.fill(Fill::NonZero, Affine::IDENTITY, c.surfaces.base, None, &header);
        draw::text(
            scene,
            tr,
            title,
            theme.tokens.typography.scale.x2l,
            c.text_primary,
            Point::new(header.x0 + PAD + 8.0, header.center().y - 14.0),
            TextWeight::Bold,
        );

        // Primary action button (right-aligned).
        let mut right = header.x1 - PAD;
        if let Some((label, hit)) = action_label {
            let bw = 150.0;
            let btn = Rect::new(right - bw, header.center().y - 17.0, right, header.center().y + 17.0);
            let hov = self.hovered(btn);
            draw::fill_round(scene, btn, theme.tokens.radii.md as f64, if hov { c.accent[4] } else { c.accent[5] });
            draw::icon(
                scene,
                Icon::Plus,
                Rect::new(btn.x0 + 12.0, btn.center().y - 8.0, btn.x0 + 28.0, btn.center().y + 8.0),
                Color::WHITE,
                1.8,
            );
            draw::text(
                scene,
                tr,
                label,
                theme.tokens.typography.scale.base,
                Color::WHITE,
                Point::new(btn.x0 + 34.0, btn.center().y - 8.0),
                TextWeight::Semibold,
            );
            self.push_hit(btn, hit);
            right = btn.x0 - 12.0;
        }

        // Search box (fills remaining width up to a cap).
        let search_w = (right - (header.x0 + PAD + 8.0) - 220.0).clamp(180.0, 360.0);
        let sx0 = right - search_w;
        let search = Rect::new(sx0, header.center().y - 17.0, right, header.center().y + 17.0);
        let focused = self.search_focus;
        draw::fill_round(scene, search, theme.tokens.radii.md as f64, c.surfaces.raised);
        draw::stroke_round(
            scene,
            search,
            theme.tokens.radii.md as f64,
            if focused { 2.0 } else { 1.0 },
            if focused { c.border_focus } else { c.border_standard },
        );
        draw::icon(
            scene,
            Icon::Search,
            Rect::new(search.x0 + 8.0, search.center().y - 8.0, search.x0 + 24.0, search.center().y + 8.0),
            c.text_dim,
            1.5,
        );
        let q = &view.search;
        if q.is_empty() {
            draw::text(
                scene,
                tr,
                "Search…",
                theme.tokens.typography.scale.base,
                c.text_dim,
                Point::new(search.x0 + 30.0, search.center().y - 8.0),
                TextWeight::Regular,
            );
        } else {
            draw::text_clip(
                scene,
                tr,
                q,
                theme.tokens.typography.scale.base,
                c.text_primary,
                Point::new(search.x0 + 30.0, search.center().y - 8.0),
                search_w - 56.0,
                TextWeight::Regular,
            );
            // caret when focused
            if focused {
                let (qw, _) = tr.measure(q, theme.tokens.typography.scale.base, None);
                let cx = (search.x0 + 30.0 + qw as f64).min(search.x1 - 20.0);
                let caret = Rect::new(cx, search.center().y - 9.0, cx + 1.5, search.center().y + 9.0);
                scene.fill(Fill::NonZero, Affine::IDENTITY, c.accent[5], None, &caret);
            }
            // clear button
            let clr = Rect::new(search.x1 - 24.0, search.center().y - 10.0, search.x1 - 6.0, search.center().y + 10.0);
            draw::text(scene, tr, "✕", theme.tokens.typography.scale.sm, c.text_dim, Point::new(clr.x0 + 4.0, clr.center().y - 7.0), TextWeight::Regular);
            self.push_hit(clr, Hit::ClearSearch);
        }
        self.push_hit(search, Hit::SearchFocus);

        // bottom divider
        draw::divider(scene, header.x0, header.x1, header.y1 - 0.5, c.border_standard);
        header.y1
    }

    // =================================================================
    // Containers section
    // =================================================================

    pub(crate) fn draw_containers(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        view: &ViewState,
    ) {
        let body_y = self.draw_header(
            scene,
            tr,
            theme,
            content,
            "Containers",
            view,
            Some(("New container", Hit::NewContainer)),
        );

        // Split content into list + optional detail.
        let detail_open = view.selected.is_some();
        let list_w = if detail_open {
            (content.width() - crate::app::DETAIL_WIDTH).max(240.0)
        } else {
            content.width()
        };
        let list = Rect::new(content.x0, body_y, content.x0 + list_w, content.y1);

        let query = view.search.clone();
        let rows: Vec<Container> = self.visible_containers(&query).into_iter().cloned().collect();

        // Column header.
        let colhead = Rect::new(list.x0, list.y0, list.x1, list.y0 + 28.0);
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, Affine::IDENTITY, c.surfaces.base, None, &colhead);
        let cols = self.container_columns(list, detail_open);
        // The compact (detail-open) layout drops the image + stats columns,
        // so its header must match — otherwise the labels collide.
        let labels: [&str; 5] = if detail_open {
            ["NAME", "STATUS", "", "", ""]
        } else {
            ["NAME", "IMAGE", "STATUS", "CPU / MEM", ""]
        };
        for (i, lbl) in labels.iter().enumerate() {
            if lbl.is_empty() {
                continue;
            }
            draw::text(
                scene,
                tr,
                lbl,
                theme.tokens.typography.scale.xs,
                c.text_dim,
                Point::new(cols[i], colhead.center().y - 5.0),
                TextWeight::Semibold,
            );
        }
        draw::divider(scene, list.x0 + PAD, list.x1 - PAD, colhead.y1, c.border_standard);

        // Rows (scrollable).
        let rows_area = Rect::new(list.x0, colhead.y1, list.x1, list.y1);
        let content_h = rows.len() as f64 * crate::app::ROW_HEIGHT;
        let max_scroll = (content_h - rows_area.height()).max(0.0);
        self.clamp_list_scroll(max_scroll);
        let mut y = rows_area.y0 - self.list_scroll;
        for container in &rows {
            let row = Rect::new(rows_area.x0, y, rows_area.x1, y + crate::app::ROW_HEIGHT);
            if row.y1 >= rows_area.y0 && row.y0 <= rows_area.y1 {
                self.draw_container_row(scene, tr, theme, row, container, &cols, detail_open, view);
            }
            y += crate::app::ROW_HEIGHT;
        }
        if rows.is_empty() {
            self.draw_empty(scene, tr, theme, rows_area, "No containers match your search.");
        }
        self.draw_scrollbar(scene, theme, rows_area, self.list_scroll, content_h);

        // Detail pane.
        if let Some(sel) = &view.selected {
            let detail = Rect::new(list.x1, body_y, content.x1, content.y1);
            if let Some(container) = self.engine.find(sel).cloned() {
                self.draw_detail(scene, tr, theme, detail, &container, view);
            } else {
                // selected id no longer exists; close it implicitly
                let panel = detail;
                let c = &theme.tokens.colors;
                scene.fill(Fill::NonZero, Affine::IDENTITY, c.surfaces.raised, None, &panel);
                self.draw_empty(scene, tr, theme, panel, "Container no longer exists.");
            }
        }
    }

    fn container_columns(&self, list: Rect, detail_open: bool) -> [f64; 5] {
        let x = list.x0 + PAD;
        let w = list.width() - PAD * 2.0;
        if detail_open {
            // Narrow layout: name, status, actions.
            [x, x + w * 0.42, x + w * 0.62, x + w * 0.62, list.x1 - PAD]
        } else {
            [
                x,
                x + w * 0.28,
                x + w * 0.54,
                x + w * 0.68,
                list.x1 - PAD,
            ]
        }
    }

    fn draw_container_row(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        container: &Container,
        cols: &[f64; 5],
        compact: bool,
        view: &ViewState,
    ) {
        let c = &theme.tokens.colors;
        let selected = view.selected.as_deref() == Some(container.id.as_str());
        let inner = Rect::new(row.x0 + 8.0, row.y0 + 4.0, row.x1 - 8.0, row.y1 - 4.0);
        if selected {
            draw::fill_round(scene, inner, theme.tokens.radii.md as f64, draw::with_alpha(c.accent[5], 40));
            draw::stroke_round(scene, inner, theme.tokens.radii.md as f64, 1.0, draw::with_alpha(c.accent[5], 110));
        } else if self.hovered(row) {
            draw::fill_round(scene, inner, theme.tokens.radii.md as f64, c.surface_white_5);
        }

        // status accent bar
        let accent = draw::status_color(theme, container.status);
        let bar = Rect::new(inner.x0, inner.y0 + 6.0, inner.x0 + 3.0, inner.y1 - 6.0);
        draw::fill_round(scene, bar, 1.5, accent);

        let name_y = row.center().y - 16.0;
        let sub_y = row.center().y + 4.0;
        // Name + short id
        draw::text_clip(
            scene,
            tr,
            &container.name,
            theme.tokens.typography.scale.base,
            c.text_primary,
            Point::new(cols[0] + 8.0, name_y),
            (cols[1] - cols[0] - 16.0).max(40.0),
            TextWeight::Semibold,
        );
        draw::text(
            scene,
            tr,
            container.short_id(),
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(cols[0] + 8.0, sub_y),
            TextWeight::Regular,
        );

        if compact {
            // status pill near the right, then actions
            let pill_x = cols[1];
            draw::status_pill(scene, tr, theme, container.status, container.status.label(), Point::new(pill_x, row.center().y - 10.0), 20.0);
        } else {
            // Image
            draw::text_clip(
                scene,
                tr,
                &container.image,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                Point::new(cols[1], row.center().y - 6.0),
                (cols[2] - cols[1] - 12.0).max(40.0),
                TextWeight::Regular,
            );
            // Status pill + detail
            draw::status_pill(scene, tr, theme, container.status, container.status.label(), Point::new(cols[2], name_y), 20.0);
            draw::text_clip(
                scene,
                tr,
                &container.status_detail,
                theme.tokens.typography.scale.xs,
                c.text_dim,
                Point::new(cols[2], sub_y + 2.0),
                (cols[3] - cols[2] - 12.0).max(30.0),
                TextWeight::Regular,
            );
            // CPU / MEM mini-stats
            self.draw_row_stats(scene, tr, theme, container, Point::new(cols[3], row.y0 + 12.0), cols[4] - cols[3] - 96.0);
        }

        // Action buttons cluster on the far right.
        self.draw_row_actions(scene, tr, theme, container, row);

        // Whole-row selection hit (lowest priority — pushed before actions so
        // action hits, pushed earlier, win at resolve time which scans last).
        self.push_hit(row, Hit::SelectContainer(container.id.clone()));
    }

    fn draw_row_stats(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        container: &Container,
        at: Point,
        width: f64,
    ) {
        let c = &theme.tokens.colors;
        let w = width.max(80.0).min(160.0);
        if !container.status.is_running() {
            draw::text(scene, tr, "—", theme.tokens.typography.scale.sm, c.text_dim, Point::new(at.x, at.y + 12.0), TextWeight::Regular);
            return;
        }
        let s = &container.stats;
        // CPU
        draw::text(scene, tr, &format!("{:.0}% CPU", s.cpu_percent * 100.0), theme.tokens.typography.scale.xs, c.text_dim, at, TextWeight::Regular);
        let cpu_bar = Rect::new(at.x, at.y + 14.0, at.x + w, at.y + 19.0);
        draw::usage_bar(scene, theme, cpu_bar, (s.cpu_percent).min(1.0), c.accent[5]);
        // MEM
        let mem_label = format!("{} MEM", human_bytes(s.mem_bytes));
        draw::text(scene, tr, &mem_label, theme.tokens.typography.scale.xs, c.text_dim, Point::new(at.x, at.y + 24.0), TextWeight::Regular);
        let mem_bar = Rect::new(at.x, at.y + 38.0, at.x + w, at.y + 43.0);
        let mem_color = if s.mem_fraction() > 0.85 { c.semantic.danger } else { c.semantic.info };
        draw::usage_bar(scene, theme, mem_bar, s.mem_fraction(), mem_color);
    }

    fn draw_row_actions(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        container: &Container,
        row: Rect,
    ) {
        let c = &theme.tokens.colors;
        let running = container.status.is_running();
        let actions: Vec<(Icon, Action, Color)> = if running {
            vec![
                (Icon::Stop, Action::Stop, c.semantic.warning),
                (Icon::Restart, Action::Restart, c.semantic.info),
                (Icon::Trash, Action::Remove, c.semantic.danger),
            ]
        } else if container.status == ContainerStatus::Paused {
            vec![
                (Icon::Play, Action::Unpause, c.semantic.success),
                (Icon::Trash, Action::Remove, c.semantic.danger),
            ]
        } else {
            vec![
                (Icon::Play, Action::Start, c.semantic.success),
                (Icon::Trash, Action::Remove, c.semantic.danger),
            ]
        };
        let mut x = row.x1 - 12.0;
        for (icon, action, color) in actions.into_iter().rev() {
            x -= ICON_BTN;
            let btn = Rect::new(x, row.center().y - ICON_BTN / 2.0, x + ICON_BTN, row.center().y + ICON_BTN / 2.0);
            let hov = self.hovered(btn);
            if hov {
                draw::fill_round(scene, btn, theme.tokens.radii.md as f64, draw::with_alpha(color, 46));
            }
            draw::icon(
                scene,
                icon,
                Rect::new(btn.x0 + 7.0, btn.y0 + 7.0, btn.x1 - 7.0, btn.y1 - 7.0),
                if hov { color } else { draw::with_alpha(color, 200) },
                1.6,
            );
            self.push_hit(btn, Hit::Action { id: container.id.clone(), action });
            let _ = tr; // keep signature consistent; icons are vector-only
            x -= 4.0;
        }
    }

    // =================================================================
    // Container detail pane
    // =================================================================

    fn draw_detail(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        container: &Container,
        view: &ViewState,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, Affine::IDENTITY, c.surfaces.raised, None, &panel);
        let border = Rect::new(panel.x0, panel.y0, panel.x0 + 1.0, panel.y1);
        scene.fill(Fill::NonZero, Affine::IDENTITY, c.border_standard, None, &border);

        // Header: name + close.
        let head = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 56.0);
        draw::text_clip(
            scene,
            tr,
            &container.name,
            theme.tokens.typography.scale.xl,
            c.text_primary,
            Point::new(head.x0 + PAD, head.y0 + 12.0),
            head.width() - PAD * 2.0 - 32.0,
            TextWeight::Bold,
        );
        draw::text_clip(
            scene,
            tr,
            &container.image,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(head.x0 + PAD, head.y0 + 34.0),
            head.width() - PAD * 2.0 - 32.0,
            TextWeight::Regular,
        );
        let close = Rect::new(head.x1 - 36.0, head.y0 + 12.0, head.x1 - 12.0, head.y0 + 36.0);
        if self.hovered(close) {
            draw::fill_round(scene, close, theme.tokens.radii.md as f64, c.surface_white_10);
        }
        draw::text(scene, tr, "✕", theme.tokens.typography.scale.lg, c.text_dim, Point::new(close.x0 + 6.0, close.y0 + 2.0), TextWeight::Regular);
        self.push_hit(close, Hit::CloseDetail);

        // Tab strip.
        let strip = Rect::new(panel.x0, head.y1, panel.x1, head.y1 + 36.0);
        let tab_w = strip.width() / DetailTab::ALL.len() as f64;
        for (i, tab) in DetailTab::ALL.iter().enumerate() {
            let tx0 = strip.x0 + i as f64 * tab_w;
            let trect = Rect::new(tx0, strip.y0, tx0 + tab_w, strip.y1);
            let active = view.tab == *tab;
            let fg = if active { c.text_primary } else { c.text_dim };
            let (lw, _) = tr.measure_weighted(tab.label(), theme.tokens.typography.scale.sm, None, TextWeight::Semibold);
            draw::text(
                scene,
                tr,
                tab.label(),
                theme.tokens.typography.scale.sm,
                fg,
                Point::new(trect.center().x - lw as f64 / 2.0, trect.center().y - 7.0),
                if active { TextWeight::Semibold } else { TextWeight::Medium },
            );
            if active {
                let underline = Rect::new(trect.center().x - lw as f64 / 2.0 - 4.0, trect.y1 - 2.5, trect.center().x + lw as f64 / 2.0 + 4.0, trect.y1);
                draw::fill_round(scene, underline, 1.0, c.accent[5]);
            } else if self.hovered(trect) {
                let underline = Rect::new(trect.x0 + 8.0, trect.y1 - 2.0, trect.x1 - 8.0, trect.y1);
                draw::fill_round(scene, underline, 1.0, c.surface_white_10);
            }
            self.push_hit(trect, Hit::DetailTab(*tab));
        }
        draw::divider(scene, strip.x0, strip.x1, strip.y1, c.border_standard);

        // Action toolbar.
        let toolbar = Rect::new(panel.x0, strip.y1, panel.x1, strip.y1 + 48.0);
        self.draw_detail_toolbar(scene, tr, theme, toolbar, container);
        draw::divider(scene, toolbar.x0, toolbar.x1, toolbar.y1, c.border_standard);

        // Tab body.
        let body = Rect::new(panel.x0, toolbar.y1, panel.x1, panel.y1);
        match view.tab {
            DetailTab::Logs => self.draw_logs(scene, tr, theme, body, container),
            DetailTab::Stats => self.draw_stats(scene, tr, theme, body, container),
            DetailTab::Inspect => self.draw_inspect(scene, tr, theme, body, container),
            DetailTab::Terminal => self.draw_terminal(scene, tr, theme, body, container),
        }
    }

    fn draw_detail_toolbar(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        toolbar: Rect,
        container: &Container,
    ) {
        let c = &theme.tokens.colors;
        let running = container.status.is_running();
        let buttons: Vec<(&str, Icon, Action, Color)> = if running {
            vec![
                ("Stop", Icon::Stop, Action::Stop, c.semantic.warning),
                ("Restart", Icon::Restart, Action::Restart, c.semantic.info),
                ("Remove", Icon::Trash, Action::Remove, c.semantic.danger),
            ]
        } else if container.status == ContainerStatus::Paused {
            vec![
                ("Resume", Icon::Play, Action::Unpause, c.semantic.success),
                ("Remove", Icon::Trash, Action::Remove, c.semantic.danger),
            ]
        } else {
            vec![
                ("Start", Icon::Play, Action::Start, c.semantic.success),
                ("Remove", Icon::Trash, Action::Remove, c.semantic.danger),
            ]
        };
        let mut x = toolbar.x0 + PAD;
        for (label, icon, action, color) in buttons {
            let (lw, _) = tr.measure_weighted(label, theme.tokens.typography.scale.sm, None, TextWeight::Semibold);
            let bw = lw as f64 + 38.0;
            let btn = Rect::new(x, toolbar.center().y - 15.0, x + bw, toolbar.center().y + 15.0);
            let hov = self.hovered(btn);
            draw::fill_round(scene, btn, theme.tokens.radii.md as f64, if hov { draw::with_alpha(color, 60) } else { c.surface_white_5 });
            draw::stroke_round(scene, btn, theme.tokens.radii.md as f64, 1.0, draw::with_alpha(color, 120));
            draw::icon(scene, icon, Rect::new(btn.x0 + 9.0, btn.center().y - 7.0, btn.x0 + 23.0, btn.center().y + 7.0), color, 1.5);
            draw::text(scene, tr, label, theme.tokens.typography.scale.sm, c.text_primary, Point::new(btn.x0 + 28.0, btn.center().y - 7.0), TextWeight::Semibold);
            self.push_hit(btn, Hit::Action { id: container.id.clone(), action });
            x += bw + 8.0;
        }
    }

    fn draw_logs(&mut self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, body: Rect, container: &Container) {
        let c = &theme.tokens.colors;
        let term_bg = c.neutral[0];
        let inner = Rect::new(body.x0 + 8.0, body.y0 + 8.0, body.x1 - 8.0, body.y1 - 8.0);
        draw::fill_round(scene, inner, theme.tokens.radii.md as f64, term_bg);
        if container.logs.is_empty() {
            self.draw_empty(scene, tr, theme, inner, "No log output yet.");
            return;
        }
        let mono = theme.tokens.typography.scale.xs;
        let line_h = (mono * 1.5) as f64;
        let visible = ((inner.height() - 16.0) / line_h).floor() as usize;
        let total = container.logs.len();
        let content_h = total as f64 * line_h;
        let max_scroll = (content_h - (inner.height() - 16.0)).max(0.0);
        self.clamp_detail_scroll(max_scroll);
        let first = (self.detail_scroll / line_h).floor() as usize;
        let mut y = inner.y0 + 10.0 - (self.detail_scroll - first as f64 * line_h);
        for line in container.logs.iter().skip(first).take(visible + 2) {
            let color = log_line_color(theme, line);
            draw::text_clip(
                scene,
                tr,
                line,
                mono,
                color,
                Point::new(inner.x0 + 12.0, y),
                inner.width() - 24.0,
                TextWeight::Regular,
            );
            y += line_h;
        }
        self.draw_scrollbar(scene, theme, inner, self.detail_scroll, content_h);
        // tail indicator
        let tail = format!("{} lines", total);
        draw::text(scene, tr, &tail, theme.tokens.typography.scale.xs, c.text_dim, Point::new(inner.x1 - 70.0, inner.y1 - 16.0), TextWeight::Regular);
    }

    fn draw_stats(&mut self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, body: Rect, container: &Container) {
        let c = &theme.tokens.colors;
        let s = &container.stats;
        let mut y = body.y0 + 16.0;
        let card_w = body.width() - PAD * 2.0;

        // Big CPU + MEM gauges.
        let gauge = |scene: &mut Scene, tr: &mut TextRenderer, app: &ContainersApp, label: &str, value: String, frac: f32, color: Color, y: f64| {
            let card = Rect::new(body.x0 + PAD, y, body.x0 + PAD + card_w, y + 72.0);
            draw::fill_round(scene, card, theme.tokens.radii.md as f64, c.surfaces.base);
            draw::stroke_round(scene, card, theme.tokens.radii.md as f64, 1.0, c.border_standard);
            draw::text(scene, tr, label, theme.tokens.typography.scale.xs, c.text_dim, Point::new(card.x0 + 14.0, card.y0 + 12.0), TextWeight::Semibold);
            draw::text(scene, tr, &value, theme.tokens.typography.scale.xl, c.text_primary, Point::new(card.x0 + 14.0, card.y0 + 26.0), TextWeight::Bold);
            let bar = Rect::new(card.x0 + 14.0, card.y1 - 16.0, card.x1 - 14.0, card.y1 - 10.0);
            draw::usage_bar(scene, theme, bar, frac, color);
            let _ = app;
        };
        gauge(scene, tr, self, "CPU", format!("{:.1}%", s.cpu_percent * 100.0), s.cpu_percent.min(1.0), c.accent[5], y);
        y += 84.0;
        let mem_label = if s.mem_limit_bytes > 0 {
            format!("{} / {}", human_bytes(s.mem_bytes), human_bytes(s.mem_limit_bytes))
        } else {
            human_bytes(s.mem_bytes)
        };
        gauge(scene, tr, self, "MEMORY", mem_label, s.mem_fraction(), c.semantic.info, y);
        y += 84.0;

        // Network + block IO + pids as a small grid.
        let rows = [
            ("Network RX", human_bytes(s.net_rx_bytes)),
            ("Network TX", human_bytes(s.net_tx_bytes)),
            ("Block read", human_bytes(s.block_read_bytes)),
            ("Block write", human_bytes(s.block_write_bytes)),
            ("Processes", s.pids.to_string()),
        ];
        let card = Rect::new(body.x0 + PAD, y, body.x0 + PAD + card_w, y + rows.len() as f64 * 30.0 + 12.0);
        draw::fill_round(scene, card, theme.tokens.radii.md as f64, c.surfaces.base);
        draw::stroke_round(scene, card, theme.tokens.radii.md as f64, 1.0, c.border_standard);
        let mut ry = card.y0 + 8.0;
        for (i, (k, v)) in rows.iter().enumerate() {
            draw::text(scene, tr, k, theme.tokens.typography.scale.sm, c.text_dim, Point::new(card.x0 + 14.0, ry + 6.0), TextWeight::Regular);
            let (vw, _) = tr.measure_weighted(v, theme.tokens.typography.scale.sm, None, TextWeight::Semibold);
            draw::text(scene, tr, v, theme.tokens.typography.scale.sm, c.text_primary, Point::new(card.x1 - 14.0 - vw as f64, ry + 6.0), TextWeight::Semibold);
            if i + 1 < rows.len() {
                draw::divider(scene, card.x0 + 14.0, card.x1 - 14.0, ry + 28.0, draw::with_alpha(c.border_standard, 120));
            }
            ry += 30.0;
        }
        let _ = Stats::default();
    }

    fn draw_inspect(&mut self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, body: Rect, container: &Container) {
        let c = &theme.tokens.colors;
        // Build a flat list of inspect fields so it scrolls as one column.
        let mut lines: Vec<(String, String, bool)> = Vec::new();
        lines.push(("Id".into(), container.id.clone(), true));
        lines.push(("Command".into(), container.command.clone(), false));
        lines.push(("Created".into(), container.created.clone(), false));
        lines.push(("Status".into(), container.status_detail.clone(), false));
        // section: ports
        if !container.ports.is_empty() {
            lines.push(("— Ports —".into(), String::new(), false));
            for p in &container.ports {
                lines.push((format!("{}/{}", p.container_port, p.protocol), format!("host :{}", p.host_port), true));
            }
        }
        if !container.mounts.is_empty() {
            lines.push(("— Mounts —".into(), String::new(), false));
            for m in &container.mounts {
                let ro = if m.read_only { " (ro)" } else { "" };
                lines.push((m.destination.clone(), format!("{}{}", m.source, ro), true));
            }
        }
        if !container.env.is_empty() {
            lines.push(("— Environment —".into(), String::new(), false));
            for (k, v) in &container.env {
                lines.push((k.clone(), v.clone(), true));
            }
        }
        if !container.labels.is_empty() {
            lines.push(("— Labels —".into(), String::new(), false));
            for (k, v) in &container.labels {
                lines.push((k.clone(), v.clone(), false));
            }
        }

        let line_h = 26.0;
        let area = Rect::new(body.x0, body.y0 + 4.0, body.x1, body.y1);
        let content_h = lines.len() as f64 * line_h + 16.0;
        let max_scroll = (content_h - area.height()).max(0.0);
        self.clamp_detail_scroll(max_scroll);
        let mut y = area.y0 + 8.0 - self.detail_scroll;
        for (k, v, mono) in &lines {
            if y + line_h >= area.y0 && y <= area.y1 {
                let is_section = k.starts_with("—");
                if is_section {
                    draw::text(scene, tr, k, theme.tokens.typography.scale.xs, c.accent[6], Point::new(area.x0 + PAD, y + 6.0), TextWeight::Semibold);
                } else {
                    let key_w = (area.width() * 0.4).min(150.0);
                    draw::text_clip(scene, tr, k, theme.tokens.typography.scale.sm, c.text_dim, Point::new(area.x0 + PAD, y + 6.0), key_w, TextWeight::Regular);
                    let vx = area.x0 + PAD + key_w + 8.0;
                    let weight = if *mono { TextWeight::Medium } else { TextWeight::Regular };
                    draw::text_clip(scene, tr, v, theme.tokens.typography.scale.sm, c.text_primary, Point::new(vx, y + 6.0), area.x1 - vx - PAD, weight);
                }
            }
            y += line_h;
        }
        self.draw_scrollbar(scene, theme, area, self.detail_scroll, content_h);
    }

    fn draw_terminal(&mut self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, body: Rect, container: &Container) {
        let c = &theme.tokens.colors;
        let inner = Rect::new(body.x0 + 8.0, body.y0 + 8.0, body.x1 - 8.0, body.y1 - 8.0);
        draw::fill_round(scene, inner, theme.tokens.radii.md as f64, c.neutral[0]);
        let mono = theme.tokens.typography.scale.sm;
        let mut y = inner.y0 + 14.0;
        let prompt_lines = [
            (format!("$ podman exec -it {} /bin/sh", container.short_id()), c.semantic.success),
            ("/ # whoami".to_string(), c.text_primary),
            ("root".to_string(), c.text_dim),
            ("/ # ps -o pid,comm".to_string(), c.text_primary),
            (format!("  1 {}", container.command.split_whitespace().next().unwrap_or("sh")), c.text_dim),
            ("/ # ".to_string(), c.text_primary),
        ];
        for (line, color) in prompt_lines {
            draw::text(scene, tr, &line, mono, color, Point::new(inner.x0 + 14.0, y), TextWeight::Regular);
            y += (mono * 1.6) as f64;
        }
        // blinking caret
        let caret = Rect::new(inner.x0 + 14.0 + 30.0, y - (mono * 1.6) as f64, inner.x0 + 14.0 + 38.0, y - (mono * 1.6) as f64 + mono as f64);
        scene.fill(Fill::NonZero, Affine::IDENTITY, draw::with_alpha(c.semantic.success, 180), None, &caret);
        // hint
        let hint = if container.status.is_running() {
            "Interactive exec attaches a shell in the running container."
        } else {
            "Start the container to attach an interactive shell."
        };
        draw::text(scene, tr, hint, theme.tokens.typography.scale.xs, c.text_dim, Point::new(inner.x0 + 14.0, inner.y1 - 24.0), TextWeight::Regular);
    }

    // =================================================================
    // Images section
    // =================================================================

    pub(crate) fn draw_images(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        view: &ViewState,
    ) {
        let body_y = self.draw_header(scene, tr, theme, content, "Images", view, None);
        let c = &theme.tokens.colors;
        let list = Rect::new(content.x0, body_y, content.x1, content.y1);

        // Summary banner.
        let banner = Rect::new(list.x0 + PAD, list.y0 + 8.0, list.x1 - PAD, list.y0 + 44.0);
        draw::fill_round(scene, banner, theme.tokens.radii.md as f64, c.surfaces.raised);
        let disk = format!(
            "{} images · {} on disk",
            self.engine.images.len(),
            human_bytes(self.engine.image_disk_bytes())
        );
        draw::text(scene, tr, &disk, theme.tokens.typography.scale.sm, c.text_dim, Point::new(banner.x0 + 14.0, banner.center().y - 7.0), TextWeight::Medium);

        let query = view.search.clone();
        let images: Vec<usize> = self
            .engine
            .images
            .iter()
            .enumerate()
            .filter(|(_, im)| im.matches(&query))
            .map(|(i, _)| i)
            .collect();

        let rows_area = Rect::new(list.x0, banner.y1 + 8.0, list.x1, list.y1);
        let row_h = 56.0;
        let content_h = images.len() as f64 * row_h;
        let max_scroll = (content_h - rows_area.height()).max(0.0);
        self.clamp_list_scroll(max_scroll);
        let mut y = rows_area.y0 - self.list_scroll;
        for &idx in &images {
            let image = self.engine.images[idx].clone();
            let row = Rect::new(rows_area.x0, y, rows_area.x1, y + row_h);
            if row.y1 >= rows_area.y0 && row.y0 <= rows_area.y1 {
                let inner = Rect::new(row.x0 + PAD, row.y0 + 4.0, row.x1 - PAD, row.y1 - 4.0);
                if self.hovered(row) {
                    draw::fill_round(scene, inner, theme.tokens.radii.md as f64, c.surface_white_5);
                }
                draw::icon(scene, Icon::Layers, Rect::new(inner.x0 + 8.0, inner.center().y - 12.0, inner.x0 + 32.0, inner.center().y + 12.0), c.accent[6], 1.5);
                draw::text_clip(scene, tr, &image.reference(), theme.tokens.typography.scale.base, c.text_primary, Point::new(inner.x0 + 44.0, row.center().y - 16.0), inner.width() * 0.5, TextWeight::Semibold);
                draw::text(scene, tr, image.short_id(), theme.tokens.typography.scale.xs, c.text_dim, Point::new(inner.x0 + 44.0, row.center().y + 4.0), TextWeight::Regular);
                // right cluster: size, created, in-use chip
                let size = human_bytes(image.size_bytes);
                let (sw, _) = tr.measure_weighted(&size, theme.tokens.typography.scale.base, None, TextWeight::Semibold);
                draw::text(scene, tr, &size, theme.tokens.typography.scale.base, c.text_primary, Point::new(inner.x1 - sw as f64, row.center().y - 16.0), TextWeight::Semibold);
                draw::text(scene, tr, &image.created, theme.tokens.typography.scale.xs, c.text_dim, Point::new(inner.x1 - 110.0, row.center().y + 4.0), TextWeight::Regular);
                if image.in_use_by > 0 {
                    draw::chip(scene, tr, theme, &format!("in use ×{}", image.in_use_by), Point::new(inner.x0 + inner.width() * 0.5, row.center().y - 8.0), 16.0, c.semantic.success);
                } else {
                    draw::chip(scene, tr, theme, "unused", Point::new(inner.x0 + inner.width() * 0.5, row.center().y - 8.0), 16.0, c.text_dim);
                }
                draw::divider(scene, inner.x0, inner.x1, row.y1, draw::with_alpha(c.border_standard, 90));
            }
            self.push_hit(row, Hit::SelectImage(idx));
            y += row_h;
        }
        if images.is_empty() {
            self.draw_empty(scene, tr, theme, rows_area, "No images match your search.");
        }
        self.draw_scrollbar(scene, theme, rows_area, self.list_scroll, content_h);
    }

    // =================================================================
    // Distrobox section
    // =================================================================

    pub(crate) fn draw_distrobox(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        view: &ViewState,
    ) {
        let body_y = self.draw_header(scene, tr, theme, content, "Distrobox", view, Some(("Create box", Hit::DistroCreate)));
        let c = &theme.tokens.colors;
        let list = Rect::new(content.x0, body_y, content.x1, content.y1);

        let intro = "Distroboxes are tightly host-integrated containers — your shell, home, and devices are shared. Enter one to get a mutable userspace on the immutable OS.";
        draw::text_clip(scene, tr, intro, theme.tokens.typography.scale.sm, c.text_dim, Point::new(list.x0 + PAD, list.y0 + 12.0), list.width() - PAD * 2.0, TextWeight::Regular);

        let query = view.search.clone();
        let boxes: Vec<_> = self.engine.distroboxes.iter().filter(|b| b.matches(&query)).cloned().collect();

        // Card grid (two columns).
        let grid_top = list.y0 + 48.0;
        let cols = if list.width() > 760.0 { 2 } else { 1 };
        let gap = 12.0;
        let card_w = (list.width() - PAD * 2.0 - gap * (cols as f64 - 1.0)) / cols as f64;
        let card_h = 96.0;
        for (i, b) in boxes.iter().enumerate() {
            let col = i % cols;
            let rowi = i / cols;
            let x = list.x0 + PAD + col as f64 * (card_w + gap);
            let y = grid_top + rowi as f64 * (card_h + gap) - self.list_scroll;
            let card = Rect::new(x, y, x + card_w, y + card_h);
            if card.y1 < list.y0 || card.y0 > list.y1 {
                continue;
            }
            let hov = self.hovered(card);
            draw::fill_round(scene, card, theme.tokens.radii.lg as f64, c.surfaces.raised);
            draw::stroke_round(scene, card, theme.tokens.radii.lg as f64, 1.0, if hov { c.border_focus } else { c.border_standard });
            draw::icon(scene, Icon::Terminal, Rect::new(card.x0 + 14.0, card.y0 + 14.0, card.x0 + 38.0, card.y0 + 38.0), c.accent[6], 1.6);
            draw::text_clip(scene, tr, &b.name, theme.tokens.typography.scale.lg, c.text_primary, Point::new(card.x0 + 48.0, card.y0 + 14.0), card_w - 60.0, TextWeight::Bold);
            draw::status_pill(scene, tr, theme, b.status, b.status.label(), Point::new(card.x0 + 48.0, card.y0 + 38.0), 20.0);
            draw::text_clip(scene, tr, &b.image, theme.tokens.typography.scale.xs, c.text_dim, Point::new(card.x0 + 14.0, card.y1 - 24.0), card_w - 120.0, TextWeight::Regular);
            // enter button
            let bw = 86.0;
            let btn = Rect::new(card.x1 - 14.0 - bw, card.y1 - 36.0, card.x1 - 14.0, card.y1 - 8.0);
            let bhov = self.hovered(btn);
            draw::fill_round(scene, btn, theme.tokens.radii.md as f64, if bhov { c.accent[4] } else { c.accent[5] });
            draw::icon(scene, Icon::Enter, Rect::new(btn.x0 + 10.0, btn.center().y - 8.0, btn.x0 + 26.0, btn.center().y + 8.0), Color::WHITE, 1.6);
            draw::text(scene, tr, "Enter", theme.tokens.typography.scale.sm, Color::WHITE, Point::new(btn.x0 + 30.0, btn.center().y - 7.0), TextWeight::Semibold);
            self.push_hit(btn, Hit::DistroEnter(b.name.clone()));
        }
        if boxes.is_empty() {
            let area = Rect::new(list.x0, grid_top, list.x1, list.y1);
            self.draw_empty(scene, tr, theme, area, "No distroboxes yet — create one to get started.");
        }
    }

    // =================================================================
    // Modals + overlays
    // =================================================================

    pub(crate) fn draw_create_modal(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        view: &ViewState,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, Affine::IDENTITY, c.surfaces.overlay, None, &full);
        let mw = 480.0_f64.min(full.width() - 48.0);
        let mh = 420.0_f64;
        let modal = Rect::from_center_size(full.center(), (mw, mh));
        draw::fill_round(scene, modal, theme.tokens.radii.lg as f64, c.surfaces.raised);
        draw::stroke_round(scene, modal, theme.tokens.radii.lg as f64, 1.0, c.border_standard);

        draw::text(scene, tr, "New container", theme.tokens.typography.scale.xl, c.text_primary, Point::new(modal.x0 + 24.0, modal.y0 + 20.0), TextWeight::Bold);
        draw::text(scene, tr, "podman run, rootless", theme.tokens.typography.scale.xs, c.text_dim, Point::new(modal.x0 + 24.0, modal.y0 + 46.0), TextWeight::Regular);

        let mut y = modal.y0 + 76.0;
        for (i, field) in CreateField::ALL.iter().enumerate() {
            let focused = self.create_focus == i;
            draw::text(scene, tr, field.label(), theme.tokens.typography.scale.xs, c.text_dim, Point::new(modal.x0 + 24.0, y), TextWeight::Medium);
            let input = Rect::new(modal.x0 + 24.0, y + 16.0, modal.x1 - 24.0, y + 48.0);
            draw::fill_round(scene, input, theme.tokens.radii.md as f64, c.surfaces.base);
            draw::stroke_round(scene, input, theme.tokens.radii.md as f64, if focused { 2.0 } else { 1.0 }, if focused { c.border_focus } else { c.border_standard });
            let value = &view.create[i];
            if value.is_empty() {
                draw::text(scene, tr, field.placeholder(), theme.tokens.typography.scale.sm, draw::with_alpha(c.text_dim, 150), Point::new(input.x0 + 12.0, input.center().y - 7.0), TextWeight::Regular);
            } else {
                draw::text_clip(scene, tr, value, theme.tokens.typography.scale.sm, c.text_primary, Point::new(input.x0 + 12.0, input.center().y - 7.0), input.width() - 24.0, TextWeight::Regular);
            }
            if focused {
                let (vw, _) = tr.measure(value, theme.tokens.typography.scale.sm, None);
                let cx = (input.x0 + 12.0 + vw as f64).min(input.x1 - 14.0);
                let caret = Rect::new(cx, input.center().y - 8.0, cx + 1.5, input.center().y + 8.0);
                scene.fill(Fill::NonZero, Affine::IDENTITY, c.accent[5], None, &caret);
            }
            self.push_hit(input, Hit::CreateField(*field));
            y += 62.0;
        }

        // Buttons
        let cancel = Rect::new(modal.x1 - 24.0 - 200.0, modal.y1 - 52.0, modal.x1 - 24.0 - 108.0, modal.y1 - 16.0);
        let create = Rect::new(modal.x1 - 24.0 - 96.0, modal.y1 - 52.0, modal.x1 - 24.0, modal.y1 - 16.0);
        draw::fill_round(scene, cancel, theme.tokens.radii.md as f64, if self.hovered(cancel) { c.surface_white_10 } else { c.surface_white_5 });
        draw::text(scene, tr, "Cancel", theme.tokens.typography.scale.sm, c.text_primary, Point::new(cancel.center().x - 22.0, cancel.center().y - 7.0), TextWeight::Medium);
        draw::fill_round(scene, create, theme.tokens.radii.md as f64, if self.hovered(create) { c.accent[4] } else { c.accent[5] });
        draw::text(scene, tr, "Create", theme.tokens.typography.scale.sm, Color::WHITE, Point::new(create.center().x - 21.0, create.center().y - 7.0), TextWeight::Semibold);
        self.push_hit(cancel, Hit::CreateCancel);
        self.push_hit(create, Hit::CreateSubmit);
    }

    pub(crate) fn draw_confirm(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        confirm: &crate::app::Confirm,
        hits: &mut Vec<HitRegion>,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, Affine::IDENTITY, c.surfaces.overlay, None, &full);
        let modal = Rect::from_center_size(full.center(), (400.0, 188.0));
        draw::fill_round(scene, modal, theme.tokens.radii.lg as f64, c.surfaces.raised);
        draw::stroke_round(scene, modal, theme.tokens.radii.lg as f64, 1.0, c.border_standard);
        draw::icon(scene, Icon::Trash, Rect::new(modal.x0 + 24.0, modal.y0 + 22.0, modal.x0 + 48.0, modal.y0 + 46.0), c.semantic.danger, 1.8);
        draw::text(scene, tr, &format!("{} container?", confirm.action.label()), theme.tokens.typography.scale.lg, c.text_primary, Point::new(modal.x0 + 58.0, modal.y0 + 24.0), TextWeight::Bold);
        let msg = format!("This permanently removes \"{}\". This cannot be undone.", confirm.name);
        draw::text_clip(scene, tr, &msg, theme.tokens.typography.scale.sm, c.text_dim, Point::new(modal.x0 + 24.0, modal.y0 + 64.0), modal.width() - 48.0, TextWeight::Regular);

        let cancel = Rect::new(modal.x1 - 24.0 - 196.0, modal.y1 - 52.0, modal.x1 - 24.0 - 104.0, modal.y1 - 16.0);
        let ok = Rect::new(modal.x1 - 24.0 - 92.0, modal.y1 - 52.0, modal.x1 - 24.0, modal.y1 - 16.0);
        draw::fill_round(scene, cancel, theme.tokens.radii.md as f64, c.surface_white_5);
        draw::text(scene, tr, "Cancel", theme.tokens.typography.scale.sm, c.text_primary, Point::new(cancel.center().x - 22.0, cancel.center().y - 7.0), TextWeight::Medium);
        draw::fill_round(scene, ok, theme.tokens.radii.md as f64, c.semantic.danger);
        draw::text(scene, tr, confirm.action.label(), theme.tokens.typography.scale.sm, Color::WHITE, Point::new(ok.center().x - 22.0, ok.center().y - 7.0), TextWeight::Semibold);
        hits.push(HitRegion { rect: cancel, hit: Hit::ConfirmCancel });
        hits.push(HitRegion { rect: ok, hit: Hit::ConfirmOk });
    }

    pub(crate) fn draw_toast(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
        toast: &crate::app::Toast,
    ) {
        let c = &theme.tokens.colors;
        let accent = if toast.error { c.semantic.danger } else { c.semantic.success };
        let (tw, _) = tr.measure(&toast.message, theme.tokens.typography.scale.sm, None);
        let w = (tw as f64 + 64.0).min(full.width() - 48.0);
        let h = 44.0;
        let toast_rect = Rect::new(full.center().x - w / 2.0, full.y1 - h - 24.0, full.center().x + w / 2.0, full.y1 - 24.0);
        draw::fill_round(scene, toast_rect, theme.tokens.radii.md as f64, c.neutral[2]);
        draw::stroke_round(scene, toast_rect, theme.tokens.radii.md as f64, 1.0, draw::with_alpha(accent, 160));
        let dot = vello::kurbo::Circle::new(Point::new(toast_rect.x0 + 20.0, toast_rect.center().y), 4.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, accent, None, &dot);
        draw::text_clip(scene, tr, &toast.message, theme.tokens.typography.scale.sm, c.text_primary, Point::new(toast_rect.x0 + 34.0, toast_rect.center().y - 7.0), w - 48.0, TextWeight::Medium);
    }

    // =================================================================
    // Shared helpers
    // =================================================================

    fn draw_empty(&self, scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, area: Rect, msg: &str) {
        let c = &theme.tokens.colors;
        let (w, _) = tr.measure(msg, theme.tokens.typography.scale.base, None);
        let p = Point::new(area.center().x - w as f64 / 2.0, area.center().y - 8.0);
        draw::text(scene, tr, msg, theme.tokens.typography.scale.base, c.text_dim, p, TextWeight::Regular);
        let _ = scene;
    }

    fn draw_scrollbar(&self, scene: &mut Scene, theme: &Theme, area: Rect, scroll: f64, content_h: f64) {
        if content_h <= area.height() {
            return;
        }
        let c = &theme.tokens.colors;
        let track_h = area.height();
        let thumb_h = (track_h * (track_h / content_h)).max(28.0);
        let max_scroll = content_h - track_h;
        let t = if max_scroll > 0.0 { (scroll / max_scroll).clamp(0.0, 1.0) } else { 0.0 };
        let thumb_y = area.y0 + t * (track_h - thumb_h);
        let thumb = Rect::new(area.x1 - 7.0, thumb_y, area.x1 - 3.0, thumb_y + thumb_h);
        draw::fill_round(scene, thumb, 2.0, c.surface_white_20);
    }

    fn clamp_list_scroll(&mut self, max: f64) {
        if self.list_scroll > max {
            self.list_scroll = max;
        }
        if self.list_scroll < 0.0 {
            self.list_scroll = 0.0;
        }
    }

    fn clamp_detail_scroll(&mut self, max: f64) {
        if self.detail_scroll > max {
            self.detail_scroll = max;
        }
        if self.detail_scroll < 0.0 {
            self.detail_scroll = 0.0;
        }
    }

    // =================================================================
    // Interaction
    // =================================================================

    pub(crate) fn on_click(&mut self, doc: &mut AutoCommit, handled: &mut bool) {
        // Modal layers consume clicks even outside their controls.
        let modal_active = self.confirm.is_some() || self.create_open;
        // Resolve top-most hit (later draws are on top), so scan in reverse.
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|h| h.rect.contains(self.cursor))
            .map(|h| h.hit.clone());

        // Clicking outside a modal closes it (but a hit inside still routes).
        let Some(hit) = hit else {
            if modal_active {
                // click on the scrim: dismiss
                self.confirm = None;
                self.create_open = false;
                *handled = true;
            } else {
                self.search_focus = false;
                *handled = true;
            }
            self.refresh_view(doc);
            return;
        };

        *handled = true;
        match hit {
            Hit::Section(s) => {
                ViewDoc::set_section(doc, s);
                ViewDoc::set_selected(doc, None);
                self.list_scroll = 0.0;
                self.search_focus = false;
            }
            Hit::NewContainer => {
                self.create_open = true;
                self.create_focus = 1; // image is the required field
            }
            Hit::SelectContainer(id) => {
                let already = ViewDoc::selected(doc).as_deref() == Some(id.as_str());
                ViewDoc::set_selected(doc, if already { None } else { Some(&id) });
                self.detail_scroll = 0.0;
                self.search_focus = false;
            }
            Hit::SelectImage(idx) => {
                if let Some(image) = self.engine.images.get(idx) {
                    self.show_toast(format!("{} · {}", image.reference(), human_bytes(image.size_bytes)), false);
                }
            }
            Hit::Action { id, action } => {
                if action.is_destructive() {
                    let name = self.engine.find(&id).map(|c| c.name.clone()).unwrap_or_else(|| id.clone());
                    self.confirm = Some(crate::app::Confirm { id, name, action });
                } else {
                    self.apply_action(action, &id);
                }
            }
            Hit::DistroEnter(name) => {
                self.enter_distrobox(&name);
            }
            Hit::DistroCreate => {
                self.create_open = true;
                self.create_focus = 0;
            }
            Hit::DetailTab(tab) => {
                ViewDoc::set_tab(doc, tab);
                self.detail_scroll = 0.0;
            }
            Hit::CloseDetail => {
                ViewDoc::set_selected(doc, None);
            }
            Hit::ConfirmOk => {
                if let Some(confirm) = self.confirm.take() {
                    self.apply_action(confirm.action, &confirm.id);
                    if ViewDoc::selected(doc).as_deref() == Some(confirm.id.as_str()) {
                        ViewDoc::set_selected(doc, None);
                    }
                }
            }
            Hit::ConfirmCancel => {
                self.confirm = None;
            }
            Hit::CreateField(field) => {
                self.create_focus = CreateField::ALL.iter().position(|f| *f == field).unwrap_or(0);
            }
            Hit::CreateSubmit => {
                self.submit_create(doc);
            }
            Hit::CreateCancel => {
                self.create_open = false;
            }
            Hit::SearchFocus => {
                self.search_focus = true;
            }
            Hit::ClearSearch => {
                ViewDoc::set_search(doc, "");
                self.list_scroll = 0.0;
            }
        }
        self.refresh_view(doc);
    }

    fn enter_distrobox(&mut self, name: &str) {
        if self.live {
            // Launch a terminal session entering the box. Best-effort: if no
            // terminal is available this is a no-op the toast still reports.
            let _ = std::process::Command::new("distrobox")
                .args(["enter", name])
                .spawn();
        }
        self.show_toast(format!("Entering {}", name), false);
    }

    pub(crate) fn on_key(
        &mut self,
        doc: &mut AutoCommit,
        key: &KeyEvent,
        _modifiers: &winit::event::Modifiers,
        handled: &mut bool,
    ) {
        if key.state != ElementState::Pressed {
            return;
        }

        // Escape closes the top-most overlay / clears focus.
        if let Key::Named(NamedKey::Escape) = key.logical_key {
            if self.confirm.take().is_some() || self.create_open {
                self.create_open = false;
            } else if self.search_focus {
                self.search_focus = false;
            } else if ViewDoc::selected(doc).is_some() {
                ViewDoc::set_selected(doc, None);
            }
            *handled = true;
            self.refresh_view(doc);
            return;
        }

        // Create modal owns the keyboard while open.
        if self.create_open {
            self.create_modal_key(doc, key, handled);
            self.refresh_view(doc);
            return;
        }

        // Search box editing.
        if self.search_focus {
            self.search_key(doc, key, handled);
            self.refresh_view(doc);
            return;
        }

        // Global shortcuts.
        match &key.logical_key {
            Key::Character(ch) => {
                let s = ch.to_lowercase();
                match s.as_str() {
                    "1" => { ViewDoc::set_section(doc, Section::Containers); *handled = true; }
                    "2" => { ViewDoc::set_section(doc, Section::Images); *handled = true; }
                    "3" => { ViewDoc::set_section(doc, Section::Distrobox); *handled = true; }
                    "/" => { self.search_focus = true; *handled = true; }
                    "n" => { self.create_open = true; self.create_focus = 1; *handled = true; }
                    _ => {}
                }
            }
            _ => {}
        }
        self.refresh_view(doc);
    }

    fn search_key(&mut self, doc: &mut AutoCommit, key: &KeyEvent, handled: &mut bool) {
        let cur = ViewDoc::search(doc);
        match &key.logical_key {
            Key::Named(NamedKey::Backspace) => {
                let mut chars: Vec<char> = cur.chars().collect();
                chars.pop();
                ViewDoc::set_search(doc, &chars.into_iter().collect::<String>());
                self.list_scroll = 0.0;
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                self.search_focus = false;
                *handled = true;
            }
            Key::Named(NamedKey::Space) => {
                ViewDoc::set_search(doc, &format!("{cur} "));
                *handled = true;
            }
            Key::Character(ch) => {
                ViewDoc::set_search(doc, &format!("{cur}{ch}"));
                self.list_scroll = 0.0;
                *handled = true;
            }
            _ => {}
        }
    }

    fn create_modal_key(&mut self, doc: &mut AutoCommit, key: &KeyEvent, handled: &mut bool) {
        let field = CreateField::ALL[self.create_focus];
        match &key.logical_key {
            Key::Named(NamedKey::Tab) => {
                self.create_focus = (self.create_focus + 1) % CreateField::ALL.len();
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                self.submit_create(doc);
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) => {
                let cur = ViewDoc::create_field(doc, field);
                let mut chars: Vec<char> = cur.chars().collect();
                chars.pop();
                ViewDoc::set_create_field(doc, field, &chars.into_iter().collect::<String>());
                *handled = true;
            }
            Key::Named(NamedKey::Space) => {
                let cur = ViewDoc::create_field(doc, field);
                ViewDoc::set_create_field(doc, field, &format!("{cur} "));
                *handled = true;
            }
            Key::Character(ch) => {
                let cur = ViewDoc::create_field(doc, field);
                ViewDoc::set_create_field(doc, field, &format!("{cur}{ch}"));
                *handled = true;
            }
            _ => {}
        }
    }
}

/// Colour a log line by its severity keyword so the log view reads like a
/// real console.
fn log_line_color(theme: &Theme, line: &str) -> Color {
    let c = &theme.tokens.colors;
    let upper = line.to_ascii_uppercase();
    if upper.contains("ERROR") || upper.contains("FATAL") || upper.contains("SIGKILL") || upper.contains("404") {
        c.semantic.danger
    } else if upper.contains("WARN") {
        c.semantic.warning
    } else if upper.contains("INFO") || upper.contains("NOTICE") || upper.contains("LOG:") {
        c.text_primary
    } else {
        c.text_dim
    }
}
