//! Panel rendering + interaction for [`ContainersApp`].
//!
//! Split out of `app.rs` to keep the widget-trait wiring legible. Everything
//! here is an inherent method on `ContainersApp`: the `draw_*` methods paint a
//! region and record `Hit` regions; `on_click` / `on_key` resolve input
//! against those regions and mutate the CRDT view state.

use automerge::AutoCommit;
use ui::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key, NamedKey};

use crate::app::{
    ConfirmTarget, ContainersApp, Hit, HitRegion, PromptKind, PullProgress, ViewState,
};
use crate::combobox;
use crate::doc::{CreateDoc, CreateField};
use crate::draw::{self, Icon};
use crate::model::{
    Container, ContainerStatus, DetailTab, DistroBox, Image, Section, Stats, Volume,
};
use crate::podman::{Action, ImageAction};
use crate::presentation::{create_form_fields, create_form_geometry, draw_field_frame};

/// Sizes used across the panels — kept here so the layout reads coherently.
const PAD: f64 = 16.0;
const ICON_BTN: f64 = 32.0;
/// Inset of a container row's text from its column edge (clears the status
/// accent bar); the column heading shares it.
const ROW_TEXT_INSET: f64 = 8.0;
/// Header search launcher placeholders: the full hint, and the short form used
/// when the field cannot hold the full one beside its ⌘K key cap.
pub(crate) const LAUNCHER_PLACEHOLDER: &str = "Search containers…";
const LAUNCHER_PLACEHOLDER_SHORT: &str = "Search";

/// One container row to paint: its position, the container itself, the
/// shared column x-positions, and whether the compact/detail-open layout
/// applies.
#[derive(Clone, Copy)]
struct ContainerRow<'a> {
    row: Rect,
    container: &'a Container,
    cols: &'a [f64; 5],
    compact: bool,
}

/// One CPU/MEM gauge card's content, for [`ContainersApp::draw_stat_gauge`].
struct GaugeCard {
    label: &'static str,
    value: String,
    frac: f32,
    color: Color,
}

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
        compact: bool,
    ) {
        let c = &theme.tokens.colors;
        // `expanded` gates the text labels; the rail keeps its look — the same
        // raised surface, accent wordmark, section pills, and footer — just
        // narrowed to an icon strip while collapsed.
        let expanded = !compact && self.rail_expanded();
        draw::fill_round(scene, rail, 0.0, c.surfaces.raised);
        // The rail's right edge: a hairline derived against the raised rail
        // (a declared `border_standard` vanished on the dark preset).
        let border = Rect::new(rail.x1 - 1.0, rail.y0, rail.x1, rail.y1);
        scene.fill(
            Fill::NonZero,
            draw::base(),
            draw::edge(c.surfaces.raised),
            None,
            &border,
        );

        // A compact rail is intentionally fixed: a toggle would only steal
        // precious content width and make its state unpredictable. Section
        // icons remain directly reachable, while shortcuts 1–4 remain an
        // equivalent keyboard route.
        if !compact {
            self.draw_rail_toggle(scene, tr, theme, rail, expanded);
        }
        self.draw_rail_sections(scene, tr, theme, rail, view, expanded, compact);
        if !compact {
            self.draw_rail_footer(scene, tr, theme, rail, expanded);
        }
    }

    /// The collapse/expand chevron at the top of the rail. No app-name
    /// wordmark or brand mark — the OS owns window chrome and shows
    /// "Containers" in the titlebar.
    fn draw_rail_toggle(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        expanded: bool,
    ) {
        let c = &theme.tokens.colors;
        let title_y = rail.y0 + PAD + 8.0;
        // Collapse / expand toggle — a chevron at the top-right of the rail.
        let tgl = Rect::new(rail.x1 - 30.0, title_y - 4.0, rail.x1 - 6.0, title_y + 20.0);
        if expanded {
            if self.hovered(tgl) {
                draw::fill_round(scene, tgl, f64::from(theme.tokens.radii.sm), c.lift(0.72));
            }
            // "«" collapse glyph, centred on its measured box.
            let (gw, gh) = draw::measure(
                tr,
                "«",
                theme.tokens.typography.scale.base,
                TextWeight::Bold,
            );
            draw::text(
                scene,
                tr,
                "«",
                theme.tokens.typography.scale.base,
                c.text_dim,
                Point::new(
                    tgl.center().x - f64::from(gw) / 2.0,
                    tgl.center().y - f64::from(gh) / 2.0,
                ),
                TextWeight::Bold,
            );
            self.push_hit(tgl, Hit::ToggleSidebar);
        } else {
            // Collapsed: the toggle is a wide hit over the header area.
            let ctgl = Rect::new(rail.x0 + 4.0, rail.y0 + 28.0, rail.x1 - 4.0, rail.y0 + 46.0);
            if self.hovered(ctgl) {
                draw::fill_round(scene, ctgl, f64::from(theme.tokens.radii.sm), c.lift(0.72));
            }
            let (gw, gh) = draw::measure(
                tr,
                "»",
                theme.tokens.typography.scale.base,
                TextWeight::Bold,
            );
            draw::text(
                scene,
                tr,
                "»",
                theme.tokens.typography.scale.base,
                c.text_dim,
                Point::new(
                    ctgl.center().x - f64::from(gw) / 2.0,
                    ctgl.center().y - f64::from(gh) / 2.0,
                ),
                TextWeight::Bold,
            );
            self.push_hit(ctgl, Hit::ToggleSidebar);
        }
    }

    /// The section list (Containers / Images / Volumes / Distrobox).
    fn draw_rail_sections(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        view: &ViewState,
        expanded: bool,
        compact: bool,
    ) {
        let c = &theme.tokens.colors;
        let items = [
            (Section::Containers, Icon::Box, self.engine.containers.len()),
            (Section::Images, Icon::Layers, self.engine.images.len()),
            (Section::Volumes, Icon::Box, self.engine.volumes.len()),
            (
                Section::Distrobox,
                Icon::Terminal,
                self.engine.distroboxes.len(),
            ),
        ];
        let mut y = rail.y0 + if compact { 12.0 } else { 56.0 };
        for (section, icon, count) in items {
            let item = Rect::new(rail.x0 + 8.0, y, rail.x1 - 8.0, y + 40.0);
            let active = view.section == section;
            if active {
                draw::fill_round(
                    scene,
                    item,
                    f64::from(theme.tokens.radii.md),
                    draw::with_alpha(c.accent[5], 46),
                );
            } else if self.hovered(item) {
                draw::fill_round(scene, item, f64::from(theme.tokens.radii.md), c.lift(0.35));
            }
            let fg = if active { c.accent[6] } else { c.text_dim };
            draw::icon(
                scene,
                icon,
                Rect::new(
                    item.x0 + 10.0,
                    item.center().y - 9.0,
                    item.x0 + 28.0,
                    item.center().y + 9.0,
                ),
                fg,
                1.6,
            );
            if expanded {
                let weight = if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Medium
                };
                let label_y = draw::centered_top(
                    tr,
                    section.label(),
                    theme.tokens.typography.scale.base,
                    weight,
                    item.y0,
                    item.height(),
                );
                draw::text(
                    scene,
                    tr,
                    section.label(),
                    theme.tokens.typography.scale.base,
                    if active { c.text_primary } else { c.text_dim },
                    Point::new(item.x0 + 38.0, label_y),
                    weight,
                );
                // count chip on the right, its label centred on the chip
                let count_str = count.to_string();
                let (cw, ch) = draw::measure(
                    tr,
                    &count_str,
                    theme.tokens.typography.scale.xs,
                    TextWeight::Medium,
                );
                let chip_h = (f64::from(ch) + 4.0).max(16.0);
                let chip = Rect::new(
                    item.x1 - 14.0 - f64::from(cw),
                    item.center().y - chip_h / 2.0,
                    item.x1 - 6.0,
                    item.center().y + chip_h / 2.0,
                );
                draw::fill_round(scene, chip, chip_h / 2.0, c.lift(0.72));
                draw::text(
                    scene,
                    tr,
                    &count_str,
                    theme.tokens.typography.scale.xs,
                    c.text_dim,
                    Point::new(chip.x0 + 4.0, chip.center().y - f64::from(ch) / 2.0),
                    TextWeight::Medium,
                );
            }
            self.push_hit(item, Hit::Section(section));
            // Rail items are related rows: an `s2` gap between the 40 px
            // items (HIG §7b), not an off-grid 6 px.
            y += 40.0 + f64::from(theme.tokens.spacing.s2);
        }
    }

    /// Engine status footer (text only when expanded; the live dot always).
    fn draw_rail_footer(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        rail: Rect,
        expanded: bool,
    ) {
        let c = &theme.tokens.colors;
        let foot_y = rail.y1 - 76.0;
        let dot_color = if !self.live {
            c.accent[5]
        } else if self.engine.live {
            c.semantic.success
        } else {
            c.semantic.warning
        };
        let dot = vello::kurbo::Circle::new(Point::new(rail.x0 + PAD + 4.0, foot_y + 4.0), 4.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, dot_color, None, &dot);
        if expanded {
            draw::separator(
                scene,
                rail.x0 + PAD,
                rail.x1 - PAD,
                foot_y - 12.0,
                c.surfaces.raised,
            );
            draw::text(
                scene,
                tr,
                if !self.live {
                    "Demo snapshot"
                } else if self.engine.live {
                    "Connected"
                } else {
                    "Offline"
                },
                theme.tokens.typography.scale.sm,
                c.text_primary,
                Point::new(rail.x0 + PAD + 16.0, foot_y - 3.0),
                TextWeight::Medium,
            );
            draw::text_clip(
                scene,
                tr,
                &self.engine.connection,
                &draw::TextStyle {
                    size: theme.tokens.typography.scale.xs,
                    color: c.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(rail.x0 + PAD, foot_y + 18.0),
                rail.width() - PAD * 2.0,
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
    }

    // =================================================================
    // Shared header (title + search + primary action)
    // =================================================================

    /// Draws the header's right-aligned primary action and returns the x the
    /// title must stop before.
    fn draw_header_action(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        header: Rect,
        (label, hit): (&str, Hit),
    ) -> f64 {
        let c = &theme.tokens.colors;
        let compact = self.uses_compact_layout(theme);
        let right = header.x1 - PAD;
        let shown = if compact { "New" } else { label };
        let (lw, lh) = draw::measure(
            tr,
            shown,
            theme.tokens.typography.scale.base,
            TextWeight::Semibold,
        );
        // icon slot (12 + 16 + 6) + label + trailing padding
        let bw = 34.0 + f64::from(lw) + 16.0;
        let btn = Rect::new(
            right - bw,
            if compact {
                header.y0 + 10.0
            } else {
                header.center().y - 17.0
            },
            right,
            if compact {
                header.y0 + 46.0
            } else {
                header.center().y + 17.0
            },
        );
        let hov = self.hovered(btn);
        draw::fill_round(
            scene,
            btn,
            f64::from(theme.tokens.radii.md),
            if hov { c.accent[4] } else { c.accent[5] },
        );
        draw::icon(
            scene,
            Icon::Plus,
            Rect::new(
                btn.x0 + 12.0,
                btn.center().y - 8.0,
                btn.x0 + 28.0,
                btn.center().y + 8.0,
            ),
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            1.8,
        );
        draw::text(
            scene,
            tr,
            shown,
            theme.tokens.typography.scale.base,
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            Point::new(btn.x0 + 34.0, btn.center().y - f64::from(lh) / 2.0),
            TextWeight::Semibold,
        );
        self.push_hit(btn, hit);
        btn.x0 - 12.0
    }

    /// Draws the content header and returns the y of the content body start.
    fn draw_header(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        title: &str,
        action_label: Option<(&str, Hit)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let compact = self.uses_compact_layout(theme);
        let header = Rect::new(
            content.x0,
            content.y0,
            content.x1,
            content.y0 + self.header_height(theme),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            c.surfaces.base,
            None,
            &header,
        );
        let title_x = header.x0 + PAD + 8.0;
        let title_size = if compact {
            // At 200% text a compact title remains readable without consuming
            // the primary-action slot. The exact clipping bound below prevents
            // it from ever painting underneath that action.
            theme.tokens.typography.scale.base
        } else {
            theme.tokens.typography.scale.x2l
        };

        // Primary action button (right-aligned).
        let right = action_label.map_or(header.x1 - PAD, |action| {
            self.draw_header_action(scene, tr, theme, header, action)
        });

        // The title centres on its measured line box (not `centre − 14`,
        // which only centred a 1× x2l line) when it shares the header with
        // the search launcher and action; the compact header stacks it above.
        let (_, title_h) = draw::measure(tr, title, title_size, TextWeight::Bold);
        draw::text_clip(
            scene,
            tr,
            title,
            &draw::TextStyle {
                size: title_size,
                color: c.text_primary,
                weight: TextWeight::Bold,
            },
            Point::new(
                title_x,
                if compact {
                    header.y0 + 16.0
                } else {
                    header.center().y - f64::from(title_h) / 2.0
                },
            ),
            (right - title_x - if compact { 8.0 } else { 220.0 }).max(40.0),
        );

        // Search launcher — a thin affordance that opens the shared Ctrl+K
        // palette scoped to this app, instead of an inline filter. The search
        // UI and ranking live in one shared toolkit widget, not here.
        let search = if compact {
            Rect::new(
                header.x0 + PAD,
                header.y0 + 66.0,
                header.x1 - PAD,
                header.y0 + 106.0,
            )
        } else {
            let search_w = (right - title_x - 220.0).clamp(180.0, 360.0);
            Rect::new(
                right - search_w,
                header.center().y - 19.0,
                right,
                header.center().y + 19.0,
            )
        };
        self.fit_launcher_placeholder(tr, theme, search.width());
        self.draw_search_launcher(scene, tr, theme, search);

        // bottom divider
        draw::divider(
            scene,
            header.x0,
            header.x1,
            header.y1 - 0.5,
            c.border_standard,
        );
        header.y1
    }

    /// Pick the launcher placeholder that fits beside the ⌘K key cap.
    ///
    /// App-side fallback for `ui::SearchField`, which pads its text region by
    /// a fixed clear-button gutter and only measures the key cap while
    /// painting, so at 768 px "Search containers…" ran under "⌘K". Until the
    /// field reserves the cap itself (F2, composition Task 18), the app
    /// measures the placeholder against the room the field leaves and falls
    /// back to the short form.
    ///
    /// The geometry mirrors the widget's private constants and must track
    /// them: a 34 px leading icon gutter (`ICON_GUTTER`), the cap label at
    /// 11 px (`KBD_LABEL_SIZE`) with 7 px of padding either side
    /// (`KBD_PAD_X`) and a 10 px trailing inset (`KBD_INSET`); the inner
    /// `TextInput` paints the placeholder at its own 16 px (`FONT_SIZE`). Both
    /// sizes are renderer-relative, so they are measured with the legacy API
    /// exactly as the widget measures them, not as theme tokens.
    pub(crate) fn fit_launcher_placeholder(
        &mut self,
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
    ) {
        let cap_label = ui::Shortcut::command("K").label(ui::PlatformConventions::default());
        let (cap_w, _) = tr.measure(&cap_label, 11.0, None);
        let gap = f64::from(theme.tokens.spacing.s2);
        let reserved = 34.0 + f64::from(cap_w) + 2.0 * 7.0 + 10.0 + gap;
        let (full_w, _) = tr.measure(LAUNCHER_PLACEHOLDER, 16.0, None);
        let placeholder = if f64::from(full_w) <= width - reserved {
            LAUNCHER_PLACEHOLDER
        } else {
            LAUNCHER_PLACEHOLDER_SHORT
        };
        self.search_field.set_placeholder(placeholder);
    }

    /// Lay out and paint the header search launcher (a `ui::SearchField` in
    /// palette-opening mode) into `rect`. The widget self-lays-out from a
    /// throwaway taffy sized to `rect`; its click behaviour is routed in
    /// [`ContainersApp::route_to_launcher`], and activating it opens the
    /// shared palette via [`ContainersApp::open_container_palette`].
    fn draw_search_launcher(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        use std::collections::HashMap;
        use ui::widget::Widget;
        let mut taffy: taffy::TaffyTree<()> = taffy::TaffyTree::new();
        let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
        let node = self
            .search_field
            .build_layout(&mut taffy, theme, &mut node_map);
        let _ = taffy.compute_layout_with_measure(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(rect.width() as f32),
                height: taffy::AvailableSpace::Definite(rect.height() as f32),
            },
            |known, available, id, _, _| {
                node_map.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, tr)
                })
            },
        );
        self.search_field
            .apply_layout(&taffy, node, Point::new(rect.x0, rect.y0));
        self.search_field.draw(
            scene,
            draw::base(),
            tr,
            theme,
            &ui::presence::PresenceCtx::default(),
        );
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
        // Compact surfaces use a conventional drill-in: select a row to open
        // a full-width detail page, then use its Back control or Escape to
        // return. Keeping a 240px list beside a 420px detail pane created an
        // unusable sliver on phones and at 200% text.
        let detail_open = view.selected.is_some();
        let single_pane_detail = self.uses_single_pane_detail(theme);
        // A selected compact detail owns the page header, including its
        // explicit Back affordance. Keeping the list's title/search/new row
        // above it wasted vertical space and made the hierarchy ambiguous.
        let body_y = if single_pane_detail {
            content.y0
        } else {
            let primary = self
                .engine_controls_available()
                .then_some(("New container", Hit::NewContainer));
            self.draw_header(scene, tr, theme, content, "Containers", primary)
        };
        let list_w = if detail_open && !single_pane_detail {
            (content.width() - crate::app::DETAIL_WIDTH).max(240.0)
        } else {
            content.width()
        };
        let list = Rect::new(content.x0, body_y, content.x0 + list_w, content.y1);

        if !single_pane_detail {
            let list_compact = detail_open || self.uses_compact_layout(theme);
            let cols = Self::container_columns(list, list_compact);
            let colhead_bottom =
                Self::draw_container_list_header(scene, tr, theme, list, list_compact, &cols);
            let rows_area = Rect::new(list.x0, colhead_bottom, list.x1, list.y1);
            self.draw_container_rows(scene, tr, theme, rows_area, list_compact, view);
        }
        let detail = if single_pane_detail {
            Rect::new(content.x0, body_y, content.x1, content.y1)
        } else {
            Rect::new(list.x1, body_y, content.x1, content.y1)
        };
        self.draw_container_detail_pane(scene, tr, theme, detail, view);
    }

    /// The list column header (labels + divider) above the scrollable rows.
    /// Returns the y just below the divider, where the rows area starts.
    fn draw_container_list_header(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
        detail_open: bool,
        cols: &[f64; 5],
    ) -> f64 {
        let colhead = Rect::new(list.x0, list.y0, list.x1, list.y0 + 28.0);
        let c = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            c.surfaces.base,
            None,
            &colhead,
        );
        // The compact (detail-open) layout drops the image + stats columns,
        // so its header must match — otherwise the labels collide.
        let labels: [&str; 5] = if detail_open {
            ["NAME / STATUS", "", "", "", ""]
        } else {
            ["NAME", "IMAGE", "STATUS", "CPU / MEM", ""]
        };
        for (i, lbl) in labels.iter().enumerate() {
            if lbl.is_empty() {
                continue;
            }
            let Some(&x) = cols.get(i) else {
                continue;
            };
            // The first column's rows paint their name `ROW_TEXT_INSET` in
            // from the column edge (after the status bar); the heading sits
            // on the same x so it labels what is under it.
            let x = if i == 0 { x + ROW_TEXT_INSET } else { x };
            let y = draw::centered_top(
                tr,
                lbl,
                theme.tokens.typography.scale.xs,
                TextWeight::Semibold,
                colhead.y0,
                colhead.height(),
            );
            draw::text(
                scene,
                tr,
                lbl,
                theme.tokens.typography.scale.xs,
                c.text_dim,
                Point::new(x, y),
                TextWeight::Semibold,
            );
        }
        draw::divider(
            scene,
            list.x0 + PAD,
            list.x1 - PAD,
            colhead.y1,
            c.border_standard,
        );
        colhead.y1
    }

    /// The scrollable rows of the containers matching the search, laid out
    /// in `rows_area`'s columns, plus the empty-state fallbacks and the list
    /// scrollbar.
    fn draw_container_rows(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        rows_area: Rect,
        compact: bool,
        view: &ViewState,
    ) {
        let rows: Vec<Container> = self.visible_containers().into_iter().cloned().collect();
        let cols = Self::container_columns(rows_area, compact);
        let row_height = Self::row_height(theme);
        let content_h = rows.len() as f64 * row_height;
        let max_scroll = (content_h - rows_area.height()).max(0.0);
        self.clamp_list_scroll(max_scroll);
        let mut y = rows_area.y0 - self.list_scroll;
        for container in &rows {
            let row = Rect::new(rows_area.x0, y, rows_area.x1, y + row_height);
            if row.y1 >= rows_area.y0 && row.y0 <= rows_area.y1 {
                let info = ContainerRow {
                    row,
                    container,
                    cols: &cols,
                    compact,
                };
                self.draw_container_row(scene, tr, theme, info, view);
            }
            y += row_height;
        }
        if rows.is_empty() {
            if self.engine.containers.is_empty() {
                if self.engine.live {
                    Self::draw_empty_state(
                        scene,
                        tr,
                        theme,
                        rows_area,
                        "No containers yet",
                        "Pull an image and create a container to get started.",
                    );
                } else {
                    Self::draw_empty_state(
                        scene,
                        tr,
                        theme,
                        rows_area,
                        "No container engine",
                        "Start rootless podman to manage containers, images and volumes here.",
                    );
                }
            } else {
                Self::draw_empty(
                    scene,
                    tr,
                    theme,
                    rows_area,
                    "No containers match your search.",
                );
            }
        }
        Self::draw_scrollbar(scene, theme, rows_area, self.list_scroll, content_h);
    }

    /// The selected container's detail pane in `detail`, if any (or the "no
    /// longer exists" fallback when the selection went stale).
    fn draw_container_detail_pane(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        detail: Rect,
        view: &ViewState,
    ) {
        if let Some(sel) = &view.selected {
            if let Some(container) = self.engine.find(sel).cloned() {
                self.draw_detail(scene, tr, theme, detail, &container, view);
            } else {
                // selected id no longer exists; close it implicitly
                let panel = detail;
                let c = &theme.tokens.colors;
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    c.surfaces.raised,
                    None,
                    &panel,
                );
                Self::draw_empty(scene, tr, theme, panel, "Container no longer exists.");
            }
        }
    }

    fn container_columns(list: Rect, detail_open: bool) -> [f64; 5] {
        let x = list.x0 + PAD;
        let w = list.width() - PAD * 2.0;
        if detail_open {
            // Narrow layout: name, status, actions.
            [x, x + w * 0.42, x + w * 0.62, x + w * 0.62, list.x1 - PAD]
        } else {
            [x, x + w * 0.28, x + w * 0.54, x + w * 0.68, list.x1 - PAD]
        }
    }

    fn draw_container_row(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        info: ContainerRow<'_>,
        view: &ViewState,
    ) {
        let c = &theme.tokens.colors;
        let ContainerRow { row, container, .. } = info;
        let selected = view.selected.as_deref() == Some(container.id.as_str());
        let inner = Rect::new(row.x0 + 8.0, row.y0 + 4.0, row.x1 - 8.0, row.y1 - 4.0);
        if selected {
            draw::fill_round(
                scene,
                inner,
                f64::from(theme.tokens.radii.md),
                draw::with_alpha(c.accent[5], 40),
            );
            draw::stroke_round(
                scene,
                inner,
                f64::from(theme.tokens.radii.md),
                1.0,
                draw::with_alpha(c.accent[5], 110),
            );
        } else if self.hovered(row) {
            draw::fill_round(scene, inner, f64::from(theme.tokens.radii.md), c.lift(0.35));
        }

        // status accent bar
        let accent = draw::status_color(theme, container.status);
        let bar = Rect::new(inner.x0, inner.y0 + 6.0, inner.x0 + 3.0, inner.y1 - 6.0);
        draw::fill_round(scene, bar, 1.5, accent);

        let name_y = row.center().y - 16.0;
        let sub_y = row.center().y + 4.0;
        // A narrow list (detail open beside it) shows the status as a pill
        // next to the name only when that pill ends before the lifecycle
        // buttons start; otherwise the status stacks under the name exactly
        // as it does on a compact surface. One measured decision drives both
        // the text and the pill so nothing paints over the actions.
        let actions_x0 = row.x1 - PAD - Self::row_actions_width(container);
        let stacked = self.uses_compact_layout(theme)
            || (info.compact
                && !Self::status_pill_fits(tr, theme, info.cols[1], actions_x0, container));
        if stacked {
            // In the one-column list, status belongs beneath the name. This
            // gives a large-text row a stable reading order and reserves the
            // trailing edge exclusively for lifecycle buttons.
            let compact_name_y = row.y0 + 14.0;
            let compact_sub_y = row.y0
                + if Self::row_height(theme) > crate::app::ROW_HEIGHT {
                    50.0
                } else {
                    36.0
                };
            let text_width = (actions_x0 - 12.0 - (info.cols[0] + ROW_TEXT_INSET)).max(64.0);
            draw::text_clip(
                scene,
                tr,
                &container.name,
                &draw::TextStyle {
                    size: theme.tokens.typography.scale.base,
                    color: c.text_primary,
                    weight: TextWeight::Semibold,
                },
                Point::new(info.cols[0] + ROW_TEXT_INSET, compact_name_y),
                text_width,
            );
            draw::text_clip(
                scene,
                tr,
                container.status.label(),
                &draw::TextStyle {
                    size: theme.tokens.typography.scale.xs,
                    color: draw::status_color(theme, container.status),
                    weight: TextWeight::Medium,
                },
                Point::new(info.cols[0] + ROW_TEXT_INSET, compact_sub_y),
                text_width,
            );
        } else {
            // Name + short id
            draw::text_clip(
                scene,
                tr,
                &container.name,
                &draw::TextStyle {
                    size: theme.tokens.typography.scale.base,
                    color: c.text_primary,
                    weight: TextWeight::Semibold,
                },
                Point::new(info.cols[0] + ROW_TEXT_INSET, name_y),
                (info.cols[1] - info.cols[0] - 16.0).max(40.0),
            );
            draw::text(
                scene,
                tr,
                container.short_id(),
                theme.tokens.typography.scale.xs,
                c.text_dim,
                Point::new(info.cols[0] + ROW_TEXT_INSET, sub_y),
                TextWeight::Regular,
            );

            Self::draw_container_row_layout(scene, tr, theme, info, name_y, sub_y);
        }

        // Action buttons cluster on the far right.
        self.draw_row_actions(scene, tr, theme, container, row);

        // Whole-row selection hit (lowest priority — pushed before actions so
        // action hits, pushed earlier, win at resolve time which scans last).
        self.push_hit(row, Hit::SelectContainer(container.id.clone()));
    }

    /// The compact (detail-open) vs. full row content: a bare status pill,
    /// or the image / status-detail text plus the CPU/MEM mini-stats.
    fn draw_container_row_layout(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        info: ContainerRow<'_>,
        name_y: f64,
        sub_y: f64,
    ) {
        let c = &theme.tokens.colors;
        let ContainerRow {
            row,
            container,
            cols,
            compact,
        } = info;
        if compact {
            // status pill near the right, then actions
            let pill_x = cols[1];
            draw::status_pill(
                scene,
                tr,
                theme,
                container.status,
                container.status.label(),
                Point::new(pill_x, row.center().y - 10.0),
                20.0,
            );
        } else {
            // Image
            draw::text_clip(
                scene,
                tr,
                &container.image,
                &draw::TextStyle {
                    size: theme.tokens.typography.scale.sm,
                    color: c.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(cols[1], row.center().y - 6.0),
                (cols[2] - cols[1] - 12.0).max(40.0),
            );
            // Status pill + detail
            draw::status_pill(
                scene,
                tr,
                theme,
                container.status,
                container.status.label(),
                Point::new(cols[2], name_y),
                20.0,
            );
            draw::text_clip(
                scene,
                tr,
                &container.status_detail,
                &draw::TextStyle {
                    size: theme.tokens.typography.scale.xs,
                    color: c.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(cols[2], sub_y + 2.0),
                (cols[3] - cols[2] - 12.0).max(30.0),
            );
            // CPU / MEM mini-stats, bounded by where the action cluster starts.
            let actions_x0 = row.x1 - PAD - Self::row_actions_width(container);
            Self::draw_row_stats(
                scene,
                tr,
                theme,
                container,
                Point::new(cols[3], row.y0 + 12.0),
                actions_x0 - 12.0 - cols[3],
            );
        }
    }

    /// Width the lifecycle button cluster paints for `container` (see
    /// [`Self::draw_row_actions`]): `n` buttons with 4px gaps.
    fn row_actions_width(container: &Container) -> f64 {
        let n = if container.status.is_running() {
            3.0
        } else {
            2.0
        };
        n * ICON_BTN + (n - 1.0) * 4.0
    }

    /// Whether the status pill placed at `pill_x` ends before the action
    /// cluster begins at `actions_x0`, with a gap either side.
    fn status_pill_fits(
        tr: &mut TextRenderer,
        theme: &Theme,
        pill_x: f64,
        actions_x0: f64,
        container: &Container,
    ) -> bool {
        let pill_w = draw::status_pill_width(tr, theme, container.status.label());
        pill_x + pill_w + 12.0 <= actions_x0
    }

    fn draw_row_stats(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        container: &Container,
        at: Point,
        width: f64,
    ) {
        let c = &theme.tokens.colors;
        // No room for a readable gauge: draw nothing rather than paint over
        // the action buttons to the right.
        if width < 60.0 {
            return;
        }
        let w = width.min(160.0);
        if !container.status.is_running() {
            draw::text(
                scene,
                tr,
                "—",
                theme.tokens.typography.scale.sm,
                c.text_dim,
                Point::new(at.x, at.y + 12.0),
                TextWeight::Regular,
            );
            return;
        }
        let s = &container.stats;
        // CPU
        draw::text(
            scene,
            tr,
            &format!("{:.0}% CPU", s.cpu_percent * 100.0),
            theme.tokens.typography.scale.xs,
            c.text_dim,
            at,
            TextWeight::Regular,
        );
        let cpu_bar = Rect::new(at.x, at.y + 14.0, at.x + w, at.y + 19.0);
        draw::usage_bar(scene, theme, cpu_bar, (s.cpu_percent).min(1.0), c.accent[5]);
        // MEM
        let mem_label = format!("{} MEM", ui::format::bytes(s.mem_bytes));
        draw::text(
            scene,
            tr,
            &mem_label,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(at.x, at.y + 24.0),
            TextWeight::Regular,
        );
        let mem_bar = Rect::new(at.x, at.y + 38.0, at.x + w, at.y + 43.0);
        let mem_color = if s.mem_fraction() > 0.85 {
            c.semantic.danger
        } else {
            c.semantic.info
        };
        draw::usage_bar(scene, theme, mem_bar, s.mem_fraction(), mem_color);
    }

    fn draw_row_actions(
        &mut self,
        scene: &mut Scene,
        // Icons here are vector-only (no text), but kept for signature
        // consistency with the sibling `draw_row_*` methods.
        _tr: &TextRenderer,
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
        let mut x = row.x1 - PAD;
        for (icon, action, color) in actions.into_iter().rev() {
            x -= ICON_BTN;
            let btn = Rect::new(
                x,
                row.center().y - ICON_BTN / 2.0,
                x + ICON_BTN,
                row.center().y + ICON_BTN / 2.0,
            );
            let hov = self.hovered(btn);
            if hov {
                draw::fill_round(
                    scene,
                    btn,
                    f64::from(theme.tokens.radii.md),
                    draw::with_alpha(color, 46),
                );
            }
            draw::icon(
                scene,
                icon,
                Rect::new(btn.x0 + 7.0, btn.y0 + 7.0, btn.x1 - 7.0, btn.y1 - 7.0),
                if hov {
                    color
                } else {
                    draw::with_alpha(color, 200)
                },
                1.6,
            );
            self.push_hit(
                btn,
                Hit::Action {
                    id: container.id.clone(),
                    action,
                },
            );
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
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            c.surfaces.raised,
            None,
            &panel,
        );
        let border = Rect::new(panel.x0, panel.y0, panel.x0 + 1.0, panel.y1);
        scene.fill(
            Fill::NonZero,
            draw::base(),
            draw::edge(c.surfaces.raised),
            None,
            &border,
        );

        let head = self.draw_detail_header(scene, tr, theme, panel, container);
        let strip = self.draw_detail_tab_strip(scene, tr, theme, panel, head.y1, view);

        // Action toolbar.
        let toolbar = Rect::new(panel.x0, strip.y1, panel.x1, strip.y1 + 48.0);
        self.draw_detail_toolbar(scene, tr, theme, toolbar, container);
        draw::separator(scene, toolbar.x0, toolbar.x1, toolbar.y1, c.surfaces.raised);

        // Tab body.
        let body = Rect::new(panel.x0, toolbar.y1, panel.x1, panel.y1);
        match view.tab {
            DetailTab::Logs => self.draw_logs(scene, tr, theme, body, container),
            DetailTab::Stats => Self::draw_stats(scene, tr, theme, body, container),
            DetailTab::Inspect => self.draw_inspect(scene, tr, theme, body, container),
            DetailTab::Terminal => self.draw_terminal(scene, tr, theme, body, container),
        }
    }

    /// The detail panel header: name + image, and the close button. Returns
    /// the header's rect (its `y1` is where the tab strip starts).
    fn draw_detail_header(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        container: &Container,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let compact = self.uses_compact_layout(theme);
        let head = Rect::new(
            panel.x0,
            panel.y0,
            panel.x1,
            panel.y0 + if compact { 76.0 } else { 56.0 },
        );
        let close_label = if compact { "‹ Back" } else { "✕" };
        let close_w = if compact { 88.0 } else { 24.0 };
        draw::text_clip(
            scene,
            tr,
            &container.name,
            &draw::TextStyle {
                size: if compact {
                    theme.tokens.typography.scale.base
                } else {
                    theme.tokens.typography.scale.xl
                },
                color: c.text_primary,
                weight: TextWeight::Bold,
            },
            Point::new(head.x0 + PAD, head.y0 + 12.0),
            head.width() - PAD * 2.0 - close_w - 12.0,
        );
        draw::text_clip(
            scene,
            tr,
            &container.image,
            &draw::TextStyle {
                size: theme.tokens.typography.scale.xs,
                color: c.text_dim,
                weight: TextWeight::Regular,
            },
            Point::new(
                head.x0 + PAD,
                if compact {
                    head.y0 + 48.0
                } else {
                    head.y0 + 34.0
                },
            ),
            head.width() - PAD * 2.0 - close_w - 12.0,
        );
        let close = Rect::new(
            head.x1 - close_w - 12.0,
            head.y0 + if compact { 10.0 } else { 12.0 },
            head.x1 - 12.0,
            head.y0 + if compact { 42.0 } else { 36.0 },
        );
        if self.hovered(close) {
            draw::fill_round(scene, close, f64::from(theme.tokens.radii.md), c.lift(0.72));
        }
        draw::text(
            scene,
            tr,
            close_label,
            if compact {
                theme.tokens.typography.scale.sm
            } else {
                theme.tokens.typography.scale.lg
            },
            c.text_dim,
            Point::new(close.x0 + 6.0, close.y0 + 2.0),
            TextWeight::Regular,
        );
        self.push_hit(close, Hit::CloseDetail);
        head
    }

    /// The detail panel's tab strip (Logs/Stats/Inspect/Terminal). Returns
    /// its rect (its `y1` is where the action toolbar starts).
    fn draw_detail_tab_strip(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        head_bottom: f64,
        view: &ViewState,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let compact = self.uses_compact_layout(theme);
        let strip = Rect::new(
            panel.x0,
            head_bottom,
            panel.x1,
            head_bottom + if compact { 40.0 } else { 36.0 },
        );
        let tab_w = strip.width() / DetailTab::ALL.len() as f64;
        for (i, tab) in DetailTab::ALL.iter().enumerate() {
            let tx0 = strip.x0 + i as f64 * tab_w;
            let trect = Rect::new(tx0, strip.y0, tx0 + tab_w, strip.y1);
            let active = view.tab == *tab;
            let fg = if active { c.text_primary } else { c.text_dim };
            let label = if compact {
                match tab {
                    DetailTab::Logs => "Logs",
                    DetailTab::Stats => "Stats",
                    DetailTab::Inspect => "Info",
                    DetailTab::Terminal => "Shell",
                }
            } else {
                tab.label()
            };
            let label_size = if compact {
                theme.tokens.typography.scale.xs
            } else {
                theme.tokens.typography.scale.sm
            };
            let (lw, lh) = draw::measure(tr, label, label_size, TextWeight::Semibold);
            draw::text(
                scene,
                tr,
                label,
                label_size,
                fg,
                Point::new(
                    trect.center().x - f64::from(lw) / 2.0,
                    trect.center().y - f64::from(lh) / 2.0,
                ),
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Medium
                },
            );
            if active {
                let underline = Rect::new(
                    trect.center().x - f64::from(lw) / 2.0 - 4.0,
                    trect.y1 - 2.5,
                    trect.center().x + f64::from(lw) / 2.0 + 4.0,
                    trect.y1,
                );
                draw::fill_round(scene, underline, 1.0, c.accent[5]);
            } else if self.hovered(trect) {
                let underline = Rect::new(trect.x0 + 8.0, trect.y1 - 2.0, trect.x1 - 8.0, trect.y1);
                draw::fill_round(scene, underline, 1.0, c.lift(0.72));
            }
            self.push_hit(trect, Hit::DetailTab(*tab));
        }
        draw::separator(scene, strip.x0, strip.x1, strip.y1, c.surfaces.raised);
        strip
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
        let compact = self.uses_compact_layout(theme);
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
            let bw = if compact {
                // Keep all lifecycle actions reachable at large text without
                // letting their labels collide. Their colour + icon language
                // is identical to the row actions.
                40.0
            } else {
                let (lw, _) = draw::measure(
                    tr,
                    label,
                    theme.tokens.typography.scale.sm,
                    TextWeight::Semibold,
                );
                f64::from(lw) + 38.0
            };
            let btn = Rect::new(
                x,
                toolbar.center().y - 15.0,
                x + bw,
                toolbar.center().y + 15.0,
            );
            let hov = self.hovered(btn);
            draw::fill_round(
                scene,
                btn,
                f64::from(theme.tokens.radii.md),
                if hov {
                    draw::with_alpha(color, 60)
                } else {
                    c.lift(0.35)
                },
            );
            draw::stroke_round(
                scene,
                btn,
                f64::from(theme.tokens.radii.md),
                1.0,
                draw::with_alpha(color, 120),
            );
            draw::icon(
                scene,
                icon,
                Rect::new(
                    btn.x0 + 9.0,
                    btn.center().y - 7.0,
                    btn.x0 + 23.0,
                    btn.center().y + 7.0,
                ),
                color,
                1.5,
            );
            if !compact {
                let label_y = draw::centered_top(
                    tr,
                    label,
                    theme.tokens.typography.scale.sm,
                    TextWeight::Semibold,
                    btn.y0,
                    btn.height(),
                );
                draw::text(
                    scene,
                    tr,
                    label,
                    theme.tokens.typography.scale.sm,
                    c.text_primary,
                    Point::new(btn.x0 + 28.0, label_y),
                    TextWeight::Semibold,
                );
            }
            self.push_hit(
                btn,
                Hit::Action {
                    id: container.id.clone(),
                    action,
                },
            );
            x += bw + 8.0;
        }
    }

    fn draw_logs(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        container: &Container,
    ) {
        let c = &theme.tokens.colors;
        // The log console sits one surface step below the raised detail pane
        // (base inside raised) and closes with its derived edge.
        let term_bg = c.surfaces.base;
        let inner = Rect::new(body.x0 + 8.0, body.y0 + 8.0, body.x1 - 8.0, body.y1 - 8.0);
        draw::card(scene, inner, f64::from(theme.tokens.radii.md), term_bg);
        if container.logs.is_empty() {
            Self::draw_empty(scene, tr, theme, inner, "No log output yet.");
            return;
        }
        let mono = theme.tokens.typography.scale.xs;
        let line_h = f64::from(mono * 1.5);
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
                &draw::TextStyle {
                    size: mono,
                    color,
                    weight: TextWeight::Regular,
                },
                Point::new(inner.x0 + 12.0, y),
                inner.width() - 24.0,
            );
            y += line_h;
        }
        Self::draw_scrollbar(scene, theme, inner, self.detail_scroll, content_h);
        // tail indicator
        let tail = format!("{total} lines");
        draw::text(
            scene,
            tr,
            &tail,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(inner.x1 - 70.0, inner.y1 - 16.0),
            TextWeight::Regular,
        );
    }

    fn draw_stats(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        container: &Container,
    ) {
        let c = &theme.tokens.colors;
        let s = &container.stats;
        let mut y = body.y0 + 16.0;
        let card_w = body.width() - PAD * 2.0;

        // Big CPU + MEM gauges.
        let cpu_card = Rect::new(body.x0 + PAD, y, body.x0 + PAD + card_w, y + 72.0);
        Self::draw_stat_gauge(
            scene,
            tr,
            theme,
            cpu_card,
            &GaugeCard {
                label: "CPU",
                value: format!("{:.1}%", s.cpu_percent * 100.0),
                frac: s.cpu_percent.min(1.0),
                color: c.accent[5],
            },
        );
        y += 84.0;
        let mem_label = if s.mem_limit_bytes > 0 {
            format!(
                "{} / {}",
                ui::format::bytes(s.mem_bytes),
                ui::format::bytes(s.mem_limit_bytes)
            )
        } else {
            ui::format::bytes(s.mem_bytes)
        };
        let mem_card = Rect::new(body.x0 + PAD, y, body.x0 + PAD + card_w, y + 72.0);
        Self::draw_stat_gauge(
            scene,
            tr,
            theme,
            mem_card,
            &GaugeCard {
                label: "MEMORY",
                value: mem_label,
                frac: s.mem_fraction(),
                color: c.semantic.info,
            },
        );
        y += 84.0;

        // Network + block IO + pids as a small grid.
        Self::draw_stat_grid(scene, tr, theme, body, card_w, y, s);
    }

    /// One big CPU/MEM gauge card: label, value, and a usage bar.
    fn draw_stat_gauge(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        spec: &GaugeCard,
    ) {
        let c = &theme.tokens.colors;
        draw::card(
            scene,
            card,
            f64::from(theme.tokens.radii.md),
            c.surfaces.base,
        );
        draw::text(
            scene,
            tr,
            spec.label,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(card.x0 + 14.0, card.y0 + 12.0),
            TextWeight::Semibold,
        );
        draw::text(
            scene,
            tr,
            &spec.value,
            theme.tokens.typography.scale.xl,
            c.text_primary,
            Point::new(card.x0 + 14.0, card.y0 + 26.0),
            TextWeight::Bold,
        );
        let bar = Rect::new(
            card.x0 + 14.0,
            card.y1 - 16.0,
            card.x1 - 14.0,
            card.y1 - 10.0,
        );
        draw::usage_bar(scene, theme, bar, spec.frac, spec.color);
    }

    /// Network + block IO + pids as a small key/value grid card.
    fn draw_stat_grid(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        card_w: f64,
        y: f64,
        s: &Stats,
    ) {
        let c = &theme.tokens.colors;
        let rows = [
            ("Network RX", ui::format::bytes(s.net_rx_bytes)),
            ("Network TX", ui::format::bytes(s.net_tx_bytes)),
            ("Block read", ui::format::bytes(s.block_read_bytes)),
            ("Block write", ui::format::bytes(s.block_write_bytes)),
            ("Processes", s.pids.to_string()),
        ];
        let card = Rect::new(
            body.x0 + PAD,
            y,
            body.x0 + PAD + card_w,
            y + rows.len() as f64 * 30.0 + 12.0,
        );
        draw::card(
            scene,
            card,
            f64::from(theme.tokens.radii.md),
            c.surfaces.base,
        );
        let mut ry = card.y0 + 8.0;
        for (i, (k, v)) in rows.iter().enumerate() {
            draw::text(
                scene,
                tr,
                k,
                theme.tokens.typography.scale.sm,
                c.text_dim,
                Point::new(card.x0 + 14.0, ry + 6.0),
                TextWeight::Regular,
            );
            let (vw, _) = draw::measure(
                tr,
                v,
                theme.tokens.typography.scale.sm,
                TextWeight::Semibold,
            );
            draw::text(
                scene,
                tr,
                v,
                theme.tokens.typography.scale.sm,
                c.text_primary,
                Point::new(card.x1 - 14.0 - f64::from(vw), ry + 6.0),
                TextWeight::Semibold,
            );
            if i + 1 < rows.len() {
                draw::separator(
                    scene,
                    card.x0 + 14.0,
                    card.x1 - 14.0,
                    ry + 28.0,
                    c.surfaces.base,
                );
            }
            ry += 30.0;
        }
    }

    fn draw_inspect(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        container: &Container,
    ) {
        let c = &theme.tokens.colors;
        // Build a flat list of inspect fields so it scrolls as one column.
        let mut lines: Vec<(String, String, bool)> = vec![
            ("Id".into(), container.id.clone(), true),
            ("Command".into(), container.command.clone(), false),
            ("Created".into(), container.created.clone(), false),
            ("Status".into(), container.status_detail.clone(), false),
        ];
        // section: ports
        if !container.ports.is_empty() {
            lines.push(("— Ports —".into(), String::new(), false));
            for p in &container.ports {
                lines.push((
                    format!("{}/{}", p.container_port, p.protocol),
                    format!("host :{}", p.host_port),
                    true,
                ));
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
                    draw::text(
                        scene,
                        tr,
                        k,
                        theme.tokens.typography.scale.xs,
                        c.accent[6],
                        Point::new(area.x0 + PAD, y + 6.0),
                        TextWeight::Semibold,
                    );
                } else {
                    let key_w = (area.width() * 0.4).min(150.0);
                    draw::text_clip(
                        scene,
                        tr,
                        k,
                        &draw::TextStyle {
                            size: theme.tokens.typography.scale.sm,
                            color: c.text_dim,
                            weight: TextWeight::Regular,
                        },
                        Point::new(area.x0 + PAD, y + 6.0),
                        key_w,
                    );
                    let vx = area.x0 + PAD + key_w + 8.0;
                    let weight = if *mono {
                        TextWeight::Medium
                    } else {
                        TextWeight::Regular
                    };
                    draw::text_clip(
                        scene,
                        tr,
                        v,
                        &draw::TextStyle {
                            size: theme.tokens.typography.scale.sm,
                            color: c.text_primary,
                            weight,
                        },
                        Point::new(vx, y + 6.0),
                        area.x1 - vx - PAD,
                    );
                }
            }
            y += line_h;
        }
        Self::draw_scrollbar(scene, theme, area, self.detail_scroll, content_h);
    }

    /// The "Attach shell" button in the terminal card's bottom-right corner.
    fn draw_attach_button(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        inner: Rect,
        container: &Container,
    ) {
        let c = &theme.tokens.colors;
        let bw = 150.0;
        let btn = Rect::new(
            inner.x1 - 14.0 - bw,
            inner.y1 - 44.0,
            inner.x1 - 14.0,
            inner.y1 - 12.0,
        );
        let hov = self.hovered(btn);
        draw::fill_round(
            scene,
            btn,
            f64::from(theme.tokens.radii.md),
            if hov { c.accent[4] } else { c.accent[5] },
        );
        draw::icon(
            scene,
            Icon::Terminal,
            Rect::new(
                btn.x0 + 10.0,
                btn.center().y - 8.0,
                btn.x0 + 26.0,
                btn.center().y + 8.0,
            ),
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            1.5,
        );
        draw::text(
            scene,
            tr,
            "Attach shell",
            theme.tokens.typography.scale.sm,
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            Point::new(btn.x0 + 32.0, btn.center().y - 7.0),
            TextWeight::Semibold,
        );
        self.push_hit(btn, Hit::AttachShell(container.id.clone()));
    }

    fn draw_terminal(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        container: &Container,
    ) {
        let c = &theme.tokens.colors;
        let inner = Rect::new(body.x0 + 8.0, body.y0 + 8.0, body.x1 - 8.0, body.y1 - 8.0);
        draw::card(
            scene,
            inner,
            f64::from(theme.tokens.radii.md),
            c.surfaces.base,
        );
        let mono = theme.tokens.typography.scale.sm;
        let mut y = inner.y0 + 14.0;
        let prompt_lines = [
            (
                format!("$ podman exec -it {} /bin/sh", container.short_id()),
                c.semantic.success,
            ),
            ("/ # whoami".to_string(), c.text_primary),
            ("root".to_string(), c.text_dim),
            ("/ # ps -o pid,comm".to_string(), c.text_primary),
            (
                format!(
                    "  1 {}",
                    container.command.split_whitespace().next().unwrap_or("sh")
                ),
                c.text_dim,
            ),
            ("/ # ".to_string(), c.text_primary),
        ];
        for (line, color) in prompt_lines {
            draw::text(
                scene,
                tr,
                &line,
                mono,
                color,
                Point::new(inner.x0 + 14.0, y),
                TextWeight::Regular,
            );
            y += f64::from(mono * 1.6);
        }
        // blinking caret
        let caret = Rect::new(
            inner.x0 + 14.0 + 30.0,
            y - f64::from(mono * 1.6),
            inner.x0 + 14.0 + 38.0,
            y - f64::from(mono * 1.6) + f64::from(mono),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            draw::with_alpha(c.semantic.success, 180),
            None,
            &caret,
        );
        // Attach button (real exec via the Tilo terminal) for running containers.
        if container.status.is_running() {
            self.draw_attach_button(scene, tr, theme, inner, container);
        }
        // hint
        let hint = if container.status.is_running() {
            "Attach opens the Tilo terminal running podman exec -it in this container."
        } else {
            "Start the container to attach an interactive shell."
        };
        draw::text(
            scene,
            tr,
            hint,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(inner.x0 + 14.0, inner.y1 - 64.0),
            TextWeight::Regular,
        );
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
    ) {
        let primary = self
            .engine_controls_available()
            .then_some(("Pull image", Hit::PullTyped));
        let body_y = self.draw_header(scene, tr, theme, content, "Images", primary);
        let list = Rect::new(content.x0, body_y, content.x1, content.y1);
        let banner = self.draw_images_banner(scene, tr, theme, list);

        let images: Vec<usize> = self
            .engine
            .images
            .iter()
            .enumerate()
            .map(|(i, _)| i)
            .collect();

        let rows_area = Rect::new(list.x0, banner.y1 + 8.0, list.x1, list.y1);
        self.draw_image_rows(scene, tr, theme, rows_area, &images);
    }

    /// The summary banner above the image list (count + total disk usage).
    fn draw_images_banner(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let banner = Rect::new(list.x0 + PAD, list.y0 + 8.0, list.x1 - PAD, list.y0 + 44.0);
        draw::card(
            scene,
            banner,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
        );
        let disk = format!(
            "{} images · {} on disk",
            self.engine.images.len(),
            ui::format::bytes(self.engine.image_disk_bytes())
        );
        draw::text(
            scene,
            tr,
            &disk,
            theme.tokens.typography.scale.sm,
            c.text_dim,
            Point::new(banner.x0 + 14.0, banner.center().y - 7.0),
            TextWeight::Medium,
        );
        banner
    }

    /// The scrollable image rows (by index into `self.engine.images`), plus
    /// the empty-state fallback and the list scrollbar.
    fn draw_image_rows(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        rows_area: Rect,
        images: &[usize],
    ) {
        let row_h = 56.0;
        let content_h = images.len() as f64 * row_h;
        let max_scroll = (content_h - rows_area.height()).max(0.0);
        self.clamp_list_scroll(max_scroll);
        let mut y = rows_area.y0 - self.list_scroll;
        for &idx in images {
            let Some(image) = self.engine.images.get(idx).cloned() else {
                continue;
            };
            let row = Rect::new(rows_area.x0, y, rows_area.x1, y + row_h);
            // Whole-row select FIRST (lowest priority — action buttons pushed
            // later win at resolve time, which returns the last matching hit).
            self.push_hit(row, Hit::SelectImage(idx));
            if row.y1 >= rows_area.y0 && row.y0 <= rows_area.y1 {
                self.draw_image_row(scene, tr, theme, row, &image);
            }
            y += row_h;
        }
        if images.is_empty() {
            Self::draw_empty(scene, tr, theme, rows_area, "No images match your search.");
        }
        Self::draw_scrollbar(scene, theme, rows_area, self.list_scroll, content_h);
    }

    /// One image row's content: icon, reference + short id, action cluster,
    /// size/created metadata, and the in-use/unused chip.
    fn draw_image_row(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        image: &Image,
    ) {
        let c = &theme.tokens.colors;
        let inner = Rect::new(row.x0 + PAD, row.y0 + 4.0, row.x1 - PAD, row.y1 - 4.0);
        if self.hovered(row) {
            draw::fill_round(scene, inner, f64::from(theme.tokens.radii.md), c.lift(0.35));
        }
        draw::icon(
            scene,
            Icon::Layers,
            Rect::new(
                inner.x0 + 8.0,
                inner.center().y - 12.0,
                inner.x0 + 32.0,
                inner.center().y + 12.0,
            ),
            c.accent[6],
            1.5,
        );
        draw::text_clip(
            scene,
            tr,
            &image.reference(),
            &draw::TextStyle {
                size: theme.tokens.typography.scale.base,
                color: c.text_primary,
                weight: TextWeight::Semibold,
            },
            Point::new(inner.x0 + 44.0, row.center().y - 16.0),
            inner.width() * 0.42,
        );
        draw::text(
            scene,
            tr,
            image.short_id(),
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(inner.x0 + 44.0, row.center().y + 4.0),
            TextWeight::Regular,
        );
        // Action cluster on the far right (pull again / tag / inspect / remove).
        let acts_right = self.draw_image_actions(scene, theme, &image.reference(), row);
        Self::draw_image_row_meta(scene, tr, theme, row, inner, acts_right, image);
        draw::separator(scene, inner.x0, inner.x1, row.y1, c.surfaces.base);
    }

    /// The right-hand metadata cluster on an image row: size, created,
    /// in-use/unused chip — left of the action buttons.
    fn draw_image_row_meta(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        inner: Rect,
        acts_right: f64,
        image: &Image,
    ) {
        let c = &theme.tokens.colors;
        let meta_right = acts_right - 12.0;
        let size = ui::format::bytes(image.size_bytes);
        let (sw, _) = draw::measure(
            tr,
            &size,
            theme.tokens.typography.scale.base,
            TextWeight::Semibold,
        );
        draw::text(
            scene,
            tr,
            &size,
            theme.tokens.typography.scale.base,
            c.text_primary,
            Point::new(meta_right - f64::from(sw), row.center().y - 16.0),
            TextWeight::Semibold,
        );
        draw::text(
            scene,
            tr,
            &image.created,
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(meta_right - 100.0, row.center().y + 4.0),
            TextWeight::Regular,
        );
        if image.in_use_by > 0 {
            draw::chip(
                scene,
                tr,
                theme,
                &format!("in use ×{}", image.in_use_by),
                Point::new(inner.x0 + inner.width() * 0.42, row.center().y - 8.0),
                16.0,
                c.semantic.success,
            );
        } else {
            draw::chip(
                scene,
                tr,
                theme,
                "unused",
                Point::new(inner.x0 + inner.width() * 0.42, row.center().y - 8.0),
                16.0,
                c.text_dim,
            );
        }
    }

    /// Draw the per-image action cluster (pull-again / tag / inspect /
    /// remove) on the far right of an image row. Returns the left x of the
    /// cluster so the metadata can sit beside it.
    fn draw_image_actions(
        &mut self,
        scene: &mut Scene,
        theme: &Theme,
        reference: &str,
        row: Rect,
    ) -> f64 {
        let c = &theme.tokens.colors;
        // ordered left→right: pull, tag, inspect, remove
        let actions: [(Icon, Hit, Color); 4] = [
            (
                Icon::Restart,
                Hit::PullImage(reference.to_string()),
                c.semantic.info,
            ),
            (
                Icon::Plus,
                Hit::TagImage(reference.to_string()),
                c.accent[5],
            ),
            (
                Icon::Search,
                Hit::InspectImage(reference.to_string()),
                c.text_dim,
            ),
            (
                Icon::Trash,
                Hit::ImageAct {
                    reference: reference.to_string(),
                    action: ImageAction::Remove,
                },
                c.semantic.danger,
            ),
        ];
        let mut x = row.x1 - 12.0;
        for (icon, hit, color) in actions.into_iter().rev() {
            x -= ICON_BTN;
            let btn = Rect::new(
                x,
                row.center().y - ICON_BTN / 2.0,
                x + ICON_BTN,
                row.center().y + ICON_BTN / 2.0,
            );
            let hov = self.hovered(btn);
            if hov {
                draw::fill_round(
                    scene,
                    btn,
                    f64::from(theme.tokens.radii.md),
                    draw::with_alpha(color, 46),
                );
            }
            draw::icon(
                scene,
                icon,
                Rect::new(btn.x0 + 7.0, btn.y0 + 7.0, btn.x1 - 7.0, btn.y1 - 7.0),
                if hov {
                    color
                } else {
                    draw::with_alpha(color, 200)
                },
                1.6,
            );
            self.push_hit(btn, hit);
            x -= 4.0;
        }
        x
    }

    // =================================================================
    // Volumes section
    // =================================================================

    pub(crate) fn draw_volumes(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        let primary = self
            .engine_controls_available()
            .then_some(("New volume", Hit::CreateVolume));
        let body_y = self.draw_header(scene, tr, theme, content, "Volumes", primary);
        let list = Rect::new(content.x0, body_y, content.x1, content.y1);
        let banner = self.draw_volumes_banner(scene, tr, theme, list);

        let vols: Vec<Volume> = self.engine.volumes.clone();

        let rows_area = Rect::new(list.x0, banner.y1 + 8.0, list.x1, list.y1);
        self.draw_volume_rows(scene, tr, theme, rows_area, &vols);
    }

    /// The summary banner above the volume list (count + in-use count).
    fn draw_volumes_banner(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        list: Rect,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let banner = Rect::new(list.x0 + PAD, list.y0 + 8.0, list.x1 - PAD, list.y0 + 44.0);
        draw::card(
            scene,
            banner,
            f64::from(theme.tokens.radii.md),
            c.surfaces.raised,
        );
        let in_use = self
            .engine
            .volumes
            .iter()
            .filter(|v| v.in_use_by > 0)
            .count();
        let summary = format!("{} volumes · {} in use", self.engine.volumes.len(), in_use);
        draw::text(
            scene,
            tr,
            &summary,
            theme.tokens.typography.scale.sm,
            c.text_dim,
            Point::new(banner.x0 + 14.0, banner.center().y - 7.0),
            TextWeight::Medium,
        );
        banner
    }

    /// The scrollable volume rows, plus the empty-state fallback and the
    /// list scrollbar.
    fn draw_volume_rows(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        rows_area: Rect,
        vols: &[Volume],
    ) {
        let row_h = 56.0;
        let content_h = vols.len() as f64 * row_h;
        let max_scroll = (content_h - rows_area.height()).max(0.0);
        self.clamp_list_scroll(max_scroll);
        let mut y = rows_area.y0 - self.list_scroll;
        for v in vols {
            let row = Rect::new(rows_area.x0, y, rows_area.x1, y + row_h);
            if row.y1 >= rows_area.y0 && row.y0 <= rows_area.y1 {
                self.draw_volume_row(scene, tr, theme, row, v);
            }
            y += row_h;
        }
        if vols.is_empty() {
            Self::draw_empty(
                scene,
                tr,
                theme,
                rows_area,
                "No volumes yet — create one to persist container data.",
            );
        }
        Self::draw_scrollbar(scene, theme, rows_area, self.list_scroll, content_h);
    }

    /// One volume row's content: icon, name + mountpoint, usage chip, and
    /// the remove button.
    fn draw_volume_row(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        v: &Volume,
    ) {
        let c = &theme.tokens.colors;
        let inner = Rect::new(row.x0 + PAD, row.y0 + 4.0, row.x1 - PAD, row.y1 - 4.0);
        if self.hovered(row) {
            draw::fill_round(scene, inner, f64::from(theme.tokens.radii.md), c.lift(0.35));
        }
        draw::icon(
            scene,
            Icon::Box,
            Rect::new(
                inner.x0 + 8.0,
                inner.center().y - 12.0,
                inner.x0 + 32.0,
                inner.center().y + 12.0,
            ),
            c.accent[6],
            1.5,
        );
        draw::text_clip(
            scene,
            tr,
            &v.name,
            &draw::TextStyle {
                size: theme.tokens.typography.scale.base,
                color: c.text_primary,
                weight: TextWeight::Semibold,
            },
            Point::new(inner.x0 + 44.0, row.center().y - 16.0),
            inner.width() * 0.5,
        );
        draw::text_clip(
            scene,
            tr,
            &v.mountpoint,
            &draw::TextStyle {
                size: theme.tokens.typography.scale.xs,
                color: c.text_dim,
                weight: TextWeight::Regular,
            },
            Point::new(inner.x0 + 44.0, row.center().y + 4.0),
            inner.width() * 0.55,
        );
        // driver + usage chips
        if v.in_use_by > 0 {
            draw::chip(
                scene,
                tr,
                theme,
                &format!("in use ×{}", v.in_use_by),
                Point::new(inner.x0 + inner.width() * 0.62, row.center().y - 8.0),
                16.0,
                c.semantic.success,
            );
        } else {
            draw::chip(
                scene,
                tr,
                theme,
                "unused",
                Point::new(inner.x0 + inner.width() * 0.62, row.center().y - 8.0),
                16.0,
                c.text_dim,
            );
        }
        self.draw_volume_row_remove_button(scene, theme, row, inner, v);
        draw::separator(scene, inner.x0, inner.x1, row.y1, c.surfaces.base);
    }

    /// The remove button on the far right of a volume row.
    fn draw_volume_row_remove_button(
        &mut self,
        scene: &mut Scene,
        theme: &Theme,
        row: Rect,
        inner: Rect,
        v: &Volume,
    ) {
        let c = &theme.tokens.colors;
        let bw = ICON_BTN;
        let btn = Rect::new(
            inner.x1 - bw,
            row.center().y - bw / 2.0,
            inner.x1,
            row.center().y + bw / 2.0,
        );
        let hov = self.hovered(btn);
        if hov {
            draw::fill_round(
                scene,
                btn,
                f64::from(theme.tokens.radii.md),
                draw::with_alpha(c.semantic.danger, 46),
            );
        }
        draw::icon(
            scene,
            Icon::Trash,
            Rect::new(btn.x0 + 7.0, btn.y0 + 7.0, btn.x1 - 7.0, btn.y1 - 7.0),
            if hov {
                c.semantic.danger
            } else {
                draw::with_alpha(c.semantic.danger, 200)
            },
            1.6,
        );
        self.push_hit(btn, Hit::RemoveVolume(v.name.clone()));
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
    ) {
        let primary = self
            .engine_controls_available()
            .then_some(("Create box", Hit::DistroCreate));
        let body_y = self.draw_header(scene, tr, theme, content, "Distrobox", primary);
        let c = &theme.tokens.colors;
        let list = Rect::new(content.x0, body_y, content.x1, content.y1);

        let intro = "Distroboxes are tightly host-integrated containers — your shell, home, and devices are shared. Enter one to get a mutable userspace on the immutable OS.";
        draw::text_clip(
            scene,
            tr,
            intro,
            &draw::TextStyle {
                size: theme.tokens.typography.scale.sm,
                color: c.text_dim,
                weight: TextWeight::Regular,
            },
            Point::new(list.x0 + PAD, list.y0 + 12.0),
            list.width() - PAD * 2.0,
        );

        let boxes: Vec<DistroBox> = self.engine.distroboxes.clone();

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
            self.draw_distrobox_card(scene, tr, theme, card, card_w, b);
        }
        if boxes.is_empty() {
            let area = Rect::new(list.x0, grid_top, list.x1, list.y1);
            Self::draw_empty(
                scene,
                tr,
                theme,
                area,
                "No distroboxes yet — create one to get started.",
            );
        }
    }

    /// One distrobox card: icon, name + status pill, image, and the Enter
    /// button.
    fn draw_distrobox_card(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        card_w: f64,
        b: &DistroBox,
    ) {
        let c = &theme.tokens.colors;
        let hov = self.hovered(card);
        draw::card(
            scene,
            card,
            f64::from(theme.tokens.radii.lg),
            c.surfaces.raised,
        );
        if hov {
            draw::stroke_round(
                scene,
                card,
                f64::from(theme.tokens.radii.lg),
                1.0,
                c.border_focus,
            );
        }
        draw::icon(
            scene,
            Icon::Terminal,
            Rect::new(
                card.x0 + 14.0,
                card.y0 + 14.0,
                card.x0 + 38.0,
                card.y0 + 38.0,
            ),
            c.accent[6],
            1.6,
        );
        draw::text_clip(
            scene,
            tr,
            &b.name,
            &draw::TextStyle {
                size: theme.tokens.typography.scale.lg,
                color: c.text_primary,
                weight: TextWeight::Bold,
            },
            Point::new(card.x0 + 48.0, card.y0 + 14.0),
            card_w - 60.0,
        );
        draw::status_pill(
            scene,
            tr,
            theme,
            b.status,
            b.status.label(),
            Point::new(card.x0 + 48.0, card.y0 + 38.0),
            20.0,
        );
        draw::text_clip(
            scene,
            tr,
            &b.image,
            &draw::TextStyle {
                size: theme.tokens.typography.scale.xs,
                color: c.text_dim,
                weight: TextWeight::Regular,
            },
            Point::new(card.x0 + 14.0, card.y1 - 24.0),
            card_w - 120.0,
        );
        self.draw_distrobox_enter_button(scene, tr, theme, card, b);
    }

    /// The "Enter" button on a distrobox card.
    fn draw_distrobox_enter_button(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        b: &DistroBox,
    ) {
        let c = &theme.tokens.colors;
        let bw = 86.0;
        let btn = Rect::new(
            card.x1 - 14.0 - bw,
            card.y1 - 36.0,
            card.x1 - 14.0,
            card.y1 - 8.0,
        );
        let bhov = self.hovered(btn);
        draw::fill_round(
            scene,
            btn,
            f64::from(theme.tokens.radii.md),
            if bhov { c.accent[4] } else { c.accent[5] },
        );
        draw::icon(
            scene,
            Icon::Enter,
            Rect::new(
                btn.x0 + 10.0,
                btn.center().y - 8.0,
                btn.x0 + 26.0,
                btn.center().y + 8.0,
            ),
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            1.6,
        );
        draw::text(
            scene,
            tr,
            "Enter",
            theme.tokens.typography.scale.sm,
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            Point::new(btn.x0 + 30.0, btn.center().y - 7.0),
            TextWeight::Semibold,
        );
        self.push_hit(btn, Hit::DistroEnter(b.name.clone()));
    }

    // =================================================================
    // Modals + overlays
    // =================================================================

    pub(crate) fn draw_create_modal(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        full: Rect,
    ) {
        let c = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            c.surfaces.overlay,
            None,
            &full,
        );
        let modal = create_form_geometry(full).modal;
        // A floating panel: the shell's Panel ground and its derived border.
        draw::fill_round(
            scene,
            modal,
            f64::from(theme.tokens.radii.lg),
            ui::chrome::panel_ground(theme),
        );
        draw::stroke_round(
            scene,
            modal.inset(-0.5),
            f64::from(theme.tokens.radii.lg) - 0.5,
            1.0,
            ui::chrome::panel_border(theme),
        );

        draw::text(
            scene,
            tr,
            "New container",
            theme.tokens.typography.scale.xl,
            c.text_primary,
            Point::new(modal.x0 + 24.0, modal.y0 + 18.0),
            TextWeight::Bold,
        );
        draw::text(
            scene,
            tr,
            "podman run, rootless",
            theme.tokens.typography.scale.xs,
            c.text_dim,
            Point::new(modal.x0 + 24.0, modal.y0 + 42.0),
            TextWeight::Regular,
        );

        let combo_input = self.draw_create_modal_fields(scene, tr, theme, presence, modal);
        self.draw_create_modal_buttons(scene, tr, theme, modal);

        // Image suggestion dropdown — drawn LAST so it overlays the rows below.
        if self.combo.open
            && let Some(input) = combo_input
        {
            let typed = self.create_input(CreateField::Image).value().to_string();
            self.draw_image_dropdown(scene, tr, theme, input, &typed);
        }
    }

    /// The create-form field rows (label, input box, value/placeholder,
    /// caret, and the image combobox chevron). Returns the image field's
    /// input rect, if any, so the caller can anchor the suggestion dropdown.
    fn draw_create_modal_fields(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        modal: Rect,
    ) -> Option<Rect> {
        // Deferred so the caller can overlay the combobox dropdown on top of
        // the fields below it.
        let mut combo_input: Option<Rect> = None;
        for geometry in create_form_fields(modal) {
            let field = geometry.field;
            let input = geometry.input;
            draw_field_frame(scene, tr, theme, geometry, self.create_focus == field);
            let is_image = field == CreateField::Image;
            let text_right = if is_image {
                input.x1 - 38.0
            } else {
                input.x1 - 12.0
            };
            let editor = self.create_input_mut(field);
            editor.apply_layout_external(Rect::new(
                input.x0 + 12.0,
                input.y0,
                text_right,
                input.y1,
            ));
            Widget::draw(editor, scene, Affine::IDENTITY, tr, theme, presence);
            self.push_hit(input, Hit::CreateField(field));
            if is_image {
                // Dropdown chevron toggles the suggestion list.
                self.draw_create_field_combo_toggle(scene, tr, theme, input);
                combo_input = Some(input);
            }
        }
        combo_input
    }

    /// The image field's dropdown-toggle chevron.
    fn draw_create_field_combo_toggle(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        input: Rect,
    ) {
        let c = &theme.tokens.colors;
        let chev = Rect::new(input.x1 - 28.0, input.y0, input.x1, input.y1);
        let glyph = if self.combo.open { "▴" } else { "▾" };
        draw::text(
            scene,
            tr,
            glyph,
            theme.tokens.typography.scale.sm,
            c.text_dim,
            Point::new(chev.center().x - 5.0, chev.center().y - 8.0),
            TextWeight::Bold,
        );
        self.push_hit(chev, Hit::ComboToggle);
    }

    /// The Cancel / Create buttons at the bottom of the create modal.
    fn draw_create_modal_buttons(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        modal: Rect,
    ) {
        let cancel = Rect::new(
            modal.x1 - 24.0 - 200.0,
            modal.y1 - 52.0,
            modal.x1 - 24.0 - 108.0,
            modal.y1 - 16.0,
        );
        let create = Rect::new(
            modal.x1 - 24.0 - 96.0,
            modal.y1 - 52.0,
            modal.x1 - 24.0,
            modal.y1 - 16.0,
        );
        Button::paint(
            scene,
            draw::base(),
            tr,
            theme,
            &ButtonFace::new(cancel, "Cancel")
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(theme.tokens.typography.scale.sm)
                .weight(TextWeight::Medium)
                .hovered(self.hovered(cancel)),
        );
        Button::paint(
            scene,
            draw::base(),
            tr,
            theme,
            &ButtonFace::new(create, "Create")
                .primary()
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(theme.tokens.typography.scale.sm)
                .weight(TextWeight::Semibold)
                .hovered(self.hovered(create)),
        );
        self.push_hit(cancel, Hit::CreateCancel);
        self.push_hit(create, Hit::CreateSubmit);
    }

    /// The combobox suggestion dropdown: locally-installed image references
    /// filtered by the typed query, each pickable to fill the Image field.
    fn draw_image_dropdown(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        input: Rect,
        typed: &str,
    ) {
        let c = &theme.tokens.colors;
        let options = self.engine.image_references();
        let filtered = combobox::filter_options(&options, typed);
        let row_h = 30.0;
        let max_rows = 6usize;
        let shown = filtered.len().min(max_rows);
        let panel_h = if shown == 0 {
            row_h
        } else {
            shown as f64 * row_h + 8.0
        };
        let panel = Rect::new(input.x0, input.y1 + 4.0, input.x1, input.y1 + 4.0 + panel_h);
        // An opaque popover surface so the suggestions sit above the fields.
        draw::fill_round(
            scene,
            panel,
            f64::from(theme.tokens.radii.md),
            ui::chrome::panel_ground(theme),
        );
        draw::stroke_round(
            scene,
            panel.inset(-0.5),
            f64::from(theme.tokens.radii.md) - 0.5,
            1.0,
            ui::chrome::panel_border(theme),
        );

        if filtered.is_empty() {
            draw::text(
                scene,
                tr,
                "Type a custom image reference, then Pull or Create",
                theme.tokens.typography.scale.xs,
                c.text_dim,
                Point::new(panel.x0 + 12.0, panel.center().y - 7.0),
                TextWeight::Regular,
            );
            return;
        }
        let mut y = panel.y0 + 4.0;
        for (pos, &opt_idx) in filtered.iter().take(max_rows).enumerate() {
            let row = Rect::new(panel.x0 + 4.0, y, panel.x1 - 4.0, y + row_h);
            let hl = self.combo.highlight == pos;
            if hl || self.hovered(row) {
                draw::fill_round(
                    scene,
                    row,
                    f64::from(theme.tokens.radii.sm),
                    draw::with_alpha(c.accent[5], if hl { 60 } else { 36 }),
                );
            }
            draw::icon(
                scene,
                Icon::Layers,
                Rect::new(
                    row.x0 + 6.0,
                    row.center().y - 8.0,
                    row.x0 + 22.0,
                    row.center().y + 8.0,
                ),
                c.accent[6],
                1.4,
            );
            if let Some(reference) = options.get(opt_idx) {
                draw::text_clip(
                    scene,
                    tr,
                    reference,
                    &draw::TextStyle {
                        size: theme.tokens.typography.scale.sm,
                        color: c.text_primary,
                        weight: TextWeight::Regular,
                    },
                    Point::new(row.x0 + 28.0, row.center().y - 7.0),
                    row.width() - 36.0,
                );
            }
            self.push_hit(row, Hit::ComboPick(pos));
            y += row_h;
        }
    }

    /// A live `podman pull` progress overlay — a console of streamed lines
    /// with a header and a Close button that appears once the pull settles.
    pub(crate) fn draw_pull_overlay(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        full: Rect,
    ) {
        let Some(pull) = &self.pull else { return };
        let c = &theme.tokens.colors;
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            c.surfaces.overlay,
            None,
            &full,
        );
        let mw = 560.0_f64.min(full.width() - 48.0);
        let mh = 360.0_f64.min(full.height() - 48.0);
        let modal = Rect::from_center_size(full.center(), (mw, mh));
        // A floating panel: the shell's Panel ground and its derived border.
        draw::fill_round(
            scene,
            modal,
            f64::from(theme.tokens.radii.lg),
            ui::chrome::panel_ground(theme),
        );
        draw::stroke_round(
            scene,
            modal.inset(-0.5),
            f64::from(theme.tokens.radii.lg) - 0.5,
            1.0,
            ui::chrome::panel_border(theme),
        );

        Self::draw_pull_overlay_header(scene, tr, theme, modal, pull);
        Self::draw_pull_overlay_console(scene, tr, theme, modal, pull);
        // Computed before the close-button call, which needs `&mut self` —
        // `pull` (borrowed from `self.pull`) must not still be live then.
        let done_or_failed = pull.done || pull.failed.is_some();
        self.draw_pull_overlay_close_button(scene, tr, theme, modal, done_or_failed);
    }

    /// The pull overlay's header: spinner-ish dot + reference + state text.
    fn draw_pull_overlay_header(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        modal: Rect,
        pull: &PullProgress,
    ) {
        let c = &theme.tokens.colors;
        let state_color = if pull.failed.is_some() {
            c.semantic.danger
        } else if pull.done {
            c.semantic.success
        } else {
            c.semantic.info
        };
        let dot = vello::kurbo::Circle::new(Point::new(modal.x0 + 28.0, modal.y0 + 28.0), 5.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, state_color, None, &dot);
        draw::text(
            scene,
            tr,
            &format!("Pulling {}", pull.reference),
            theme.tokens.typography.scale.lg,
            c.text_primary,
            Point::new(modal.x0 + 44.0, modal.y0 + 18.0),
            TextWeight::Bold,
        );
        let status = if let Some(e) = &pull.failed {
            format!("failed: {e}")
        } else if pull.done {
            "complete".into()
        } else {
            "in progress…".into()
        };
        draw::text(
            scene,
            tr,
            &status,
            theme.tokens.typography.scale.xs,
            state_color,
            Point::new(modal.x0 + 44.0, modal.y0 + 40.0),
            TextWeight::Medium,
        );
    }

    /// The pull overlay's console body (tail-bounded streamed lines).
    fn draw_pull_overlay_console(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        modal: Rect,
        pull: &PullProgress,
    ) {
        let c = &theme.tokens.colors;
        let body = Rect::new(
            modal.x0 + 16.0,
            modal.y0 + 60.0,
            modal.x1 - 16.0,
            modal.y1 - 60.0,
        );
        draw::card(
            scene,
            body,
            f64::from(theme.tokens.radii.md),
            c.surfaces.base,
        );
        let mono = theme.tokens.typography.scale.xs;
        let line_h = f64::from(mono * 1.5);
        let visible = ((body.height() - 16.0) / line_h).floor() as usize;
        let start = pull.lines.len().saturating_sub(visible);
        let mut y = body.y0 + 10.0;
        for line in pull.lines.iter().skip(start) {
            draw::text_clip(
                scene,
                tr,
                line,
                &draw::TextStyle {
                    size: mono,
                    color: c.text_dim,
                    weight: TextWeight::Regular,
                },
                Point::new(body.x0 + 12.0, y),
                body.width() - 24.0,
            );
            y += line_h;
        }
    }

    /// The close/hide button (always available; cancels by dismissing the
    /// overlay). `done_or_failed` selects the "Close" vs. "Hide" label.
    fn draw_pull_overlay_close_button(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        modal: Rect,
        done_or_failed: bool,
    ) {
        let c = &theme.tokens.colors;
        let close = Rect::new(
            modal.x1 - 24.0 - 92.0,
            modal.y1 - 48.0,
            modal.x1 - 24.0,
            modal.y1 - 16.0,
        );
        draw::fill_round(
            scene,
            close,
            f64::from(theme.tokens.radii.md),
            if self.hovered(close) {
                c.accent[4]
            } else {
                c.lift(0.72)
            },
        );
        let label = if done_or_failed { "Close" } else { "Hide" };
        draw::text(
            scene,
            tr,
            label,
            theme.tokens.typography.scale.sm,
            c.text_primary,
            Point::new(close.center().x - 16.0, close.center().y - 7.0),
            TextWeight::Semibold,
        );
        self.push_hit(close, Hit::PromptCancel); // reuse the dismiss path
    }

    // =================================================================
    // Shared helpers
    // =================================================================

    fn draw_empty(scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, area: Rect, msg: &str) {
        let c = &theme.tokens.colors;
        let (w, h) = draw::measure(
            tr,
            msg,
            theme.tokens.typography.scale.base,
            TextWeight::Regular,
        );
        let p = Point::new(
            area.center().x - f64::from(w) / 2.0,
            area.center().y - f64::from(h) / 2.0,
        );
        draw::text(
            scene,
            tr,
            msg,
            theme.tokens.typography.scale.base,
            c.text_dim,
            p,
            TextWeight::Regular,
        );
        let _ = scene;
    }

    /// A composed zero-state: a semibold title over a dim explanatory line,
    /// centred in `area`. Used for the section-level empty states (no engine,
    /// no containers) where a single grey sentence would read as unfinished.
    fn draw_empty_state(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        title: &str,
        subtitle: &str,
    ) {
        let c = &theme.tokens.colors;
        let scale = &theme.tokens.typography.scale;
        let cx = area.center().x;
        let cy = area.center().y;
        let (tw, _) = draw::measure(tr, title, scale.lg, TextWeight::Semibold);
        draw::text(
            scene,
            tr,
            title,
            scale.lg,
            c.text_primary,
            Point::new(cx - f64::from(tw) / 2.0, cy - 22.0),
            TextWeight::Semibold,
        );
        let (sw, _) = draw::measure(tr, subtitle, scale.sm, TextWeight::Regular);
        draw::text(
            scene,
            tr,
            subtitle,
            scale.sm,
            c.text_dim,
            Point::new(cx - f64::from(sw) / 2.0, cy + 4.0),
            TextWeight::Regular,
        );
    }

    fn draw_scrollbar(scene: &mut Scene, theme: &Theme, area: Rect, scroll: f64, content_h: f64) {
        if content_h <= area.height() {
            return;
        }
        let c = &theme.tokens.colors;
        let track_h = area.height();
        let thumb_h = (track_h * (track_h / content_h)).max(28.0);
        let max_scroll = content_h - track_h;
        let t = if max_scroll > 0.0 {
            (scroll / max_scroll).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let thumb_y = area.y0 + t * (track_h - thumb_h);
        let thumb = Rect::new(area.x1 - 7.0, thumb_y, area.x1 - 3.0, thumb_y + thumb_h);
        draw::fill_round(scene, thumb, 2.0, c.lift(1.0));
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
        // Modal layers consume clicks even outside their controls. (The confirm
        // and prompt dialogs are modal and captured upstream in `handle_event`,
        // so they are never active here.)
        let modal_active = self.pull.is_some() || self.create_open;
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
                self.dismiss_top_modal();
            }
            *handled = true;
            self.refresh_view(doc);
            return;
        };

        *handled = true;
        self.apply_hit(doc, hit);
        self.refresh_view(doc);
    }

    /// Dismiss the top-most open overlay when a click lands on the scrim
    /// behind it: pull progress, then the combobox dropdown, then the
    /// create modal (in that priority order).
    fn dismiss_top_modal(&mut self) {
        if self.pull.take().is_some() {
        } else if self.combo.open {
            // First click outside the dropdown only closes it.
            self.combo.close();
        } else {
            self.create_open = false;
        }
    }

    /// Apply the resolved click hit against the engine/document.
    pub(crate) fn apply_hit(&mut self, doc: &mut AutoCommit, hit: Hit) {
        match hit {
            Hit::Section(s) => self.show_section(s),
            Hit::ToggleSidebar => {
                self.toggle_sidebar();
                self.structure_dirty = true;
            }
            Hit::NewContainer => self.open_create(),
            Hit::SelectContainer(id) => {
                let already = self.view.selected.as_deref() == Some(id.as_str());
                self.select_container((!already).then_some(id));
            }
            Hit::SelectImage(idx) => self.apply_hit_select_image(idx),
            Hit::Action { id, action } => self.apply_hit_action(id, action),
            Hit::ImageAct { reference, action } => self.apply_hit_image_act(reference, action),
            Hit::PullImage(reference) => self.start_pull(&reference),
            Hit::PullTyped => self.apply_hit_pull_typed(doc),
            Hit::TagImage(reference) => self.apply_hit_tag_image(reference),
            Hit::InspectImage(reference) => self.apply_hit_inspect_image(&reference),
            Hit::CreateVolume => {
                self.prompt = Some(crate::app::Prompt {
                    kind: PromptKind::NewVolume,
                    title: "New volume".into(),
                    placeholder: "my-volume".into(),
                    value: String::new(),
                    context: String::new(),
                });
            }
            Hit::RemoveVolume(name) => {
                self.confirm = Some(crate::app::Confirm {
                    name: name.clone(),
                    verb: "Remove volume".into(),
                    target: ConfirmTarget::Volume { name },
                });
            }
            Hit::AttachShell(id) => {
                self.attach_shell(&id);
            }
            Hit::DistroEnter(name) => {
                self.enter_distrobox(&name);
            }
            Hit::DistroCreate => {
                self.create_open = true;
                self.focus_create_field(CreateField::Name);
            }
            Hit::DetailTab(tab) => self.show_tab(tab),
            Hit::CloseDetail => self.select_container(None),
            Hit::CreateField(field) => self.apply_hit_create_field(field),
            Hit::ComboToggle => {
                self.focus_create_field(CreateField::Image);
                self.combo.open = !self.combo.open;
                self.combo.highlight = 0;
            }
            Hit::ComboPick(pos) => self.apply_hit_combo_pick(doc, pos),
            Hit::CreateSubmit => {
                self.submit_create(doc);
            }
            Hit::CreateCancel => {
                self.create_open = false;
                self.blur_create_fields();
                self.combo.close();
            }
            Hit::PromptCancel => {
                // The pull overlay's Close button reuses this dismiss hit.
                self.pull = None;
            }
        }
    }

    fn apply_hit_select_image(&mut self, idx: usize) {
        if let Some(image) = self.engine.images.get(idx) {
            self.show_toast(
                format!(
                    "{} · {}",
                    image.reference(),
                    ui::format::bytes(image.size_bytes)
                ),
                false,
            );
        }
    }

    fn apply_hit_action(&mut self, id: String, action: Action) {
        if action.is_destructive() {
            let name = self
                .engine
                .find(&id)
                .map_or_else(|| id.clone(), |c| c.name.clone());
            self.confirm = Some(crate::app::Confirm {
                name,
                verb: "Remove container".into(),
                target: ConfirmTarget::Container { id, action },
            });
        } else {
            self.apply_action(action, &id);
        }
    }

    fn apply_hit_image_act(&mut self, reference: String, action: ImageAction) {
        // Only Remove exists; it's destructive → confirm.
        let _ = action;
        self.confirm = Some(crate::app::Confirm {
            name: reference.clone(),
            verb: "Remove image".into(),
            target: ConfirmTarget::Image { reference },
        });
    }

    /// Pull whatever is typed into the create-form image field, or prompt
    /// via a quick image pull dialog when empty.
    fn apply_hit_pull_typed(&mut self, doc: &AutoCommit) {
        let typed = CreateDoc::read(doc).get(CreateField::Image).to_owned();
        if typed.trim().is_empty() {
            self.prompt = Some(crate::app::Prompt {
                kind: PromptKind::NewVolume, // generic text path; handled below
                title: "Pull image".into(),
                placeholder: "docker.io/library/alpine:3.20".into(),
                value: String::new(),
                context: "__pull__".into(),
            });
        } else {
            self.start_pull(&typed);
        }
    }

    fn apply_hit_tag_image(&mut self, reference: String) {
        self.prompt = Some(crate::app::Prompt {
            kind: PromptKind::TagImage,
            title: format!("Tag {reference}"),
            placeholder: "registry.local/app:edge".into(),
            value: String::new(),
            context: reference,
        });
    }

    fn apply_hit_inspect_image(&mut self, reference: &str) {
        if self.live {
            let json = crate::podman::inspect_image(reference);
            let n = json.lines().count();
            self.show_toast(format!("Inspected {reference} ({n} lines)"), false);
        } else {
            self.show_toast(format!("podman image inspect {reference}"), false);
        }
    }

    fn apply_hit_create_field(&mut self, field: CreateField) {
        self.focus_create_field(field);
        // Focusing a non-image field closes the suggestion dropdown.
        if field == CreateField::Image {
            self.combo.open = true;
        } else {
            self.combo.close();
        }
    }

    fn apply_hit_combo_pick(&mut self, doc: &mut AutoCommit, pos: usize) {
        let typed = CreateDoc::read(doc).get(CreateField::Image).to_owned();
        let options = self.engine.image_references();
        let filtered = combobox::filter_options(&options, &typed);
        if let Some(&idx) = filtered.get(pos)
            && let Some(reference) = options.get(idx)
        {
            self.set_create_field(doc, CreateField::Image, reference);
        }
        self.combo.close();
    }

    /// Apply a committed text prompt (new volume / image tag / image pull).
    /// `value` is the (already-trimmed) text read from the dialog's real field.
    pub(crate) fn commit_prompt(&mut self, value: &str) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        match prompt.kind {
            PromptKind::NewVolume if prompt.context == "__pull__" => {
                self.start_pull(value);
            }
            PromptKind::NewVolume => {
                self.create_volume(value);
            }
            PromptKind::TagImage => {
                self.tag_image(&prompt.context, value);
            }
        }
    }

    /// Launch a REAL terminal entering the distrobox: spawn the Tilo terminal
    /// app running `distrobox enter <box>` (see [`crate::terminal`]).
    fn enter_distrobox(&mut self, name: &str) {
        if self.engine.live {
            let _ = self
                .pending_control_invocation
                .emit(ui::tilo_actions::ControlInvocation {
                    address: ui::tilo_actions::ActionAddress::new(
                        format!("distrobox:{name}"),
                        "enter",
                    ),
                    arguments: serde_json::Map::new(),
                });
            self.show_toast(format!("Enter {name} requested"), false);
        } else if !self.live {
            self.show_toast(format!("terminal → distrobox enter {name}"), false);
        } else {
            self.show_toast("Container engine is offline", true);
        }
    }

    pub(crate) fn on_key(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        key: &KeyEvent,
    ) {
        if key.state != ElementState::Pressed {
            return;
        }

        // Escape closes the top-most overlay / clears focus. (The confirm and
        // prompt dialogs are modal and handle their own Escape upstream in
        // `handle_event`, so they never reach here.)
        if let Key::Named(NamedKey::Escape) = key.logical_key {
            if self.pull.take().is_some() {
            } else if self.combo.open {
                self.combo.close();
            } else if self.create_open {
                self.create_open = false;
            } else if self.view.selected.is_some() {
                self.select_container(None);
            }
            *ctx.handled = true;
            self.refresh_view(ctx.state);
            return;
        }

        // Create modal owns the keyboard while open.
        if self.create_open {
            self.create_modal_key(event, ctx, key);
            self.refresh_view(ctx.state);
            return;
        }

        // Global shortcuts.
        if let Key::Character(ch) = &key.logical_key {
            let s = ch.to_lowercase();
            match s.as_str() {
                "1" => {
                    self.show_section(Section::Containers);
                    *ctx.handled = true;
                }
                "2" => {
                    self.show_section(Section::Images);
                    *ctx.handled = true;
                }
                "3" => {
                    self.show_section(Section::Volumes);
                    *ctx.handled = true;
                }
                "4" => {
                    self.show_section(Section::Distrobox);
                    *ctx.handled = true;
                }
                "/" => {
                    self.open_container_palette();
                    *ctx.handled = true;
                }
                "n" => {
                    self.create_open = true;
                    self.create_focus = CreateField::Image;
                    *ctx.handled = true;
                }
                "b" => {
                    self.toggle_sidebar();
                    self.structure_dirty = true;
                    *ctx.handled = true;
                }
                _ => {}
            }
        }
        self.refresh_view(ctx.state);
    }

    fn create_modal_key(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        key: &KeyEvent,
    ) {
        let field = self.create_focus;
        let on_image = field == CreateField::Image;
        // The combobox owns Up/Down/Enter while its dropdown is open and the
        // image field is focused.
        if on_image && self.combo.open {
            match &key.logical_key {
                Key::Named(NamedKey::ArrowDown) => {
                    let len = self.filtered_image_len(ctx.state);
                    self.combo.move_highlight(1, len);
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    let len = self.filtered_image_len(ctx.state);
                    self.combo.move_highlight(-1, len);
                    *ctx.handled = true;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    // Pick the highlighted suggestion, else submit.
                    let typed = CreateDoc::read(ctx.state)
                        .get(CreateField::Image)
                        .to_owned();
                    let options = self.engine.image_references();
                    let filtered = combobox::filter_options(&options, &typed);
                    if let Some(reference) = filtered
                        .get(self.combo.highlight)
                        .and_then(|&idx| options.get(idx))
                        .cloned()
                    {
                        self.set_create_field(ctx.state, CreateField::Image, &reference);
                        self.combo.close();
                    } else {
                        self.submit_create(ctx.state);
                    }
                    *ctx.handled = true;
                    return;
                }
                _ => {}
            }
        }
        match &key.logical_key {
            Key::Named(NamedKey::Tab) => {
                let next = (self.create_focus.index() + 1) % CreateField::ALL.len();
                if let Some(field) = CreateField::ALL.get(next).copied() {
                    self.focus_create_field(field);
                }
                self.combo.close();
                *ctx.handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                self.submit_create(ctx.state);
                *ctx.handled = true;
            }
            _ => {
                self.focus_create_field(field);
                self.route_create_input(
                    field,
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                *ctx.handled = true;
            }
        }
    }

    /// Number of locally-installed images matching the currently-typed image
    /// query — drives the combobox highlight clamp.
    fn filtered_image_len(&self, doc: &AutoCommit) -> usize {
        let typed = CreateDoc::read(doc).get(CreateField::Image).to_owned();
        let options = self.engine.image_references();
        combobox::filter_options(&options, &typed).len()
    }
}

/// Colour a log line by its severity keyword so the log view reads like a
/// real console.
fn log_line_color(theme: &Theme, line: &str) -> Color {
    let c = &theme.tokens.colors;
    let upper = line.to_ascii_uppercase();
    if upper.contains("ERROR")
        || upper.contains("FATAL")
        || upper.contains("SIGKILL")
        || upper.contains("404")
    {
        c.semantic.danger
    } else if upper.contains("WARN") {
        c.semantic.warning
    } else if upper.contains("INFO") || upper.contains("NOTICE") || upper.contains("LOG:") {
        c.text_primary
    } else {
        c.text_dim
    }
}

#[cfg(test)]
mod row_layout_tests {
    use super::*;
    use crate::model::sample_engine;

    fn running_container() -> Container {
        sample_engine()
            .containers
            .into_iter()
            .find(|c| c.status.is_running())
            .expect("the sample engine has a running container")
    }

    #[test]
    fn control_custody_containers_all_five_boundaries_copy_empty_descriptors_and_offline_preserves_last()
     {
        let (mut app, _requests, _results) = ContainersApp::controlled_runtime_root();
        app.engine = ContainersApp::controlled_engine("controlled Podman", true);

        let pending = |app: &ContainersApp| {
            let invocation = app
                .pending_control_invocation
                .controlled_pending_for_test()
                .expect("retained private descriptor");
            assert!(invocation.arguments.is_empty());
            invocation.address
        };

        app.apply_action(Action::Restart, "armed-container");
        assert_eq!(
            pending(&app),
            ui::tilo_actions::ActionAddress::new("container:armed-container", "restart")
        );
        app.remove_image("quay.io/example/image:1");
        assert_eq!(
            pending(&app),
            ui::tilo_actions::ActionAddress::new("image:quay.io/example/image:1", "remove")
        );
        app.remove_volume("example-volume");
        assert_eq!(
            pending(&app),
            ui::tilo_actions::ActionAddress::new("volume:example-volume", "remove")
        );
        app.attach_shell("armed-container");
        assert_eq!(
            pending(&app),
            ui::tilo_actions::ActionAddress::new("container:armed-container", "shell")
        );
        app.enter_distrobox("example-box");
        let last = pending(&app);
        assert_eq!(
            last,
            ui::tilo_actions::ActionAddress::new("distrobox:example-box", "enter")
        );

        app.engine.live = false;
        app.remove_volume("offline-volume");
        assert_eq!(
            pending(&app),
            last,
            "an offline attempt does not clear the older descriptor"
        );
    }

    /// Narrow rows stack the status pill before it reaches the actions, and heading and row names share one x.
    #[expect(
        clippy::float_cmp,
        reason = "layout reproduces authored whole-pixel sizes and their sums exactly in binary floating point; exact equality is what the test checks"
    )]
    #[test]
    fn list_rows_stack_status_when_narrow_and_align_names() {
        // --- phase: narrow_list_stacks_status_when_the_pill_would_reach_the_actions ---
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let container = running_container();
            assert_eq!(
                ContainersApp::row_actions_width(&container),
                3.0 * ICON_BTN + 2.0 * 4.0,
                "a running container shows stop / restart / remove"
            );
            // 240px list (detail open): the pill column starts at 42% of the row.
            let narrow = Rect::new(0.0, 0.0, 240.0, 64.0);
            let cols = ContainersApp::container_columns(narrow, true);
            let actions_x0 = narrow.x1 - PAD - ContainersApp::row_actions_width(&container);
            assert!(
                !ContainersApp::status_pill_fits(&mut tr, &theme, cols[1], actions_x0, &container),
                "a Running pill at x={} cannot end before the actions at x={actions_x0}",
                cols[1]
            );
            // The same row layout at a laptop-wide list fits comfortably.
            let wide = Rect::new(0.0, 0.0, 740.0, 64.0);
            let cols = ContainersApp::container_columns(wide, true);
            let actions_x0 = wide.x1 - PAD - ContainersApp::row_actions_width(&container);
            assert!(ContainersApp::status_pill_fits(
                &mut tr, &theme, cols[1], actions_x0, &container
            ));
        }

        // --- phase: name_heading_and_row_name_share_one_x ---
        {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            draw::set_base(Affine::IDENTITY);
            let list = Rect::new(200.0, 60.0, 1000.0, 660.0);
            let cols = ContainersApp::container_columns(list, false);

            let mut header = Scene::new();
            ContainersApp::draw_container_list_header(
                &mut header,
                &mut tr,
                &theme,
                list,
                false,
                &cols,
            );
            let heading_x = header
                .encoding()
                .resources
                .glyph_runs
                .first()
                .map(|run| f64::from(run.transform.translation[0]))
                .expect("the list header paints its NAME heading");

            // The row paints its name on `cols[0] + ROW_TEXT_INSET` in both the
            // stacked and the wide layout, so the app's viewport does not matter.
            let mut app = ContainersApp::sample();
            let view = ViewState::default();
            let container = running_container();
            let row = Rect::new(
                list.x0,
                list.y0 + 28.0,
                list.x1,
                list.y0 + 28.0 + crate::app::ROW_HEIGHT,
            );
            let info = ContainerRow {
                row,
                container: &container,
                cols: &cols,
                compact: false,
            };
            let mut scene = Scene::new();
            app.draw_container_row(&mut scene, &mut tr, &theme, info, &view);
            let name_x = scene
                .encoding()
                .resources
                .glyph_runs
                .first()
                .map(|run| f64::from(run.transform.translation[0]))
                .expect("the row paints its name first");

            assert!(
                (heading_x - name_x).abs() < 0.01,
                "heading at x={heading_x} but the row name at x={name_x}"
            );
        }
    }
}
