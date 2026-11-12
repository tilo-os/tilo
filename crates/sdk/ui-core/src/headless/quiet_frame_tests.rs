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
    /// Arm the probe's next draw to ask for its tree to be built again, as a slide landing drops
    /// the parts its last pose hid.
    landing: Rc<Cell<bool>>,
    /// The rebuild a draw asked for, for the frame that reads the structure change.
    landed: Rc<Cell<bool>>,
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
    fn draw(&mut self, _: &mut Scene, _: Affine, _: &mut TextRenderer, _: &Theme, _: &PresenceCtx) {
        // What the frame drew asked for its parts to be laid out again.
        if self.seen.landing.replace(false) {
            self.seen.landed.set(true);
        }
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.seen.landed.replace(false)
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
    // A click is judged event by event, as the live App judges input: the
    // move onto the probe changes nothing a frame reads and its frame stays
    // quiet, while the press and release, which nothing took, sync and
    // publish.
    let (syncs, published) = (seen.syncs.get(), seen.published.get());
    frame.click_at(20.0, 20.0);
    assert!(
        seen.syncs.get() > syncs && seen.published.get() > published,
        "a click's own frames synced and published"
    );
    assert_eq!(
        polled(&mut frame, &seen),
        Quiet::Yes,
        "the frame after that, after a click"
    );
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

/// A wheel nothing takes (past the end of a list; the probe scrolls
/// nothing) activates nothing, as the live App reads it: the frame it runs
/// and the one after are quiet. Each such notch synced and republished.
#[test]
fn a_wheel_nothing_takes_leaves_frames_quiet() {
    let (mut frame, seen) = probe();
    assert_eq!(polled(&mut frame, &seen), Quiet::Yes);
    let quiet = frame.quiet_frames();
    frame.scroll(0.0, -1.0);
    assert!(
        frame.quiet_frames() > quiet,
        "the wheel's own frame was quiet"
    );
    assert_eq!(polled(&mut frame, &seen), Quiet::Yes);
}

/// A frame that draws a rebuild into being hands it to the next one, which syncs for it: work a
/// widget's tick asks for after the paint (a slide landing drops the parts its last pose hid)
/// arrives once the frame has decided to sync nothing, so the frame that draws it does not rest.
#[test]
fn a_frame_that_draws_a_rebuild_into_being_hands_it_to_the_next_one() {
    let (mut frame, seen) = probe();
    assert_eq!(polled(&mut frame, &seen), Quiet::Yes);
    seen.landing.set(true);
    // The frame that drew the landing synced nothing, and the one after it rebuilds and syncs.
    assert_eq!(polled(&mut frame, &seen), Quiet::Yes);
    assert_eq!(polled(&mut frame, &seen), Quiet::No);
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

/// Every public `&mut self` entry point of [`FrameHarness`], and every
/// `pub(crate)` one, starts by invalidating local state, or says in its own
/// doc comment why it cannot leave a frame stale:
///
/// ```text
/// /// Quiet: drains the profiler's counters; nothing a frame reads.
/// ```
///
/// A new entry point that forgot would let a caller change a widget behind a
/// quiet frame's back, so this fails until it invalidates or documents why it
/// need not. The scan reads the crate's sources from disk, so an entry point in
/// any file of the headless host is covered without being named here.
#[test]
fn every_mutable_entry_point_invalidates_local_state() {
    const QUIET: &str = "Quiet: ";
    let sources = crate_sources();
    let mut checked = 0;
    let mut impls = 0;
    let mut missing = Vec::new();
    for (file, source) in &sources {
        let lines: Vec<&str> = source.lines().collect();
        let mut index = 0;
        // Depth of the `impl FrameHarness` block `index` is in, when it is in one.
        let mut depth = 0;
        while index < lines.len() {
            let line = lines[index];
            let code = line.trim_start();
            if depth == 0 && code.starts_with("impl") && code.contains("FrameHarness") {
                assert!(
                    code.ends_with('{'),
                    "{file}:{} the scan reads `impl FrameHarness` only written on one line ending in `{{`",
                    index + 1
                );
                impls += 1;
                depth = brace_depth(code);
                index += 1;
                continue;
            }
            if depth == 0 {
                index += 1;
                continue;
            }
            let mut end = index;
            let entry = (depth == 1)
                .then(|| {
                    let rest = code
                        .strip_prefix("pub fn ")
                        .or_else(|| code.strip_prefix("pub(crate) fn "))?;
                    Some(rest.split(['(', '<']).next()?.to_owned())
                })
                .flatten();
            let Some(name) = entry else {
                depth += brace_depth(line);
                index += 1;
                continue;
            };
            // rustfmt ends a method's signature with `{`, or with `;` where it has no body.
            while !code_ends(lines[end]) {
                end += 1;
            }
            let signature = lines[index..=end].join(" ");
            if signature.contains("&mut self") && lines[end].trim_end().ends_with('{') {
                checked += 1;
                let body = &lines[end + 1..];
                let first = body
                    .iter()
                    .map(|line| line.trim())
                    .find(|line| !line.is_empty() && !line.starts_with("//"));
                if first != Some("self.invalidate_local();")
                    && quiet_reason(&lines, index).is_none()
                {
                    missing.push(format!(
                        "{file}:{} {name} starts with {first:?} and has no `{QUIET}` reason",
                        index + 1
                    ));
                }
            }
            depth += brace_depth(&lines[index..=end].join(" "));
            index = end + 1;
            assert!(depth >= 0, "the scan lost the brace depth in {file}");
        }
    }
    assert!(
        sources.len() > 100,
        "the scan found the crate's sources: {}",
        sources.len()
    );
    assert!(impls > 1, "the scan found the harness: {impls} impl blocks");
    assert!(checked > 100, "the scan found the entry points: {checked}");
    assert!(
        missing.is_empty(),
        "entry points that can change what a frame reads without invalidating: {missing:?}"
    );
}

/// The crate's Rust sources, as their path under `src` and their text.
fn crate_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("the crate's sources are readable")
            .map(|entry| entry.expect("a source directory entry").path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path).expect("a source file is readable");
                let file = path
                    .strip_prefix(root)
                    .expect("a source lives under src")
                    .display()
                    .to_string();
                out.push((file, text));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    walk(&root, &root, &mut sources);
    sources
}

/// The `Quiet: <why>` reason the doc comment above `line` gives, when it gives one.
fn quiet_reason(lines: &[&str], line: usize) -> Option<String> {
    let mut line = line;
    // Attributes (`#[must_use]`, `#[cfg(...)]`) may stand between the doc and the item.
    while line > 0 && lines[line - 1].trim_start().starts_with("#[") {
        line -= 1;
    }
    let mut reason = None;
    while line > 0 {
        let Some(doc) = lines[line - 1].trim_start().strip_prefix("///") else {
            break;
        };
        let text = doc.trim();
        if let Some(rest) = text.strip_prefix("Quiet: ") {
            reason = Some(rest.trim().to_owned());
        }
        line -= 1;
    }
    reason
}

/// Whether `line` closes what stands open in a method's signature: the body's opening brace, or
/// the `;` of a bodyless item.
fn code_ends(line: &str) -> bool {
    let code = line.trim_end();
    code.ends_with('{') || code.ends_with(';')
}

/// The braces `line` opens, less the ones it closes, counting no comment, string or char literal.
fn brace_depth(line: &str) -> i64 {
    let chars: Vec<char> = line.chars().collect();
    let mut depth = 0;
    let mut index = 0;
    let mut literal: Option<char> = None;
    while index < chars.len() {
        let character = chars[index];
        index += 1;
        if let Some(quote) = literal {
            if character == '\\' {
                index += 1;
            } else if character == quote {
                literal = None;
            }
            continue;
        }
        match character {
            '"' => literal = Some('"'),
            '\'' => {
                // A lifetime (`'a`) quotes nothing; a char literal (`'{'`) does.
                let escaped = chars.get(index) == Some(&'\\');
                let closes = chars.get(index + 1) == Some(&'\'');
                if !escaped && !closes {
                    continue;
                }
                literal = Some('\'');
            }
            '/' if chars.get(index) == Some(&'/') => break,
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}
