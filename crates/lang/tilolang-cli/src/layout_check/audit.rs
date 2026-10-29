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
}

impl Kind {
    pub const ALL: [Self; 5] = [
        Self::Overflow,
        Self::ClippedText,
        Self::Overlap,
        Self::SmallHitTarget,
        Self::ScrollsSideways,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Overflow => "overflow",
            Self::ClippedText => "clipped-text",
            Self::Overlap => "overlap",
            Self::SmallHitTarget => "small-hit-target",
            Self::ScrollsSideways => "scrolls-sideways",
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

fn rect(r: [f64; 4]) -> Rect {
    Rect::new(r[0], r[1], r[0] + r[2], r[1] + r[3])
}

fn has_area(r: Rect) -> bool {
    r.width() > EPSILON && r.height() > EPSILON
}

fn size(r: Rect) -> String {
    format!("{:.0}×{:.0}", r.width(), r.height())
}

/// Whether a widget scrolls its content.
fn scrolls(widget: &str) -> bool {
    let lower = widget.to_ascii_lowercase();
    lower.contains("scroll") || lower.contains("virtuallist") || lower.contains("describedlist")
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
        "Layers" | "NavShell" | "NavPane" | "Pane" | "TilolangWindow" | "Decorated" | "Toast"
    ) || widget.contains("Overlay")
        || widget.contains("Layer")
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
    /// The smallest a target may be, where the face is the target (on touch).
    min_target: Option<f64>,
    out: &'a mut Vec<Problem>,
}

/// Every problem of the frame `root`, laid out in `viewport`, where a target needs `min_target`
/// logical pixels on each side. On a fine pointer the toolkit grows each control's hit area
/// about its centre to the minimum, so targets are checked where the face itself must be the
/// target: on touch, where faces grow to it.
pub fn inspect(
    root: &InspectionNode,
    viewport: (u32, u32),
    min_target: Option<f64>,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let viewport = Rect::new(0.0, 0.0, f64::from(viewport.0), f64::from(viewport.1));
    let mut walk = Walk {
        viewport,
        beyond: false,
        min_target,
        out: &mut out,
    };
    walk.node(root, &mut Vec::new(), viewport, viewport, false);
    out
}

impl Walk<'_> {
    /// `parent`: the nearest box above with geometry. `clip`: what of the window the node can show in. `scroller`:
    /// whether a scroll view above lets content go on past its container's bottom.
    fn node(
        &mut self,
        node: &InspectionNode,
        keys: &mut Vec<String>,
        parent: Rect,
        clip: Rect,
        scroller: bool,
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
        scroller: bool,
    ) {
        let own = rect(node.rect);
        let geometric = has_area(own);
        // What of the window is not shown at all needs no checking: a column pushed aside, a
        // row scrolled out of view.
        if geometric && !has_area(own.intersect(clip)) {
            return;
        }
        let was_beyond = self.beyond;
        if geometric && !hangs(&node.widget) {
            self.reach(node, keys, own, parent, scroller);
            if let Some(min) = self.min_target {
                self.target(node, keys, own, min);
            }
        }
        let scrolling = scrolls(&node.widget);
        // The box children are measured against, and whether it scrolls them.
        let (inner_parent, inner_scroller) = if geometric {
            (own, scrolling)
        } else {
            (parent, scroller)
        };
        // A scroll view shows its content within its own box, whether or not it reports a clip.
        let mut inner_clip = clip;
        if let Some(child_clip) = node.child_clip {
            inner_clip = clip.intersect(rect(child_clip));
        } else if scrolling && geometric {
            inner_clip = clip.intersect(own);
        }
        if scrolling && geometric {
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
    }

    /// A box past its container's, or past the window's sides.
    fn reach(
        &mut self,
        node: &InspectionNode,
        keys: &[String],
        own: Rect,
        parent: Rect,
        scroller: bool,
    ) {
        let past_window =
            own.x0 < self.viewport.x0 - EPSILON || own.x1 > self.viewport.x1 + EPSILON;
        if past_window {
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
        // In a scroll view, content goes on past it on its axis: only its sides count.
        let across = own.x0 < parent.x0 - EPSILON || own.x1 > parent.x1 + EPSILON;
        let along = own.y0 < parent.y0 - EPSILON || own.y1 > parent.y1 + EPSILON;
        if across || (along && !scroller) {
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
    /// can be read.
    fn overlaps(&mut self, node: &InspectionNode, keys: &[String], clip: Rect) {
        let layered = layers(&node.widget);
        if layered && node.widget != "Layers" {
            return;
        }
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
                if has_area(both) && (!layered || (has_text(a) && has_text(b))) {
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
        let narrowest = widget.measure(
            Size {
                width: None,
                height: None,
            },
            Size {
                width: AvailableSpace::MinContent,
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
                "a word needs {:.0} wide, has {} ({shown:?})",
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
