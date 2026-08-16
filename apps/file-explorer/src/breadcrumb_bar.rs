//! `BreadcrumbBar` — the compact path trail that sits directly under the
//! toolbar. It fills the available width and draws a leading themed vector
//! icon (Home / hard-drive) for the trail root followed by clickable text
//! segments joined by chevrons. Clicking a segment navigates to that
//! ancestor via the shared [`CommandSink`].
//!
//! Everything is drawn in Vello from the toolkit's [`Icon`] set and the
//! active [`Theme`] — no font glyphs, so nothing renders as tofu. Unlike a
//! stack of fixed-size `Button` pills (the previous breadcrumb, which
//! floated one oversized 120×40 chip alone under the toolbar), this is one
//! tight bar whose height is a single line of text plus padding, so the file
//! list starts immediately below it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::Scene;
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::color_compat::ColorCompat;
use ui::icons::{draw_icon, Icon, LUCIDE_DEFAULT_STROKE_PX};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::colors::ACCENT_PRIMARY_IDX;
use ui::widget::Widget;

use crate::command::{push, CommandSink, FileCommand};

/// Bar height in logical pixels — one line of `sm`-ish text plus padding.
pub const BAR_HEIGHT: f32 = 32.0;

const TEXT_SIZE: f32 = 13.0;
const ICON_PX: f64 = 16.0;
const LEFT_PAD: f64 = 2.0;
/// Gap between the leading root icon and the root label.
const ICON_GAP: f64 = 8.0;
/// Horizontal padding on each side of a chevron separator.
const SEP_PAD: f64 = 6.0;
const CHEVRON: &str = "›";
const ELLIPSIS: &str = "…";

/// What a laid-out slot paints.
enum Slot {
    /// The trail root, drawn as a leading vector glyph.
    Icon(Icon),
    /// A path component (or the ellipsis when `target` is `None`).
    Text(String),
}

/// One laid-out, hit-testable slot in the trail.
struct LaidSlot {
    slot: Slot,
    /// Content rect (used for both painting and hit-testing).
    rect: Rect,
    /// Navigation target; `None` for the leading icon and the ellipsis.
    target: Option<PathBuf>,
    /// Whether this slot is the current directory (rendered brighter).
    is_current: bool,
    /// Paint a chevron separator immediately before this slot.
    chevron_before: bool,
}

/// The breadcrumb bar widget. See the module docs.
pub struct BreadcrumbBar {
    /// `(label, navigation_target)` in trail order; the first is the root.
    segments: Vec<(String, PathBuf)>,
    sink: CommandSink,
    rect: Rect,
    /// Laid-out slots as of the last `draw`; drives the click hit-test.
    laid: Vec<LaidSlot>,
    /// Index into `laid` the cursor currently hovers (clickable only).
    hover: Option<usize>,
    last_cursor: Point,
    /// Set when a press landed on the trail but not on a navigable ancestor
    /// (the current segment, or the empty space after the trail) — the owner
    /// reads it via [`take_edit_requested`](Self::take_edit_requested) to switch
    /// the path bar into its editable-text mode.
    edit_requested: bool,
}

impl BreadcrumbBar {
    pub fn new(segments: Vec<(String, PathBuf)>, sink: CommandSink) -> Self {
        Self {
            segments,
            sink,
            rect: Rect::ZERO,
            laid: Vec::new(),
            hover: None,
            last_cursor: Point::ZERO,
            edit_requested: false,
        }
    }

    /// Replace the trail's segments (the owner calls this each frame with the
    /// current directory's components so a persistent bar tracks navigation).
    pub fn set_segments(&mut self, segments: Vec<(String, PathBuf)>) {
        self.segments = segments;
    }

    /// Place the bar at `rect` without a taffy pass — for an owner compositing
    /// the bar in its own immediate-mode chrome (the path bar).
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Rising-edge read of whether the last press asked to edit the path (a
    /// click on the current location or the empty trail area).
    pub fn take_edit_requested(&mut self) -> bool {
        std::mem::take(&mut self.edit_requested)
    }

    /// Root glyph: a house for `$HOME`, a hard-drive for the filesystem root.
    fn root_icon(&self) -> Icon {
        match self.segments.first() {
            Some((_, p)) if p == Path::new("/") => Icon::HardDrive,
            _ => Icon::Home,
        }
    }

    /// Recompute the laid-out slots against the current `rect`. Called from
    /// `draw` (which has the `TextRenderer`) so the click hit-test and paint
    /// pass always agree on geometry. Applies a tail-priority collapse: the
    /// root icon is always shown; when the full trail doesn't fit, leading
    /// components fold into a single `…` so the deepest (most relevant)
    /// components stay visible.
    fn relayout(&mut self, tr: &mut TextRenderer) {
        self.laid.clear();
        if self.segments.is_empty() {
            return;
        }
        let cy = self.rect.center().y;
        let text_h = TEXT_SIZE as f64;
        let (chevron_w, _) = tr.measure(CHEVRON, TEXT_SIZE, None);
        let chevron_w = chevron_w as f64;
        let sep_block = SEP_PAD + chevron_w + SEP_PAD;
        let last_idx = self.segments.len() - 1;
        let avail_right = self.rect.x1 - 4.0;

        // Leading root glyph — a decorative anchor (the root *label* right
        // after it carries the click, so the icon never reads as a stray,
        // ambiguously-clickable control).
        let mut x = self.rect.x0 + LEFT_PAD;
        self.laid.push(LaidSlot {
            slot: Slot::Icon(self.root_icon()),
            rect: Rect::new(x, cy - ICON_PX / 2.0, x + ICON_PX, cy + ICON_PX / 2.0),
            target: None,
            is_current: false,
            chevron_before: false,
        });
        x += ICON_PX + ICON_GAP;

        // Helper: push a text slot at the running x, advancing it.
        let push_text =
            |laid: &mut Vec<LaidSlot>, x: &mut f64, tr: &mut TextRenderer, label: &str,
             target: Option<PathBuf>, is_current: bool, chevron_before: bool| {
                if chevron_before {
                    *x += sep_block;
                }
                let (w, _) = tr.measure(label, TEXT_SIZE, None);
                laid.push(LaidSlot {
                    slot: Slot::Text(label.to_string()),
                    rect: Rect::new(*x, cy - text_h / 2.0, *x + w as f64, cy + text_h / 2.0),
                    target,
                    is_current,
                    chevron_before,
                });
                *x += w as f64;
            };

        // The root label always shows (no chevron — it pairs with the icon).
        let (root_label, root_path) = self.segments[0].clone();
        push_text(
            &mut self.laid, &mut x, tr, &root_label,
            Some(root_path), last_idx == 0, false,
        );

        // Components below the root, chevron-joined. Tail-priority collapse:
        // when the trail can't fit, fold leading components into a single `…`
        // so the deepest (most relevant) components stay on screen.
        let comps: Vec<(usize, String, f64)> = self.segments[1..]
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let (w, _) = tr.measure(label, TEXT_SIZE, None);
                (i + 1, label.clone(), w as f64)
            })
            .collect();
        let rest_needed: f64 = comps.iter().map(|(_, _, w)| sep_block + *w).sum();

        let mut show: Vec<usize> = comps.iter().map(|(i, _, _)| *i).collect();
        let mut collapsed = false;
        if x + rest_needed > avail_right && comps.len() > 1 {
            let (ell_w, _) = tr.measure(ELLIPSIS, TEXT_SIZE, None);
            let budget = avail_right - x - (sep_block + ell_w as f64);
            let mut used = 0.0;
            show.clear();
            for (i, _, w) in comps.iter().rev() {
                let need = sep_block + *w;
                if used + need > budget && !show.is_empty() {
                    collapsed = true;
                    break;
                }
                used += need;
                show.push(*i);
            }
            show.reverse();
            collapsed |= show.len() < comps.len();
        }

        if collapsed {
            push_text(&mut self.laid, &mut x, tr, ELLIPSIS, None, false, true);
        }
        for idx in show {
            let (label, target) = self.segments[idx].clone();
            push_text(
                &mut self.laid, &mut x, tr, &label,
                Some(target), idx == last_idx, true,
            );
        }
    }
}

impl Widget for BreadcrumbBar {
    fn sync(&mut self, _state: &mut automerge::AutoCommit, _path: &automerge::ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(BAR_HEIGHT),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
        // Rebuild geometry each frame so a resize re-triggers the collapse.
        self.relayout(text_renderer);

        let c = &theme.tokens.colors;
        let dim = c.text_dim;
        let primary = c.text_primary;
        let accent = c.accent[ACCENT_PRIMARY_IDX];

        // Hairline divider along the bottom, separating the bar from the
        // list below — the clean "toolbar / breadcrumb / list" banding.
        let divider = c.border_standard.with_alpha_factor(0.6);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            divider,
            None,
            &Line::new(
                Point::new(self.rect.x0, self.rect.y1 - 0.5),
                Point::new(self.rect.x1, self.rect.y1 - 0.5),
            ),
        );

        for (i, ls) in self.laid.iter().enumerate() {
            let hovered = self.hover == Some(i);
            let clickable = ls.target.is_some() && !ls.is_current;
            let color = if ls.is_current {
                primary
            } else if hovered && clickable {
                accent
            } else {
                dim
            };

            // Chevron separator, painted only where the layout marked it.
            if ls.chevron_before {
                let (cw, _) = text_renderer.measure(CHEVRON, TEXT_SIZE, None);
                let sep_x = ls.rect.x0 - SEP_PAD - cw as f64;
                let sep_y = crate::text_layout::centered_text_y(ls.rect.y0, ls.rect.height(), TEXT_SIZE);
                text_renderer.draw_text(
                    scene,
                    CHEVRON,
                    TEXT_SIZE,
                    dim,
                    transform * Affine::translate((sep_x, sep_y)),
                    None,
                );
            }

            match &ls.slot {
                Slot::Icon(icon) => {
                    draw_icon(
                        scene,
                        transform,
                        ls.rect,
                        *icon,
                        color,
                        LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
                Slot::Text(label) => {
                    let ty = crate::text_layout::centered_text_y(ls.rect.y0, ls.rect.height(), TEXT_SIZE);
                    text_renderer.draw_text(
                        scene,
                        label,
                        TEXT_SIZE,
                        color,
                        transform * Affine::translate((ls.rect.x0, ty)),
                        None,
                    );
                }
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut automerge::AutoCommit,
        _path: &automerge::ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Latch hover on clickable (non-current) slots so the accent
                // recolour + pointer affordance read as "this is a link".
                let mut new_hover = None;
                for (i, ls) in self.laid.iter().enumerate() {
                    let hot = Rect::new(
                        ls.rect.x0 - 3.0,
                        self.rect.y0,
                        ls.rect.x1 + 3.0,
                        self.rect.y1,
                    );
                    if ls.target.is_some() && !ls.is_current && hot.contains(self.last_cursor) {
                        new_hover = Some(i);
                        break;
                    }
                }
                if new_hover != self.hover {
                    self.hover = new_hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(i) = self.hover {
                    if let Some(Some(target)) = self.laid.get(i).map(|ls| ls.target.clone()) {
                        push(&self.sink, FileCommand::Navigate(target));
                        *handled = true;
                    }
                } else if self.rect.contains(self.last_cursor) {
                    // A press on the trail that isn't a navigable ancestor — the
                    // current location or the empty area after it — asks to edit
                    // the path (Finder/Nautilus: click the path to type one).
                    self.edit_requested = true;
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        for ls in &self.laid {
            if ls.target.is_some() && !ls.is_current {
                let hot = Rect::new(ls.rect.x0 - 3.0, self.rect.y0, ls.rect.x1 + 3.0, self.rect.y1);
                if hot.contains(point) {
                    return Some(winit::window::CursorIcon::Pointer);
                }
            }
        }
        // Over the rest of the trail (current location / empty area): a text
        // caret, hinting that a click opens the editable path field.
        if self.rect.contains(point) {
            return Some(winit::window::CursorIcon::Text);
        }
        None
    }

    fn name(&self) -> &str {
        "BreadcrumbBar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::new_sink;

    fn lay(bar: &mut BreadcrumbBar, width: f64) {
        bar.rect = Rect::new(0.0, 0.0, width, BAR_HEIGHT as f64);
        let mut tr = TextRenderer::new();
        bar.relayout(&mut tr);
    }

    #[test]
    fn root_icon_is_home_under_home_and_drive_under_root() {
        let home = BreadcrumbBar::new(
            vec![("Home".into(), PathBuf::from("/home/u"))],
            new_sink(),
        );
        assert_eq!(home.root_icon(), Icon::Home);
        let root = BreadcrumbBar::new(vec![("/".into(), PathBuf::from("/"))], new_sink());
        assert_eq!(root.root_icon(), Icon::HardDrive);
    }

    #[test]
    fn wide_bar_lays_out_icon_plus_every_component() {
        let mut bar = BreadcrumbBar::new(
            vec![
                ("Home".into(), PathBuf::from("/home/u")),
                ("Documents".into(), PathBuf::from("/home/u/Documents")),
                ("Projects".into(), PathBuf::from("/home/u/Documents/Projects")),
            ],
            new_sink(),
        );
        lay(&mut bar, 900.0);
        // Leading icon + root label + two components, none collapsed.
        assert_eq!(bar.laid.len(), 4);
        assert!(matches!(bar.laid[0].slot, Slot::Icon(_)));
        assert!(bar.laid[0].target.is_none(), "the icon is a decorative anchor");
        // The root *label* carries the click to home.
        assert!(matches!(&bar.laid[1].slot, Slot::Text(t) if t == "Home"));
        assert_eq!(bar.laid[1].target.as_deref(), Some(Path::new("/home/u")));
        // The deepest component is the current dir; nothing earlier is.
        assert!(bar.laid[3].is_current);
        assert!(!bar.laid[0].is_current && !bar.laid[1].is_current);
    }

    #[test]
    fn narrow_bar_collapses_leading_components_but_keeps_the_tail() {
        let mut bar = BreadcrumbBar::new(
            vec![
                ("Home".into(), PathBuf::from("/home/u")),
                ("a-really-long-directory-name".into(), PathBuf::from("/home/u/a")),
                ("another-long-one".into(), PathBuf::from("/home/u/a/b")),
                ("Projects".into(), PathBuf::from("/home/u/a/b/Projects")),
            ],
            new_sink(),
        );
        lay(&mut bar, 240.0);
        // The root icon always survives; at least one ellipsis appears; the
        // current (deepest) component is still shown.
        assert!(matches!(bar.laid[0].slot, Slot::Icon(_)));
        let has_ellipsis = bar
            .laid
            .iter()
            .any(|ls| matches!(&ls.slot, Slot::Text(t) if t == ELLIPSIS));
        assert!(has_ellipsis, "narrow trail must fold leading components into …");
        let shows_current = bar.laid.iter().any(|ls| ls.is_current);
        assert!(shows_current, "the current directory must always be visible");
        // Every laid slot fits within the bar's right edge.
        for ls in &bar.laid {
            assert!(ls.rect.x1 <= bar.rect.x1 + 0.5, "slot overflowed the bar");
        }
    }

    #[test]
    fn single_root_shows_only_the_icon_marked_current() {
        let mut bar =
            BreadcrumbBar::new(vec![("Home".into(), PathBuf::from("/home/u"))], new_sink());
        lay(&mut bar, 400.0);
        // Decorative icon + the root label (which is the current dir).
        assert_eq!(bar.laid.len(), 2);
        assert!(matches!(bar.laid[0].slot, Slot::Icon(_)));
        assert!(!bar.laid[0].is_current);
        assert!(matches!(&bar.laid[1].slot, Slot::Text(t) if t == "Home"));
        assert!(bar.laid[1].is_current, "root label is the current dir when alone");
    }
}
