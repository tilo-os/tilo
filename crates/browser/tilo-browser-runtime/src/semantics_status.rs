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
//!
//! A wheel scrolling moves every node on screen on every frame, which no
//! reader follows: while it keeps turning, what changed waits until it rests
//! ([`SCROLL_REST`]), or until [`SCROLL_LONGEST`] after the last delivery in
//! a long scroll. Only focus moving or the host asking goes out at once.
//!
//! The host keeps the tree it holds, so after the first tree a delivery
//! carries only what changed since the last one
//! ([`ui::headless::write_inspection_patch`]), numbered so the host can tell
//! a patch made for a tree it does not hold and ask for the whole tree
//! again (`request_semantics`).
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

/// How long a wheel rests before the tree a scroll moved is delivered.
pub(crate) const SCROLL_REST: std::time::Duration = std::time::Duration::from_millis(150);

/// The longest a scroll holds the tree after the last delivery.
pub(crate) const SCROLL_LONGEST: std::time::Duration = std::time::Duration::from_secs(1);

/// How long after a presented frame its tree is built, at the soonest: long
/// enough that the build runs in a task of its own, after the browser took
/// the frame.
pub(crate) const AFTER_FRAME: std::time::Duration = std::time::Duration::from_millis(1);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SemanticsDecision {
    /// The host already holds this exact tree.
    Unchanged,
    /// Include the tree or the patch to it (see [`SemanticsGate::text`]);
    /// `forced` is true when a focus, structure, IME or host request
    /// bypassed the window.
    Deliver { forced: bool },
    /// A content-only change inside the window; deliver at the instant.
    Deferred(ui::time::Instant),
}

/// What the page's loop does about a tree waiting for its deadline.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TreeWait {
    /// Deliver it now, in a status of its own.
    Publish,
    /// Come back at the instant, when it falls due later.
    Wake(ui::time::Instant),
}

/// A tree past its deadline goes out with a status of its own, whatever the
/// frame before it did. Asking for one more frame instead loses the tree
/// whenever that frame does not come or does not draw — the GPU still holds
/// the last one, the window presents itself as HTML, the page goes idle
/// first — and the host then reads a screen whose content it cannot describe
/// until some unrelated change delivers a tree. A status settles the windows
/// a replayed frame replayed before it reads the tree, so a frame that
/// replayed its windows needs no frame drawn first.
pub(crate) fn tree_wait(
    due: Option<ui::time::Instant>,
    now: ui::time::Instant,
) -> Option<TreeWait> {
    let due = due?;
    Some(if due <= now {
        TreeWait::Publish
    } else {
        TreeWait::Wake(due)
    })
}

pub(crate) struct SemanticsGate {
    schedule: PublicationSchedule,
    /// The status members of the last delivery: the whole tree, or a patch.
    text: Vec<u8>,
    /// The patch to the held tree, written while deciding.
    ops: Vec<u8>,
    /// The tree the host holds: the last one delivered.
    last: Option<InspectionSnapshot>,
    /// Number of the tree the host holds.
    sequence: u64,
    structure: u64,
    ime: u64,
    requested: bool,
    /// The last input that could move things on screen was a wheel: until a
    /// key, a press or a touch, the caret's line moving on screen is the
    /// content scrolling under it, not the caret going to another line.
    scrolling: bool,
    /// When the last wheel input came.
    wheel_at: Option<ui::time::Instant>,
}

impl SemanticsGate {
    pub(crate) fn new(window: std::time::Duration) -> Self {
        Self {
            schedule: PublicationSchedule::new(window),
            text: Vec::new(),
            ops: Vec::new(),
            last: None,
            sequence: 0,
            structure: 0,
            ime: 0,
            requested: false,
            scrolling: false,
            wheel_at: None,
        }
    }
    /// Read an input the mount took: a wheel scrolls what the caret is in, a
    /// key, a press or a touch may move the caret itself.
    pub(crate) fn note_input(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        now: ui::time::Instant,
    ) {
        use ui::winit::event::WindowEvent;
        match event {
            WindowEvent::MouseWheel { .. } => {
                self.scrolling = true;
                self.wheel_at = Some(now);
            }
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::MouseInput { .. }
            | WindowEvent::Touch(_) => self.scrolling = false,
            _ => {}
        }
    }
    /// Whether the IME window `ime` (its signature) stands on another line
    /// than the tree the host holds says, as a reader must hear at once. While
    /// a wheel scrolls, the line moves with the content and waits like it.
    fn ime_moved(&self, ime: u64) -> bool {
        ime != self.ime && !self.scrolling
    }
    /// While a wheel scrolls, when the tree it moved is due: once the wheel
    /// rested, or [`SCROLL_LONGEST`] after the last delivery. `None` when no
    /// wheel scrolls or that instant passed.
    fn scroll_due(&self, now: ui::time::Instant) -> Option<ui::time::Instant> {
        if !self.scrolling {
            return None;
        }
        let rest = self.wheel_at?.checked_add(SCROLL_REST)?;
        let due = match self.schedule.last_published() {
            Some(last) => last
                .checked_add(SCROLL_LONGEST)
                .map_or(rest, |cap| rest.min(cap)),
            None => rest,
        };
        (now < due).then_some(due)
    }
    /// Hold what a scroll changed until it is due ([`Self::scroll_due`]).
    fn hold_scroll(&mut self, now: ui::time::Instant) -> Option<ui::time::Instant> {
        let due = self.scroll_due(now)?;
        self.schedule.hold(due);
        Some(due)
    }
    /// The host wants the full tree with the next status even if unchanged.
    pub(crate) fn request(&mut self) {
        self.requested = true;
    }
    /// The keyboard owner path of the tree the host holds: what the page
    /// matches its own accessible elements against, without building the
    /// tree again. A focus change is a structure change, so it is published
    /// at once rather than deferred, and this is the current one.
    pub(crate) fn published_keyboard_owner(&self) -> Option<String> {
        self.last
            .as_ref()
            .and_then(|last| last.keyboard_focus_owner.clone())
    }
    /// Whether a frame that only moved laid-out content would leave its
    /// tree unbuilt at `now` ([`Self::defer_unbuilt`], the IME line held),
    /// without scheduling anything.
    pub(crate) fn would_defer_unbuilt(&self, now: ui::time::Instant) -> bool {
        !self.requested && self.last.is_some() && self.schedule.would_defer(now)
    }
    /// Leave a presented frame's tree to a task of its own after the frame:
    /// building it walks every window's semantics, often the largest part
    /// of a frame, and the pixels an input asked for must not wait on it.
    /// The tree is due at once in that task when the host asked for one or
    /// the IME line moved; otherwise at the end of the window the last
    /// delivery opened, so typing builds one tree per window rather than
    /// one per key. False for the first tree, which goes with the first
    /// frame.
    pub(crate) fn after_frame(
        &mut self,
        ime: Option<&ImeWindowState>,
        now: ui::time::Instant,
    ) -> bool {
        if self.last.is_none() {
            return false;
        }
        let soon = now.checked_add(AFTER_FRAME).unwrap_or(now);
        let urgent = self.requested || self.ime_moved(ime_signature(ime));
        if !urgent && self.hold_scroll(now).is_some() {
            return true;
        }
        let at = match self.schedule.window_end(now) {
            Some(end) if !urgent => end.max(soon),
            _ => soon,
        };
        let at = self.schedule.due().map_or(at, |held| held.min(at));
        self.schedule.hold(at);
        true
    }
    /// Hold a moved frame's tree, whose window is over, for the frame after
    /// (due `now`), unless something else asks for it now: a request, the
    /// first tree, or the IME line moved. Returns whether it is held.
    pub(crate) fn hold_for_next_frame(
        &mut self,
        ime: Option<&ImeWindowState>,
        now: ui::time::Instant,
    ) -> bool {
        if self.requested || self.last.is_none() || self.ime_moved(ime_signature(ime)) {
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
    /// is too. Such a change waits unseen for the window's end, or a
    /// window from now when none is open, and the deadline delivers the
    /// tree as it then is. False, deferring nothing, when the host asked
    /// for a tree, none was delivered yet, the IME line moved or a held
    /// change is due.
    pub(crate) fn defer_unbuilt(
        &mut self,
        ime: Option<&ImeWindowState>,
        now: ui::time::Instant,
    ) -> bool {
        if self.requested || self.last.is_none() || self.ime_moved(ime_signature(ime)) {
            return false;
        }
        self.hold_scroll(now).is_some() || self.schedule.defer(now).is_some()
    }
    /// Leave the tree of a frame that laid nothing out unbuilt: it shows
    /// what the last tree did, so there is nothing to deliver and nothing
    /// to schedule. False when the host asked for a tree, none was
    /// delivered yet, the IME line moved or a held change is due.
    pub(crate) fn unchanged_unbuilt(
        &self,
        ime: Option<&ImeWindowState>,
        now: ui::time::Instant,
    ) -> bool {
        !self.requested
            && self.last.is_some()
            && !self.ime_moved(ime_signature(ime))
            && self.schedule.nothing_due(now)
    }
    /// The status members a delivery from the last [`Self::decide`] adds,
    /// without a leading comma: `"semanticsSeq":n,"semantics":{…}` for the
    /// whole tree, `"semanticsPatch":{"base":n-1,"seq":n,…}` for a patch.
    pub(crate) fn text(&self) -> &[u8] {
        &self.text
    }
    /// Compares the tree with the one the host holds and decides whether
    /// this status carries it; a delivery writes the patch to the held tree
    /// (the whole tree only for the first one or when the host asks).
    pub(crate) fn decide(
        &mut self,
        mut snapshot: InspectionSnapshot,
        ime: Option<&ImeWindowState>,
        now: ui::time::Instant,
    ) -> SemanticsDecision {
        let structure = structure_signature(&snapshot);
        let ime = ime_signature(ime);
        let requested = std::mem::take(&mut self.requested);
        self.ops.clear();
        let unchanged = self.last.as_ref().is_some_and(|last| {
            let stats = ui::headless::write_inspection_patch(last, &mut snapshot, &mut self.ops);
            stats.is_empty()
                && last.keyboard_focus_owner == snapshot.keyboard_focus_owner
                && last.viewport == snapshot.viewport
        });
        if unchanged && !requested {
            // The IME line may still have moved (editor blur), which counts
            // as a change for the next tree.
            self.ime = ime;
            self.schedule.clear_due();
            return SemanticsDecision::Unchanged;
        }
        // Content a scroll brought on screen or took off it changes the
        // structure too, and waits with the rest of the scroll; focus moving
        // does not.
        let focus_moved = self
            .last
            .as_ref()
            .is_some_and(|last| last.keyboard_focus_owner != snapshot.keyboard_focus_owner);
        let scroll_due = (!requested && !focus_moved)
            .then(|| self.scroll_due(now))
            .flatten();
        let forced = requested
            || focus_moved
            || (structure != self.structure && scroll_due.is_none())
            || self.ime_moved(ime);
        if let Some(due) = scroll_due.filter(|_| !forced) {
            self.schedule.hold(due);
            return SemanticsDecision::Deferred(due);
        }
        if forced {
            self.schedule.mark_published(now);
        } else if let PublicationDecision::Deferred(due) = self.schedule.decide(now) {
            return SemanticsDecision::Deferred(due);
        }
        let base = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        self.text.clear();
        if requested || self.last.is_none() {
            self.ops.clear();
            if serde_json::to_writer(&mut self.ops, &snapshot).is_err() {
                self.ops.clear();
                self.ops.extend_from_slice(b"null");
            }
            self.text.extend_from_slice(
                format!("\"semanticsSeq\":{},\"semantics\":", self.sequence).as_bytes(),
            );
            self.text.extend_from_slice(&self.ops);
        } else {
            let head = serde_json::json!({
                "base": base,
                "seq": self.sequence,
                "viewport": snapshot.viewport,
                "keyboard_focus_owner": snapshot.keyboard_focus_owner,
            })
            .to_string();
            self.text.extend_from_slice(b"\"semanticsPatch\":");
            // The object's closing brace is left off so the ops follow inside it.
            self.text
                .extend_from_slice(head.strip_suffix('}').unwrap_or(&head).as_bytes());
            self.text.extend_from_slice(b",\"ops\":");
            self.text.extend_from_slice(&self.ops);
            self.text.push(b'}');
        }
        self.last = Some(snapshot);
        self.structure = structure;
        self.ime = ime;
        SemanticsDecision::Deliver { forced }
    }
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
            page_title: None,
            layout: None,
            paint_key: None,
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
            page_title: None,
            layout: None,
            paint_key: None,
            semantics: None,
            children,
        };
        InspectionSnapshot {
            viewport: [100, 100],
            keyboard_focus_owner: Some(format!("/Column/TextInput[{focused}]")),
            root,
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
    fn a_presented_frame_leaves_its_tree_to_a_task_after_it() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        // The first tree goes with the first frame.
        assert!(!gate.after_frame(None, ms(0)));
        gate.decide(tree(&["a"], 0), None, ms(0));
        // A keystroke inside the window: one tree at its end, however many
        // frames the keys present before it.
        assert!(gate.after_frame(None, ms(20)));
        assert_eq!(gate.due(), Some(ms(100)));
        assert!(gate.after_frame(None, ms(60)));
        assert_eq!(gate.due(), Some(ms(100)));
        assert_eq!(
            gate.decide(tree(&["ab"], 0), None, ms(100)),
            SemanticsDecision::Deliver { forced: false }
        );
        // Past the window, the tree is built right after the frame, in a
        // task of its own.
        assert!(gate.after_frame(None, ms(300)));
        assert_eq!(gate.due(), Some(ms(300) + AFTER_FRAME));
        gate.decide(tree(&["abc"], 0), None, ms(301));
        // The IME line moving, or the host asking, does not wait for the
        // window.
        assert!(gate.after_frame(Some(&ime(0, 40)), ms(320)));
        assert_eq!(gate.due(), Some(ms(320) + AFTER_FRAME));
        gate.decide(tree(&["abc"], 0), Some(&ime(0, 40)), ms(321));
        gate.request();
        assert!(gate.after_frame(Some(&ime(0, 40)), ms(330)));
        assert_eq!(gate.due(), Some(ms(330) + AFTER_FRAME));
    }

    #[test]
    fn a_moved_frame_waits_unbuilt_for_the_window_or_a_window_from_now() {
        let t0 = ui::time::Instant::now();
        let mut gate = window();
        // Nothing delivered yet: the first tree is always built.
        assert!(!gate.defer_unbuilt(None, t0));
        assert_eq!(
            gate.decide(tree(&["a"], 0), None, t0),
            SemanticsDecision::Deliver { forced: true }
        );
        assert!(gate.defer_unbuilt(None, t0 + Duration::from_millis(10)));
        assert_eq!(gate.due(), Some(t0 + Duration::from_millis(100)));
        // An IME line that moved needs the tree at once.
        assert!(!gate.defer_unbuilt(Some(&ime(0, 40)), t0 + Duration::from_millis(20)));
        // So does a host request.
        gate.request();
        assert!(!gate.defer_unbuilt(None, t0 + Duration::from_millis(30)));
        assert_eq!(
            gate.decide(tree(&["b"], 0), None, t0 + Duration::from_millis(30)),
            SemanticsDecision::Deliver { forced: true }
        );
        // Past the window it waits a whole window from now, and later moved
        // frames keep that instant.
        assert!(gate.defer_unbuilt(None, t0 + Duration::from_millis(200)));
        assert_eq!(gate.due(), Some(t0 + Duration::from_millis(300)));
        assert!(gate.defer_unbuilt(None, t0 + Duration::from_millis(250)));
        assert_eq!(gate.due(), Some(t0 + Duration::from_millis(300)));
        assert!(!gate.would_defer_unbuilt(t0 + Duration::from_millis(300)));
        assert!(!gate.defer_unbuilt(None, t0 + Duration::from_millis(300)));
    }

    #[test]
    fn a_frame_that_laid_nothing_out_needs_no_tree_unless_one_is_due() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        // Nothing delivered yet: the first tree is always built.
        assert!(!gate.unchanged_unbuilt(None, ms(0)));
        gate.decide(tree(&["a"], 0), None, ms(0));
        // Long after, a frame that laid nothing out schedules nothing.
        assert!(gate.unchanged_unbuilt(None, ms(500)));
        assert_eq!(gate.due(), None);
        // An IME line that moved needs the tree.
        assert!(!gate.unchanged_unbuilt(Some(&ime(0, 40)), ms(510)));
        assert!(gate.defer_unbuilt(None, ms(520)));
        assert!(gate.unchanged_unbuilt(None, ms(600)));
        assert!(!gate.unchanged_unbuilt(None, ms(620)));
        // A host request needs the tree.
        gate.decide(tree(&["b"], 0), None, ms(620));
        gate.request();
        assert!(!gate.unchanged_unbuilt(None, ms(630)));
    }

    #[test]
    fn the_first_tree_and_unchanged_trees_follow_the_hash_gate() {
        let t0 = ui::time::Instant::now();
        let mut gate = window();
        assert_eq!(
            gate.decide(tree(&["a", ""], 0), None, t0),
            SemanticsDecision::Deliver { forced: true }
        );
        let whole = format!(
            "\"semanticsSeq\":1,\"semantics\":{}",
            serde_json::to_string(&tree(&["a", ""], 0)).unwrap()
        );
        assert_eq!(gate.text(), whole.as_bytes());
        assert_eq!(
            gate.decide(tree(&["a", ""], 0), None, t0 + Duration::from_millis(10)),
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
            gate.decide(tree(&["a", ""], 0), Some(&ime(10, 20)), ms(0)),
            SemanticsDecision::Deliver { forced: true }
        );
        // Typing: the value and the caret's horizontal position change every key.
        assert_eq!(
            gate.decide(tree(&["ab", ""], 0), Some(&ime(18, 20)), ms(30)),
            SemanticsDecision::Deferred(ms(100))
        );
        assert_eq!(gate.due(), Some(ms(100)));
        assert_eq!(
            gate.decide(tree(&["abc", ""], 0), Some(&ime(26, 20)), ms(60)),
            SemanticsDecision::Deferred(ms(100))
        );
        assert_eq!(
            gate.decide(tree(&["abcd", ""], 0), Some(&ime(34, 20)), ms(100)),
            SemanticsDecision::Deliver { forced: false }
        );
        // After the first tree, a delivery is the patch to the last one: the
        // edited field alone.
        let patch = std::str::from_utf8(gate.text()).unwrap();
        // Read as the page reads it: the members, in whatever order the
        // build's JSON writer puts them.
        let members: serde_json::Value = serde_json::from_str(&format!("{{{patch}}}")).unwrap();
        let head = &members["semanticsPatch"];
        assert_eq!(head["base"], 1, "{patch}");
        assert_eq!(head["seq"], 2, "{patch}");
        assert_eq!(
            head["keyboard_focus_owner"], "/Column/TextInput[0]",
            "{patch}"
        );
        assert_eq!(head["viewport"], serde_json::json!([100, 100]), "{patch}");
        assert_eq!(head["ops"].as_array().map(Vec::len), Some(1), "{patch}");
        assert_eq!(
            head["ops"][0]["p"],
            serde_json::json!(["root", 0, "s"]),
            "{patch}"
        );
        assert!(head["ops"][0]["own"].is_object(), "{patch}");
        assert!(patch.contains(r#""value":"abcd""#), "{patch}");
        assert!(!patch.contains("TextInput[1]"), "{patch}");
        assert_eq!(gate.due(), None);
        // Once the deadline passes with no further change the deferred tree is still the one that gets sent.
        assert_eq!(
            gate.decide(tree(&["abcde", ""], 0), Some(&ime(42, 20)), ms(150)),
            SemanticsDecision::Deferred(ms(200))
        );
        assert_eq!(
            gate.decide(tree(&["abcde", ""], 0), Some(&ime(42, 20)), ms(200)),
            SemanticsDecision::Deliver { forced: false }
        );
    }

    #[test]
    fn a_deferred_change_that_reverts_clears_the_deadline() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(tree(&["a", ""], 0), None, ms(0));
        assert_eq!(
            gate.decide(tree(&["ab", ""], 0), None, ms(30)),
            SemanticsDecision::Deferred(ms(100))
        );
        assert_eq!(
            gate.decide(tree(&["a", ""], 0), None, ms(60)),
            SemanticsDecision::Unchanged
        );
        assert_eq!(gate.due(), None);
    }

    #[test]
    fn focus_and_structure_changes_are_never_throttled() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(tree(&["a", ""], 0), None, ms(0));
        assert_eq!(
            gate.decide(tree(&["ab", ""], 0), None, ms(20)),
            SemanticsDecision::Deferred(ms(100))
        );
        // Focus moves to the second field: delivered at once, and the window restarts.
        assert_eq!(
            gate.decide(tree(&["ab", ""], 1), None, ms(40)),
            SemanticsDecision::Deliver { forced: true }
        );
        assert_eq!(gate.due(), None);
        assert_eq!(
            gate.decide(tree(&["ab", "x"], 1), None, ms(60)),
            SemanticsDecision::Deferred(ms(140))
        );
        // A node appears (Enter creates a line): delivered at once.
        assert_eq!(
            gate.decide(tree(&["ab", "x", ""], 1), None, ms(80)),
            SemanticsDecision::Deliver { forced: true }
        );
        // A node disappears: delivered at once.
        assert_eq!(
            gate.decide(tree(&["ab", "x"], 1), None, ms(90)),
            SemanticsDecision::Deliver { forced: true }
        );
    }

    #[test]
    fn an_ime_line_change_is_delivered_at_once_but_horizontal_caret_motion_is_not() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(tree(&["a", ""], 0), Some(&ime(10, 20)), ms(0));
        assert_eq!(
            gate.decide(tree(&["ab", ""], 0), Some(&ime(18, 20)), ms(20)),
            SemanticsDecision::Deferred(ms(100))
        );
        assert_eq!(
            gate.decide(tree(&["abc", ""], 0), Some(&ime(0, 36)), ms(40)),
            SemanticsDecision::Deliver { forced: true }
        );
        // The IME window disappearing (editor blur) with an unchanged tree needs no resend.
        assert_eq!(
            gate.decide(tree(&["abc", ""], 0), None, ms(50)),
            SemanticsDecision::Unchanged
        );
        // But its next content change is delivered at once because the IME state moved.
        assert_eq!(
            gate.decide(tree(&["abcd", ""], 0), Some(&ime(0, 36)), ms(60)),
            SemanticsDecision::Deliver { forced: true }
        );
    }

    fn wheel() -> ui::winit::event::WindowEvent {
        ui::winit::event::WindowEvent::MouseWheel {
            device_id: ui::winit::event::DeviceId::dummy(),
            delta: ui::winit::event::MouseScrollDelta::PixelDelta(
                ui::winit::dpi::PhysicalPosition::new(0.0, 40.0),
            ),
            phase: ui::winit::event::TouchPhase::Moved,
        }
    }

    #[test]
    fn a_scroll_carrying_the_caret_line_waits_for_the_wheel_to_rest_until_the_caret_moves() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(tree(&["a"], 0), Some(&ime(10, 200)), ms(0));
        // A wheel scrolls the note under the caret: its line moves on screen
        // on every frame, but nothing about it changed for a reader, and the
        // tree waits until the wheel rests, past the window.
        for (step, at) in [10, 50, 90, 130].into_iter().enumerate() {
            gate.note_input(&wheel(), ms(at));
            let y = 200 - 40 * u32::try_from(step + 1).unwrap();
            assert!(
                gate.defer_unbuilt(Some(&ime(10, y)), ms(at)),
                "a scrolled frame waits for the wheel to rest"
            );
        }
        assert_eq!(gate.due(), Some(ms(130) + SCROLL_REST));
        assert_eq!(
            gate.decide(tree(&["a"], 0), Some(&ime(10, 40)), ms(130) + SCROLL_REST),
            SemanticsDecision::Unchanged
        );
        // A key moves the caret to another line: a reader hears it at once.
        gate.note_input(
            &ui::synthetic_input::named_key(ui::winit::keyboard::NamedKey::ArrowDown),
            ms(300),
        );
        assert!(!gate.defer_unbuilt(Some(&ime(10, 56)), ms(300)));
    }

    #[test]
    fn a_long_scroll_delivers_now_and_then_and_focus_moving_at_once() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let longest = u64::try_from(SCROLL_LONGEST.as_millis()).unwrap();
        let mut gate = window();
        gate.decide(tree(&["a", ""], 0), None, ms(0));
        // Content the scroll brings on screen is a structure change: it waits
        // with the scroll instead of going out on every frame.
        gate.note_input(&wheel(), ms(20));
        assert_eq!(
            gate.decide(tree(&["a", "", "c"], 0), None, ms(20)),
            SemanticsDecision::Deferred(ms(20) + SCROLL_REST)
        );
        // The wheel keeps turning: the tree waits, but never longer than the
        // longest hold after the last delivery.
        let mut at = 20;
        while at + 16 < longest {
            at += 16;
            gate.note_input(&wheel(), ms(at));
            assert!(gate.defer_unbuilt(None, ms(at)));
        }
        assert_eq!(gate.due(), Some(ms(longest)));
        assert_eq!(
            gate.decide(tree(&["a", "", "c"], 0), None, ms(longest)),
            SemanticsDecision::Deliver { forced: true }
        );
        // Focus moving mid-scroll is heard at once.
        gate.note_input(&wheel(), ms(longest + 16));
        assert_eq!(
            gate.decide(tree(&["a", "", "c"], 1), None, ms(longest + 16)),
            SemanticsDecision::Deliver { forced: true }
        );
    }

    #[test]
    fn a_host_request_resends_even_an_unchanged_tree() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut gate = window();
        gate.decide(tree(&["a", ""], 0), None, ms(0));
        assert_eq!(
            gate.decide(tree(&["a", ""], 0), None, ms(10)),
            SemanticsDecision::Unchanged
        );
        gate.request();
        assert_eq!(
            gate.decide(tree(&["a", ""], 0), None, ms(20)),
            SemanticsDecision::Deliver { forced: true }
        );
        assert_eq!(
            gate.decide(tree(&["a", ""], 0), None, ms(30)),
            SemanticsDecision::Unchanged
        );
        assert_eq!(
            gate.decide(tree(&["ab", ""], 0), None, ms(40)),
            SemanticsDecision::Deferred(ms(120))
        );
        gate.request();
        assert_eq!(
            gate.decide(tree(&["abc", ""], 0), None, ms(50)),
            SemanticsDecision::Deliver { forced: true }
        );
        assert_eq!(gate.due(), None);
    }

    #[test]
    fn a_tree_past_its_deadline_is_published_whatever_the_frame_before_it_did() {
        let t0 = ui::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        // A frame holds its tree for after it, then the deadline carries the
        // tree out: the loop never waits for one more frame to draw it. A
        // frame that replayed its windows instead of drawing them is the same
        // case — its tree is settled before it is read — and so is a frame that
        // never came.
        assert_eq!(tree_wait(Some(ms(10)), ms(10)), Some(TreeWait::Publish));
        assert_eq!(tree_wait(Some(ms(10)), ms(11)), Some(TreeWait::Publish));
        // Before its instant the loop comes back for it.
        assert_eq!(tree_wait(Some(ms(10)), ms(9)), Some(TreeWait::Wake(ms(10))));
        // With nothing waiting, nothing is delivered or woken for.
        assert_eq!(tree_wait(None, ms(10)), None);
        // The gate leaves a presented frame's tree for its deadline, and the
        // deadline alone carries it: the status is read from the screen the
        // frame drew, not from a frame that would have to come first.
        let mut gate = window();
        gate.decide(tree(&["a", ""], 0), None, ms(0));
        assert!(gate.after_frame(None, ms(20)));
        let due = gate.due().expect("the tree waits for the window's end");
        assert_eq!(tree_wait(Some(due), due), Some(TreeWait::Publish));
        assert_eq!(
            gate.decide(tree(&["ab", ""], 0), None, due),
            SemanticsDecision::Deliver { forced: false }
        );
        assert_eq!(gate.due(), None);
    }

    #[test]
    fn a_zero_window_delivers_every_changed_tree() {
        let t0 = ui::time::Instant::now();
        let mut gate = SemanticsGate::new(Duration::ZERO);
        gate.decide(tree(&["a", ""], 0), None, t0);
        assert_eq!(
            gate.decide(tree(&["ab", ""], 0), None, t0),
            SemanticsDecision::Deliver { forced: false }
        );
        assert_eq!(
            gate.decide(tree(&["abc", ""], 0), None, t0),
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
