//! The settings sidebar: a cohesive navigation surface, not a stack of
//! free-floating buttons.
//!
//! ## Why a custom row widget
//!
//! `ui::widget::Button` is a fixed 120×40 pill that centres its label. A
//! navigation sidebar needs the opposite: a *full-width* row that left-aligns
//! an icon + the category's complete label, sizes itself to the text so
//! nothing is ever clipped, and carries an unmistakable active highlight. So
//! [`SidebarRow`] is a small purpose-built widget. Its activation publishes a
//! typed process-local navigation request which [`crate::SettingsApp::sync`]
//! consumes to switch the active category. Navigation is presentation state:
//! it must not become a replicated preference, an undo entry, or a document
//! click flag.
//!
//! ## What makes it a sidebar
//!
//! [`build`] assembles the rows into one [`Container`] surface tinted a step
//! below the content background, with per-[`Section`] headers and the rows
//! stacked inside a [`Scrollable`] so a short window can still reach every
//! category. The surface — not the individual rows — is what reads as "the
//! sidebar". It carries no app title (the OS pill does) and no search field
//! (that lives in the app's top strip).

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::chrome::{SelectionState, selection_tier};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::{Metrics, Spacing};
use ui::widget::{Column, Container, DEFAULT_SHELL_SIDEBAR_WIDTH, Scrollable, Text, Widget};

use crate::categories::{Category, Section};
use crate::widgets::{El, caption, spacing};

/// Width (logical px) of the docked sidebar — the shared `AppShell`'s own
/// default, named here so the app and its geometry tests read the value the
/// shell docks rather than a copy of it. Wide enough for the widest row
/// ("Device Privacy & Security" with its glyph, plus the row's horizontal
/// padding) without clipping.
pub const SIDEBAR_WIDTH: f64 = DEFAULT_SHELL_SIDEBAR_WIDTH;

/// The docked sidebar needs to grow with system text. Its labels are rendered
/// by the shared text renderer at this scale, while the shell itself is a
/// geometry-only primitive; exposing the matching width here keeps Settings
/// from painting 200%-text labels into a normal-width navigation rail.
pub fn width_for_text_scale(text_scale: f32) -> f64 {
    SIDEBAR_WIDTH * f64::from(text_scale.clamp(0.5, 4.0))
}

/// Height (logical px) of a single category row at 1×: it seats the
/// toolkit's minimum pointer target (the selection pill,
/// `Metrics::min_pointer_target`) plus one `s1` inset above and below it. A
/// row whose label measures taller than that — under a large-text
/// preference — grows to fit (see [`SidebarRow::measure`]); at 1× every row
/// is exactly this.
const ROW_HEIGHT: f32 = Metrics::BASE.min_pointer_target + 2.0 * Spacing::BASE.s1;

/// Fixed width (logical px) of the icon column — the standard `icon_md`
/// glyph slot. The glyph is centred within it so every label starts at the
/// same x regardless of glyph advance.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const, and an f32 -> f64 widening is exact"
)]
const ROW_ICON_COL: f64 = Metrics::BASE.icon_md as f64;

/// Inset (logical px) at which the shared `AppShell` places the search field
/// from the sidebar edge: its band padding is the `s3` step (a private
/// constant in `ui`, not exported yet — see the review's open item for `F1`),
/// so this names the same token instead of restating the number. The
/// selection pill and the search field share that left edge, so every
/// horizontal measure in this file derives from it; the geometry tests import
/// this one constant rather than retyping it.
pub const SHELL_FIELD_INSET: f32 = Spacing::BASE.s3;

/// Horizontal margin of each row inside the sidebar column: the pill is inset
/// `s2` from the row, so the row starts `SHELL_FIELD_INSET - s2` in and the
/// pill lands exactly under the search field's left edge.
fn row_margin_x() -> f32 {
    SHELL_FIELD_INSET - spacing().s2
}

/// Left inset of a section header from the sidebar column edge: the pill inset
/// plus the pill's own content gutter, so the header starts on the same x as
/// the icon column beneath it.
fn header_inset_x() -> f32 {
    spacing().s2 + spacing().s3
}

/// Text and icon ink for a sidebar row, in every state. Selection is a lift
/// of the sidebar surface (`ui::chrome::selection_tier`, HIG §9.3), never an
/// accent fill, so the primary text colour stays legible on the chosen row
/// too; the function stays the one place both the icon and the label take
/// their ink from, and the contrast test below proves it against the lifted
/// fill on every preset.
fn row_ink(theme: &Theme) -> vello::peniko::Color {
    theme.text_primary
}

/// The resolved on-screen geometry of one sidebar row: the selection pill and
/// the icon / label / baseline positions inside it. Computed once in
/// [`RowGeometry::compute`] so the draw path and the alignment test share the
/// exact same math — the test asserts the icon is centred in its column, the
/// label clears the icon by a fixed gap, and everything sits inside the pill
/// with even breathing room.
#[derive(Clone, Copy, Debug)]
pub struct RowGeometry {
    /// The selection / hover pill, inset `s2` / `s1` from the row edges.
    pub pill: Rect,
    /// Left edge (logical px) where the icon column begins, inset from the
    /// pill by the `s3` content gutter.
    pub content_left: f64,
    /// X translate for the icon glyph — centred within the icon column.
    pub icon_x: f64,
    /// X translate for the label, one `s3` gap past the icon column.
    pub label_x: f64,
    /// Y translate (top of the text line box) that centres the line in the row.
    pub text_y: f64,
}

impl RowGeometry {
    /// Resolve the row geometry for a row of `rect` whose icon glyph measured
    /// `glyph_w` wide and whose label shaped to a line box `line_height` tall
    /// at `ROW_TEXT_SIZE`. Every inset and gap is a step of the theme's
    /// spacing scale, so the pill and the shell's search field share an edge
    /// by construction rather than by coincidence.
    pub fn compute(
        rect: Rect,
        glyph_w: f64,
        line_height: f64,
        spacing: &ui::tokens::Spacing,
    ) -> Self {
        let inset_x = f64::from(spacing.s2);
        let inset_y = f64::from(spacing.s1);
        let pill = Rect::new(
            rect.x0 + inset_x,
            rect.y0 + inset_y,
            rect.x1 - inset_x,
            rect.y1 - inset_y,
        );
        let content_left = pill.x0 + f64::from(spacing.s3);
        // Centre the glyph within a fixed-width column so every label starts at
        // the same x regardless of the glyph's advance width.
        let icon_x = content_left + (ROW_ICON_COL - glyph_w).max(0.0) / 2.0;
        let label_x = content_left + ROW_ICON_COL + f64::from(spacing.s3);
        // `draw_text` treats the translate as the top-left of the shaped line
        // box; centre the *measured* box in the row so the glyphs sit on the
        // pill's optical centre whatever the renderer's line metrics are.
        let text_y = rect.y0 + (rect.height() - line_height) / 2.0;
        Self {
            pill,
            content_left,
            icon_x,
            label_x,
            text_y,
        }
    }
}

/// One selectable category row in the sidebar.
///
/// Fills the available width, left-aligns its icon + full label, and paints
/// the shared selection lift when it is the active category. Clicking it emits
/// a local selection request which [`crate::SettingsApp::sync`] consumes to
/// change panes without writing presentation state into the document.
#[allow(
    clippy::struct_excessive_bools,
    reason = "selection, hover, press, and keyboard focus are independent interaction states"
)]
pub struct SidebarRow {
    category: Category,
    /// The category's label in the active locale: painted, measured, spoken
    /// and used as the row's `test_id`.
    label: String,
    /// Whether this row is the currently-selected category — drives the active
    /// highlight. Passed in at build time from the app's selection.
    active: bool,
    /// One-shot local navigation request. Category selection is deliberately
    /// not document state: it is only this window's current presentation.
    selection_request: ui::LocalState<Option<Category>>,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
    /// The label's type size as the theme resolves it (`scale.base`, already
    /// text-scaled), captured in `build_layout` so `measure` and `draw` shape
    /// the label identically. Handed to the renderer through
    /// [`TextRenderer::renderer_input_style`] so the text scale is applied
    /// exactly once.
    label_size: f32,
}

impl SidebarRow {
    pub fn new(category: Category, active: bool) -> Self {
        Self::with_selection_request(category, active, ui::LocalState::new(None))
    }

    pub fn with_selection_request(
        category: Category,
        active: bool,
        selection_request: ui::LocalState<Option<Category>>,
    ) -> Self {
        Self {
            category,
            label: category.label(),
            active,
            selection_request,
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
            label_size: ui::tokens::TypeScale::default().base,
        }
    }

    /// The renderer-relative size the label is measured and drawn at.
    fn renderer_label_size(&self, text_renderer: &TextRenderer) -> f32 {
        text_renderer
            .renderer_input_style(&TextStyle::new(self.label_size))
            .size
    }

    /// The width the label may take in a row `row_w` wide: from its left edge
    /// (past the pill inset, the content gutter, the icon column and its gap)
    /// to the pill's inner gutter on the right. `measure` and `draw` shape the
    /// label against this same width, so a label that wraps under large text
    /// grows the row instead of running past the pill.
    fn label_max_width(row_w: f64) -> f32 {
        let sp = spacing();
        let taken = 2.0 * f64::from(sp.s2) + 3.0 * f64::from(sp.s3) + ROW_ICON_COL;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a logical-pixel width comfortably representable by f32"
        )]
        let width = (row_w - taken).max(0.0) as f32;
        width
    }

    /// The row height its label needs in a row `row_w` wide: the measured
    /// line box (wrapped to the label's width if it must) with one `s1` of
    /// breathing room inside the pill and the pill's own `s1` inset, top and
    /// bottom — never below [`ROW_HEIGHT`], so 1× rows stay exactly that.
    fn measured_height(&self, text_renderer: &mut TextRenderer, row_w: f64) -> f32 {
        let size = self.renderer_label_size(text_renderer);
        let (_, line_height) =
            text_renderer.measure(&self.label, size, Some(Self::label_max_width(row_w)));
        (line_height + 4.0 * spacing().s1).max(ROW_HEIGHT)
    }

    /// Publish the same local presentation request for pointer, keyboard and
    /// assistive-technology activation. The app consumes it as a one-shot
    /// edge in `SettingsApp::sync`.
    fn activate(&self) {
        self.selection_request.set(Some(self.category));
    }
}

impl Widget for SidebarRow {
    ui::provides!(dyn ui::undo::UndoPoints);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.label_size = theme.tokens.typography.scale.base;
        // Full width; the height comes from `measure` (the label's line box,
        // never below ROW_HEIGHT) — the row is a leaf that draws itself, so it
        // needs no taffy children.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::auto(),
                },
                min_size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(ROW_HEIGHT),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        // The row's width: the known dimension when the parent has resolved
        // it, else the definite available width taffy hands the final layout
        // pass, else (a bare measure with no parent at all) the docked
        // sidebar's own. The label wraps at this width in `draw` too.
        let available_w = match available.width {
            taffy::AvailableSpace::Definite(w) => Some(w),
            _ => None,
        };
        let row_w = known.width.or(available_w).map_or_else(
            || SIDEBAR_WIDTH - 2.0 * f64::from(row_margin_x()),
            f64::from,
        );
        let height = self.measured_height(text_renderer, row_w);
        taffy::Size {
            width: known.width.unwrap_or(0.0),
            height: known.height.unwrap_or(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Active row: the shared selection lift — a raised fill with a derived
        // hairline and inner glow (HIG §9.3), never an accent wash. Hover: the
        // half lift. Otherwise nothing, so inactive rows read flat against the
        // sidebar surface. The pill is inset from the panel edges and given a
        // little vertical breathing room so the highlight reads as a
        // deliberate selection chip rather than a full-bleed band.
        let radius = f64::from(theme.tokens.radii.md);
        let icon_size = 18.0;
        // All positions come from one geometry pass so the drawn layout and the
        // alignment test read the exact same math. The label's line box is
        // measured with the style it is drawn with, never derived from the
        // font size.
        //
        // The pill inset must be the same scale the row margins in [`build`]
        // were laid out with — the default toolkit scale, because the sidebar
        // tree is composed before a theme is resolved. Reading a theme-retuned
        // `s2` here would detach the pill from the search field it aligns to.
        let label = self.label.as_str();
        let label_size = self.renderer_label_size(text_renderer);
        // Shaped against the same width `measure` used, so a label that wraps
        // under large text is centred as the two-line box the row grew for.
        let label_max_w = Self::label_max_width(self.rect.width());
        let (_, line_height) = text_renderer.measure(label, label_size, Some(label_max_w));
        let geo = RowGeometry::compute(self.rect, icon_size, f64::from(line_height), &spacing());

        // The shared four-tier lift: idle transparent, hover a half lift,
        // active a full lift with a derived hairline and inner glow, pressed
        // the active look scaled down about the pill's centre (the press cue
        // the tier carries in `scale`, applied the way the tab strip does).
        let tier = selection_tier(
            theme,
            SelectionState::new(self.active, self.is_hovered, self.is_pressed),
        );
        let pill_transform = if (tier.scale - 1.0).abs() > f64::EPSILON {
            let centre = geo.pill.center();
            transform
                * Affine::translate((centre.x, centre.y))
                * Affine::scale(tier.scale)
                * Affine::translate((-centre.x, -centre.y))
        } else {
            transform
        };
        if tier.fill.components[3] > 0.0 {
            let pill = RoundedRect::from_rect(geo.pill, radius);
            scene.fill(
                vello::peniko::Fill::NonZero,
                pill_transform,
                tier.fill,
                None,
                &pill,
            );
        }
        if let Some(border) = tier.border {
            // A crisp 1 px edge half a pixel inside the pill, so it covers the
            // pill's own outermost pixel row and never bleeds onto the sidebar.
            scene.stroke(
                &Stroke::new(1.0),
                pill_transform,
                border,
                None,
                &ui::surface::edge_outline(geo.pill, radius),
            );
        }
        if let Some(glow) = tier.inner_glow {
            scene.stroke(
                &Stroke::new(1.0),
                pill_transform,
                glow,
                None,
                &RoundedRect::from_rect(geo.pill.inset(-1.5), (radius - 1.5).max(0.0)),
            );
        }
        if self.is_focused {
            let pill = RoundedRect::from_rect(geo.pill, radius);
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.border_focus,
                None,
                &pill,
            );
        }

        // Icon + label, left-aligned, vertically centred, in the primary text
        // colour on every state: a lift keeps the ink legible where an accent
        // fill would not.
        let fg = row_ink(theme);

        let icon_y = self.rect.center().y - icon_size / 2.0;
        self.category.icon().draw(
            scene,
            transform,
            Rect::new(
                geo.icon_x,
                icon_y,
                geo.icon_x + icon_size,
                icon_y + icon_size,
            ),
            fg,
        );
        // The label wraps at the pill's inner gutter — the width the row was
        // measured for — rather than being painted past the pill's edge.
        text_renderer.draw_text(
            scene,
            label,
            label_size,
            fg,
            transform * Affine::translate((geo.label_x, geo.text_y)),
            Some(label_max_w),
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hover = self.rect.contains(logical);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *ctx.handled = true;
                        // Fire selection on release (matches Button semantics).
                        if !pressed {
                            self.activate();
                        }
                        if pressed {
                            *ctx.focus_target = Some(ctx.node_id);
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.is_focused
                    && event.state == ElementState::Pressed
                    && matches!(
                        event.logical_key,
                        Key::Named(NamedKey::Enter | NamedKey::Space)
                    ) =>
            {
                self.activate();
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "SidebarRow"
    }

    /// Addressable by the category's display label so interaction tests can
    /// resolve a row's on-screen rect and click it.
    fn test_id(&self) -> Option<&str> {
        Some(&self.label)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::RadioButton)
                .with_name(&self.label)
                .with_action_target(crate::actions::open_pane_address(self.category).encode())
                .with_state(ui::a11y::AccessState {
                    focusable: true,
                    focused: self.is_focused,
                    checked: Some(self.active),
                    selected: self.active,
                    ..Default::default()
                }),
        )
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        match action {
            ui::a11y::Action::Activate => {
                self.activate();
                true
            }
            _ => false,
        }
    }
}

impl ui::undo::UndoPoints for SidebarRow {
    /// Expose the active state so a rebuilt tree can be inspected — used by
    /// the interaction tests to confirm the highlight followed the click. The
    /// `Widget::get_state` channel (ephemeral, non-persisted) is the natural
    /// place for this read-only snapshot.
    fn get_state(&self) -> Option<String> {
        Some(if self.active {
            "active".into()
        } else {
            "inactive".into()
        })
    }
}

/// Build the full sidebar surface: either the sectioned category list (when
/// `query` is empty) or a flat filtered results list (when the user is
/// searching). `selected` drives which row shows the active highlight. The
/// app title and search field are NOT here — the OS pill carries the title and
/// the search field lives in the app's top strip.
///
/// The whole thing is one [`Container`] tinted a step below the content
/// background so it reads as a single cohesive sidebar pane, with the rows
/// inside a [`Scrollable`] so every category stays reachable on short windows.
pub fn build(
    selected: Category,
    query: &str,
    selection_request: &ui::LocalState<Option<Category>>,
) -> Box<dyn Widget> {
    // Three explicit steps of the spacing scale carry the whole rhythm, so
    // the eye reads groups rather than a list of equally spaced lines:
    //
    // - rows inside a section sit `s1` apart (their pills, inset `s1` each,
    //   end up `s3` apart);
    // - a header sits `s2` above its first row, close enough to own it;
    // - sections are `s6` apart, the largest step on the surface.
    //
    // The sidebar draws **no app title and no search field**: the app name is
    // shown by the OS window-group pill (an app-drawn "Settings" header would
    // duplicate it), and the search field lives in the app's top strip so it
    // is reachable by keyboard focus (see [`crate::settings_app`]). The list
    // starts `s2` below that strip, and every row is inset so its pill shares
    // the search field's left edge.
    let sp = spacing();
    let mut col = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_horizontal_padding(row_margin_x(), row_margin_x())
        .with_vertical_padding(sp.s2)
        .with_gap(sp.s6);

    let row = |category: Category| -> El {
        Box::new(SidebarRow::with_selection_request(
            category,
            category == selected,
            selection_request.clone(),
        ))
    };

    let trimmed = query.trim();
    if trimmed.is_empty() {
        for section in Section::ALL.iter().copied() {
            let rows: Vec<El> = Category::in_section(section).map(row).collect();
            col = col.push(section_block(&section.label(), rows));
        }
    } else {
        // Filtered results — a flat list of matching categories under a
        // "RESULTS" header, or an honest empty state.
        let matches: Vec<El> = Category::ALL
            .iter()
            .copied()
            .filter(|c| c.matches(trimmed))
            .map(row)
            .collect();
        let body = if matches.is_empty() {
            vec![caption(&format!(
                "No settings match \u{201c}{trimmed}\u{201d}"
            ))]
        } else {
            matches
        };
        col = col.push(section_block("Results", body));
    }

    let scroll = Scrollable::new_vertical().with_child(Box::new(col));
    // No surface tint or border here: the enclosing `AppShell` draws the one
    // cohesive sidebar panel (header + this list) as a single flat surface, so
    // the nav is just the transparent, scrollable list of rows that sits on it.
    // Drawing our own tinted/bordered box would reintroduce the "card scatter"
    // the shell exists to kill.
    Box::new(
        Container::new()
            .filling()
            .with_no_padding()
            .with_child(Box::new(scroll)),
    )
}

/// An uppercase, dimmed section header line. Kept at 12 px so the category
/// groups remain comfortably readable instead of becoming low-contrast chrome.
fn section_header(label: &str) -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_horizontal_padding(header_inset_x(), 0.0)
            .push(Box::new(
                Text::new(&label.to_uppercase()).with_size(12.0).dimmed(),
            )),
    )
}

/// One navigation section: its header hugging the rows it names (`s2`), the
/// rows themselves one `s1` apart. The enclosing column separates sections by
/// `s6`, so the header always sits closer to its own rows than to the section
/// above it.
fn section_block(label: &str, rows: Vec<El>) -> El {
    let sp = spacing();
    let mut list = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(sp.s1);
    for row in rows {
        list = list.push(row);
    }
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(sp.s2)
            .push(section_header(label))
            .push(Box::new(list)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    fn count_rows(w: &dyn Widget) -> usize {
        let mut n = usize::from(w.name() == "SidebarRow");
        for c in w.children() {
            n += count_rows(c);
        }
        n
    }

    /// The sidebar lists exactly the categories matching its query, and each
    /// row is addressable by its category label.
    #[test]
    fn sidebar_query_filters_rows_and_rows_are_addressable() {
        let night = Category::ALL.iter().filter(|c| c.matches("night")).count();
        assert!(
            night >= 1 && night < Category::ALL.len(),
            "genuinely filtered"
        );
        // (case, selected, query, rows)
        for (case, selected, query, rows) in [
            (
                "unfiltered lists all categories",
                Category::Network,
                "",
                Category::ALL.len(),
            ),
            // "night" is a Displays-only keyword.
            ("keyword filters", Category::Displays, "night", night),
            ("no match lists no rows", Category::Network, "zzzznope", 0),
        ] {
            let sb = build(selected, query, &ui::LocalState::new(None));
            assert_eq!(count_rows(sb.as_ref()), rows, "case {case}");
        }

        let row = SidebarRow::new(Category::MouseKeyboard, false);
        assert_eq!(Widget::test_id(&row), Some("Mouse & Keyboard"));
    }

    /// Row geometry: the pill inset, centred icon column, glyph-independent
    /// label start, centred line box, and the shared shell field edge.
    #[test]
    fn row_geometry_aligns_icon_label_and_pill_to_shared_edges() {
        let sp = spacing();
        // A representative row rect (full sidebar width, one ROW_HEIGHT tall).
        let rect = Rect::new(0.0, 100.0, SIDEBAR_WIDTH, 100.0 + f64::from(ROW_HEIGHT));
        // A glyph narrower than the icon column — the common case — and a
        // measured line box for the label (what the renderer reports for the
        // base size; the maths must hold for any value).
        let glyph_w = 13.0;
        let line_box = 17.5;
        let g = RowGeometry::compute(rect, glyph_w, line_box, &sp);

        // The pill is inset evenly from every row edge, by the `s2` / `s1`
        // steps of the scale.
        assert!((g.pill.x0 - (rect.x0 + f64::from(sp.s2))).abs() < f64::EPSILON);
        assert!((g.pill.x1 - (rect.x1 - f64::from(sp.s2))).abs() < f64::EPSILON);
        assert!((g.pill.y0 - (rect.y0 + f64::from(sp.s1))).abs() < f64::EPSILON);
        assert!((g.pill.y1 - (rect.y1 - f64::from(sp.s1))).abs() < f64::EPSILON);

        // Content starts one `s3` gutter inside the pill's left edge (even left
        // padding), and both icon and label sit inside the pill.
        assert!((g.content_left - (g.pill.x0 + f64::from(sp.s3))).abs() < f64::EPSILON);
        assert!(
            g.icon_x >= g.pill.x0,
            "icon must not spill past the pill's left"
        );
        let label_right = g.label_x; // label starts here; its ink extends right
        assert!(label_right < g.pill.x1, "label must start within the pill");

        // The glyph is horizontally centred within the fixed icon column: its
        // centre lands on the column centre, not jammed to the left.
        let icon_center = g.icon_x + glyph_w / 2.0;
        let column_center = g.content_left + ROW_ICON_COL / 2.0;
        assert!(
            (icon_center - column_center).abs() < 0.001,
            "icon should be centred in its column (icon_center={icon_center}, column_center={column_center})"
        );

        // The label clears the icon column by exactly one `s3` gap, so every
        // label starts at the same x regardless of glyph width.
        assert!(
            (g.label_x - (g.content_left + ROW_ICON_COL + f64::from(sp.s3))).abs() < f64::EPSILON,
            "label starts one gap past the icon column"
        );

        // The *measured* text line box is vertically centred in the row (equal
        // top/bottom slack), so the content sits on the pill's optical centre.
        let top_slack = g.text_y - rect.y0;
        let bottom_slack = rect.y1 - (g.text_y + line_box);
        assert!(
            (top_slack - bottom_slack).abs() < 0.001,
            "text line box must be vertically centred (top={top_slack}, bottom={bottom_slack})"
        );

        // Two rows whose icon glyphs measure differently must still align their
        // labels to the same x — the point of the fixed icon column.
        let rect = Rect::new(0.0, 0.0, SIDEBAR_WIDTH, f64::from(ROW_HEIGHT));
        let narrow = RowGeometry::compute(rect, 8.0, 17.5, &sp);
        let wide = RowGeometry::compute(rect, 18.0, 17.5, &sp);
        assert!(
            (narrow.label_x - wide.label_x).abs() < f64::EPSILON,
            "labels must start at the same x regardless of glyph advance width"
        );

        // The row margin plus the pill inset must land on the shell's field
        // inset, so the selection highlight and the search field read as one
        // column edge instead of two.
        let row = Rect::new(
            f64::from(row_margin_x()),
            0.0,
            SIDEBAR_WIDTH - f64::from(row_margin_x()),
            f64::from(ROW_HEIGHT),
        );
        let g = RowGeometry::compute(row, 13.0, 17.5, &sp);
        assert!((g.pill.x0 - f64::from(SHELL_FIELD_INSET)).abs() < f64::EPSILON);
        // The section header shares the pill's content edge (the icon column).
        assert!(
            (f64::from(row_margin_x() + header_inset_x()) - g.content_left).abs() < f64::EPSILON,
            "header inset {} + margin {} must equal content_left {}",
            header_inset_x(),
            row_margin_x(),
            g.content_left
        );
    }

    #[test]
    fn active_row_ink_meets_text_contrast_on_the_lifted_fill() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ink = row_ink(&theme);
            let tier = selection_tier(&theme, SelectionState::active(true));
            // `Colors::lift(1.0)` is an opaque mix, so the fill the eye sees is
            // the fill the tier hands back — no compositing step to fake.
            assert!(
                tier.fill.components[3] >= 1.0,
                "the active lift must be an opaque fill for {}",
                theme.name
            );
            assert!(
                tier.border.is_some() && tier.inner_glow.is_some(),
                "the active row must carry a derived edge and glow for {}",
                theme.name
            );
            assert!(
                ui::a11y::contrast_ratio(ink, tier.fill) >= 4.5,
                "active sidebar ink must remain readable on the lifted fill for {}",
                theme.name
            );
            let hover = selection_tier(&theme, SelectionState::new(false, true, false));
            assert!(
                hover.fill.components[3] > 0.0 && hover.border.is_none(),
                "hover is the half lift without an edge for {}",
                theme.name
            );
            let pressed = selection_tier(&theme, SelectionState::new(false, false, true));
            assert!(
                pressed.scale < 1.0,
                "a press carries its own cue (scale) for {}",
                theme.name
            );
            let idle = selection_tier(&theme, SelectionState::new(false, false, false));
            assert!(
                idle.fill.components[3] == 0.0,
                "an idle row paints nothing for {}",
                theme.name
            );
        }
    }

    /// Rows request local selection on release and cover keyboard/assistive activation, never touching the document.
    #[test]
    fn sidebar_rows_select_locally_and_expose_semantics() {
        // Case `row_requests_local_selection_on_release_without_mutating_the_document`.
        {
            let request = ui::LocalState::new(None);
            let mut row =
                SidebarRow::with_selection_request(Category::About, false, request.clone());
            // Seat a rect and pretend the cursor is inside it.
            row.rect = Rect::new(0.0, 0.0, 200.0, 40.0);
            row.is_hovered = true;

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let mods = winit::event::Modifiers::default();
            let mut taffy = TaffyTree::<()>::new();
            let node = taffy.new_leaf(Style::default()).unwrap();

            let press = WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            let release = WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Released,
                button: MouseButton::Left,
            };
            for ev in [&press, &release] {
                let mut handled = false;
                let mut focus = None;
                row.handle_event(
                    ev,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &mods,
                        state: &mut doc,
                        path: &root,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
            }

            assert_eq!(request.with(|selection| *selection), Some(Category::About));
            assert!(
                doc.get_heads().is_empty(),
                "navigation must not create a replicated click-flag change"
            );
        }

        // Case `row_activation_and_semantics_cover_keyboard_and_assistive_technology`.
        {
            let request = ui::LocalState::new(None);
            let mut row =
                SidebarRow::with_selection_request(Category::Accessibility, false, request.clone());
            row.set_focus(true);
            let node = row.accessibility().expect("sidebar row has semantics");
            assert_eq!(node.role, ui::a11y::Role::RadioButton);
            assert!(node.state.focusable);
            assert!(node.state.focused);
            assert_eq!(node.state.checked, Some(false));

            let mut doc = AutoCommit::new();
            assert!(row.perform_action(&ui::a11y::Action::Activate, &mut doc, &automerge::ROOT,));
            assert_eq!(
                request.with(|selection| *selection),
                Some(Category::Accessibility),
                "the semantic activation uses the same local navigation request as a pointer click"
            );
            assert!(doc.get_heads().is_empty());
        }
    }
}

#[cfg(test)]
mod incoming_large_text_tests {
    use super::*;
    #[test]
    fn large_text_sidebar_scales_the_rail_row_and_label_geometry_together() {
        let theme = Theme::dark().with_text_scale(2.0);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let width = width_for_text_scale(2.0);
        let request = ui::LocalState::new(None);
        let mut row = SidebarRow::with_selection_request(Category::Accessibility, true, request);
        let mut tree = TaffyTree::new();
        row.build_layout(&mut tree, &theme, &mut std::collections::HashMap::new());
        let height = f64::from(row.measured_height(&mut tr, width));
        let rect = Rect::new(0.0, 0.0, width, height);
        let size = row.renderer_label_size(&tr);
        let (_, line_box) = tr.measure(
            &row.category.label(),
            size,
            Some(SidebarRow::label_max_width(width)),
        );
        let geometry = RowGeometry::compute(rect, 18.0, f64::from(line_box), &spacing());
        assert!(
            geometry.pill.width() > SIDEBAR_WIDTH,
            "active target grows with text"
        );
        assert!(
            geometry.label_x > f64::from(row.label_size),
            "icon gutter clears the label"
        );
        assert!(
            geometry.text_y >= rect.y0 && geometry.text_y + f64::from(line_box) <= rect.y1,
            "scaled text remains inside its measured row"
        );
    }
}
