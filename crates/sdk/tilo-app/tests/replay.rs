#![allow(clippy::unwrap_used, reason = "test assertions")]

//! A recording replays in the app it recorded: a private window that reads
//! and is never written, opening History at the recording's first step,
//! each step drawn by the app's own view as the state it was
//! (`docs/architecture/session-replay.md`).

use tilo_app::prelude::*;
use tilo_resource::recording::{Limits, record};

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Shopping {
    pub title: String,
    pub items: Collection<Entry>,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Entry {
    pub text: String,
}

#[resource(
    kind = "test.shopping",
    label = "Shopping",
    app = "so.tilo.test-shopping"
)]
impl Shopping {
    /// Rename the list.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }

    /// Add an entry.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, text: String) {
        self.items.push(cx.id(), Entry { text });
    }
}

struct ShoppingApp;

impl ResourceApp for ShoppingApp {
    type Resource = Shopping;
    const ID: &'static str = "so.tilo.test-shopping";
    const TITLE: &'static str = "Shopping";

    fn view(cx: &mut ViewCx<Shopping>) -> Element {
        Box::new(
            Column::new()
                .push(Box::new(cx.text(|list| list.title.clone())))
                .push(Box::new(
                    cx.text(|list| format!("{} items", list.items.len())),
                )),
        )
    }
}

/// A list someone edited: renamed it, then added three things.
fn edited() -> (tilo_resource::Doc, usize) {
    let mut app = HeadlessApp::<ShoppingApp>::new().unwrap();
    app.invoke(Shopping::RENAME.with("Groceries")).unwrap();
    let before = app.versions().len();
    for text in ["bread", "milk", "eggs"] {
        app.invoke(Shopping::ADD.with(text)).unwrap();
    }
    app.invoke(Shopping::RENAME.with("Sunday groceries"))
        .unwrap();
    let made = app.versions().len().saturating_sub(before);
    (app.peer_document(), made)
}

fn recording_file(doc: &tilo_resource::Doc, steps: usize) -> (OpenFile, Vec<Shopping>) {
    let mut doc = doc.clone();
    let heads = doc.get_heads();
    let cut = record(
        &doc,
        0,
        &heads,
        &Limits {
            steps,
            ..Limits::default()
        },
    )
    .unwrap();
    // The state the recorded document held at each step, the start first.
    let start = doc
        .get_change_by_hash(&cut.steps.first().unwrap().original)
        .unwrap()
        .deps()
        .to_vec();
    let states = std::iter::once(start)
        .chain(cut.steps.iter().map(|step| vec![step.original]))
        .map(|heads| Shopping::read(&doc.fork_at(&heads).unwrap()).unwrap())
        .collect();
    let (file, _) = OpenFile::new(
        "list.tilo-recording",
        tilo_content::RECORDING.mime,
        cut.bytes(),
    )
    .unwrap();
    (file, states)
}

#[test]
fn a_recording_replays_read_only_and_opens_history_at_its_start() {
    let (doc, made) = edited();
    let (file, states) = recording_file(&doc, made);
    let mut replay = HeadlessApp::<ShoppingApp>::replaying(file).unwrap();
    replay.settle();
    assert_eq!(
        replay.state(),
        *states.last().unwrap(),
        "the window shows the recording's last state"
    );
    assert!(
        replay.harness().targets().contains_key("activity_timeline"),
        "History opens with the window"
    );
    assert!(
        replay.invoke(Shopping::ADD.with("butter")).is_err(),
        "a replay is read, never written"
    );
    assert_eq!(replay.state(), *states.last().unwrap());
}

#[test]
fn every_step_of_a_replay_draws_the_state_the_recorded_window_had() {
    let (doc, made) = edited();
    let (file, states) = recording_file(&doc, made);
    let replay = HeadlessApp::<ShoppingApp>::replaying(file).unwrap();
    // The replay's own record holds the recording's changes after its
    // identity: each step is one of them, in order.
    let recorded: Vec<_> = replay
        .versions()
        .into_iter()
        .filter(|version| version.time_ms > 0 || version.context.is_some())
        .collect();
    let steps: Vec<_> = recorded
        .iter()
        .skip_while(|version| {
            version
                .context
                .as_ref()
                .is_none_or(|context| context.change != tilo_actions::ChangeKind::External)
        })
        .collect();
    assert_eq!(steps.len(), states.len(), "{recorded:?}");
    for (step, (version, state)) in steps.iter().zip(&states).enumerate() {
        let shown = replay.past_version(&[version.hash]).unwrap();
        assert_eq!(*shown.state(), *state, "step {step}");
    }
}

#[test]
fn a_recording_opens_only_in_a_private_window() {
    let (doc, made) = edited();
    let (file, _) = recording_file(&doc, made);
    assert!(
        HeadlessApp::<ShoppingApp>::handed(file).is_err(),
        "a window that reaches the person's records never takes a recording in"
    );
}

/// Stepping a replay looks at past states, a read: an agent steps the
/// read-only replay window through its History, each step drawn as the
/// state it was, and nothing in the window changes.
#[test]
fn an_agent_steps_a_replay_through_its_history() {
    let (doc, made) = edited();
    let (file, states) = recording_file(&doc, made);
    let mut replay = HeadlessApp::<ShoppingApp>::replaying(file).unwrap();
    replay.settle();
    // As an agent's `tilo_invoke` reaches the window.
    let mut call = |action: &str, arguments: serde_json::Value| {
        replay.call(action, arguments).unwrap().unwrap()
    };
    let listed = call("history", serde_json::json!({}));
    let changes: Vec<String> = listed["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|version| version["change"].as_str().unwrap().to_owned())
        .collect();
    let steps = &changes[changes.len() - states.len()..];
    let mut drawn = Vec::new();
    for change in steps {
        let shown = call("render", serde_json::json!({"version": change}));
        drawn.push(shown["revision"].clone());
    }
    drawn.dedup();
    assert_eq!(drawn.len(), states.len(), "each step draws its own state");
    assert_eq!(
        replay.state(),
        *states.last().unwrap(),
        "stepping writes nothing"
    );
}
