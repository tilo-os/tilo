//! Quiet frames: a polled frame that nothing new reached lays out and draws
//! but neither syncs nor publishes, and anything that can reach a widget,
//! the document or what a frame presents brings both back on its next frame.
use super::*;
use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use std::cell::Cell;
use std::rc::Rc;

/// What the probe saw, shared with the test.
#[derive(Clone, Default)]
struct Seen {
    syncs: Rc<Cell<usize>>,
    published: Rc<Cell<usize>>,
    can_undo: Rc<Cell<bool>>,
    body: Rc<std::cell::RefCell<String>>,
    actions: Rc<Cell<usize>>,
}

/// A widget that always wants a frame (so every poll runs one), counts its
/// syncs and the undo statuses published to it, and keeps local state a
/// caller can change only through the harness.
struct Probe {
    seen: Seen,
    label: String,
    focused: bool,
}

impl crate::undo::UndoControl for Probe {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> crate::undo::UndoStatus) {
        self.seen.published.set(self.seen.published.get() + 1);
        self.seen.can_undo.set(status().can_undo);
    }
}

impl Widget for Probe {
    crate::provides!(dyn crate::undo::UndoControl);

    fn name(&self) -> &'static str {
        "Probe"
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            taffy::Style {
                size: Size::from_lengths(200.0, 40.0),
                ..Default::default()
            },
            node_map,
            self,
        )
    }
    fn get_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, 200.0, 40.0)
    }
    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.seen.syncs.set(self.seen.syncs.get() + 1);
        let (doc, _) = crate::doc::document_ref(state, path);
        if let Ok(Some((value, _))) = doc.get(automerge::ROOT, "body") {
            *self.seen.body.borrow_mut() = value.to_string();
        }
    }
    fn needs_redraw(&self) -> bool {
        true
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Button)
                .with_name(self.label.clone())
                .with_semantic_key("probe"),
        )
    }
    fn perform_action(
        &mut self,
        _: &crate::a11y::Action,
        _: &[u64],
        _: &mut crate::widget::Edits<'_>,
        _: &crate::widget::ModelPath,
    ) -> bool {
        self.seen.actions.set(self.seen.actions.get() + 1);
        true
    }
}

fn probe() -> (FrameHarness, Seen) {
    let seen = Seen::default();
    let mut frame = FrameHarness::for_root(
        State::new(),
        Probe {
            seen: seen.clone(),
            label: "Probe".into(),
            focused: false,
        },
        (300, 100),
        Theme::dark(),
    );
    // The first polled frame follows construction, which counts as a change.
    assert!(frame.poll_frame().unwrap());
    (frame, seen)
}

/// Polls one frame and says whether it was quiet. A loud frame synced and
/// published; a quiet one did neither on the production path (a debug
/// build then runs both anyway, to prove they would have changed nothing,
/// so the probe's counters cannot tell).
fn polled(frame: &mut FrameHarness, seen: &Seen) -> Quiet {
    let quiet = frame.quiet_frames();
    let (syncs, published) = (seen.syncs.get(), seen.published.get());
    assert!(frame.poll_frame().unwrap(), "the probe wants every frame");
    if frame.quiet_frames() > quiet {
        return Quiet::Yes;
    }
    assert!(
        seen.syncs.get() > syncs && seen.published.get() > published,
        "a loud frame syncs and publishes"
    );
    Quiet::No
}

#[derive(Debug, PartialEq)]
enum Quiet {
    Yes,
    No,
}

#[test]
fn a_frame_nothing_new_reached_neither_syncs_nor_publishes() {
    let (mut frame, seen) = probe();
    let quiet = frame.quiet_frames();
    for _ in 0..5 {
        assert_eq!(polled(&mut frame, &seen), Quiet::Yes);
    }
    assert_eq!(frame.quiet_frames(), quiet + 5);
}

/// Runs `entry` and proves nothing it changed is left behind a quiet
/// frame: either it ran a frame of its own, which synced and published, or
/// the next polled frame is loud.
fn entered(
    frame: &mut FrameHarness,
    seen: &Seen,
    what: &str,
    entry: impl FnOnce(&mut FrameHarness),
) {
    let frames = frame.relayout_frames();
    let (syncs, published) = (seen.syncs.get(), seen.published.get());
    let quiet = frame.quiet_frames();
    entry(frame);
    if frame.relayout_frames() > frames {
        assert_eq!(
            frame.quiet_frames(),
            quiet,
            "{what} ran a quiet frame of its own"
        );
        assert!(
            seen.syncs.get() > syncs && seen.published.get() > published,
            "{what}'s own frame synced and published"
        );
    } else {
        assert_eq!(polled(frame, seen), Quiet::No, "the frame after {what}");
    }
    assert_eq!(
        polled(frame, seen),
        Quiet::Yes,
        "the frame after that, after {what}"
    );
}

#[test]
fn each_entry_point_brings_sync_and_publication_back() {
    let (mut frame, seen) = probe();
    assert_eq!(polled(&mut frame, &seen), Quiet::Yes);

    entered(&mut frame, &seen, "root_mut", |frame| {
        frame.root_mut::<Probe>().label = "Renamed".into();
    });
    assert!(
        serde_json::to_string(&frame.inspection())
            .unwrap()
            .contains("Renamed"),
        "the frame shows the renamed probe"
    );
    entered(&mut frame, &seen, "a click", |frame| {
        frame.click_at(20.0, 20.0)
    });
    let target = frame.a11y_node_named("Renamed").expect("the probe's node");
    let actions = seen.actions.get();
    entered(&mut frame, &seen, "an accessibility action", |frame| {
        frame
            .perform_a11y_action(target, &crate::a11y::Action::Activate)
            .unwrap();
    });
    assert!(seen.actions.get() > actions, "the action reached the probe");
    entered(&mut frame, &seen, "presence_mut", |frame| {
        frame.presence_mut();
    });
    entered(&mut frame, &seen, "state_mut", |frame| {
        let state = frame.state_mut();
        state.doc.put(automerge::ROOT, "body", "edited").unwrap();
        state.commit_action(None, None);
    });
    assert_eq!(*seen.body.borrow(), "\"edited\"");
}

#[test]
fn a_document_change_no_entry_point_made_syncs_and_publishes_on_its_frame() {
    let (mut frame, seen) = probe();
    assert_eq!(polled(&mut frame, &seen), Quiet::Yes);
    assert!(!seen.can_undo.get());
    // As a merge from a peer lands: through no entry point of the harness.
    frame
        .state
        .doc
        .put(automerge::ROOT, "body", "merged")
        .unwrap();
    frame.state.commit_action(None, None);
    assert_eq!(polled(&mut frame, &seen), Quiet::No);
    assert_eq!(*seen.body.borrow(), "\"merged\"");
    assert!(seen.can_undo.get(), "undo is available on the same frame");
    assert_eq!(polled(&mut frame, &seen), Quiet::Yes);
}

#[test]
fn an_uncommitted_document_keeps_every_frame_loud() {
    let (mut frame, seen) = probe();
    frame
        .state
        .doc
        .put(automerge::ROOT, "body", "pending")
        .unwrap();
    assert_eq!(polled(&mut frame, &seen), Quiet::No);
    assert_eq!(polled(&mut frame, &seen), Quiet::No);
}

/// Every public `&mut self` entry point of [`FrameHarness`] (and every
/// crate-visible one) starts by invalidating local state, except the few
/// that judge their own effect. A new entry point that forgot would let a
/// caller change a widget behind a quiet frame's back, so this fails until
/// it is added, here or to the reasoned list below.
#[test]
fn every_mutable_entry_point_invalidates_local_state() {
    /// Entry points that call `invalidate_local` themselves when what they
    /// did can matter, or that reach nothing a frame reads.
    const JUDGE_THEMSELVES: &[(&str, &str)] = &[
        ("poll_frame", "the frame itself"),
        (
            "send_host_event",
            "a wheel coalesced into the next frame counts once dispatched",
        ),
        (
            "service_pending_owner_models_report",
            "invalidates when it took work up",
        ),
        (
            "service_pending_owner_models",
            "the report above, which judges",
        ),
        (
            "take_browser_font_demands",
            "drains the text renderer's fetch list",
        ),
        (
            "set_pan_replay",
            "chooses how later frames draw; reaches nothing a frame reads",
        ),
        (
            "settle_replayed_draws",
            "draws again a frame that only moved laid-out content, as a polled frame would",
        ),
        (
            "take_text_shape_stats",
            "drains the text renderer's shaping counters",
        ),
        (
            "send",
            "judged once dispatched, as the live App judges input: all but a wheel that only panned and inert input that moved nothing",
        ),
        ("scroll", "a wheel through `send`, which judges it"),
        (
            "idle",
            "invalidates when its control tick or save offer moved the document",
        ),
        ("take_profile_samples", "drains the profiler"),
        ("clear_profile_samples", "drains the profiler"),
    ];
    let sources = [
        ("headless.rs", include_str!("../headless.rs")),
        ("caller_palette.rs", include_str!("caller_palette.rs")),
        ("controls.rs", include_str!("controls.rs")),
        ("document_history.rs", include_str!("document_history.rs")),
        (
            "invocation_results.rs",
            include_str!("invocation_results.rs"),
        ),
        ("search_open.rs", include_str!("search_open.rs")),
        ("system_search.rs", include_str!("system_search.rs")),
    ];
    let mut checked = 0;
    let mut missing = Vec::new();
    for (file, source) in sources {
        let lines: Vec<&str> = source.lines().collect();
        let mut in_impl = false;
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            if line == "impl FrameHarness {" {
                in_impl = true;
            } else if in_impl && line == "}" {
                in_impl = false;
            }
            let name = line
                .strip_prefix("    pub fn ")
                .or_else(|| line.strip_prefix("    pub(crate) fn "))
                .filter(|_| in_impl)
                .and_then(|rest| rest.split(['(', '<']).next());
            let Some(name) = name else {
                index += 1;
                continue;
            };
            // rustfmt closes a method's signature on a line of its own
            // indentation ending in `{`.
            let closes = |line: &str| {
                line.ends_with(';') || (line.starts_with("    ") && line.ends_with('{'))
            };
            let mut end = index;
            while !closes(lines[end]) {
                end += 1;
            }
            let signature = lines[index..=end].join(" ");
            if signature.contains("&mut self") && lines[end].ends_with('{') {
                checked += 1;
                let first = lines[end + 1..]
                    .iter()
                    .map(|line| line.trim())
                    .find(|line| !line.is_empty() && !line.starts_with("//"));
                let judged = JUDGE_THEMSELVES.iter().any(|(quiet, _)| *quiet == name);
                if first != Some("self.invalidate_local();") && !judged {
                    missing.push(format!("{file}:{} {name}", index + 1));
                }
            }
            index = end + 1;
        }
    }
    assert!(checked > 100, "the scan found the entry points: {checked}");
    assert!(
        missing.is_empty(),
        "entry points that can change what a frame reads without invalidating: {missing:?}"
    );
}
