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
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX, draw_icon};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::colors::ACCENT_PRIMARY_IDX;
use ui::widget::Widget;

use crate::command::{CommandSink, FileCommand, push};
use crate::i18n::{arg, t, t_args};

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
    /// The workspace mounts, so the root slot can wear the same emblem the
    /// places sidebar gives the same location. Empty until the owner says
    /// otherwise, which keeps every non-workspace trail as it was.
    workspace_roots: Vec<PathBuf>,
    /// Index into `laid` the cursor currently hovers (clickable only).
    hover: Option<usize>,
    last_cursor: Point,
    /// Set when a press landed on the trail but not on a navigable ancestor
    /// (the current segment, or the empty space after the trail) — the owner
    /// reads it via [`take_edit_requested`](Self::take_edit_requested) to switch
    /// the path bar into its editable-text mode.
    edit_requested: bool,
    /// Keyboard cursor over path components. The current component is a valid
    /// target: activating it switches to editable-path mode.
    keyboard_index: Option<usize>,
    focused: bool,
}

impl BreadcrumbBar {
    pub fn new(segments: Vec<(String, PathBuf)>, sink: CommandSink) -> Self {
        Self {
            segments,
            sink,
            rect: Rect::ZERO,
            laid: Vec::new(),
            workspace_roots: Vec::new(),
            hover: None,
            last_cursor: Point::ZERO,
            edit_requested: false,
            keyboard_index: None,
            focused: false,
        }
    }

    /// Replace the trail's segments (the owner calls this each frame with the
    /// current directory's components so a persistent bar tracks navigation).
    pub fn set_segments(&mut self, segments: Vec<(String, PathBuf)>) {
        self.segments = segments;
        self.keyboard_index = match (self.keyboard_index, self.segments.len()) {
            (_, 0) => None,
            (Some(index), len) => Some(index.min(len - 1)),
            (None, len) => Some(len - 1),
        };
    }

    /// Tell the bar which mounts are workspaces, so a trail rooted at one
    /// is marked as such rather than as a folder in somebody's home.
    pub fn set_workspace_roots(&mut self, roots: Vec<PathBuf>) {
        self.workspace_roots = roots;
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

    pub fn activate_keyboard_segment(&mut self) -> bool {
        let Some(index) = self.keyboard_index else {
            return false;
        };
        let Some((_, target)) = self.segments.get(index) else {
            return false;
        };
        if index + 1 == self.segments.len() {
            self.edit_requested = true;
        } else {
            push(&self.sink, FileCommand::Navigate(target.clone()));
        }
        true
    }

    /// Root glyph: the cloud for a workspace, a house for `$HOME`, a
    /// hard-drive for the filesystem root.
    ///
    /// The workspace emblem is the one the places sidebar already puts on
    /// the same location. A place that is reachable from two surfaces has
    /// to look like one place from both of them, or the trail is not
    /// telling the reader where the sidebar just took them.
    fn root_icon(&self) -> Icon {
        match self.segments.first() {
            Some((_, p)) if self.workspace_roots.iter().any(|r| r == p) => Icon::CloudDownload,
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
        let text_h = f64::from(TEXT_SIZE);
        let (chevron_w, _) = tr.measure(CHEVRON, TEXT_SIZE, None);
        let chevron_w = f64::from(chevron_w);
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
        let push_text = |laid: &mut Vec<LaidSlot>,
                         x: &mut f64,
                         tr: &mut TextRenderer,
                         label: &str,
                         target: Option<PathBuf>,
                         is_current: bool,
                         chevron_before: bool| {
            if chevron_before {
                *x += sep_block;
            }
            let (w, _) = tr.measure(label, TEXT_SIZE, None);
            laid.push(LaidSlot {
                slot: Slot::Text(label.to_string()),
                rect: Rect::new(*x, cy - text_h / 2.0, *x + f64::from(w), cy + text_h / 2.0),
                target,
                is_current,
                chevron_before,
            });
            *x += f64::from(w);
        };

        // The root label always shows (no chevron — it pairs with the icon).
        let Some((root_label, root_path)) = self.segments.first().cloned() else {
            return;
        };
        push_text(
            &mut self.laid,
            &mut x,
            tr,
            &root_label,
            Some(root_path),
            last_idx == 0,
            false,
        );

        // Components below the root, chevron-joined. Tail-priority collapse:
        // when the trail can't fit, fold leading components into a single `…`
        // so the deepest (most relevant) components stay on screen.
        let comps: Vec<(usize, String, f64)> = self
            .segments
            .get(1..)
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                let (w, _) = tr.measure(label, TEXT_SIZE, None);
                (i + 1, label.clone(), f64::from(w))
            })
            .collect();
        let rest_needed: f64 = comps.iter().map(|(_, _, w)| sep_block + *w).sum();

        let mut show: Vec<usize> = comps.iter().map(|(i, _, _)| *i).collect();
        let mut collapsed = false;
        if x + rest_needed > avail_right && comps.len() > 1 {
            let (ell_w, _) = tr.measure(ELLIPSIS, TEXT_SIZE, None);
            let budget = avail_right - x - (sep_block + f64::from(ell_w));
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
            let Some((label, target)) = self.segments.get(idx).cloned() else {
                continue;
            };
            push_text(
                &mut self.laid,
                &mut x,
                tr,
                &label,
                Some(target),
                idx == last_idx,
                true,
            );
        }
    }
}

impl Widget for BreadcrumbBar {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only fails on tree-internal bookkeeping errors, never on
        // caller input; there is none to report here.
        #[allow(
            clippy::unwrap_used,
            reason = "new_leaf cannot fail for a freshly built style"
        )]
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
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `node` was created by `build_layout` in this same tree, so a
        // lookup miss would mean the frame cycle called these out of order.
        #[allow(
            clippy::unwrap_used,
            reason = "node was built in this tree by build_layout; a miss means the frame cycle called these out of order"
        )]
        let l = taffy.layout(node).unwrap();
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
        // Rebuild geometry each frame so a resize re-triggers the collapse.
        self.relayout(text_renderer);

        let c = &theme.tokens.colors;
        let dim = c.text_dim;
        let primary = c.text_primary;
        let accent = c.accent[ACCENT_PRIMARY_IDX];

        // Hairline divider along the bottom, separating the bar from the
        // list below — the clean "toolbar / breadcrumb / list" banding. It is
        // derived against the base the bar sits on (HIG §9.5), so it reads on
        // the dark preset where a declared `border_standard` is one shade off.
        let divider = ui::surface::edge_on(c.surfaces.base);
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
                let sep_x = ls.rect.x0 - SEP_PAD - f64::from(cw);
                let sep_y = crate::text_layout::centered_text_y(
                    ls.rect.y0,
                    ls.rect.height(),
                    TEXT_SIZE,
                    text_renderer.text_scale(),
                );
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
                    let ty = crate::text_layout::centered_text_y(
                        ls.rect.y0,
                        ls.rect.height(),
                        TEXT_SIZE,
                        text_renderer.text_scale(),
                    );
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && self.focused =>
            {
                use winit::keyboard::{Key, NamedKey};
                let len = self.segments.len();
                let current = self.keyboard_index.unwrap_or_else(|| len.saturating_sub(1));
                match event.logical_key {
                    Key::Named(NamedKey::ArrowLeft) => {
                        self.keyboard_index = (len > 0).then_some(current.saturating_sub(1));
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        self.keyboard_index =
                            (len > 0).then_some((current + 1).min(len.saturating_sub(1)));
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Home) => {
                        self.keyboard_index = (len > 0).then_some(0);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::End) => {
                        self.keyboard_index = (len > 0).then_some(len - 1);
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        *ctx.handled = self.activate_keyboard_segment();
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    *ctx.handled = true;
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
                        *ctx.handled = true;
                    }
                } else if self.rect.contains(self.last_cursor) {
                    // A press on the trail that isn't a navigable ancestor — the
                    // current location or the empty area after it — asks to edit
                    // the path (Finder/Nautilus: click the path to type one).
                    self.edit_requested = true;
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        for ls in &self.laid {
            if ls.target.is_some() && !ls.is_current {
                let hot = Rect::new(
                    ls.rect.x0 - 3.0,
                    self.rect.y0,
                    ls.rect.x1 + 3.0,
                    self.rect.y1,
                );
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

    fn name(&self) -> &'static str {
        "BreadcrumbBar"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused && self.keyboard_index.is_none() && !self.segments.is_empty() {
            self.keyboard_index = Some(self.segments.len() - 1);
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let count = self.segments.len();
        let children = self
            .segments
            .iter()
            .enumerate()
            .map(|(index, (label, path))| {
                let current = index + 1 == count;
                AccessNode::new(if current {
                    Role::StaticText
                } else {
                    Role::Link
                })
                .with_name(label.clone())
                .with_description(if current {
                    t_args(
                        "files-a11y-current-breadcrumb",
                        &[arg("path", path.display().to_string())],
                    )
                } else {
                    t_args(
                        "files-a11y-open-folder",
                        &[arg("path", path.display().to_string())],
                    )
                })
                .with_position(index + 1, count)
                .with_state(AccessState {
                    focusable: true,
                    focused: self.focused && self.keyboard_index == Some(index),
                    selected: current,
                    ..Default::default()
                })
            })
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("files-a11y-breadcrumbs"))
                .with_optional_value(
                    self.keyboard_index
                        .and_then(|index| self.segments.get(index).map(|(label, _)| label.clone())),
                )
                .with_state(AccessState {
                    focusable: true,
                    focused: self.focused,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        matches!(action, ui::a11y::Action::Activate) && self.activate_keyboard_segment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::new_sink;

    fn lay(bar: &mut BreadcrumbBar, width: f64) {
        bar.rect = Rect::new(0.0, 0.0, width, f64::from(BAR_HEIGHT));
        let mut tr = TextRenderer::new();
        bar.relayout(&mut tr);
    }

    /// The root icon, wide and narrow layouts, single-root state and the accessible keyboard-driven trail.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn breadcrumb_bar_lays_out_collapses_and_is_accessible() {
        // --- phase: root_icon_is_home_under_home_and_drive_under_root ---
        {
            let home =
                BreadcrumbBar::new(vec![("Home".into(), PathBuf::from("/home/u"))], new_sink());
            assert_eq!(home.root_icon(), Icon::Home);
            let root = BreadcrumbBar::new(vec![("/".into(), PathBuf::from("/"))], new_sink());
            assert_eq!(root.root_icon(), Icon::HardDrive);
        }

        // --- phase: wide_bar_lays_out_icon_plus_every_component ---
        {
            let mut bar = BreadcrumbBar::new(
                vec![
                    ("Home".into(), PathBuf::from("/home/u")),
                    ("Documents".into(), PathBuf::from("/home/u/Documents")),
                    (
                        "Projects".into(),
                        PathBuf::from("/home/u/Documents/Projects"),
                    ),
                ],
                new_sink(),
            );
            lay(&mut bar, 900.0);
            // Leading icon + root label + two components, none collapsed.
            assert_eq!(bar.laid.len(), 4);
            assert!(matches!(bar.laid[0].slot, Slot::Icon(_)));
            assert!(
                bar.laid[0].target.is_none(),
                "the icon is a decorative anchor"
            );
            // The root *label* carries the click to home.
            assert!(matches!(&bar.laid[1].slot, Slot::Text(t) if t == "Home"));
            assert_eq!(bar.laid[1].target.as_deref(), Some(Path::new("/home/u")));
            // The deepest component is the current dir; nothing earlier is.
            assert!(bar.laid[3].is_current);
            assert!(!bar.laid[0].is_current && !bar.laid[1].is_current);
        }

        // --- phase: narrow_bar_collapses_leading_components_but_keeps_the_tail ---
        {
            let mut bar = BreadcrumbBar::new(
                vec![
                    ("Home".into(), PathBuf::from("/home/u")),
                    (
                        "a-really-long-directory-name".into(),
                        PathBuf::from("/home/u/a"),
                    ),
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
            assert!(
                has_ellipsis,
                "narrow trail must fold leading components into …"
            );
            let shows_current = bar.laid.iter().any(|ls| ls.is_current);
            assert!(
                shows_current,
                "the current directory must always be visible"
            );
            // Every laid slot fits within the bar's right edge.
            for ls in &bar.laid {
                assert!(ls.rect.x1 <= bar.rect.x1 + 0.5, "slot overflowed the bar");
            }
        }

        // --- phase: single_root_shows_only_the_icon_marked_current ---
        {
            let mut bar =
                BreadcrumbBar::new(vec![("Home".into(), PathBuf::from("/home/u"))], new_sink());
            lay(&mut bar, 400.0);
            // Decorative icon + the root label (which is the current dir).
            assert_eq!(bar.laid.len(), 2);
            assert!(matches!(bar.laid[0].slot, Slot::Icon(_)));
            assert!(!bar.laid[0].is_current);
            assert!(matches!(&bar.laid[1].slot, Slot::Text(t) if t == "Home"));
            assert!(
                bar.laid[1].is_current,
                "root label is the current dir when alone"
            );
        }

        // --- phase: accessibility_exposes_trail_and_activation_uses_keyboard_cursor ---
        {
            use ui::a11y::{Action, Role};

            let sink = new_sink();
            let mut bar = BreadcrumbBar::new(
                vec![
                    ("Home".into(), PathBuf::from("/home/u")),
                    ("Documents".into(), PathBuf::from("/home/u/Documents")),
                    ("Reports".into(), PathBuf::from("/home/u/Documents/Reports")),
                ],
                sink.clone(),
            );
            bar.set_focus(true);
            bar.keyboard_index = Some(1);
            let node = bar.accessibility().expect("breadcrumbs have semantics");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("Breadcrumbs"));
            assert_eq!(node.children.len(), 3);
            assert_eq!(node.children[0].role, Role::Link);
            assert_eq!(node.children[2].role, Role::StaticText);
            assert!(node.children[1].state.focused);
            assert!(node.children[2].state.selected);

            let mut doc = automerge::AutoCommit::new();
            assert!(bar.perform_action(&Action::Activate, &mut doc, &automerge::ROOT));
            assert!(matches!(
                sink.borrow().last(),
                Some(FileCommand::Navigate(path)) if path == Path::new("/home/u/Documents")
            ));

            bar.keyboard_index = Some(2);
            assert!(bar.perform_action(&Action::Activate, &mut doc, &automerge::ROOT));
            assert!(
                bar.take_edit_requested(),
                "current crumb enters path editing"
            );
        }
    }
}
