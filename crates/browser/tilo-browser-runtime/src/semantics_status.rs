//! Decides when a presented status carries the semantics tree.
//!
//! The tree is the bulk of every status and the host mirrors it into the
//! accessibility DOM. Unchanged trees (pointer frames) are omitted; while
//! typing the tree changes on every key, so this gate additionally throttles
//! content-only changes to one delivery per window, with a deadline wake so
//! the final tree always reaches the host within the window. Changes assistive
//! technology must see at once are never throttled: keyboard focus moving,
//! the tree's structure changing (nodes added or removed, roles or paths
//! changed), the IME window moving to another line or field, or the host
//! asking for a fresh tree.
use crate::throttle::{PublicationDecision, PublicationSchedule};
use std::hash::{Hash, Hasher};
use ui::{
    a11y::AccessNode,
    headless::{InspectionNode, InspectionSnapshot},
    ime::ImeWindowState,
};

/// Content-only semantics changes are delivered at most this often while they
/// keep arriving; structure, focus and IME-line changes bypass the window.
#[cfg(target_arch = "wasm32")]
pub(crate) const SEMANTICS_WINDOW_MS: u64 = 100;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SemanticsDecision {
    /// The host already holds this exact tree.
    Unchanged,
    /// Include the serialized tree (see [`SemanticsGate::text`]); `forced` is
    /// true when a focus, structure, IME or host request bypassed the window.
    Deliver { forced: bool },
    /// A content-only change inside the window; deliver at the instant.
    Deferred(ui::time::Instant),
}

pub(crate) struct SemanticsGate {
    schedule: PublicationSchedule,
    text: Vec<u8>,
    delivered: u64,
    structure: u64,
    ime: u64,
    requested: bool,
}

impl SemanticsGate {
    pub(crate) fn new(window: std::time::Duration) -> Self {
        Self {
            schedule: PublicationSchedule::new(window),
            text: Vec::new(),
            delivered: 0,
            structure: 0,
            ime: 0,
            requested: false,
        }
    }
    /// The host wants the full tree with the next status even if unchanged.
    pub(crate) fn request(&mut self) {
        self.requested = true;
    }
    /// Whether a frame that only moved laid-out content would leave its
    /// tree unbuilt at `now` ([`Self::defer_unbuilt`], the IME line held),
    /// without scheduling anything.
    pub(crate) fn would_defer_unbuilt(&self, now: ui::time::Instant) -> bool {
        !self.requested && self.delivered != 0 && self.schedule.would_defer(now)
    }
    /// Hold a moved frame's tree, whose window is over, for the frame after
    /// (due `now`), unless something else asks for it now: a request, the
    /// first tree, or the IME line moved. Returns whether it is held.
    pub(crate) fn hold_for_next_frame(
        &mut self,
        ime: Option<&ImeWindowState>,
        now: ui::time::Instant,
    ) -> bool {
        if self.requested || self.delivered == 0 || ime_signature(ime) != self.ime {
            return false;
        }
        self.schedule.hold(now);
        true
    }
    /// Deadline of a deferred content change, if one is pending.
    pub(crate) fn due(&self) -> Option<ui::time::Instant> {
        self.schedule.due()
    }
    /// Leave a frame's tree unbuilt when it can only be a content-only
    /// change: the caller knows the frame only moved laid-out content, so
    /// its structure and focus are the last tree's, and here the IME line
    /// is too. Inside the window such a change would be deferred anyway;
    /// this defers it unseen, and the deadline delivers the tree as it
    /// then is. False, deferring nothing, when the host asked for a tree,
    /// none was delivered yet, the IME line moved or the window is over.
    pub(crate) fn defer_unbuilt(
        &mut self,
        ime: Option<&ImeWindowState>,
        now: ui::time::Instant,
    ) -> bool {
        if self.requested || self.delivered == 0 || ime_signature(ime) != self.ime {
            return false;
        }
        self.schedule.defer(now).is_some()
    }
    /// Serialized JSON of the tree from the last [`Self::decide`] call.
    pub(crate) fn text(&self) -> &[u8] {
        &self.text
    }
    /// Serializes the tree once (the buffer is reused frame to frame) and decides
    /// whether this status carries it.
    pub(crate) fn decide(
        &mut self,
        snapshot: &InspectionSnapshot,
        ime: Option<&ImeWindowState>,
        now: ui::time::Instant,
    ) -> SemanticsDecision {
        self.text.clear();
        if serde_json::to_writer(&mut self.text, snapshot).is_err() {
            self.text.clear();
            self.text.extend_from_slice(b"null");
        }
        let content = hash_bytes(&self.text);
        let structure = structure_signature(snapshot);
        let ime = ime_signature(ime);
        let requested = std::mem::take(&mut self.requested);
        if content == self.delivered && !requested {
            // Identical text implies identical structure; the IME line may still
            // have moved (editor blur), which counts as a change for the next tree.
            self.ime = ime;
            self.schedule.clear_due();
            return SemanticsDecision::Unchanged;
        }
        let forced = requested || structure != self.structure || ime != self.ime;
        if forced {
            self.schedule.mark_published(now);
        } else if let PublicationDecision::Deferred(due) = self.schedule.decide(now) {
            return SemanticsDecision::Deferred(due);
        }
        self.delivered = content;
        self.structure = structure;
        self.ime = ime;
        SemanticsDecision::Deliver { forced }
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Identity of the tree's shape: paths, widgets, roles, keys, focus and child
/// counts, but not values, names, rects or other per-key content.
pub(crate) fn structure_signature(snapshot: &InspectionSnapshot) -> u64 {
    fn visit_node(node: &InspectionNode, hasher: &mut std::hash::DefaultHasher) {
        node.path.hash(hasher);
        node.widget.hash(hasher);
        node.focusable.hash(hasher);
        match &node.semantics {
            Some(access) => {
                1u8.hash(hasher);
                visit_access(access, hasher);
            }
            None => 0u8.hash(hasher),
        }
        node.children.len().hash(hasher);
        for child in &node.children {
            visit_node(child, hasher);
        }
    }
    fn visit_access(access: &AccessNode, hasher: &mut std::hash::DefaultHasher) {
        access.role.hash(hasher);
        access.semantic_key.hash(hasher);
        access.action_target.hash(hasher);
        access.key.hash(hasher);
        access.state.focused.hash(hasher);
        access.children.len().hash(hasher);
        for child in &access.children {
            visit_access(child, hasher);
        }
    }
    let mut hasher = std::hash::DefaultHasher::new();
    visit_node(&snapshot.root, &mut hasher);
    if let Some(presentation) = &snapshot.presentation {
        visit_node(presentation, &mut hasher);
    }
    hasher.finish()
}

/// Identity of the IME window's line: presence, content type, vertical position and
/// height. The caret's horizontal position moves with every typed character
/// and is ordinary typing, not a change assistive technology must see at once.
pub(crate) fn ime_signature(ime: Option<&ImeWindowState>) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    match ime {
        None => 0u8.hash(&mut hasher),
        Some(ime) => {
            1u8.hash(&mut hasher);
            ime.content.hash(&mut hasher);
            ime.y.hash(&mut hasher);
            ime.height.hash(&mut hasher);
        }
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use ui::a11y::{AccessNode, Role};

    fn leaf(path: &str, role: Role, value: &str, focused: bool) -> InspectionNode {
        let mut access = AccessNode::new(role);
        access.value = Some(value.into());
        access.state.focused = focused;
        access.state.focusable = true;
        InspectionNode {
            path: path.into(),
            widget: "TextInput".into(),
            test_id: None,
            focusable: true,
            rect: [0.0, 0.0, 10.0, 10.0],
            child_clip: None,
            layout: None,
            semantics: Some(access),
            children: vec![],
        }
    }
    fn tree(values: &[&str], focused: usize) -> InspectionSnapshot {
        let children = values
            .iter()
            .enumerate()
            .map(|(i, value)| {
                leaf(
                    &format!("/Column/TextInput[{i}]"),
                    Role::TextInput,
                    value,
                    i == focused,
                )
            })
            .collect();
        let root = InspectionNode {
            path: "/Column".into(),
            widget: "Column".into(),
            test_id: None,
            focusable: false,
            rect: [0.0, 0.0, 100.0, 100.0],
            child_clip: None,
            layout: None,
            semantics: None,
            children,
        };
        InspectionSnapshot {
            viewport: [100, 100],
            keyboard_focus_owner: Some(format!("/Column/TextInput[{focused}]")),
            root,
            presentation: None,
        }
    }
    fn ime(x: u32, y: u32) -> ImeWindowState {
        ImeWindowState {
            content: ui::ime::InputContent::TEXT,
            x,
            y,
            width: 1,
            height: 16,
        }
    }
    fn window() -> SemanticsGate {
        SemanticsGate::new(Duration::from_millis(100))
    }

    #[test]
    fn a_moved_frame_waits_unbuilt_only_inside_the_window() {
        let t0 = ui::time::Instant::now();
        let mut gate = window();
        // Nothing delivered yet: the first tree is always built.
        assert!(!gate.defer_unbuilt(None, t0));
        assert_eq!(
            gate.decide(&tree(&["a"], 0), None, t0),
            SemanticsDecision::Deliver { forced: true }
        );
        // Inside the window a moved frame waits, with the deadline set.
        assert!(gate.defer_unbuilt(None, t0 + Duration::from_millis(10)));
        assert_eq!(gate.due(), Some(t0 + Duration::from_millis(100)));
        // An IME line that moved needs the tree at once.
        assert!(!gate.defer_unbuilt(Some(&ime(0, 40)), t0 + Duration::from_millis(20)));
        // So does a host request.
        gate.request();
        assert!(!gate.defer_unbuilt(None, t0 + Duration::from_millis(30)));
        assert_eq!(
            gate.decide(&tree(&["b"], 0), None, t0 + Duration::from_millis(30)),
            SemanticsDecision::Deliver { forced: true }
        );
        // Past the window the frame is looked at again.
        assert!(!gate.defer_unbuilt(None, t0 + Duration::from_millis(200)));
    }

    #[test]
    fn the_first_tree_and_unchanged_trees_follow_the_hash_gate() {
        let t0 = ui::time::Instant::now();
        let mut gate = window();
        assert_eq!(
            gate.decide(&tree(&["a", ""], 0), None, t0),
            SemanticsDecision::Deliver { forced: true }
        );
        assert_eq!(
            gate.text(),
            serde_json::to_vec(&tree(&["a", ""], 0)).unwrap().as_slice()
        );
        assert_eq!(
            gate.decide(&tree(&["a", ""], 0), None, t0 + Duration::from_millis(10)),
            SemanticsDecision::Unchanged
        );
        assert_eq!(gate.due(), None);
    }

    #[test]
    fn content_only_changes_inside_the_window_defer_to_the_deadline_and_the_final_tree_is_delivered()
     {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        assert_eq!(
            gate.decide(&tree(&["a", ""], 0), Some(&ime(10, 20)), ms(0)),
            SemanticsDecision::Deliver { forced: true }
        );
        // Typing: the value and the caret's horizontal position change every key.
        assert_eq!(
            gate.decide(&tree(&["ab", ""], 0), Some(&ime(18, 20)), ms(30)),
            SemanticsDecision::Deferred(ms(100))
        );
        assert_eq!(gate.due(), Some(ms(100)));
        assert_eq!(
            gate.decide(&tree(&["abc", ""], 0), Some(&ime(26, 20)), ms(60)),
            SemanticsDecision::Deferred(ms(100))
        );
        assert_eq!(
            gate.decide(&tree(&["abcd", ""], 0), Some(&ime(34, 20)), ms(100)),
            SemanticsDecision::Deliver { forced: false }
        );
        assert_eq!(
            gate.text(),
            serde_json::to_vec(&tree(&["abcd", ""], 0))
                .unwrap()
                .as_slice()
        );
        assert_eq!(gate.due(), None);
        // Once the deadline passes with no further change the deferred tree is still the one that gets sent.
        assert_eq!(
            gate.decide(&tree(&["abcde", ""], 0), Some(&ime(42, 20)), ms(150)),
            SemanticsDecision::Deferred(ms(200))
        );
        assert_eq!(
            gate.decide(&tree(&["abcde", ""], 0), Some(&ime(42, 20)), ms(200)),
            SemanticsDecision::Deliver { forced: false }
        );
    }

    #[test]
    fn a_deferred_change_that_reverts_clears_the_deadline() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(&tree(&["a", ""], 0), None, ms(0));
        assert_eq!(
            gate.decide(&tree(&["ab", ""], 0), None, ms(30)),
            SemanticsDecision::Deferred(ms(100))
        );
        assert_eq!(
            gate.decide(&tree(&["a", ""], 0), None, ms(60)),
            SemanticsDecision::Unchanged
        );
        assert_eq!(gate.due(), None);
    }

    #[test]
    fn focus_and_structure_changes_are_never_throttled() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(&tree(&["a", ""], 0), None, ms(0));
        assert_eq!(
            gate.decide(&tree(&["ab", ""], 0), None, ms(20)),
            SemanticsDecision::Deferred(ms(100))
        );
        // Focus moves to the second field: delivered at once, and the window restarts.
        assert_eq!(
            gate.decide(&tree(&["ab", ""], 1), None, ms(40)),
            SemanticsDecision::Deliver { forced: true }
        );
        assert_eq!(gate.due(), None);
        assert_eq!(
            gate.decide(&tree(&["ab", "x"], 1), None, ms(60)),
            SemanticsDecision::Deferred(ms(140))
        );
        // A node appears (Enter creates a line): delivered at once.
        assert_eq!(
            gate.decide(&tree(&["ab", "x", ""], 1), None, ms(80)),
            SemanticsDecision::Deliver { forced: true }
        );
        // A node disappears: delivered at once.
        assert_eq!(
            gate.decide(&tree(&["ab", "x"], 1), None, ms(90)),
            SemanticsDecision::Deliver { forced: true }
        );
    }

    #[test]
    fn an_ime_line_change_is_delivered_at_once_but_horizontal_caret_motion_is_not() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(&tree(&["a", ""], 0), Some(&ime(10, 20)), ms(0));
        assert_eq!(
            gate.decide(&tree(&["ab", ""], 0), Some(&ime(18, 20)), ms(20)),
            SemanticsDecision::Deferred(ms(100))
        );
        assert_eq!(
            gate.decide(&tree(&["abc", ""], 0), Some(&ime(0, 36)), ms(40)),
            SemanticsDecision::Deliver { forced: true }
        );
        // The IME window disappearing (editor blur) with an unchanged tree needs no resend.
        assert_eq!(
            gate.decide(&tree(&["abc", ""], 0), None, ms(50)),
            SemanticsDecision::Unchanged
        );
        // But its next content change is delivered at once because the IME state moved.
        assert_eq!(
            gate.decide(&tree(&["abcd", ""], 0), Some(&ime(0, 36)), ms(60)),
            SemanticsDecision::Deliver { forced: true }
        );
    }

    #[test]
    fn a_host_request_resends_even_an_unchanged_tree() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(&tree(&["a", ""], 0), None, ms(0));
        assert_eq!(
            gate.decide(&tree(&["a", ""], 0), None, ms(10)),
            SemanticsDecision::Unchanged
        );
        gate.request();
        assert_eq!(
            gate.decide(&tree(&["a", ""], 0), None, ms(20)),
            SemanticsDecision::Deliver { forced: true }
        );
        assert_eq!(
            gate.decide(&tree(&["a", ""], 0), None, ms(30)),
            SemanticsDecision::Unchanged
        );
        // A request during a deferred change delivers the current tree now.
        assert_eq!(
            gate.decide(&tree(&["ab", ""], 0), None, ms(40)),
            SemanticsDecision::Deferred(ms(120))
        );
        gate.request();
        assert_eq!(
            gate.decide(&tree(&["abc", ""], 0), None, ms(50)),
            SemanticsDecision::Deliver { forced: true }
        );
        assert_eq!(gate.due(), None);
    }

    #[test]
    fn a_zero_window_delivers_every_changed_tree() {
        let t0 = ui::time::Instant::now();
        let mut gate = SemanticsGate::new(Duration::ZERO);
        gate.decide(&tree(&["a", ""], 0), None, t0);
        assert_eq!(
            gate.decide(&tree(&["ab", ""], 0), None, t0),
            SemanticsDecision::Deliver { forced: false }
        );
        assert_eq!(
            gate.decide(&tree(&["abc", ""], 0), None, t0),
            SemanticsDecision::Deliver { forced: false }
        );
    }

    #[test]
    fn signatures_ignore_values_and_caret_x_but_see_focus_roles_and_shape() {
        assert_eq!(
            structure_signature(&tree(&["a", ""], 0)),
            structure_signature(&tree(&["abc", "xyz"], 0))
        );
        assert_ne!(
            structure_signature(&tree(&["a", ""], 0)),
            structure_signature(&tree(&["a", ""], 1))
        );
        assert_ne!(
            structure_signature(&tree(&["a", ""], 0)),
            structure_signature(&tree(&["a", "", ""], 0))
        );
        let mut relabelled = tree(&["a", ""], 0);
        relabelled.root.children[1].semantics.as_mut().unwrap().role = Role::Button;
        assert_ne!(
            structure_signature(&tree(&["a", ""], 0)),
            structure_signature(&relabelled)
        );
        assert_eq!(
            ime_signature(Some(&ime(10, 20))),
            ime_signature(Some(&ime(90, 20)))
        );
        assert_ne!(
            ime_signature(Some(&ime(10, 20))),
            ime_signature(Some(&ime(10, 36)))
        );
        assert_ne!(ime_signature(Some(&ime(10, 20))), ime_signature(None));
    }
}
