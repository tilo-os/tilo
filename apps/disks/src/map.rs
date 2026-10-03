//! The partition map: one drive drawn as a proportional bar.
//!
//! Genuine content drawing. The bar is the data (each partition's share of the
//! device), so it paints itself; everything around it is the toolkit's: the
//! "Whole disk" chip is a [`ChipFace`], the legend is measured by the renderer
//! and wraps, and every colour, radius and gap comes from the theme.
//! Partitions take their colour from the presence series, the same one every
//! chart uses.

use std::collections::HashMap;

use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::taffy::style_helpers::{auto, percent};
use ui::taffy::{NodeId, Style, TaffyTree};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Point, Rect};
use ui::vello::peniko::Color;
use ui::widget::{AppModel, ChipFace, ChipLead, EventCtx, ModelPath, TaffyTreeExt, Widget};
use ui::winit::event::{ElementState, MouseButton, WindowEvent};
use ui::winit::window::CursorIcon;

/// One block of the bar: a partition, or the unallocated tail.
#[derive(Clone, Debug, PartialEq)]
pub struct MapEntry {
    /// Index into the drive's partitions; `None` for the unallocated tail.
    pub partition: Option<usize>,
    /// Kernel name (`nvme0n1p2`), for the accessible name and the action target.
    pub device: String,
    /// What the legend calls it (`system 214.7 GB`).
    pub label: String,
    /// Bytes the block spans.
    pub size: u64,
}

/// What the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hover {
    Whole,
    Segment(usize),
    Legend(usize),
}

/// Where everything landed for one width, relative to the widget's own origin.
#[derive(Clone, Debug, Default)]
struct Geometry {
    chip: Rect,
    track: Rect,
    segments: Vec<Rect>,
    legend: Vec<Rect>,
    height: f64,
}

/// The partition map widget. Selecting a block (or the chip, which means "no
/// partition") reports through `on_select`.
pub struct PartitionMap {
    entries: Vec<MapEntry>,
    selected: Option<usize>,
    on_select: Box<dyn FnMut(Option<usize>)>,
    theme: Theme,
    rect: Rect,
    cursor: Point,
    hover: Option<Hover>,
    geometry: Geometry,
}

impl PartitionMap {
    pub fn new(
        entries: Vec<MapEntry>,
        selected: Option<usize>,
        on_select: impl FnMut(Option<usize>) + 'static,
    ) -> Self {
        Self {
            entries,
            selected,
            on_select: Box::new(on_select),
            theme: Theme::dark(),
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            hover: None,
            geometry: Geometry::default(),
        }
    }

    fn is_selected(&self, entry: &MapEntry) -> bool {
        entry.partition.is_some() && entry.partition == self.selected
    }

    /// Lay everything out for `width`, relative to the widget's origin.
    fn lay_out(&self, width: f64, text: &mut TextRenderer, theme: &Theme) -> Geometry {
        let spacing = &theme.tokens.spacing;
        let gap = f64::from(spacing.rhythm.sibling);
        let chip_h = ChipFace::height(theme);
        let chip_w = ChipFace::intrinsic_width(
            "Whole disk",
            ChipLead::Glyph(Icon::HardDrive),
            false,
            text,
            theme,
        );
        let chip = Rect::new(0.0, 0.0, chip_w.min(width), chip_h);

        let track_h = f64::from(spacing.s12);
        let track = Rect::new(0.0, chip.y1 + gap, width, chip.y1 + gap + track_h);

        // Proportional widths with a floor so a sliver stays visible and
        // clickable, then squeezed back into the track if the floors overflow.
        let seam = f64::from(spacing.s1) / 2.0;
        let floor = f64::from(spacing.s2);
        let total: f64 = self
            .entries
            .iter()
            .map(|e| e.size as f64)
            .sum::<f64>()
            .max(1.0);
        let mut widths: Vec<f64> = self
            .entries
            .iter()
            .map(|e| (e.size as f64 / total * width).max(floor))
            .collect();
        let used: f64 = widths.iter().sum();
        if used > width && used > 0.0 {
            let scale = width / used;
            for w in &mut widths {
                *w *= scale;
            }
        }
        let mut x = 0.0;
        let segments: Vec<Rect> = widths
            .iter()
            .map(|w| {
                let rect = Rect::new(x, track.y0, (x + w - seam).max(x + 1.0), track.y1);
                x += w;
                rect
            })
            .collect();

        let legend_top = track.y1 + gap;
        let legend = self.lay_out_legend(width, legend_top, text, theme);
        let height = legend.iter().map(|r| r.y1).fold(track.y1, f64::max);
        Geometry {
            chip,
            track,
            segments,
            legend,
            height,
        }
    }

    /// One entry per block (swatch, then its label in `Caption`), measured by
    /// the renderer and wrapped before the trailing edge so a long label never
    /// overlaps its neighbour.
    fn lay_out_legend(
        &self,
        width: f64,
        top: f64,
        text: &mut TextRenderer,
        theme: &Theme,
    ) -> Vec<Rect> {
        let style = theme.text(TextRole::Caption);
        let swatch = f64::from(theme.tokens.spacing.s2);
        let gap = f64::from(theme.tokens.spacing.rhythm.section);
        let row = f64::from(text.measure_style("Ag", None, &style).1)
            + f64::from(theme.tokens.spacing.rhythm.tight);
        let available = width.max(1.0);
        let mut x = 0.0;
        let mut y = top;
        self.entries
            .iter()
            .map(|entry| {
                let label = f64::from(text.measure_style(&entry.label, None, &style).0);
                let entry_w = (swatch + f64::from(theme.tokens.metrics.icon_label_gap) + label)
                    .ceil()
                    .min(available);
                if x > 0.0 && x + entry_w > width {
                    x = 0.0;
                    y += row;
                }
                let rect = Rect::new(x, y, x + entry_w, y + row);
                x += entry_w + gap;
                rect
            })
            .collect()
    }

    fn paint_legend_entry(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        colour: Color,
        label: &str,
        hovered: bool,
    ) {
        let theme = ctx.theme;
        let style = theme.text(TextRole::Caption);
        let swatch = f64::from(theme.tokens.spacing.s2);
        let gap = f64::from(theme.tokens.metrics.icon_label_gap);
        let centre = rect.center().y;
        let dot =
            Rect::from_center_size(Point::new(rect.x0 + swatch / 2.0, centre), (swatch, swatch));
        ctx.fill_round(dot, f64::from(theme.tokens.radii.full), colour);
        let room = (rect.width() - swatch - gap).max(0.0) as f32;
        let shown = ctx.text.elide_to_width_style(label, room, &style);
        let height = f64::from(ctx.text.measure_style(&shown, None, &style).1);
        let ink = if hovered {
            theme.text_primary
        } else {
            theme.text_dim
        };
        ctx.text.draw_style(
            ctx.scene,
            &shown,
            ink,
            ctx.transform * Affine::translate((rect.x0 + swatch + gap, centre - height / 2.0)),
            None,
            &style,
        );
    }

    fn colour_of(theme: &Theme, index: usize, entry: &MapEntry) -> Color {
        let colors = &theme.tokens.colors;
        if entry.partition.is_some() {
            colors.presence.series(index)
        } else {
            colors.lift_on(colors.surfaces.raised, 1.0)
        }
    }

    fn hit(&self, p: Point) -> Option<Hover> {
        let g = &self.geometry;
        let local = Point::new(p.x - self.rect.x0, p.y - self.rect.y0);
        if g.chip.contains(local) {
            return Some(Hover::Whole);
        }
        if let Some(i) = g.segments.iter().position(|r| r.contains(local)) {
            return Some(Hover::Segment(i));
        }
        g.legend
            .iter()
            .position(|r| r.contains(local))
            .map(Hover::Legend)
    }

    fn choose(&mut self, hover: Hover) {
        match hover {
            Hover::Whole => (self.on_select)(None),
            Hover::Segment(i) | Hover::Legend(i) => {
                if let Some(partition) = self.entries.get(i).and_then(|e| e.partition) {
                    (self.on_select)(Some(partition));
                }
            }
        }
    }
}

impl Widget for PartitionMap {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = theme.clone();
        let node = taffy.leaf_node(Style {
            size: ui::taffy::Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: ui::taffy::Size<Option<f32>>,
        available: ui::taffy::Size<ui::taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> ui::taffy::Size<f32> {
        let width = known
            .width
            .or(match available.width {
                ui::taffy::AvailableSpace::Definite(width) => Some(width),
                _ => None,
            })
            .unwrap_or(480.0)
            .max(1.0);
        let theme = self.theme.clone();
        let geometry = self.lay_out(f64::from(width), text_renderer, &theme);
        ui::taffy::Size {
            width,
            height: geometry.height as f32,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
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
        let geometry = self.lay_out(self.rect.width(), text_renderer, theme);
        self.geometry = geometry.clone();
        let origin = self.rect.origin().to_vec2();
        let at = |r: Rect| r + origin;
        let colors = &theme.tokens.colors;
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);

        // "Whole disk": a chip, lifted while it is what the actions target.
        ChipFace::new(at(geometry.chip), "Whole disk")
            .lead(ChipLead::Glyph(Icon::HardDrive))
            .selected(self.selected.is_none())
            .hovered(self.hover == Some(Hover::Whole))
            .paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);

        // The device: a track, and each partition's share of it.
        let radii = &theme.tokens.radii;
        let track = at(geometry.track);
        ctx.fill_round(track, f64::from(radii.control), colors.surfaces.raised);
        for (i, (entry, segment)) in self.entries.iter().zip(&geometry.segments).enumerate() {
            let hovered =
                matches!(self.hover, Some(Hover::Segment(h) | Hover::Legend(h)) if h == i);
            let base = Self::colour_of(theme, i, entry);
            let fill = if hovered {
                colors.lift_on(base, 0.5)
            } else {
                base
            };
            let rect = at(*segment);
            let radius = f64::from(radii.item);
            ctx.fill_round(rect, radius, fill);
            if self.is_selected(entry) {
                let ring = f64::from(theme.tokens.metrics.chrome_rule_px) * 2.0;
                ctx.stroke_round(rect, radius, colors.text_primary, ring);
            }
        }

        // The legend names every block, including the ones too thin to label.
        for (i, (entry, rect)) in self.entries.iter().zip(&geometry.legend).enumerate() {
            let colour = Self::colour_of(theme, i, entry);
            let hovered =
                matches!(self.hover, Some(Hover::Segment(h) | Hover::Legend(h)) if h == i);
            Self::paint_legend_entry(&mut ctx, at(*rect), colour, &entry.label, hovered);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hover = self.hit(self.cursor);
            }
            WindowEvent::CursorLeft { .. } => self.hover = None,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(hover) = self.hit(self.cursor) {
                    self.choose(hover);
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<CursorIcon> {
        self.hit(point).map(|_| CursorIcon::Pointer)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let g = &self.geometry;
        let origin = self.rect.origin().to_vec2();
        let mut group = AccessNode::new(Role::Group)
            .with_name("Partition map")
            .with_hashed_key("disks.partition-map")
            .with_bounds(self.rect);
        group.children.push(
            AccessNode::new(Role::Button)
                .with_name("Select whole disk")
                .with_bounds(g.chip + origin)
                .with_action_target("disks.part:whole")
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .selected(self.selected.is_none()),
                ),
        );
        for (entry, rect) in self.entries.iter().zip(&g.segments) {
            if entry.partition.is_none() {
                continue;
            }
            group.children.push(
                AccessNode::new(Role::Button)
                    .with_name(format!("Select partition {}", entry.device))
                    .with_description(entry.label.clone())
                    .with_bounds(*rect + origin)
                    .with_action_target(format!("disks.part:{}", entry.device))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(self.is_selected(entry)),
                    ),
            );
        }
        Some(group)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, A11yAction::Activate) {
            return matches!(action, A11yAction::Focus);
        }
        let Some(name) = target.strip_prefix("disks.part:") else {
            return false;
        };
        if name == "whole" {
            (self.on_select)(None);
            return true;
        }
        let Some(partition) = self
            .entries
            .iter()
            .find(|e| e.device == name)
            .and_then(|e| e.partition)
        else {
            return false;
        };
        (self.on_select)(Some(partition));
        true
    }

    fn name(&self) -> &'static str {
        "PartitionMap"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use ui::headless::Framed;

    type Picks = Rc<RefCell<Vec<Option<usize>>>>;

    fn entries() -> Vec<MapEntry> {
        let block = |partition: Option<usize>, device: &str, label: &str, size: u64| MapEntry {
            partition,
            device: device.into(),
            label: label.into(),
            size,
        };
        vec![
            block(Some(0), "sda1", "EFI System 536.9 MB", 536_870_912),
            block(Some(1), "sda2", "system 214.7 GB", 214_748_364_800),
            block(Some(2), "sda3", "home 750.0 GB", 750_000_000_000),
            block(None, "", "Unallocated 17.7 GB", 17_700_000_000),
        ]
    }

    fn map(selected: Option<usize>, width: f64) -> (Framed<PartitionMap>, Picks) {
        let picks: Picks = Rc::default();
        let sink = Rc::clone(&picks);
        let map = PartitionMap::new(entries(), selected, move |part| {
            sink.borrow_mut().push(part);
        });
        let app = Framed::new(
            ui::State::new(),
            map,
            ui::headless::viewport((width, 400.0)),
            Theme::dark(),
        );
        (app, picks)
    }

    fn absolute(map: &PartitionMap, r: Rect) -> Point {
        let c = r.center();
        Point::new(map.rect.x0 + c.x, map.rect.y0 + c.y)
    }

    /// The bar's blocks tile the track in order, never overflowing it, and a sliver stays a target.
    #[test]
    fn blocks_tile_the_track_and_slivers_stay_clickable() {
        let (app, _) = map(None, 900.0);
        let g = &app.geometry;
        assert_eq!(g.segments.len(), 4);
        for pair in g.segments.windows(2) {
            assert!(pair[0].x1 <= pair[1].x0, "blocks do not overlap: {pair:?}");
        }
        let last = g.segments.last().unwrap();
        assert!(
            last.x1 <= g.track.x1 + 0.5,
            "the last block ends inside the track"
        );
        let sliver = g.segments.first().unwrap();
        assert!(
            sliver.width() >= 4.0,
            "a 0.05 % partition is still a visible target, got {}",
            sliver.width()
        );
        assert!(
            g.chip.y1 < g.track.y0,
            "the chip sits above the bar with a gap"
        );
        assert!(
            g.legend.iter().all(|r| r.y0 >= g.track.y1),
            "the legend sits below the bar"
        );
    }

    /// A long legend wraps instead of running off the edge or overprinting.
    #[test]
    fn the_legend_wraps_at_narrow_widths() {
        let (wide, _) = map(None, 900.0);
        let (narrow, _) = map(None, 260.0);
        let rows = |g: &Geometry| {
            let mut tops: Vec<i64> = g.legend.iter().map(|r| r.y0 as i64).collect();
            tops.dedup();
            tops.len()
        };
        assert_eq!(rows(&wide.geometry), 1, "four short entries fit one row");
        assert!(
            rows(&narrow.geometry) > 1,
            "they wrap when the width runs out"
        );
        assert!(
            narrow.geometry.legend.iter().all(|r| r.x1 <= 260.5),
            "no entry runs past the trailing edge"
        );
        assert!(narrow.geometry.height > wide.geometry.height);
    }

    /// Choosing a block, its legend entry or the chip reports the partition, or none for the whole disk.
    #[test]
    fn blocks_legend_entries_and_the_chip_report_the_choice() {
        let (mut app, picks) = map(Some(1), 900.0);
        let segment = absolute(&app, app.geometry.segments[2]);
        Framed::harness_mut(&mut app).click_at(segment.x, segment.y);
        let legend = absolute(&app, app.geometry.legend[0]);
        Framed::harness_mut(&mut app).click_at(legend.x, legend.y);
        let chip = absolute(&app, app.geometry.chip);
        Framed::harness_mut(&mut app).click_at(chip.x, chip.y);
        // The unallocated tail is not a partition: choosing it selects nothing.
        let tail = absolute(&app, app.geometry.segments[3]);
        Framed::harness_mut(&mut app).click_at(tail.x, tail.y);
        assert_eq!(*picks.borrow(), vec![Some(2), Some(0), None]);
    }

    /// Every partition is a named, activatable control for assistive technology and agents.
    #[test]
    fn partitions_are_exposed_as_named_controls() {
        let (mut app, picks) = map(Some(1), 900.0);
        let node = Widget::accessibility(&*app).expect("the map exports a group");
        let names: Vec<_> = node
            .children
            .iter()
            .filter_map(|c| c.name.clone())
            .collect();
        assert_eq!(
            names,
            [
                "Select whole disk",
                "Select partition sda1",
                "Select partition sda2",
                "Select partition sda3"
            ]
        );
        let selected: Vec<_> = node.children.iter().map(|c| c.state.selected).collect();
        assert_eq!(
            selected,
            [false, false, true, false],
            "the chosen partition reports selected"
        );
        let mut doc = ui::automerge::AutoCommit::new();
        assert!(Widget::perform_semantic_action(
            &mut *app,
            "disks.part:sda3",
            &A11yAction::Activate,
            &mut doc,
            &ui::automerge::ROOT,
        ));
        assert_eq!(*picks.borrow(), vec![Some(2)]);
    }
}
