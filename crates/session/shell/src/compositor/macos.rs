//! Native macOS shell session using the existing shell chrome and workspace tree.

use crate::shell::{
    hosted_inputs::HostedTileInputs,
    overlay_host::ShellOverlayHost,
    persist,
    state::{WorkspaceTree, WorkspaceTreeHandle},
};
use std::{path::PathBuf, process::ExitCode};

type Error = Box<dyn std::error::Error>;

#[derive(Debug)]
pub struct Options {
    pub state_dir: PathBuf,
    pub files_dir: PathBuf,
    pub shots: Option<PathBuf>,
}

/// A live session owns its storage lock until after the native event loop exits.
pub struct Session {
    pub tree: WorkspaceTreeHandle,
    pub records: ui::record::RecordStore,
    pub host: ShellOverlayHost,
    pub chrome: crate::shell::chrome_state::Chrome,
    _lock: advisory_lock::FileLock,
}

/// Prepare the shell; invalid stored state is never replaced by a new tree.
/// # Errors
/// Returns invalid directory, session lock, state, record-store or construction errors.
pub fn prepare(options: &Options) -> Result<Session, Error> {
    std::fs::create_dir_all(&options.state_dir)?;
    let state_dir = options.state_dir.canonicalize()?;
    // The apps this host mounts keep their records beside its state.
    let apps = crate::shell::in_process::InProcessApps::at(
        &state_dir.join("apps"),
        &crate::shell::state::local_user_id(),
    )
    .map_err(|error| error.to_string())?;
    let inputs = HostedTileInputs::for_host_directory(&options.files_dir, apps)?;
    let lock = advisory_lock::FileLock::try_open(
        &state_dir.join("session.lock"),
        advisory_lock::LockMode::Exclusive,
    )
    .map_err(|e| format!("this shell profile is already open: {e}"))?;
    let path = state_dir.join("state.json");
    let salt = crate::shell::session_policy::load_or_create_workspace_salt(
        &path,
        &state_dir.join("peer-key"),
    )?;
    WorkspaceTree::set_process_peer_salt(salt);
    let mut tree = match persist::load_tree_from(&path) {
        Ok(tree) => tree,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => WorkspaceTree::bootstrap_default(),
        Err(e) => return Err(format!("cannot open {}: {e}", path.display()).into()),
    };
    tree.persist_path = Some(path.clone());
    inputs.restore_documents(&mut tree);
    tree.adopt_ownerless_workspaces(&crate::shell::state::local_user_id());
    persist::save_tree_to(&tree, &path)?;
    let tree = tree.shared();
    let records = ui::record::RecordOwners::new(state_dir.join("records"))
        .open(crate::shell::SHELL_RECORDS_PACKAGE, "local")?;
    let (host, chrome) = crate::shell::build_tilo_overlay_host_with_inputs(
        tree.clone(),
        false,
        records.clone(),
        inputs,
    );
    Ok(Session {
        tree,
        records,
        host,
        chrome,
        _lock: lock,
    })
}

/// Parse the native runner's arguments. `None` requests help without side effects.
/// # Errors
/// Rejects unsupported flags and flags missing their path argument.
pub fn parse(args: &[String]) -> Result<Option<Options>, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Ok(None);
    }
    let mut options = Options {
        state_dir: tilo_paths::data_local_dir()
            .ok_or("no local data directory")?
            .join("tilo/macos-shell"),
        files_dir: tilo_paths::home().ok_or("no home directory")?,
        shots: None,
    };
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--state-dir" | "--files-dir" | "--shots" => {
                let value = args
                    .next()
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| format!("{flag} requires a path"))?;
                match flag.as_str() {
                    "--state-dir" => options.state_dir = value.into(),
                    "--files-dir" => options.files_dir = value.into(),
                    _ => {
                        let path = PathBuf::from(value);
                        if path.is_dir() {
                            return Err(format!(
                                "--shots writes one PNG file, but {} is a directory; name a \
                                 file such as {}",
                                path.display(),
                                path.join("shell.png").display()
                            ));
                        }
                        options.shots = Some(path);
                    }
                }
            }
            _ => return Err(format!("unsupported macOS option {flag:?}; see --help")),
        }
    }
    Ok(Some(options))
}

pub fn run() -> ExitCode {
    run_args(&std::env::args().skip(1).collect::<Vec<_>>())
}

pub fn run_args(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!(
                "Tilo shell for macOS\n\n  --state-dir PATH   Workspace and record storage\n  --files-dir PATH   Initial Files directory\n  --shots PATH       Render the shell to PNG and exit\n  --help             Show this help"
            );
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let _ = tracing_subscriber::fmt::try_init();
    match run_session(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Tilo shell: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_session(options: Options) -> Result<(), Error> {
    let scope = ui::wake::WakeScope::new();
    let _entry = scope.enter();
    ui::i18n::I18nManager::init("en-US");
    let Session {
        tree,
        records,
        host,
        chrome: _,
        _lock,
    } = prepare(&options)?;
    if let Some(path) = options.shots {
        let driver = ui::headless::FrameHarness::new(ui::State::new(), Box::new(host), (1280, 800));
        driver.render_png(&path);
        println!("Rendered {}", path.display());
    } else {
        ui::App::from_scoped_parts(scope, ui::State::new(), Box::new(host))
            .with_window_title("Tilo")
            .run();
    }
    records.try_flush()?;
    persist::save_tree_to(&tree.read(), &options.state_dir.join("state.json"))?;
    persist::wait_for_saves();
    Ok(())
}
