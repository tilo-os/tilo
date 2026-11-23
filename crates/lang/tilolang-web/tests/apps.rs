//! The repository's tilolang apps as a page runs them: their files handed over as text, compiled,
//! run over their first scenario and written as HTML, then acted on as a person acts on the page.
#![allow(
    clippy::expect_used,
    reason = "a failed expectation in a test is its assertion"
)]

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value as Json;

/// Every file below `apps/tl`, by its path from the repository's root, as a page holds them.
fn app_files() -> BTreeMap<String, String> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut out = BTreeMap::new();
    let mut dirs = vec![repo.join("apps/tl")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("apps/tl reads") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                let shown = path.strip_prefix(&repo).expect("below the repo");
                out.insert(shown.display().to_string(), text);
            }
        }
    }
    out
}

fn files_json(files: &BTreeMap<String, String>) -> String {
    serde_json::to_string(files).expect("files serialize")
}

fn open(files: &str, root: &str, options: &str) -> Json {
    serde_json::from_str(&tilolang_web::open_app(files, root, options)).expect("JSON answer")
}

fn act(handle: u32, id: u32, target: Option<&str>, action: &str) -> Json {
    serde_json::from_str(&tilolang_web::app_act(
        handle,
        id,
        target.map(str::to_owned),
        action,
    ))
    .expect("JSON answer")
}

/// The `data-p` and `data-t` of the first element whose `data-a` is `action` and whose tag, or
/// what follows it, holds `near`.
fn element(body: &str, action: &str, near: &str) -> Option<(u32, Option<String>)> {
    let marker = format!("data-a=\"{action}\"");
    body.split('<').find_map(|tag| {
        let (tag, after) = tag.split_once('>')?;
        if !tag.contains(&marker) || !(tag.contains(near) || after.contains(near)) {
            return None;
        }
        let attr = |name: &str| {
            let (_, from) = tag.split_once(&format!("{name}=\""))?;
            from.split_once('"').map(|(value, _)| value.to_owned())
        };
        Some((attr("data-p")?.parse().ok()?, attr("data-t")))
    })
}

fn handle(answer: &Json) -> u32 {
    answer["handle"]
        .as_u64()
        .and_then(|handle| u32::try_from(handle).ok())
        .expect("a handle")
}

#[test]
fn every_app_opens_over_its_first_scenario_as_a_page() {
    let files = files_json(&app_files());
    // What each app's first scenario puts in front of the person.
    let apps = [
        ("apps/tl/counter", None),
        ("apps/tl/board", None),
        ("apps/tl/discussion", None),
        ("apps/tl/tasks", Some("Buy oat milk")),
        ("apps/tl/notes", None),
        ("apps/tl/chat", None),
        ("apps/tl/mail", None),
        ("apps/tl/tube", None),
        ("apps/tl/today", None),
        ("apps/tl/views", None),
        ("apps/tl/gallery", None),
    ];
    let mut failures = Vec::new();
    for (root, shows) in apps {
        let answer = open(&files, root, "");
        if answer["ok"] != true {
            failures.push(format!("{root}: {}", answer["message"]));
            continue;
        }
        let body = answer["body"].as_str().expect("a body");
        let scenario = &answer["scenario"];
        eprintln!(
            "{root}: scenario {scenario}, {} bytes of HTML, {} actionable elements",
            body.len(),
            body.matches("data-a=").count()
        );
        if let Some(shows) = shows
            && !body.contains(shows)
        {
            failures.push(format!("{root}: the page does not show {shows:?}"));
        }
        eprintln!("{root} gaps: {}", answer["gaps"]);
        assert!(tilolang_web::app_close(handle(&answer)));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_task_ticked_on_the_page_is_done_in_the_next_patch() {
    let files = files_json(&app_files());
    let answer = open(&files, "apps/tl/tasks", r#"{"scenario": "a busy week"}"#);
    assert_eq!(answer["ok"], true, "{}", answer["message"]);
    let handle = handle(&answer);
    let body = answer["body"].as_str().expect("a body");
    let (id, target) = element(body, "toggle", "Sort the paper mail")
        .or_else(|| element(body, "toggle", ""))
        .expect("a task to tick");
    let acted = act(handle, id, target.as_deref(), "\"toggle\"");
    assert_eq!(acted["ok"], true, "{}", acted["message"]);
    let patches = acted["patches"].as_array().expect("patches");
    assert!(!patches.is_empty(), "ticking a task changes the page");
    tilolang_web::app_close(handle);
}

#[test]
fn the_shells_canvas_stands_its_places_where_the_layout_puts_them() {
    let files = files_json(&app_files());
    let answer = open(
        &files,
        "apps/tl/shell",
        r#"{"scenario": "a canvas of clusters", "width": 1280, "height": 800}"#,
    );
    assert_eq!(answer["ok"], true, "{}", answer["message"]);
    let body = answer["body"].as_str().expect("a body");
    eprintln!("shell gaps: {}", answer["gaps"]);
    // The canvas scrolls to its places, and each stands at its point on it rather than in a
    // column under the bar.
    assert!(
        body.contains("data-w=\"Map\"") && body.contains("position:relative;overflow:auto;"),
        "the canvas is a box that scrolls: {body}"
    );
    let placed = body.matches("position:absolute;left:").count();
    assert!(
        placed >= 7,
        "six notes and a terminal each stand at their point on the canvas, {placed} do: {body}"
    );
    // The terminal has no resource the shell can draw: its window's well names its place,
    // for the page's host to fill.
    assert!(body.contains("data-well=\"c-terminal\""), "{body}");
    tilolang_web::app_close(handle(&answer));
}

/// A workspace's layout with two windows on its canvas, the second at `second_at`.
fn layout(second_at: (i64, i64)) -> tilo_desktop_model::reading::LayoutReading {
    use tilo_desktop_model::reading::{
        LayoutPlaceReading, LayoutReading, LayoutRowReading, LayoutStripReading,
        LayoutWorkspaceReading,
    };
    let strip = |key: &str, (x, y): (i64, i64)| LayoutStripReading {
        key: key.to_owned(),
        workspace: "w".to_owned(),
        row: "r".to_owned(),
        width: 667,
        placed: true,
        canvas_x: x,
        canvas_y: y,
        canvas_width: 400,
        canvas_height: 300,
        ..LayoutStripReading::default()
    };
    let place = |key: &str, strip: &str, app: &str, name: &str| LayoutPlaceReading {
        key: key.to_owned(),
        strip: strip.to_owned(),
        shows: app.to_owned(),
        title: name.to_owned(),
        name: name.to_owned(),
        ..LayoutPlaceReading::default()
    };
    LayoutReading {
        person: "Ada".to_owned(),
        workspaces: vec![LayoutWorkspaceReading {
            key: "w".to_owned(),
            name: "Home".to_owned(),
            current: true,
        }],
        rows: vec![LayoutRowReading {
            key: "r".to_owned(),
            workspace: "w".to_owned(),
            name: "Main".to_owned(),
            current: true,
            ..LayoutRowReading::default()
        }],
        strips: vec![strip("s1", (40, 40)), strip("s2", second_at)],
        places: vec![
            place("t1", "s1", "so.tilo.notes", "Groceries — Notes"),
            place("t2", "s2", "so.tilo.tasks", "Tasks"),
        ],
        ..LayoutReading::default()
    }
}

#[test]
fn a_hosted_canvas_follows_the_layout_its_host_reads() {
    let packages = std::rc::Rc::new(tilolang_web::Packages(
        std::rc::Rc::new(tilolang_tilo::shell::compile().expect("the shell compiles")),
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages"),
    ));
    let origin = std::rc::Rc::new(std::cell::RefCell::new(
        tilolang_tilo::shell::LayoutOrigin::default(),
    ));
    origin.borrow_mut().read(layout((600, 120)));
    let mut page = tilolang_web::window::AppPage::hosted(
        tilolang_tilo::shell::SHELL_APP,
        &packages,
        tilolang_web::window::Hosting {
            root: "layout",
            view: "frames",
            origin: std::rc::Rc::clone(&origin) as tilolang_tilo::origin::SharedOrigin,
            me: ("ada", "Ada"),
            viewport: (1280, 800),
            dark: false,
        },
    )
    .expect("the canvas opens");
    page.fill_wells(
        [(
            "t2".to_owned(),
            "<a href=\"/w/ws/tasks\">Tasks</a>".to_owned(),
        )]
        .into_iter()
        .collect(),
    );
    let body = page.render();
    assert_eq!(
        body.matches("data-w=\"WindowFrame\"").count(),
        2,
        "both windows stand on the canvas: {body}"
    );
    assert!(
        body.matches("position:absolute;left:").count() >= 2,
        "each at its point: {body}"
    );
    assert!(
        body.contains("data-well=\"t2\"><a href=\"/w/ws/tasks\">Tasks</a>"),
        "the host's link stands where the window shows: {body}"
    );
    // Another member moved the second window: the page's next pass moves it, and only it.
    origin.borrow_mut().read(layout((700, 160)));
    let patches = page.follow();
    assert!(!patches.is_empty(), "the move reaches the page");
    assert!(
        patches
            .iter()
            .all(|patch| !patch.html.contains("Groceries")),
        "the window that stayed is not sent again: {patches:?}"
    );
}

#[test]
fn an_edit_that_breaks_a_file_is_reported_in_that_file() {
    let mut files = app_files();
    let scenarios = "apps/tl/tasks/scenarios.tl";
    let text = files.get_mut(scenarios).expect("tasks has scenarios");
    text.push_str("\nscenario \"broken\" { let x = no_such_thing }\n");
    let answer: Json = serde_json::from_str(&tilolang_web::compile_package(
        &files_json(&files),
        "apps/tl/tasks",
    ))
    .expect("JSON answer");
    assert_eq!(answer["ok"], false);
    let diagnostics = answer["diagnostics"].as_array().expect("diagnostics");
    assert!(
        diagnostics.iter().any(|d| d["file"]
            .as_str()
            .is_some_and(|f| f.ends_with("scenarios.tl"))
            && d["line"].as_u64().is_some_and(|line| line > 1)),
        "{diagnostics:?}"
    );
}

#[test]
fn an_import_no_file_declares_is_named() {
    let files: BTreeMap<String, String> = app_files()
        .into_iter()
        .filter(|(path, _)| path.starts_with("apps/tl/today/"))
        .collect();
    let answer: Json = serde_json::from_str(&tilolang_web::compile_package(
        &files_json(&files),
        "apps/tl/today",
    ))
    .expect("JSON answer");
    assert_eq!(answer["ok"], false);
    assert!(
        answer["message"].as_str().is_some_and(
            |m| m.contains("`use so.tilo.") && m.contains("no package with that app id")
        ),
        "{answer}"
    );
}

#[test]
fn the_playground_runs_one_source_for_two_people() {
    let source = app_files()
        .remove("apps/tl/counter/app.tl")
        .expect("the counter");
    let answer: Json =
        serde_json::from_str(&tilolang_web::playground::compile_and_run(&source)).expect("JSON");
    assert_eq!(answer["ok"], true, "{answer}");
    assert!(answer["alice"]["tree"]["kind"].is_string());
    assert_eq!(answer["online"], true);
}
