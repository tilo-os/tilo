//! What is wrong with one laid-out window: read off the toolkit's inspection of the frame (its
//! widgets' boxes, clips and semantics) and, for text, the widgets' own measurements.

use ui::headless::InspectionNode;
use ui::kurbo::Rect;
use ui::widget::Widget;

/// A kind of layout problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Content reaches past the box it is in, and is cut off or spills over what is next to it.
    Overflow,
    /// Text needs more room than its box gives it, so part of it is not shown.
    ClippedText,
    /// Two siblings cover each other.
    Overlap,
    /// Something a person presses or types into is smaller than the design system's minimum.
    SmallHitTarget,
    /// Content is wider than the window or its scroll view, so the page scrolls sideways.
    ScrollsSideways,
    /// An edge between two device pixels at the display's scale, drawn blurred across both.
    OffPixel,
}

impl Kind {
    pub const ALL: [Self; 6] = [
        Self::Overflow,
        Self::ClippedText,
        Self::Overlap,
        Self::SmallHitTarget,
        Self::ScrollsSideways,
        Self::OffPixel,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::ClippedText => "clipped-text",
            Self::Overlap => "overlap",
            Self::SmallHitTarget => "small-hit-target",
            Self::ScrollsSideways => "scrolls-sideways",
            Self::OffPixel => "off-pixel",
        }
    }
}

/// One problem in one frame, at the widget it is about.
#[derive(Clone, Debug)]
pub struct Problem {
    pub kind: Kind,
    /// The keys of the widgets at and above it that a view's nodes made, innermost first.
    pub keys: Vec<String>,
    /// The toolkit widget it is about.
    pub widget: String,
    /// That widget's own test id, if it has one.
    pub id: Option<String>,
    /// What is wrong, in logical pixels.
    pub detail: String,
}

/// Slack for rounding: layout places edges on fractions of a pixel.
const EPSILON: f64 = 1.0;

/// How far from a device pixel an edge may stand, in device pixels: what the toolkit's single
/// precision coordinates carry, far under anything a person could see.
const PIXEL_SLACK: f64 = 0.01;

fn rect(r: [f64; 4]) -> Rect {
    Rect::new(r[0], r[1], r[0] + r[2], r[1] + r[3])
}

fn has_area(r: Rect) -> bool {
    r.width() > EPSILON && r.height() > EPSILON
}

fn size(r: Rect) -> String {
    format!("{:.0}×{:.0}", r.width(), r.height())
}

/// Which ways a box lets its content go on past it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Scrolls {
    /// Content may go on past its bottom (a column's body, a list).
    down: bool,
    /// Content may go on past its sides (a board's lanes in `ui.Scroll(across: true)`).
    across: bool,
}

/// Which ways `node` scrolls its content: as a scroll view says of its axis, else down for a
/// widget that scrolls by its kind (a long list).
fn scrolls(node: &InspectionNode) -> Scrolls {
    use ui::widget::{LayoutInfo, ScrollDirection};
    if let Some(LayoutInfo::Scroll(scroll)) = &node.layout {
        return match scroll.direction {
            ScrollDirection::Vertical => Scrolls {
                down: true,
                across: false,
            },
            ScrollDirection::Horizontal => Scrolls {
                down: false,
                across: true,
            },
            ScrollDirection::Both => Scrolls {
                down: true,
                across: true,
            },
        };
    }
    let lower = node.widget.to_ascii_lowercase();
    let down = lower.contains("scroll")
        || lower.contains("virtuallist")
        || lower.contains("describedlist");
    Scrolls {
        down,
        across: false,
    }
}

/// Whether a widget hangs part of itself outside the column it is in by design: the body
/// editor's handle gutter sits in the margin, so its text lines up with the title above it.
fn hangs(widget: &str) -> bool {
    matches!(widget, "TilolangBody")
}

/// Whether a widget floats over the content it belongs to, shown only on hover or focus (a
/// row's hover toolbar over its trailing top corner): neither overflow nor overlap.
fn floats(widget: &str) -> bool {
    matches!(widget, "HoverToolbarFace")
}

/// Whether a node shows text: its own, or its children's.
fn has_text(node: &InspectionNode) -> bool {
    use ui::a11y::Role;
    node.widget == "Text"
        || node.semantics.as_ref().is_some_and(|access| {
            matches!(
                access.role,
                Role::StaticText | Role::Label | Role::Heading { .. }
            )
        })
        || node.children.iter().any(has_text)
}

/// Whether a widget lays its children over each other on purpose.
fn layers(widget: &str) -> bool {
    matches!(
        widget,
        "Layers"
            | "NavShell"
            | "NavPane"
            | "Pane"
            | "TilolangWindow"
            | "Decorated"
            | "Toast"
            | "Banner"
    ) || widget.contains("Overlay")
        || widget.contains("Layer")
}

/// Whether a node is one control a person presses: its own role says so, or it is a box (a
/// header action's frame) around exactly one.
fn is_control(node: &InspectionNode) -> bool {
    if node
        .semantics
        .as_ref()
        .is_some_and(|access| interactive(access.role) && !access.state.disabled)
    {
        return true;
    }
    match node.children.as_slice() {
        [only] => is_control(only),
        _ => false,
    }
}

/// Roles a person presses, picks or types into.
fn interactive(role: ui::a11y::Role) -> bool {
    use ui::a11y::Role;
    matches!(
        role,
        Role::Button
            | Role::ToggleButton
            | Role::Checkbox
            | Role::RadioButton
            | Role::Switch
            | Role::Slider
            | Role::SpinButton
            | Role::ComboBox
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::Link
            | Role::Tab
            | Role::MenuItem
            | Role::ListBoxOption
            | Role::TreeItem
    )
}

struct Walk<'a> {
    viewport: Rect,
    /// Inside a box already found reaching past the window's sides.
    beyond: bool,
    /// Inside a view that scrolls sideways: its content reaches past the window by design.
    across: bool,
    /// The smallest a target may be, where the face is the target (on touch).
    min_target: Option<f64>,
    /// Device pixels per logical pixel.
    display_scale: f64,
    /// Inside a box already found off the device pixels: what it holds moves with it.
    off_pixel: bool,
    out: &'a mut Vec<Problem>,
}

/// Every problem of the frame `root`, laid out in `viewport`, where a target needs `min_target`
/// logical pixels on each side. On a fine pointer the toolkit grows each control's hit area
/// about its centre to the minimum, so targets are checked where the face itself must be the
/// target: on touch, where faces grow to it. At `display_scale` device pixels per logical
/// pixel, every edge must stand on a device pixel.
pub fn inspect(
    root: &InspectionNode,
    viewport: (u32, u32),
    min_target: Option<f64>,
    display_scale: f64,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let viewport = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
    let mut walk = Walk {
        viewport,
        beyond: false,
        across: false,
        min_target,
        display_scale,
        off_pixel: false,
        out: &mut out,
    };
    walk.node(
        root,
        &mut Vec::new(),
        viewport,
        viewport,
        Scrolls::default(),
    );
    out
}

impl Walk<'_> {
    /// `parent`: the nearest box above with geometry. `clip`: what of the window the node can show in. `scroller`:
    /// which ways the box above lets content go on past it.
    fn node(
        &mut self,
        node: &InspectionNode,
        keys: &mut Vec<String>,
        parent: Rect,
        clip: Rect,
        scroller: Scrolls,
    ) {
        let pushed = node.test_id.clone().map(|id| keys.push(id)).is_some();
        self.visit(node, keys, parent, clip, scroller);
        if pushed {
            keys.pop();
        }
    }

    fn visit(
        &mut self,
        node: &InspectionNode,
        keys: &mut Vec<String>,
        parent: Rect,
        clip: Rect,
        scroller: Scrolls,
    ) {
        let own = rect(node.rect);
        let geometric = has_area(own);
        // What of the window is not shown at all needs no checking: a column pushed aside, a
        // row scrolled out of view.
        if geometric && !has_area(own.intersect(clip)) {
            return;
        }
        let was_beyond = self.beyond;
        let was_across = self.across;
        let was_off_pixel = self.off_pixel;
        if geometric {
            self.pixels(node, keys, own);
        }
        if geometric && !hangs(&node.widget) {
            self.reach(node, keys, own, parent, scroller);
            if let Some(min) = self.min_target {
                self.target(node, keys, own, min);
            }
        }
        let scrolls = scrolls(node);
        let scrolling = scrolls.down || scrolls.across;
        // The box children are measured against, and which ways it scrolls them.
        let (inner_parent, inner_scroller) = if geometric {
            (own, scrolls)
        } else {
            (parent, scroller)
        };
        if scrolls.across && geometric {
            self.across = true;
        }
        // A scroll view shows its content within its own box, whether or not it reports a clip.
        let mut inner_clip = clip;
        if let Some(child_clip) = node.child_clip {
            inner_clip = clip.intersect(rect(child_clip));
        } else if scrolling && geometric {
            inner_clip = clip.intersect(own);
        }
        // A view that scrolls sideways holds wider content by design; one that scrolls only down
        // and holds it scrolls the page sideways.
        if scrolling && !scrolls.across && geometric {
            self.sideways(node, keys, node.child_clip.map_or(own, rect));
        }
        if geometric {
            self.overlaps(node, keys, inner_clip);
        }
        for child in &node.children {
            if floats(&child.widget) {
                continue;
            }
            self.node(child, keys, inner_parent, inner_clip, inner_scroller);
        }
        self.beyond = was_beyond;
        self.across = was_across;
        self.off_pixel = was_off_pixel;
    }

    /// A box with an edge between two device pixels: the outermost such box only, since what
    /// it holds is placed from its edges.
    fn pixels(&mut self, node: &InspectionNode, keys: &[String], own: Rect) {
        if self.off_pixel {
            return;
        }
        let scale = self.display_scale;
        let off = |edge: f64| (edge * scale - (edge * scale).round()).abs() > PIXEL_SLACK;
        let edges = [
            ("left", own.x0),
            ("top", own.y0),
            ("right", own.x1),
            ("bottom", own.y1),
        ];
        let Some((side, edge)) = edges.into_iter().find(|(_, edge)| off(*edge)) else {
            return;
        };
        self.off_pixel = true;
        self.push(
            Kind::OffPixel,
            node,
            keys,
            format!(
                "its {side} edge at {edge:.3} falls at device pixel {:.3} at {scale}x",
                edge * scale
            ),
        );
    }

    /// A box past its container's, or past the window's sides.
    fn reach(
        &mut self,
        node: &InspectionNode,
        keys: &[String],
        own: Rect,
        parent: Rect,
        scroller: Scrolls,
    ) {
        let past_window =
            own.x0 < self.viewport.x0 - EPSILON || own.x1 > self.viewport.x1 + EPSILON;
        if past_window && !self.across {
            if self.beyond {
                return;
            }
            self.beyond = true;
            self.push(
                Kind::ScrollsSideways,
                node,
                keys,
                format!(
                    "{} reaches from x {:.0} to {:.0} in a window {:.0} wide",
                    size(own),
                    own.x0,
                    own.x1,
                    self.viewport.width()
                ),
            );
            return;
        }
        // In a scroll view, content goes on past it on its axis: only the other axis counts.
        let across = own.x0 < parent.x0 - EPSILON || own.x1 > parent.x1 + EPSILON;
        let along = own.y0 < parent.y0 - EPSILON || own.y1 > parent.y1 + EPSILON;
        if (across && !scroller.across) || (along && !scroller.down) {
            self.push(
                Kind::Overflow,
                node,
                keys,
                format!(
                    "{} at ({:.0}, {:.0}) reaches past its container's {} at ({:.0}, {:.0})",
                    size(own),
                    own.x0,
                    own.y0,
                    size(parent),
                    parent.x0,
                    parent.y0
                ),
            );
        }
    }

    /// A scroll view whose content is wider than it.
    fn sideways(&mut self, node: &InspectionNode, keys: &[String], clip: Rect) {
        let content = node
            .children
            .iter()
            .map(|child| rect(child.rect))
            .filter(|r| has_area(*r))
            .reduce(|a, b| a.union(b));
        if let Some(content) = content
            && content.width() > clip.width() + EPSILON
        {
            self.push(
                Kind::ScrollsSideways,
                node,
                keys,
                format!(
                    "content {:.0} wide in a scroll view {:.0} wide",
                    content.width(),
                    clip.width()
                ),
            );
        }
    }

    /// Something to press smaller than the minimum.
    fn target(&mut self, node: &InspectionNode, keys: &[String], own: Rect, min: f64) {
        let Some(semantics) = &node.semantics else {
            return;
        };
        let pressable = interactive(semantics.role)
            || (node.focusable && semantics.role == ui::a11y::Role::ListItem);
        if pressable && !semantics.state.disabled {
            let hit = semantics
                .bounds
                .map_or(own, |bounds| own.union(bounds.to_rect()));
            self.small(node, keys, hit, min, &format!("{:?}", semantics.role));
        }
        // The parts a widget draws itself (a chip's remove button), each with its own bounds.
        for part in &semantics.children {
            if interactive(part.role)
                && !part.state.disabled
                && let Some(bounds) = part.bounds
            {
                self.small(
                    node,
                    keys,
                    bounds.to_rect(),
                    min,
                    &format!("its {:?}", part.role),
                );
            }
        }
    }

    fn small(&mut self, node: &InspectionNode, keys: &[String], hit: Rect, min: f64, what: &str) {
        if hit.width() < min - 0.5 || hit.height() < min - 0.5 {
            self.push(
                Kind::SmallHitTarget,
                node,
                keys,
                format!(
                    "{what} is {} where a target is at least {:.0}×{:.0}",
                    size(hit),
                    min,
                    min
                ),
            );
        }
    }

    /// Children of a box that lays them out side by side or in turn, covering each other. In a
    /// box that layers them on purpose (a badge over an image), only text over text: neither
    /// can be read. In a box that places its own parts over each other (a window shell: its
    /// columns, and the buttons of its header row in the same plane), only one control over
    /// another: a press lands on one of them, and their faces cover each other.
    fn overlaps(&mut self, node: &InspectionNode, keys: &[String], clip: Rect) {
        let layered = layers(&node.widget);
        let places = layered && node.widget != "Layers";
        let shown: Vec<(&InspectionNode, Rect)> = node
            .children
            .iter()
            .filter(|child| !floats(&child.widget))
            .map(|child| (child, rect(child.rect).intersect(clip)))
            .filter(|(_, r)| has_area(*r))
            .collect();
        for (i, (a, ra)) in shown.iter().enumerate() {
            for (b, rb) in shown.iter().skip(i.saturating_add(1)) {
                let both = ra.intersect(*rb);
                let covers = if places {
                    is_control(a) && is_control(b)
                } else {
                    !layered || (has_text(a) && has_text(b))
                };
                if has_area(both) && covers {
                    let mut keys = keys.to_vec();
                    keys.extend(b.test_id.clone());
                    self.push(
                        Kind::Overlap,
                        b,
                        &keys,
                        format!(
                            "{} {} covers {} of {} {}",
                            b.widget,
                            size(*rb),
                            size(both),
                            a.widget,
                            size(*ra)
                        ),
                    );
                }
            }
        }
    }

    fn push(&mut self, kind: Kind, node: &InspectionNode, keys: &[String], detail: String) {
        self.out.push(Problem {
            kind,
            keys: keys.iter().rev().cloned().collect(),
            widget: node.widget.clone(),
            id: node.test_id.clone(),
            detail,
        });
    }
}

/// Text whose widget needs more room than layout gave it: taller at its width than its box, or
/// with a word wider than its box. Each widget measures itself as layout measures it.
pub fn clipped_text(
    root: &mut dyn Widget,
    text: &mut ui::TextRenderer,
    viewport: (u32, u32),
) -> Vec<Problem> {
    let mut out = Vec::new();
    let clip = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
    measure_walk(root, &mut Vec::new(), clip, text, &mut out);
    out
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "window-space sizes narrowed for taffy; never near f32::MAX"
)]
fn measure_walk(
    widget: &mut dyn Widget,
    keys: &mut Vec<String>,
    clip: Rect,
    text: &mut ui::TextRenderer,
    out: &mut Vec<Problem>,
) {
    let pushed = widget
        .test_id()
        .map(|id| keys.push(id.to_owned()))
        .is_some();
    measure_one(widget, keys, clip, text, out);
    if pushed {
        keys.pop();
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "window-space sizes narrowed for taffy; never near f32::MAX"
)]
fn measure_one(
    widget: &mut dyn Widget,
    keys: &mut Vec<String>,
    clip: Rect,
    text: &mut ui::TextRenderer,
    out: &mut Vec<Problem>,
) {
    use ui::taffy::{AvailableSpace, Size};
    let own = widget.get_rect();
    if has_area(own) && !has_area(own.intersect(clip)) {
        return;
    }
    if widget.name() == "Text" && has_area(own) {
        let width = own.width() as f32;
        let tall = widget.measure(
            Size {
                width: Some(width),
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::MaxContent,
            },
            text,
        );
        // The narrowest the text can be laid out at: a word longer than its line breaks
        // anywhere, so only text that cannot break (one line) needs more than a glyph.
        let narrowest = widget.measure(
            Size {
                width: None,
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(1.0),
                height: AvailableSpace::MaxContent,
            },
            text,
        );
        let shown = widget
            .accessibility()
            .and_then(|access| access.name)
            .unwrap_or_default();
        let detail = if f64::from(tall.height) > own.height() + EPSILON {
            Some(format!(
                "needs {:.0} high at its width, has {} ({shown:?})",
                tall.height,
                size(own)
            ))
        } else if f64::from(narrowest.width) > own.width() + EPSILON {
            Some(format!(
                "needs {:.0} wide at the narrowest, has {} ({shown:?})",
                narrowest.width,
                size(own)
            ))
        } else {
            None
        };
        if let Some(detail) = detail {
            out.push(Problem {
                kind: Kind::ClippedText,
                keys: keys.iter().rev().cloned().collect(),
                widget: widget.name().to_owned(),
                id: widget.test_id().map(str::to_owned),
                detail,
            });
        }
    }
    let inner = widget
        .child_clip_rect()
        .map_or(clip, |child| clip.intersect(child));
    for child in widget.children_mut() {
        measure_walk(child, keys, inner, text, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::{AccessNode, Role};

    fn node(widget: &str, id: &str, rect: [f64; 4], role: Option<Role>) -> InspectionNode {
        InspectionNode {
            path: id.to_owned(),
            widget: widget.to_owned(),
            test_id: Some(id.to_owned()),
            focusable: role.is_some(),
            rect,
            child_clip: None,
            layout: None,
            semantics: role.map(AccessNode::new),
            children: Vec::new(),
        }
    }

    fn overlaps(children: Vec<InspectionNode>) -> Vec<Problem> {
        let mut shell = node("NavShell", "shell", [0.0, 0.0, 400.0, 300.0], None);
        shell.children = children;
        inspect(&shell, (400, 300), None, 1.0)
            .into_iter()
            .filter(|p| p.kind == Kind::Overlap)
            .collect()
    }

    /// A window shell places its header row's buttons itself, over its columns: two of them
    /// covering each other (an action over the search button) is caught, though the shell
    /// layers its columns on purpose.
    #[test]
    fn controls_a_shell_places_over_each_other_are_caught() {
        let found = overlaps(vec![
            node(
                "Button",
                "search",
                [300.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
            node(
                "Button",
                "new",
                [316.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].keys.first().map(String::as_str), Some("new"));
    }

    /// On touch a control smaller than a finger's target is caught, at its own box grown by
    /// what it reports; with a mouse, whose hit areas grow about the face, nothing is.
    #[test]
    fn a_control_smaller_than_a_finger_is_caught_on_touch() {
        let mut root = node("Column", "root", [0.0, 0.0, 400.0, 300.0], None);
        root.children = vec![
            node(
                "Button",
                "small",
                [16.0, 16.0, 32.0, 32.0],
                Some(Role::Button),
            ),
            node(
                "Button",
                "large",
                [16.0, 64.0, 48.0, 48.0],
                Some(Role::Button),
            ),
        ];
        let small = |min| {
            inspect(&root, (400, 300), min, 1.0)
                .into_iter()
                .filter(|p| p.kind == Kind::SmallHitTarget)
                .map(|p| p.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(small(Some(48.0)), [Some("small".to_owned())]);
        assert!(small(None).is_empty());
    }

    /// A long word in a column narrower than it: text that wraps breaks it anywhere and shows
    /// all of it; text that keeps its words whole (source code's soft wrap) is clipped, and
    /// caught.
    #[test]
    fn text_that_cannot_break_a_word_wider_than_its_column_is_caught() {
        use ui::widget::arrange::{Length, Slot, Stack};
        let clipped = |mode: ui::text_render::TextWrapMode| {
            let text =
                ui::widget::Text::new("Donaudampfschifffahrtsgesellschaft").with_wrap_mode(mode);
            let mut column = Slot::new(Box::new(Stack::stack().push(Box::new(text))));
            column.fit.width.max = Some(Length::Rem(6.0));
            let mut h = ui::headless::FrameHarness::for_root(
                ui::State::new(),
                Stack::stack().push_slot(column),
                (320, 200),
                ui::Theme::light(),
            );
            let mut renderer = ui::TextRenderer::new();
            clipped_text(h.root_widget_mut(), &mut renderer, (320, 200)).len()
        };
        assert_eq!(clipped(ui::text_render::TextWrapMode::Wrap), 0);
        assert_eq!(clipped(ui::text_render::TextWrapMode::Indented), 1);
    }

    /// A header action the shell holds in a frame of its own is the control it frames.
    #[test]
    fn a_framed_action_over_a_button_is_caught() {
        let mut frame = node("Row", "actions", [316.0, 12.0, 32.0, 32.0], None);
        frame.children = vec![node(
            "Button",
            "new",
            [316.0, 12.0, 32.0, 32.0],
            Some(Role::Button),
        )];
        let found = overlaps(vec![
            node(
                "Button",
                "search",
                [300.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
            frame,
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
    }

    /// What a shell layers on purpose is no finding: a column under its header's buttons, and
    /// buttons side by side.
    #[test]
    fn a_shells_columns_and_buttons_side_by_side_are_not() {
        let mut column = node("Column", "content", [0.0, 0.0, 400.0, 300.0], None);
        column.children = vec![
            node("Text", "a", [16.0, 60.0, 100.0, 20.0], None),
            node("Text", "b", [16.0, 90.0, 100.0, 20.0], None),
        ];
        let found = overlaps(vec![
            column,
            node(
                "Button",
                "search",
                [300.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
            node(
                "Button",
                "new",
                [340.0, 12.0, 32.0, 32.0],
                Some(Role::Button),
            ),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    fn stack_node(id: &str, rect: [f64; 4], children: Vec<InspectionNode>) -> InspectionNode {
        InspectionNode {
            path: id.to_owned(),
            widget: "Stack".to_owned(),
            test_id: Some(id.to_owned()),
            focusable: false,
            rect,
            child_clip: None,
            layout: None,
            semantics: None,
            children,
        }
    }

    fn off_pixel(root: &InspectionNode, scale: f64) -> Vec<Vec<String>> {
        inspect(root, (400, 300), None, scale)
            .into_iter()
            .filter(|problem| problem.kind == Kind::OffPixel)
            .map(|problem| problem.keys)
            .collect()
    }

    #[test]
    fn an_edge_between_device_pixels_is_found_at_the_outermost_box_it_moves() {
        // 20.5 is a device pixel at 2x but not at 1x; 20 + 2/3 is one at 1.5x only.
        let third = 20.0 + 2.0 / 3.0;
        let window = |inner: [f64; 4]| {
            stack_node(
                "window",
                [0.0, 0.0, 400.0, 300.0],
                vec![stack_node(
                    "column",
                    inner,
                    vec![stack_node(
                        "label",
                        [inner[0], inner[1], 50.0, 10.0],
                        vec![],
                    )],
                )],
            )
        };
        let half = window([20.5, 10.0, 100.0, 40.0]);
        assert_eq!(
            off_pixel(&half, 1.0),
            vec![vec!["column".to_owned(), "window".to_owned()]],
            "found at the column, not again at the label it moves"
        );
        assert!(off_pixel(&half, 2.0).is_empty());
        assert_eq!(off_pixel(&half, 1.5).len(), 1);

        let thirds = window([third, 10.0, 100.0, 40.0]);
        assert!(off_pixel(&thirds, 1.5).is_empty());
        assert_eq!(off_pixel(&thirds, 2.0).len(), 1);
        assert_eq!(off_pixel(&thirds, 1.0).len(), 1);

        assert_eq!(
            off_pixel(&window([21.0, 10.0, 100.0, 40.0]), 1.5).len(),
            1,
            "a whole logical pixel is not a device pixel at 1.5x"
        );
        assert!(off_pixel(&window([20.0, 10.0, 100.0, 40.0]), 1.5).is_empty());
    }
}
