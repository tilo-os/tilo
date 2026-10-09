//! Run UX journeys against the native shell tree, with an isolated fixture profile.
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::process::ExitCode;

#[cfg(target_os = "macos")]
mod ux_bench_support;

#[cfg(target_os = "macos")]
use shell::shell::state::WorkspaceAuthority as _;

fn main() -> ExitCode {
    match execute() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("UX bench: {e}");
            ExitCode::from(2)
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn execute() -> Result<bool, String> {
    Err(
        "This shell adapter currently requires macOS. ui::ux_bench::run is platform-independent."
            .into(),
    )
}

#[cfg(target_os = "macos")]
#[allow(
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    reason = "linear isolated fixture lifecycle; validated text scale converts to renderer f32"
)]
fn execute() -> Result<bool, String> {
    let mut args = std::env::args().skip(1);
    let mut scenario_path = None;
    let mut output = None;
    let mut interactive = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--interactive" => interactive = true,
            "--scenario" => {
                scenario_path = Some(PathBuf::from(
                    args.next().ok_or("--scenario requires a JSON file")?,
                ));
            }
            "--out" => {
                output = Some(PathBuf::from(
                    args.next().ok_or("--out requires a directory")?,
                ));
            }
            "--help" | "-h" => {
                println!(
                    "ux-bench [--interactive] --scenario FILE.json --out NEW_DIRECTORY\nExit: 0 pass, 1 UX failure, 2 invalid scenario or execution error.\nEach run creates an isolated shell profile and fixture files."
                );
                return Ok(true);
            }
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let raw = std::fs::read_to_string(scenario_path.ok_or("--scenario required")?)
        .map_err(|e| e.to_string())?;
    let scenario = ui::ux_bench::Scenario::parse(&raw)?;
    let out = output.ok_or("--out required")?;
    if out.exists() {
        return Err("output directory already exists; choose a new directory to preserve evidence and isolate state".into());
    }
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let out = out.canonicalize().map_err(|e| e.to_string())?;
    // This executable is still single-threaded. Isolate ambient search,
    // preferences and identity before constructing any provider or worker.
    let home = out.join("home");
    std::fs::create_dir(&home).map_err(|e| e.to_string())?;
    for (key, path) in [
        ("HOME", home.clone()),
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local/share")),
        ("XDG_CACHE_HOME", home.join(".cache")),
    ] {
        unsafe {
            std::env::set_var(key, path);
        }
    }
    let files = home.join("Documents");
    std::fs::create_dir(&files).map_err(|e| e.to_string())?;
    std::fs::write(
        files.join("Welcome.txt"),
        "UX bench fixture: the intended file is open.\n",
    )
    .map_err(|e| e.to_string())?;
    for i in 0..80 {
        std::fs::write(
            files.join(format!("Document-{i:03}.txt")),
            format!("Fixture document {i}\n"),
        )
        .map_err(|e| e.to_string())?;
    }
    let options = shell::compositor::macos::Options {
        state_dir: out.join("profile"),
        files_dir: files,
        shots: None,
    };
    let scope = ui::wake::WakeScope::new();
    let _entry = scope.enter();
    ui::i18n::I18nManager::init("en-US");
    let session = shell::compositor::macos::prepare(&options).map_err(|e| e.to_string())?;
    if scenario.fixture == "files" {
        shell::shell::window_actions::open_hosted(
            &mut session.chrome.borrow_mut(),
            shell::shell::state::AppKind::FileExplorer,
            "Files",
            shell::shell::window_actions::Destination::Beside,
        );
    }
    let mut theme = match scenario.theme.as_str() {
        "light" => ui::Theme::light(),
        "high-contrast" => ui::Theme::high_contrast(),
        _ => ui::Theme::dark(),
    };
    theme = theme.with_text_scale(scenario.text_scale as f32);
    theme.reduce_motion = true;
    let ui_state = ui::State::new();
    let catalogue = {
        let chrome = session.chrome.borrow();
        let tree = chrome.tree();
        let workspace = tree
            .desktop
            .active_workspace()
            .ok_or("fixture has no active workspace")?;
        let actor = workspace.actor_for_person(&chrome.local_user_for_room(&workspace.room));
        let gate = shell::shell::search::visibility::PolicyVisibility::new(workspace, actor);
        let entries = session.host.action_catalog("",&chrome,&ui_state.doc,&ui_state.root,&gate).enumerate()
            .into_iter().map(|a|serde_json::json!({"provider":a.resource.provider,"id":a.resource.id,"title":a.title,"kind":format!("{:?}",a.kind),"contract":a.contract})).collect::<Vec<_>>();
        drop(tree);
        entries
    };
    std::fs::write(
        out.join("initial.actions.json"),
        serde_json::to_vec_pretty(&catalogue).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let mut driver = ui::headless::Driver::new_with_theme(
        ui_state,
        Box::new(session.host),
        (scenario.viewport[0], scenario.viewport[1]),
        theme,
    );
    let state_snapshot = || {
        let tree = session.tree.read();
        let active = tree.desktop.active_workspace();
        let snapshot = serde_json::json!({"workspace_count":tree.desktop.workspaces.len(),
            "active_workspace":active.map(|w| &w.name),
            "tab_titles":active.map(|w|w.strips.iter().flat_map(|s|s.window_groups.iter()).flat_map(|g|g.tabs.iter()).map(|t|t.title.clone()).collect::<Vec<_>>()),
            "active_tabs":active.map(|w|w.strips.iter().flat_map(|s|s.window_groups.iter()).flat_map(|g|g.tabs.iter()).map(|t|serde_json::json!({"title":t.title,"content":t.tile})).collect::<Vec<_>>())});
        drop(tree);
        snapshot
    };
    if interactive {
        return ux_bench_support::run(&mut driver, &scenario, &out, state_snapshot, || {
            session.records.try_flush()?;
            let state_path = options.state_dir.join("state.json");
            shell::shell::persist::save_tree_to(&session.tree.read(), &state_path)
                .map_err(|e| e.to_string())?;
            let reopened =
                shell::shell::persist::load_tree_from(&state_path).map_err(|e| e.to_string())?;
            Ok(reopened
                .desktop
                .workspaces
                .iter()
                .map(|w| w.name.clone())
                .collect())
        });
    }
    let report = ui::ux_bench::run_with_state(&mut driver, &scenario, &out, state_snapshot)?;
    session.records.try_flush()?;
    // Match the native runner's normal shutdown persistence boundary.
    let state_path = options.state_dir.join("state.json");
    shell::shell::persist::save_tree_to(&session.tree.read(), &state_path)
        .map_err(|e| e.to_string())?;
    let reopened = shell::shell::persist::load_tree_from(&state_path).map_err(|e| e.to_string())?;
    let expected_names: Vec<_> = session
        .tree
        .read()
        .desktop
        .workspaces
        .iter()
        .map(|w| w.name.clone())
        .collect();
    let actual_names: Vec<_> = reopened
        .desktop
        .workspaces
        .iter()
        .map(|w| w.name.clone())
        .collect();
    if expected_names != actual_names {
        return Err("workspace names did not survive shutdown persistence".into());
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(std::fs::File::open(&executable).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let executable_hash = hasher.finalize().to_hex().to_string();
    let identity = serde_json::json!({"executable":executable,"executable_blake3":executable_hash,"scenario":raw,"platform":std::env::consts::OS,"fixture_profile":options.state_dir,"persisted_workspace_names":actual_names});
    std::fs::write(
        out.join("run.json"),
        serde_json::to_vec_pretty(&identity).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{}: {} clicks, {} keys, {} failures\n{}",
        if report.passed { "PASS" } else { "FAIL" },
        report.counts.clicks,
        report.counts.keys,
        report.failures.len(),
        out.join("index.html").display()
    );
    Ok(report.passed)
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use shell::shell::state::WorkspaceExt as _;
