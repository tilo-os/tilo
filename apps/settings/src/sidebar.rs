//! The settings sidebar: a cohesive navigation surface, not a stack of
//! free-floating buttons.
//!
//! ## Why a custom row widget
//!
//! `ui::widget::Button` is a fixed 120×40 pill that centres its label. A
//! navigation sidebar needs the opposite: a *full-width* row that left-aligns
//! an icon + the category's complete label, sizes itself to the text so
//! nothing is ever clipped, and carries an unmistakable active highlight. So
//! [`SidebarRow`] is a small purpose-built widget. It still routes its click
//! through the same CRDT mechanism the rest of the app uses — pressing a row
//! writes `settings.select.<slug> = true`, which [`crate::SettingsApp::sync`]
//! reads to switch the active category. No private message bus; the selection
//! lives in the document like every other piece of state.
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

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Column, Container, Scrollable, Text, Widget};

use crate::categories::{Category, Section};
use crate::widgets::{caption, vgap};

/// CRDT key the top strip's search field writes its query into. Read by
/// [`crate::SettingsApp::sync`] to filter the category list.
pub const SEARCH_KEY: &str = "settings.search";

/// Gap (logical px) injected above the first section header so it has clear
/// breathing room at the top of the sidebar surface.
const TITLE_GAP: f32 = 14.0;

/// Extra gap (logical px) injected ABOVE each section header (beyond the
/// first). Combined with the column's own inter-child gap this makes the
/// space above a header noticeably larger than the space below it.
const SECTION_GAP_ABOVE: f32 = 18.0;

/// Small gap (logical px) injected BELOW a section header, before its
/// first row. Deliberately smaller than [`SECTION_GAP_ABOVE`] so the
/// header reads as belonging to the rows beneath it.
const SECTION_GAP_BELOW: f32 = 2.0;

/// Width (logical px) of the docked sidebar. Sized so the widest row
/// ("Device Privacy & Security" with its glyph, plus the row's horizontal
/// padding) fits without clipping, with margin to spare.
pub const SIDEBAR_WIDTH: f64 = 264.0;

/// Height (logical px) of a single category row. Generous enough to read as a
/// touch target and to seat the icon + label comfortably.
const ROW_HEIGHT: f32 = 40.0;

/// Inset (logical px) of the selection/hover pill from the row's left/right
/// edges, so the highlight reads as a chip with margin to the panel border.
const ROW_INSET_X: f64 = 8.0;

/// Inset (logical px) of the pill from the row's top/bottom edges.
const ROW_INSET_Y: f64 = 4.0;

/// Left padding (logical px) of the icon + label from the *pill's* left edge.
/// Matched to feel even against the pill's vertical padding so the content
/// sits centred inside the highlight.
const ROW_CONTENT_GUTTER: f64 = 12.0;

/// Fixed width (logical px) of the icon column. The glyph is centred within
/// it so every label starts at the same x regardless of glyph advance.
const ROW_ICON_COL: f64 = 20.0;

/// Gap (logical px) between the icon column and the label.
const ROW_ICON_LABEL_GAP: f64 = 12.0;

/// Font size (logical px) of a category label.
const ROW_TEXT_SIZE: f32 = 14.0;

/// Line-height ratio `TextRenderer` shapes with (`size * 1.25`); used to
/// centre a single text line's box within the row.
const LINE_HEIGHT_RATIO: f64 = 1.25;

/// The resolved on-screen geometry of one sidebar row: the selection pill and
/// the icon / label / baseline positions inside it. Computed once in
/// [`RowGeometry::compute`] so the draw path and the alignment test share the
/// exact same math — the test asserts the icon is centred in its column, the
/// label clears the icon by a fixed gap, and everything sits inside the pill
/// with even breathing room.
#[derive(Clone, Copy, Debug)]
pub struct RowGeometry {
    /// The selection / hover pill, inset from the row edges.
    pub pill: Rect,
    /// Left edge (logical px) where the icon column begins, inset from the
    /// pill by [`ROW_CONTENT_GUTTER`].
    pub content_left: f64,
    /// X translate for the icon glyph — centred within the icon column.
    pub icon_x: f64,
    /// X translate for the label, one gap past the icon column.
    pub label_x: f64,
    /// Y translate (top of the text line box) that centres the line in the row.
    pub text_y: f64,
}

impl RowGeometry {
    /// Resolve the row geometry for a row of `rect` whose icon glyph measured
    /// `glyph_w` wide at [`ROW_TEXT_SIZE`].
    pub fn compute(rect: Rect, glyph_w: f64) -> Self {
        let pill = Rect::new(
            rect.x0 + ROW_INSET_X,
            rect.y0 + ROW_INSET_Y,
            rect.x1 - ROW_INSET_X,
            rect.y1 - ROW_INSET_Y,
        );
        let content_left = pill.x0 + ROW_CONTENT_GUTTER;
        // Centre the glyph within a fixed-width column so every label starts at
        // the same x regardless of the glyph's advance width.
        let icon_x = content_left + (ROW_ICON_COL - glyph_w).max(0.0) / 2.0;
        let label_x = content_left + ROW_ICON_COL + ROW_ICON_LABEL_GAP;
        // `draw_text` treats the translate as the top-left of a line box of
        // height `size * LINE_HEIGHT_RATIO`; centre that box in the row so the
        // glyphs sit on the pill's optical centre.
        let line_box = f64::from(ROW_TEXT_SIZE) * LINE_HEIGHT_RATIO;
        let text_y = rect.y0 + (rect.height() - line_box) / 2.0;
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
/// Fills the available width, left-aligns its icon + full label, and paints an
/// accent background + bar when it is the active category. Clicking it writes
/// `settings.select.<slug> = true` into the document — the same key
/// [`crate::SettingsApp::sync`] consumes to change panes.
pub struct SidebarRow {
    category: Category,
    /// Whether this row is the currently-selected category — drives the active
    /// highlight. Passed in at build time from the app's selection.
    active: bool,
    /// CRDT key this row writes `true` to on click.
    select_key: String,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
}

impl SidebarRow {
    pub fn new(category: Category, active: bool) -> Self {
        Self {
            category,
            active,
            select_key: format!("settings.select.{}", category.slug()),
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
        }
    }
}

impl Widget for SidebarRow {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Full width, fixed height — the row is a leaf that draws itself, so it
        // needs no taffy children.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(ROW_HEIGHT),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
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
        // Active row: a filled accent pill. Hover/press: a subtle raised tint.
        // Otherwise nothing, so inactive rows read flat against the sidebar
        // surface. The pill is inset from the panel edges and given a little
        // vertical breathing room so the highlight reads as a deliberate
        // selection chip rather than a full-bleed band.
        let radius = f64::from(theme.tokens.radii.md);
        let glyph = self.category.glyph();
        let (glyph_w, _) = text_renderer.measure(glyph, ROW_TEXT_SIZE, None);
        // All positions come from one geometry pass so the drawn layout and the
        // alignment test read the exact same math.
        let geo = RowGeometry::compute(self.rect, f64::from(glyph_w));

        let highlight = if self.active {
            Some(theme.accent_primary)
        } else if self.is_pressed {
            Some(theme.bg_active)
        } else if self.is_hovered {
            Some(theme.bg_hover)
        } else {
            None
        };
        if let Some(color) = highlight {
            let pill = RoundedRect::from_rect(geo.pill, radius);
            scene.fill(vello::peniko::Fill::NonZero, transform, color, None, &pill);
        }

        // Icon + label, left-aligned, vertically centred. The active row uses
        // a bright on-accent foreground; inactive rows use the primary text
        // colour so they stay legible against the sidebar surface.
        let fg = if self.active {
            theme.tokens.colors.text_primary
        } else {
            theme.text_primary
        };

        text_renderer.draw_text(
            scene,
            glyph,
            ROW_TEXT_SIZE,
            fg,
            transform * Affine::translate((geo.icon_x, geo.text_y)),
            None,
        );
        let label = self.category.label();
        text_renderer.draw_text(
            scene,
            label,
            ROW_TEXT_SIZE,
            fg,
            transform * Affine::translate((geo.label_x, geo.text_y)),
            None,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
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
                        *handled = true;
                        // Fire selection on release (matches Button semantics):
                        // write the category's select key so the app's `sync`
                        // switches the active pane.
                        if !pressed {
                            let _ = state.put(root, self.select_key.as_str(), true);
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *handled = true;
                }
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
        Some(self.category.label())
    }

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

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
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
pub fn build(selected: Category, query: &str) -> Box<dyn Widget> {
    // An intrinsic-height column lays out as a block stack and ignores the
    // toolkit's inter-child gap, so we place explicit fixed-height spacers
    // to control the vertical rhythm.
    //
    // The sidebar draws **no app title and no search field**: the app name is
    // shown by the OS window-group pill (an app-drawn "Settings" header would
    // duplicate it), and the search field lives in the app's top strip so it
    // is reachable by keyboard focus (see [`crate::settings_app`]). The
    // sidebar starts directly with its first section, inset from the top.
    let mut col = Column::new().with_intrinsic_height().push(vgap(TITLE_GAP));

    let trimmed = query.trim();
    if trimmed.is_empty() {
        // Normal sectioned list. A clearly LARGER gap ABOVE each section
        // header than below it groups the rows beneath the header.
        for (i, section) in Section::ALL.iter().enumerate() {
            if i != 0 {
                col = col.push(vgap(SECTION_GAP_ABOVE));
            }
            col = col.push(section_header(section.label()));
            col = col.push(vgap(SECTION_GAP_BELOW));
            for category in Category::in_section(*section) {
                col = col.push(Box::new(SidebarRow::new(category, category == selected)));
            }
        }
    } else {
        // Filtered results — a flat list of matching categories under a
        // "RESULTS" header, or an honest empty state.
        col = col.push(section_header("Results"));
        col = col.push(vgap(SECTION_GAP_BELOW));
        let matches: Vec<Category> = Category::ALL
            .iter()
            .copied()
            .filter(|c| c.matches(trimmed))
            .collect();
        if matches.is_empty() {
            col = col.push(caption(&format!(
                "No settings match \u{201c}{trimmed}\u{201d}"
            )));
        } else {
            for category in matches {
                col = col.push(Box::new(SidebarRow::new(category, category == selected)));
            }
        }
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
        Text::new(&label.to_uppercase())
            .with_size(12.0)
            .with_color(Theme::dark().text_dim),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_rows(w: &dyn Widget) -> usize {
        let mut n = usize::from(w.name() == "SidebarRow");
        for c in w.children() {
            n += count_rows(c);
        }
        n
    }

    #[test]
    fn empty_query_lists_every_category() {
        let sb = build(Category::Network, "");
        assert_eq!(
            count_rows(sb.as_ref()),
            Category::ALL.len(),
            "the unfiltered sidebar lists all categories"
        );
    }

    #[test]
    fn query_filters_the_sidebar_to_matching_categories() {
        // "night" is a Displays-only keyword, so exactly one row survives.
        let sb = build(Category::Displays, "night");
        let expected = Category::ALL.iter().filter(|c| c.matches("night")).count();
        assert_eq!(count_rows(sb.as_ref()), expected);
        assert!(
            expected >= 1 && expected < Category::ALL.len(),
            "genuinely filtered"
        );
    }

    #[test]
    fn no_match_query_shows_no_rows() {
        let sb = build(Category::Network, "zzzznope");
        assert_eq!(
            count_rows(sb.as_ref()),
            0,
            "an empty result set lists no rows"
        );
    }

    #[test]
    fn row_is_addressable_by_category_label() {
        let row = SidebarRow::new(Category::MouseKeyboard, false);
        assert_eq!(Widget::test_id(&row), Some("Mouse & Keyboard"));
    }

    #[test]
    fn row_geometry_aligns_icon_and_label_inside_the_pill() {
        // A representative row rect (full sidebar width, one ROW_HEIGHT tall).
        let rect = Rect::new(0.0, 100.0, SIDEBAR_WIDTH, 100.0 + f64::from(ROW_HEIGHT));
        // A glyph narrower than the icon column — the common case.
        let glyph_w = 13.0;
        let g = RowGeometry::compute(rect, glyph_w);

        // The pill is inset evenly from every row edge.
        assert!((g.pill.x0 - (rect.x0 + ROW_INSET_X)).abs() < f64::EPSILON);
        assert!((g.pill.x1 - (rect.x1 - ROW_INSET_X)).abs() < f64::EPSILON);
        assert!((g.pill.y0 - (rect.y0 + ROW_INSET_Y)).abs() < f64::EPSILON);
        assert!((g.pill.y1 - (rect.y1 - ROW_INSET_Y)).abs() < f64::EPSILON);

        // Content starts one gutter inside the pill's left edge (even left
        // padding), and both icon and label sit inside the pill.
        assert!((g.content_left - (g.pill.x0 + ROW_CONTENT_GUTTER)).abs() < f64::EPSILON);
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

        // The label clears the icon column by exactly the icon/label gap, so
        // every label starts at the same x regardless of glyph width.
        assert!(
            (g.label_x - (g.content_left + ROW_ICON_COL + ROW_ICON_LABEL_GAP)).abs() < f64::EPSILON,
            "label starts one gap past the icon column"
        );

        // The text line box is vertically centred in the row (equal top/bottom
        // slack), so the content sits on the pill's optical centre.
        let line_box = f64::from(ROW_TEXT_SIZE) * LINE_HEIGHT_RATIO;
        let top_slack = g.text_y - rect.y0;
        let bottom_slack = rect.y1 - (g.text_y + line_box);
        assert!(
            (top_slack - bottom_slack).abs() < 0.001,
            "text line box must be vertically centred (top={top_slack}, bottom={bottom_slack})"
        );
    }

    #[test]
    fn row_geometry_label_start_is_glyph_width_independent() {
        // Two rows whose icon glyphs measure differently must still align their
        // labels to the same x — the point of the fixed icon column.
        let rect = Rect::new(0.0, 0.0, SIDEBAR_WIDTH, f64::from(ROW_HEIGHT));
        let narrow = RowGeometry::compute(rect, 8.0);
        let wide = RowGeometry::compute(rect, 18.0);
        assert!(
            (narrow.label_x - wide.label_x).abs() < f64::EPSILON,
            "labels must start at the same x regardless of glyph advance width"
        );
    }

    #[test]
    fn row_writes_select_key_on_release() {
        use automerge::{ReadDoc, Value};

        let mut row = SidebarRow::new(Category::About, false);
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
                1.0,
                &mods,
                &mut doc,
                &root,
                &mut handled,
                &mut focus,
                node,
                &taffy,
            );
        }

        let got = doc.get(&root, "settings.select.about");
        assert!(
            matches!(got, Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))),
            "releasing a click on the row writes its select key true"
        );
    }
}
