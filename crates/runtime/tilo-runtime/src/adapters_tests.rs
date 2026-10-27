use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    sync::{Arc, Mutex as StdMutex},
};

/// What a fake screen shows, given how many steps have changed it so far.
type Painter = Box<dyn Fn(u32) -> image::RgbaImage + Send>;

/// A control socket whose screen is whatever the test says it is: `index`
/// counts the snapshots served, so a test can change what the app shows
/// between the steps of a run.
struct FakeScreen {
    computer: Arc<Computer>,
    shown: Arc<StdMutex<(u32, Painter)>>,
    _dir: tempfile::TempDir,
}

impl FakeScreen {
    fn start(shown: impl Fn(u32) -> image::RgbaImage + Send + 'static) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("control.sock");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        let shown: Arc<StdMutex<(u32, Painter)>> = Arc::new(StdMutex::new((0u32, Box::new(shown))));
        let serving = Arc::clone(&shown);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let serving = Arc::clone(&serving);
                // One thread per connection, as the compositor serves its
                // clients: a step's screenshot opens its own connection while
                // the step's own is open.
                std::thread::spawn(move || answer(stream, &serving));
            }
        });
        Self {
            computer: Arc::new(Computer::new(socket, scratch)),
            shown,
            _dir: dir,
        }
    }

    /// Put the app back where a run starts and paint it with `shown`.
    fn show(&self, shown: impl Fn(u32) -> image::RgbaImage + Send + 'static) {
        let mut guard = self.shown.lock().unwrap();
        guard.0 = 0;
        guard.1 = Box::new(shown);
    }
}

/// Answer one line of a control connection, showing what `shown` says.
fn answer(stream: std::os::unix::net::UnixStream, shown: &StdMutex<(u32, Painter)>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let command: Value = serde_json::from_str(&line).unwrap();
    let changed = matches!(command["op"].as_str(), Some("key" | "type"));
    // A step that changes the app is what the next picture shows, so the
    // screen moves on as a real app's does.
    let mut guard = shown.lock().unwrap();
    let (steps, painter) = &mut *guard;
    if changed {
        *steps = steps.saturating_add(1);
    }
    let picture = painter(*steps);
    drop(guard);
    if command["op"] == "snapshot" {
        picture.save(command["path"].as_str().unwrap()).unwrap();
    }
    let reply = if command["op"] == "agent_seat" {
        json!({"answer":{"viewport":[0.0, 0.0, 640, 400],"window":command["window"]}})
    } else {
        json!({"ok":{"frame":0}})
    };
    let mut stream = stream;
    writeln!(stream, "{reply}").unwrap();
}

/// A blank screen, or one with a filled box at `at`: the shapes a mark is
/// made of.
fn screen(boxes: &[(u32, u32, u32, u32)]) -> image::RgbaImage {
    let mut picture = image::RgbaImage::from_pixel(640, 400, image::Rgba([255, 255, 255, 255]));
    for &(x, y, width, height) in boxes {
        for row in y..y.saturating_add(height) {
            for column in x..x.saturating_add(width) {
                picture.put_pixel(column, row, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
    picture
}

/// The drift a replay of `recording` answers with on `fake`, so a test reads
/// a failed check without `Outcome` having to be printed.
fn drift_of(recording: &Recording, fake: &FakeScreen) -> Drift {
    match recording.replay(&fake.computer, None) {
        Err(error) => match Drift::of(&error) {
            Some(drift) => drift.clone(),
            None => panic!("a replay that stops is a failed check: {error}"),
        },
        Ok(outcomes) => panic!("the check should have failed, {} steps ran", outcomes.len()),
    }
}

/// A step that acts in the legacy app's window, as an adapter's steps do:
/// coordinates are the window's pixels, so none of them needs the screen.
/// The app before the task runs and after it saved: the document, and then
/// the document with a save bar the key step brings up.
fn document_then_saved(steps: u32) -> image::RgbaImage {
    if steps == 0 {
        screen(&[(10, 10, 80, 40)])
    } else {
        screen(&[(10, 10, 80, 40), (300, 200, 60, 20)])
    }
}

/// The same app after an update moved its save bar: the second step's screen
/// is not where the recording expects it.
fn document_then_moved_bar(steps: u32) -> image::RgbaImage {
    if steps == 0 {
        screen(&[(10, 10, 80, 40)])
    } else {
        screen(&[(10, 10, 80, 40), (500, 300, 60, 20)])
    }
}

fn click(x: f64, y: f64) -> Action {
    Action {
        action: "left_click".into(),
        coordinate: Some([x, y]),
        app_id: Some("app.host:browser".into()),
        ..Action::default()
    }
}

/// A key step in the same app's window.
fn key(chord: &str) -> Action {
    Action {
        action: "key".into(),
        key: Some(chord.into()),
        app_id: Some("app.host:browser".into()),
        ..Action::default()
    }
}

fn app() -> AppId {
    AppId::parse("app.host:browser").unwrap()
}

/// A recording of two steps, marked as an agent that looked at the screen
/// after each would: it marks the frame the step it just took returned, which
/// is all a model ever sees.
fn recording(fake: &FakeScreen) -> Recording {
    let mut recording = Recording::new(app(), "save the invoice", None);
    let mark = |outcome: &Outcome| Mark {
        screen: mark_of_outcome(outcome).expect("the step returned a frame"),
        expect: false,
    };
    fake.show(document_then_saved);
    let clicked = recording
        .record(
            &fake.computer,
            None,
            click(30.0, 30.0),
            &Capture {
                mark: None,
                screenshot: Screenshot::Always,
            },
        )
        .unwrap();
    // The agent marks what it was shown.
    recording.steps[0].check = Some(mark(&clicked).screen);
    let saved = recording
        .record(
            &fake.computer,
            None,
            key("ctrl+s"),
            &Capture {
                mark: None,
                screenshot: Screenshot::Always,
            },
        )
        .unwrap();
    recording.steps[1].check = Some(mark(&saved).screen);
    recording
}

#[test]
fn a_run_an_agent_did_by_hand_replays_with_no_model() {
    let fake = FakeScreen::start(document_then_saved);
    let recording = recording(&fake);
    assert_eq!(recording.steps.len(), 2);
    assert_eq!(recording.steps[0].action.action, "left_click");
    assert_eq!(recording.steps[1].action.action, "key");

    // A later invocation runs the same steps and nothing asks a model: the
    // replay is `computer` calls alone.
    fake.show(document_then_saved);
    let outcomes = recording.replay(&fake.computer, None).unwrap();
    assert_eq!(outcomes.len(), 2);
}

#[test]
fn a_screen_that_moved_breaks_its_check_and_says_where() {
    let fake = FakeScreen::start(document_then_saved);
    let recording = recording(&fake);

    // An update moved the save bar: the first step still lands, and the
    // screen the second step is marked to leave is not there any more.
    fake.show(document_then_moved_bar);
    let drift = drift_of(&recording, &fake);
    assert_eq!(drift.step, 1);
    assert!(!drift.reason.is_empty());
}

#[test]
fn a_drifted_task_falls_back_and_proposes_a_repaired_adapter() {
    let fake = FakeScreen::start(document_then_saved);
    let mut recording = recording(&fake);

    fake.show(document_then_moved_bar);
    let drift = drift_of(&recording, &fake);

    // The agent does the task live, and those steps become the recording.
    let before = recording.current();
    assert_eq!(before.steps.len(), 2);
    recording.fall_back(&drift, None);
    assert_eq!(recording.fallbacks, 1);
    assert!(recording.repaired);
    recording
        .record(
            &fake.computer,
            None,
            key("ctrl+shift+s"),
            &Capture {
                mark: Some(Mark {
                    screen: Screen::mark(&document_then_moved_bar(1)).unwrap(),
                    expect: false,
                }),
                screenshot: Screenshot::Always,
            },
        )
        .unwrap();

    let (prior, repaired) = recording.repair();
    assert!(!repaired.is_trivial(&prior), "a new step is worth review");
    assert_eq!(prior.steps.len(), 2);
    assert_eq!(repaired.steps.len(), 1);
    let described: Value = serde_json::from_str(&repaired.describe(&prior)).unwrap();
    assert_eq!(described["fallbacks"], 1);
    assert_eq!(described["drifted_at"], 1);
}

#[test]
fn a_replay_that_leaves_the_same_steps_is_no_change_for_a_person_to_review() {
    let fake = FakeScreen::start(document_then_saved);
    let mut recording = recording(&fake);
    // The drift was a transient: the agent's live run does exactly what the
    // recording already said, so there is nothing new to review.
    let same = recording.steps.clone();
    recording.fall_back(
        &Drift {
            step: 0,
            reason: "a repaint".into(),
        },
        None,
    );
    for step in same {
        recording.push(step).unwrap();
    }
    let (prior, repaired) = recording.repair();
    assert!(repaired.is_trivial(&prior));
    assert!(repaired.describe(&prior).contains("\"moved\":0"));

    // A step that moved is a change a person is shown.
    recording.steps[0].action.coordinate = Some([500.0, 300.0]);
    let (prior, repaired) = recording.repair();
    assert!(!repaired.is_trivial(&prior));
    let described: Value = serde_json::from_str(&repaired.describe(&prior)).unwrap();
    assert_eq!(described["moved"], 1);
}

#[test]
fn a_mark_at_a_new_place_is_a_different_screen() {
    let here = Screen::mark(&screen(&[(10, 10, 80, 40)])).unwrap();
    let there = Screen::mark(&screen(&[(400, 250, 80, 40)])).unwrap();
    assert!(here.matches(&here.clone()));
    assert!(!here.matches(&there));
    assert!(here.distance(&there) > CELL_TOLERANCE);
}

#[test]
fn a_task_is_found_by_the_words_a_person_uses_and_kept_beside_other_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let adapters = Adapters::new(directory.path().join("adapters"));
    let fake = FakeScreen::start(document_then_saved);
    let saving = recording(&fake);
    adapters.save(&saving).unwrap();

    let mut printing = Recording::new(app(), "print the invoice", None);
    printing
        .push(Recorded {
            action: click(12.0, 12.0),
            check: None,
            expect: None,
            after: None,
        })
        .unwrap();
    adapters.save(&printing).unwrap();

    // The same words, said with different case and spacing, reach the same
    // adapter; the app's other task is still there.
    let found = adapters.find(&app(), "  Save  the Invoice. ").unwrap();
    assert_eq!(found.steps.len(), 2);
    let printed = adapters.find(&app(), "print the invoice").unwrap();
    assert_eq!(printed.steps.len(), 1);
    assert!(matches!(
        adapters.find(&app(), "something else"),
        Err(AdapterError::Missing)
    ));
}

#[test]
fn an_adapters_file_only_answers_for_its_own_app() {
    let directory = tempfile::tempdir().unwrap();
    let adapters = Adapters::new(directory.path().join("adapters"));
    let fake = FakeScreen::start(document_then_saved);
    adapters.save(&recording(&fake)).unwrap();

    let other = AppId::parse("app.host:office").unwrap();
    assert!(matches!(
        adapters.find(&other, "save the invoice"),
        Err(AdapterError::Missing)
    ));
}

#[test]
fn a_mark_is_the_cells_of_a_screen_however_a_tool_carried_them() {
    let screen = Screen::mark(&screen(&[(10, 10, 80, 40)])).unwrap();
    let array = Mark::parse(&json!(screen.cells)).unwrap();
    let string = Mark::parse(&json!(
        screen
            .cells
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    ))
    .unwrap();
    assert_eq!(array, string);
    assert!(Mark::parse(&json!([1, 2, 3])).is_err());
    assert!(
        Mark::parse(&json!(vec![300u32; screen.cells.len()])).is_err(),
        "a cell is a fraction of a cell, not a level"
    );
}

#[tokio::test]
async fn a_task_learned_through_the_tools_runs_live_then_needs_no_model() {
    let fake = FakeScreen::start(document_then_saved);
    let directory = tempfile::tempdir().unwrap();
    let adapters = Adapters::new(directory.path().join("adapters"));
    let cells = |steps: u32| {
        let mark = Screen::mark(&document_then_saved(steps)).unwrap();
        Value::Array(mark.cells.iter().map(|cell| json!(cell)).collect())
    };

    // An agent worked the task out by hand; these are the steps it took.
    let learned = crate::adapter_tools::call(
        "adapter_learn",
        json!({
            "app": app().as_str(),
            "task": "save the invoice",
            "steps": [
                {"action": click(30.0, 30.0), "mark": cells(0)},
                {"action": key("ctrl+s"), "mark": cells(1)},
            ],
        }),
        &adapters,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(learned["steps"], 2);
    assert_eq!(learned["learned"], true);

    // A later run replays them with no model at all.
    fake.show(document_then_saved);
    let ran = crate::adapter_tools::call(
        "adapter_run",
        json!({"app": app().as_str(), "task": "Save the invoice."}),
        &adapters,
        Some(Arc::clone(&fake.computer)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(ran["done"], true, "{ran}");

    // The app moved its save bar: the replay says where it stopped and the
    // agent does the task live instead.
    fake.show(document_then_moved_bar);
    let drifted = crate::adapter_tools::call(
        "adapter_run",
        json!({"app": app().as_str(), "task": "save the invoice"}),
        &adapters,
        Some(Arc::clone(&fake.computer)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(drifted["done"], false, "{drifted}");
    assert_eq!(drifted["drifted_at"], 1);
    assert!(!drifted["reason"].as_str().unwrap().is_empty());

    // The live run is handed back: it says the app changed, and what changed
    // is worth a person's look.
    let taught = crate::adapter_tools::call(
        "adapter_teach",
        json!({
            "app": app().as_str(),
            "task": "save the invoice",
            "steps": [
                {"action": click(30.0, 30.0), "mark": cells(0)},
                {"action": key("ctrl+shift+s"), "mark": cells(1)},
            ],
        }),
        &adapters,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(taught["review"], true, "{taught}");
    assert_eq!(taught["runs"], 2.0);

    let listed = crate::adapter_tools::call(
        "adapter_list",
        json!({"app": app().as_str()}),
        &adapters,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(listed["adapters"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["adapters"][0]["runs"], 2.0);
}

#[tokio::test]
async fn a_repair_that_changed_nothing_a_person_notices_is_applied_and_logged() {
    let fake = FakeScreen::start(document_then_saved);
    let directory = tempfile::tempdir().unwrap();
    let adapters = Adapters::new(directory.path().join("adapters"));
    let cells = |steps: u32| {
        let mark = Screen::mark(&document_then_saved(steps)).unwrap();
        Value::Array(mark.cells.iter().map(|cell| json!(cell)).collect())
    };
    let steps = || {
        json!([
            {"action": click(30.0, 30.0), "mark": cells(0)},
            {"action": key("ctrl+s"), "mark": cells(1)},
        ])
    };
    let learned = |task: &'static str| {
        let adapters = &adapters;
        async move {
            crate::adapter_tools::call(
                "adapter_learn",
                json!({"app": app().as_str(), "task": task, "steps": steps()}),
                adapters,
                None,
                None,
            )
            .await
            .unwrap()
        }
    };
    learned("save the invoice").await;

    // The app repainted after a theme change and the agent did the very same
    // steps again: nothing a person would notice, so it is applied and logged
    // rather than shown to them.
    let taught = crate::adapter_tools::call(
        "adapter_teach",
        json!({"app": app().as_str(), "task": "save the invoice", "steps": steps()}),
        &adapters,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(taught["applied"], true, "{taught}");
    assert_eq!(taught["review"], false, "{taught}");
    assert!(
        taught["change"].as_str().unwrap().contains("\"moved\":0"),
        "{taught}"
    );

    // What it applied is still the recording a later run replays.
    fake.show(document_then_saved);
    let ran = crate::adapter_tools::call(
        "adapter_run",
        json!({"app": app().as_str(), "task": "save the invoice"}),
        &adapters,
        Some(Arc::clone(&fake.computer)),
        None,
    )
    .await
    .unwrap();
    assert_eq!(ran["done"], true, "{ran}");
}
