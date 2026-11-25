//! The repository's tilolang apps as a page runs them: their files handed over as text, compiled,
//! run over their first scenario and written as HTML, then acted on as a person acts on the page.
#![allow(
    clippy::expect_used,
    reason = "a failed expectation in a test is its assertion"
)]

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value as Json;

/// Every file below `apps/tl`, by its path from the repository's root, as a page holds them: the
/// files a package ships under `media/` as base64, since a page's answer is JSON text; and the
/// manifest and surface of every native app in `apps/` that has both, which `build-page.sh`
/// hands over too and a package may `use` as metadata.
fn app_files() -> BTreeMap<String, String> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut out = BTreeMap::new();
    let mut dirs = vec![repo.join("apps/tl")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("apps/tl reads") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let shown = path.strip_prefix(&repo).expect("below the repo");
            let shown = shown.display().to_string();
            let shipped = path
                .components()
                .any(|component| component.as_os_str() == "media");
            if shipped {
                use base64::Engine as _;
                let bytes = std::fs::read(&path).expect("a shipped file reads");
                out.insert(
                    shown,
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                );
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                out.insert(shown, text);
            }
        }
    }
    for entry in std::fs::read_dir(repo.join("apps")).expect("apps reads") {
        let app = entry.expect("an entry").path();
        if !(app.join("tilo-app.toml").is_file() && app.join("surface.json").is_file()) {
            continue;
        }
        for name in ["tilo-app.toml", "surface.json"] {
            let shown = app
                .join(name)
                .strip_prefix(&repo)
                .expect("below the repo")
                .display()
                .to_string();
            out.insert(
                shown,
                std::fs::read_to_string(app.join(name)).expect("reads"),
            );
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
        ("apps/tl/music", Some("First Light")),
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
fn a_song_playing_plays_the_tone_the_package_ships() {
    let files = app_files();
    assert!(
        files.contains_key("apps/tl/music/media/first-light.wav"),
        "the scenario's tone is the package's own file"
    );
    assert!(
        !files.values().any(|text| text.contains("music.example")),
        "nothing the page is handed names a domain that cannot answer"
    );
    let answer = open(&files_json(&files), "apps/tl/music", "");
    assert_eq!(answer["ok"], true, "{}", answer["message"]);
    assert_eq!(answer["scenario"], "a song playing");
    let body = answer["body"].as_str().expect("a body");
    assert!(body.contains("First Light"), "{body}");
    assert!(!body.contains("music.example"), "no fetch to a dead domain");
    // The page installed the file the scenario names, and the address the view declares plays
    // its bytes: a page's media element gets a blob URL of them, never a URL to fetch.
    let played =
        tilolang_window::media_source::playable("pkg:media/first-light.wav", Some("First Light"));
    assert_ne!(played, "pkg:media/first-light.wav");
    assert!(!played.contains("music.example"), "{played}");
    #[cfg(not(target_arch = "wasm32"))]
    {
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps/tl/music/media/first-light.wav");
        let bytes = std::fs::read(played.strip_prefix("file://").expect("a staged file"))
            .expect("the staged tone reads");
        assert_eq!(
            bytes,
            std::fs::read(shipped).expect("the shipped tone reads")
        );
    }
    tilolang_web::app_close(handle(&answer));
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
fn a_pages_copy_link_names_the_resource_again_under_the_workspaces_page() {
    const SITE: &str = "https://tilo.so/";
    const ORIGIN: &str = "https://ada.tilo.cloud";
    const WORKSPACE: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";
    const ROOT: &str = "tl:so.tilo.notes:root-v7cub7aw";
    let files = files_json(&app_files());
    let options = serde_json::json!({
        "linkHome": {
            "origin": ORIGIN,
            "workspace": WORKSPACE,
            "web_entry": SITE,
        }
    })
    .to_string();
    let answer = open(&files, "apps/tl/notes", &options);
    assert_eq!(answer["ok"], true, "{}", answer["message"]);
    let page = handle(&answer);
    let answer: Json =
        serde_json::from_str(&tilolang_web::app_share(page, ROOT)).expect("JSON answer");
    assert_eq!(answer["ok"], true, "{answer}");
    let url = answer["url"].as_str().expect("a link");
    // The link names the workspace's page, the notebook's app, its record and the resource
    // again, as a browser opens one.
    assert!(
        url.starts_with("https://tilo.so/app/#/@ada.tilo.cloud/w/"),
        "the workspace's page, one app full tab, the host it opens on: {url}"
    );
    assert!(url.contains(WORKSPACE), "the workspace it names: {url}");
    assert!(
        url.contains("/so.tilo.notes/-/tl.so.tilo.notes.Notebook/tl:so.tilo.notes:root-v7cub7aw"),
        "the notebook it names: {url}"
    );
    // A page no workspace stands behind has no page to mint a link under: the same system words
    // a native window hears.
    let answer = open(&files, "apps/tl/notes", "");
    assert_eq!(answer["ok"], true, "{}", answer["message"]);
    let plain = handle(&answer);
    let answer: Json =
        serde_json::from_str(&tilolang_web::app_share(plain, ROOT)).expect("JSON answer");
    assert_eq!(answer["ok"], false, "{answer}");
    assert!(
        answer["message"]
            .as_str()
            .is_some_and(|message| message.contains("has no server that serves links")),
        "the system's own words: {answer}"
    );
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

/// A page keeps a file an action makes of what it holds: a scenario's `File.of_text` stages,
/// and the view that shows it stands in the page.
#[test]
fn the_page_keeps_a_file_an_action_makes() {
    let files = serde_json::json!({
        "apps/tl/slip/app.tl": "app so.tilo.slip \"Slip\" (icon: .file, category: .utility)\n\
             \n\
             type Slip {\n\
             subject: Text\n\
             paper: File\n\
             }\n\
             \n\
             root Slip\n\
             \n\
             default view slip for Slip\n\
             \n\
             view slip(s: Slip) {\n\
             ui.Text(\"{s.paper.name} {s.paper.url}\", role: .body)\n\
             }\n\
             \n\
             scenario \"a slip with its paper\" {\n\
             let s = root Slip\n\
             s.subject = \"The lease\"\n\
             s.paper = File.of_text(\"Lease.md\", \"# Tenancy agreement\")\n\
             }\n",
    })
    .to_string();
    let answer = open(
        &files,
        "apps/tl/slip",
        r#"{"scenario": "a slip with its paper"}"#,
    );
    assert_eq!(answer["ok"], true, "{}", answer["message"]);
    let body = answer["body"].as_str().expect("a body");
    assert!(
        body.contains("Lease.md") && body.contains("tilo-blob://"),
        "the file a scenario made stands in the page, with the address its bytes are kept at: {body}"
    );
    tilolang_web::app_close(handle(&answer));
}

/// The text a page's body shows, with its markup and styling taken out.
fn page_text(body: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in body.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The apps of `apps/tl` that open a window of their own show what they are: a page whose root
/// view could not run is the window's shell with nothing in it, which this holds against.
#[test]
fn an_app_with_a_window_shows_its_content_on_the_page() {
    let files = files_json(&app_files());
    // What each window shows of itself, over the scenario it opens on.
    let apps = [
        ("apps/tl/calculator", "Calculator"),
        ("apps/tl/files", "Files is starting"),
        ("apps/tl/feeds", "A quiet morning in the city"),
        ("apps/tl/photos", "Dawn"),
        ("apps/tl/canvas", "No artwork yet"),
        ("apps/tl/weather", "The week ahead"),
    ];
    let mut failures = Vec::new();
    for (root, shows) in apps {
        let answer = open(&files, root, "");
        if answer["ok"] != true {
            failures.push(format!("{root}: {}", answer["message"]));
            continue;
        }
        let body = answer["body"].as_str().expect("a body");
        let text = page_text(body);
        eprintln!("{root}: gaps {}, text {text:?}", answer["gaps"]);
        if !text.contains(shows) {
            failures.push(format!(
                "{root}: the page does not show {shows:?}: {text:?}"
            ));
        }
        assert!(tilolang_web::app_close(handle(&answer)));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// An app whose views a host draws itself (a player's card in the shell) has no window of its
/// own: the page is the host, and draws its first view over a new resource, as the shell would.
#[test]
fn an_app_with_no_window_is_drawn_as_a_host_draws_it() {
    let files = files_json(&app_files());
    let answer = open(&files, "apps/tl/playback", "");
    assert_eq!(answer["ok"], true, "{answer}");
    assert_eq!(answer["hosted"], "so.tilo.playback", "{answer}");
    let text = page_text(answer["body"].as_str().expect("a body"));
    for control in ["Play", "Next", "Stop"] {
        assert!(text.contains(control), "{control}: {text:?}");
    }
    assert!(tilolang_web::app_close(handle(&answer)));
}

/// A page opened with a file runs the app's action for its kind, as a shell opens one: the file
/// is the same `File` a chooser gives the action, and the note it makes stands in the page.
#[test]
fn a_page_opened_with_a_file_shows_what_the_app_made_of_it() {
    use base64::Engine as _;
    let files = files_json(&app_files());
    let bytes = base64::engine::general_purpose::STANDARD.encode("# Plan\n\n- one\n");
    let options = serde_json::json!({
        "scenario": "",
        "open": { "name": "plan.md", "bytes": bytes },
    })
    .to_string();
    let answer = open(&files, "apps/tl/notes", &options);
    assert_eq!(answer["ok"], true, "{}", answer["message"]);
    assert_eq!(
        answer["scenario"],
        Json::Null,
        "an app opened with a file starts empty, as a first launch does"
    );
    let body = answer["body"].as_str().expect("a body");
    assert!(
        body.contains("Plan") && body.contains("1 word"),
        "the note the file made stands in the page, named by its heading and holding its words: {body}"
    );
    tilolang_web::app_close(handle(&answer));
}

/// A page opened with a link runs the app's `open-link`: a link the app holds nothing for shows
/// the app itself rather than a window that took nothing.
#[test]
fn a_page_opened_with_a_link_runs_the_apps_open_link() {
    let files = files_json(&app_files());
    let options = serde_json::json!({ "scenario": "", "link": "tube:watch?v=unknown" }).to_string();
    let answer = open(&files, "apps/tl/tube", &options);
    assert_eq!(answer["ok"], true, "{}", answer["message"]);
    let body = answer["body"].as_str().expect("a body");
    assert!(
        !body.is_empty(),
        "the client's window stands after the link opened nothing it holds"
    );
    tilolang_web::app_close(handle(&answer));
}
