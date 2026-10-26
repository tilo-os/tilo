//! Whether the desktop has drawn the presentation a pointer action is aimed
//! at.
//!
//! A harness — an accessibility client, a script, an agent — names a target
//! and then presses it. The press only means what the caller read if the frame
//! that target lives in has been drawn: a target whose window still shows its
//! opening card, or whose overlay is still arriving, is somewhere other than
//! where a registered rectangle says, or is not on screen at all. Asking
//! whether *anything* on the desktop wants another frame answers a different
//! question, and a widget that keeps asking for frames for reasons of its own
//! is enough to hold every unrelated target back for as long as it does.
//!
//! Readiness is therefore read along one path — the widgets from the root down
//! to the one target named. Each of them must have [`Widget::presented`], the
//! single latch the toolkit defines for "this stands in for something that has
//! drawn", and the desktop as a whole must not be between two arrangements of
//! its own, where a rectangle read now is a rectangle the caller never saw.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vello::kurbo::{Point, Rect};

/// Why a target is not yet a sound thing to press, in the terms the control
/// socket answers with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReady {
    /// Nothing on the registry is named `target`: the caller is aiming at
    /// something this desktop does not have now.
    NoSuchTarget,
    /// The named widget has no area, so it has not been laid out and a press
    /// at its centre would land nowhere.
    NotLaidOut,
    /// The named widget, or one of the widgets it sits in, shows a stand-in
    /// for content that has not drawn yet.
    Unpresented,
    /// The press follows frame `from`, and no later frame has been drawn.
    NoFrameSince,
    /// The target is no longer where the caller saw it.
    Moved,
    /// The desktop is opening setup, or between workspaces, strips or window
    /// groups.
    InTransition,
}

impl NotReady {
    /// The stable wire name a harness switches on.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::NoSuchTarget => "unknown_target",
            Self::NotLaidOut => "target_not_laid_out",
            Self::Unpresented => "target_unpresented",
            Self::NoFrameSince => "no_frame_since",
            Self::Moved => "target_moved",
            Self::InTransition => "desktop_in_transition",
        }
    }

    /// Every refusal a control state can be held to, for a walk that needs
    /// the message for each.
    pub const ALL: [Self; 6] = [
        Self::NoSuchTarget,
        Self::NotLaidOut,
        Self::Unpresented,
        Self::NoFrameSince,
        Self::Moved,
        Self::InTransition,
    ];

    /// What to tell someone whose press was refused: what was found, and what
    /// has to happen before it can land.
    #[must_use]
    pub fn explain(self, target: &str) -> String {
        match self {
            Self::NoSuchTarget => format!(
                "no addressable widget named `{target}` (or its rect has zero area; \
                 not yet laid out?)"
            ),
            Self::NotLaidOut => {
                format!("`{target}` has no area yet; press it once it has been laid out")
            }
            Self::Unpresented => format!(
                "`{target}`, or a widget it sits in, still shows what stands in for content \
                 that has not drawn; press it once that has drawn"
            ),
            Self::NoFrameSince => {
                "no frame has been drawn since the one this press counts from".to_string()
            }
            Self::Moved => format!(
                "`{target}` is no longer where it was when it was read; read where it is now \
                 before pressing it"
            ),
            Self::InTransition => {
                "the desktop is between two arrangements; press it once it has settled".to_string()
            }
        }
    }
}

/// How far a target may drift between the frame a caller read it from and the
/// frame its press lands on, and still be the same target: half a logical
/// pixel, which is rounding rather than a move.
pub const MOVED: f64 = 0.5;

/// Every widget from `root` down to the one published at `path`, root first.
///
/// `path` is a published widget path from [`super::frame::walk_widget`]: each
/// step names a widget and, in brackets, which child of that name it is. Steps
/// are matched against the children in the order the tree lays them out, so a
/// step that is not there is [`NotReady::NoSuchTarget`] and never a guess at
/// its neighbour.
///
/// # Errors
///
/// Returns [`NotReady::NoSuchTarget`] when a step of `path` names no child.
pub fn widget_chain<'a>(
    root: &'a dyn ui::widget::Widget,
    path: &str,
) -> Result<Vec<&'a dyn ui::widget::Widget>, NotReady> {
    let mut chain = vec![root];
    let mut level: &dyn ui::widget::Widget = root;
    for step in path.split('/') {
        let (name, index) = match step.split_once('[') {
            Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest).parse().ok()),
            None => (step, Some(0)),
        };
        let mut wanted: usize = index.ok_or(NotReady::NoSuchTarget)?;
        let mut found: Option<&dyn ui::widget::Widget> = None;
        for child in level.children() {
            if child.name() != name {
                continue;
            }
            if wanted == 0 {
                found = Some(child);
                break;
            }
            wanted = wanted.saturating_sub(1);
        }
        level = found.ok_or(NotReady::NoSuchTarget)?;
        chain.push(level);
    }
    Ok(chain)
}

/// The nearest widget in `chain` that shows a stand-in for content which has
/// not drawn, if any.
fn unpresented(chain: &[&dyn ui::widget::Widget]) -> Option<String> {
    chain
        .iter()
        .find(|widget| !widget.presented())
        .map(|widget| widget.name().to_string())
}

/// Whether `path` is a target whose presentation has been drawn.
///
/// # Errors
///
/// Returns the [`NotReady`] naming what is missing.
pub fn target_drawn<S: std::hash::BuildHasher>(
    root: &dyn ui::widget::Widget,
    registry: &HashMap<String, Rect, S>,
    path: &str,
) -> Result<(), NotReady> {
    let rect = registry.get(path).copied().ok_or(NotReady::NoSuchTarget)?;
    if rect.area() <= 0.0 {
        return Err(NotReady::NotLaidOut);
    }
    if unpresented(&widget_chain(root, path)?).is_some() {
        return Err(NotReady::Unpresented);
    }
    Ok(())
}

/// What the control protocol knows about the desktop besides its widget tree:
/// which frame is on screen, whether the desktop is between two arrangements
/// of its own, and the presses a harness has aimed but not yet landed.
pub trait PressVantage {
    /// The identity of the last frame drawn, as [`super::test_control`]
    /// reports it to a harness. Absent when this state cannot observe frames.
    fn drawn_frame(&self) -> Option<u64>;
    /// Whether a switch is underway, so a rectangle read now is not the one
    /// the caller was shown.
    fn desktop_in_transition(&self) -> bool;
}

/// The vantage a live desktop answers with: the frame it has drawn, whether a
/// switch is underway, and the aim a harness read for a target.
#[derive(Debug, Clone, Default)]
pub struct PressFacts {
    frame: Option<u64>,
    in_transition: bool,
    /// Presses a caller aimed at a target after reading where it was, keyed
    /// by that target, each waiting for the frame that lands it.
    aimed: std::sync::Arc<Mutex<HashMap<String, PressAim>>>,
}

impl PressFacts {
    /// A desktop on frame `frame`, settled or between two arrangements of
    /// its own. Each desktop answers with its own ledger of aims, so a test
    /// never inherits the presses of another.
    #[must_use]
    pub fn at(frame: Option<u64>, in_transition: bool) -> Self {
        Self {
            frame,
            in_transition,
            aimed: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The aim this desktop holds for `target`: the frame and the point the
    /// caller read it from. `None` means the press is to be taken against
    /// what is on screen now.
    #[must_use]
    pub fn aim(&self, target: &str) -> Option<PressAim> {
        Some(*self.aimed.lock().ok()?.get(target)?)
    }

    /// Record that a caller read `target` at `point` on the frame now drawn.
    /// Landing the press consumes it, one press per aim.
    pub fn aim_at(&self, target: &str, point: Point) {
        let Some(frame) = self.frame else {
            return;
        };
        if let Ok(mut aims) = self.aimed.lock() {
            aims.insert(target.to_string(), PressAim::read_at(frame, point));
        }
    }

    /// Take back the aim recorded for `target`, so the press it was read for
    /// is the only one it lands.
    pub fn take_aim(&self, target: &str) -> Option<PressAim> {
        self.aimed.lock().ok()?.remove(target)
    }
}

impl PressVantage for PressFacts {
    fn drawn_frame(&self) -> Option<u64> {
        self.frame
    }

    fn desktop_in_transition(&self) -> bool {
        self.in_transition
    }
}

/// Where a press on the target published at `path` belongs, or why pressing
/// it would move something other than the widget whose presentation was
/// proven.
///
/// The registry is read once and used for both the proof and the point: it is
/// the keeper of a target's rectangle and of the path it was published under,
/// and mixing a rectangle from one walk of the tree with a chain from another
/// is exactly how a press lands on a neighbour.
///
/// # Errors
///
/// Returns the [`NotReady`] that explains the refusal.
pub fn press_point<S: std::hash::BuildHasher>(
    root: &dyn ui::widget::Widget,
    registry: &HashMap<String, Rect, S>,
    vantage: &dyn PressVantage,
    path: &str,
) -> Result<Point, NotReady> {
    if vantage.desktop_in_transition() {
        return Err(NotReady::InTransition);
    }
    target_drawn(root, registry, path)?;
    Ok(registry[path].center())
}

/// A press a caller asked for after reading a target, with the frame it read
/// the target from and where the target was then.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PressAim {
    pub frame: u64,
    pub at: Point,
}

impl PressAim {
    /// The aim read from frame `frame`, where the target was at `at`.
    #[must_use]
    pub fn read_at(frame: u64, at: Point) -> Self {
        Self { frame, at }
    }
}

/// Consume a press aimed with [`PressAim`]: the same proof the aim needed, at
/// the frame the press lands on.
///
/// The target must still be drawn and presented, one more frame must have
/// been drawn since the aim was read, and the target must still be where it
/// was read. A press with no aim is taken against what the desktop shows now.
///
/// # Errors
///
/// Returns the [`NotReady`] that explains the refusal.
pub fn consume_press<S: std::hash::BuildHasher>(
    root: &dyn ui::widget::Widget,
    registry: &HashMap<String, Rect, S>,
    vantage: &PressFacts,
    path: &str,
    aim: Option<PressAim>,
) -> Result<Point, NotReady> {
    let now = press_point(root, registry, vantage, path)?;
    let Some(aim) = aim else {
        return Ok(now);
    };
    // A state that cannot see frames answers from the tree alone; the aim is
    // still honoured for the position it recorded.
    if let Some(drawn) = vantage.drawn_frame()
        && drawn <= aim.frame
    {
        return Err(NotReady::NoFrameSince);
    }
    if now.distance(aim.at) > MOVED {
        return Err(NotReady::Moved);
    }
    Ok(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use taffy::TaffyTree;
    use ui::theme::Theme;
    use ui::widget::Widget;

    /// A widget that answers `presented` with what it was told, and holds
    /// named children: enough to be the reason a target cannot be pressed,
    /// and to sit between the root and a target that can.
    struct Node {
        name: &'static str,
        presented: bool,
        children: Vec<Node>,
    }

    impl Node {
        fn leaf(name: &'static str, presented: bool) -> Self {
            Self {
                name,
                presented,
                children: Vec::new(),
            }
        }

        fn parent(name: &'static str, presented: bool, children: Vec<Node>) -> Self {
            Self {
                name,
                presented,
                children,
            }
        }
    }

    impl Widget for Node {
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            _: &Theme,
            _: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            tree.new_leaf(taffy::Style::default()).expect("a leaf")
        }

        fn children(&self) -> Vec<&dyn Widget> {
            self.children
                .iter()
                .map(|child| child as &dyn Widget)
                .collect()
        }

        fn name(&self) -> &'static str {
            self.name
        }

        fn presented(&self) -> bool {
            self.presented
        }
    }

    /// A root, `Strips/Sheet`, where `Sheet` is unpresented when `sheet_drawn`
    /// is false and `Strips` likewise when `strips_drawn` is false.
    fn desktop(sheet_drawn: bool, strips_drawn: bool) -> Node {
        Node::parent(
            "Root",
            true,
            vec![Node::parent(
                "Strips",
                strips_drawn,
                vec![Node::leaf("Sheet", sheet_drawn)],
            )],
        )
    }

    fn registry(target: &str, rect: Rect) -> HashMap<String, Rect> {
        let mut registry = HashMap::new();
        registry.insert(target.to_string(), rect);
        registry
    }

    fn settled() -> PressFacts {
        PressFacts::at(Some(7), false)
    }

    #[test]
    fn a_target_whose_presentation_is_drawn_is_pressable() {
        let root = desktop(true, true);
        let registry = registry("Strips/Sheet", Rect::new(10.0, 20.0, 30.0, 60.0));
        let at = press_point(&root, &registry, &settled(), "Strips/Sheet").unwrap();
        assert_eq!(at, Point::new(20.0, 40.0));
    }

    #[test]
    fn a_target_that_has_not_drawn_is_refused_for_its_own_presentation() {
        let root = desktop(false, true);
        let registry = registry("Strips/Sheet", Rect::new(10.0, 20.0, 30.0, 60.0));
        assert_eq!(
            press_point(&root, &registry, &settled(), "Strips/Sheet"),
            Err(NotReady::Unpresented)
        );
    }

    #[test]
    fn a_presentation_ancestor_that_has_not_drawn_is_refused_too() {
        let root = desktop(true, false);
        let registry = registry("Strips/Sheet", Rect::new(10.0, 20.0, 30.0, 60.0));
        assert_eq!(
            press_point(&root, &registry, &settled(), "Strips/Sheet"),
            Err(NotReady::Unpresented)
        );
    }

    #[test]
    fn a_widget_off_the_targets_own_path_does_not_hold_it_back() {
        // A canvas well in another strip that never says it has drawn is
        // beside the path to this sheet, not on it: the sheet is pressable
        // whatever the well is doing.
        let root = Node::parent(
            "Root",
            true,
            vec![Node::parent(
                "Strips",
                true,
                vec![Node::leaf("Well", false), Node::leaf("Sheet", true)],
            )],
        );
        // `Sheet` is the only child of that name under `Strips`, so it is
        // published as the first one; `Well` is a sibling, not a step.
        let registry = registry("Strips/Sheet", Rect::new(0.0, 0.0, 10.0, 10.0));
        assert_eq!(
            press_point(&root, &registry, &settled(), "Strips/Sheet"),
            Ok(Point::new(5.0, 5.0))
        );
    }

    #[test]
    fn a_switch_underway_refuses_every_target() {
        let root = desktop(true, true);
        let registry = registry("Strips/Sheet", Rect::new(0.0, 0.0, 10.0, 10.0));
        let switching = PressFacts::at(Some(7), true);
        assert_eq!(
            press_point(&root, &registry, &switching, "Strips/Sheet"),
            Err(NotReady::InTransition)
        );
    }

    #[test]
    fn a_name_the_registry_does_not_hold_is_unknown() {
        let root = desktop(true, true);
        let registry = registry("Strips/Sheet", Rect::new(0.0, 0.0, 10.0, 10.0));
        let refusal = press_point(&root, &registry, &settled(), "Strips/Absent").unwrap_err();
        assert_eq!(refusal, NotReady::NoSuchTarget);
        assert_eq!(refusal.code(), "unknown_target");
    }

    #[test]
    fn a_target_with_no_area_has_not_been_laid_out() {
        let root = desktop(true, true);
        let registry = registry("Strips/Sheet", Rect::new(5.0, 5.0, 5.0, 5.0));
        assert_eq!(
            press_point(&root, &registry, &settled(), "Strips/Sheet"),
            Err(NotReady::NotLaidOut)
        );
    }

    #[test]
    fn a_press_waits_for_a_frame_drawn_after_the_aim_was_read() {
        let root = desktop(true, true);
        let registry = registry("Strips/Sheet", Rect::new(0.0, 0.0, 10.0, 10.0));
        let aim = Some(PressAim::read_at(7, Point::new(5.0, 5.0)));
        // The aim was read from the frame still on screen: nothing has been
        // drawn to prove the press lands on what was read.
        assert_eq!(
            consume_press(&root, &registry, &settled(), "Strips/Sheet", aim),
            Err(NotReady::NoFrameSince)
        );
        // One frame later it lands, at the point the aim names.
        let later = PressFacts::at(Some(8), false);
        assert_eq!(
            consume_press(&root, &registry, &later, "Strips/Sheet", aim),
            Ok(Point::new(5.0, 5.0))
        );
    }

    #[test]
    fn a_target_that_moved_refuses_the_press_read_before_it_moved() {
        let root = desktop(true, true);
        let registry = registry("Strips/Sheet", Rect::new(100.0, 100.0, 110.0, 110.0));
        let aim = Some(PressAim::read_at(7, Point::new(5.0, 5.0)));
        let later = PressFacts::at(Some(8), false);
        assert_eq!(
            consume_press(&root, &registry, &later, "Strips/Sheet", aim),
            Err(NotReady::Moved)
        );
    }

    #[test]
    fn a_press_with_no_aim_is_taken_against_what_is_drawn_now() {
        let root = desktop(true, true);
        let registry = registry("Strips/Sheet", Rect::new(0.0, 0.0, 10.0, 20.0));
        assert_eq!(
            consume_press(&root, &registry, &settled(), "Strips/Sheet", None),
            Ok(Point::new(5.0, 10.0))
        );
    }

    #[test]
    fn every_refusal_has_a_code_and_a_sentence() {
        let codes: Vec<&str> = NotReady::ALL.iter().map(|refusal| refusal.code()).collect();
        let mut unique = codes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len(), "codes are distinct: {codes:?}");
        for refusal in NotReady::ALL {
            let message = refusal.explain("Strips/Sheet");
            assert!(
                message.contains("Strips/Sheet") || !message.is_empty(),
                "{refusal:?} explains itself: {message}"
            );
            assert!(!message.ends_with('.'), "no trailing full stop: {message}");
        }
    }
}
