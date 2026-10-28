//! Host-desktop entry point for the primary Files workspace: start the daemon
//! only for saved, authorized state, never by creating an empty namespace.
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail, ensure};

use crate::workspace_startup;

/// systemd's `RestartPreventExitStatus=` for state that retrying cannot fix.
const NOT_READY_EXIT: i32 = 78;

#[derive(clap::Args, Debug)]
pub(super) struct PrimaryArgs {
    /// Print the daemon command line as JSON instead of starting it.
    #[arg(long)]
    dry_run: bool,
    /// The production identity and Files permission check.
    #[arg(long, default_value = "/usr/libexec/tilo/tilo-relay-admin")]
    admin_binary: PathBuf,
}

struct Paths {
    state: PathBuf,
    identity: PathBuf,
    config: PathBuf,
    runtime: PathBuf,
    /// The storage socket alone, so Settings can be handed this directory
    /// without the blob and authority sockets in `runtime`.
    storage: PathBuf,
}

fn paths(env: &dyn Fn(&str) -> Option<OsString>) -> Result<Paths> {
    let home = PathBuf::from(env("HOME").context("HOME is not set")?);
    let runtime_dir = env("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    // An empty variable counts as unset, as in the XDG base directory spec.
    let var = |name: &str| env(name).filter(|value| !value.is_empty());
    let base =
        |name: &str, fallback: &str| var(name).map_or_else(|| home.join(fallback), PathBuf::from);
    let data_home = base("XDG_DATA_HOME", ".local/share");
    let runtime_dir = PathBuf::from(runtime_dir);
    let paths = Paths {
        state: base("XDG_STATE_HOME", ".local/state").join("tilo/files"),
        identity: var("TILO_IDENTITY_ROOT").map_or_else(|| data_home.join("tilo"), PathBuf::from),
        config: base("XDG_CONFIG_HOME", ".config").join("tilo"),
        runtime: runtime_dir.join("tilo/files-workspace"),
        storage: runtime_dir.join("tilo/files-storage"),
    };
    // `storage` shares `runtime`'s base.
    for path in [&paths.state, &paths.identity, &paths.config, &paths.runtime] {
        ensure!(path.is_absolute(), "Files paths must be absolute");
    }
    Ok(paths)
}

fn private_dir(path: &Path) -> Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .with_context(|| format!("create {}", path.display()))
}

fn command(args: &PrimaryArgs, env: &dyn Fn(&str) -> Option<OsString>) -> Result<Command> {
    let paths = paths(env)?;
    ensure!(
        paths.state.join("namespace.bin").is_file(),
        "no saved Files namespace; recover or explicitly initialize first"
    );
    // This production check validates the existing certificate/key, signed
    // authority and exact Files capability. It never creates credentials.
    let status = Command::new(&args.admin_binary)
        .arg("files-workspace-check")
        .arg("--identity-root")
        .arg(&paths.identity)
        .arg("--cache-dir")
        .arg(&paths.state)
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("run {}", args.admin_binary.display()))?;
    if !status.success() {
        bail!(
            "{} files-workspace-check failed: {status}",
            args.admin_binary.display()
        );
    }
    if !workspace_startup::inspect(&paths.state)?.can_resume_existing() {
        bail!("saved workspace cannot be resumed");
    }
    let mount = paths.state.join("mount");
    private_dir(&mount)?;
    private_dir(&paths.runtime)?;
    private_dir(&paths.storage)?;
    let fs_binary = std::env::current_exe().context("locate the fs executable")?;
    let mut serve = Command::new(fs_binary);
    serve
        .arg("serve")
        .arg("--require-existing-workspace")
        .arg("--cache-dir")
        .arg(&paths.state)
        .arg("--mount-point")
        .arg(&mount)
        .arg("--acl")
        .arg(paths.config.join("fs-capabilities.json"))
        .args(["--data-plane", "fuse", "--storage-sock"])
        .arg(paths.storage.join("storage.sock"))
        .arg("--blob-sock")
        .arg(paths.runtime.join("fs-blobs.sock"))
        .env("TILO_IDENTITY_ROOT", &paths.identity)
        .env("TILO_AUTHORITY_SOCKET_DIR", &paths.runtime);
    Ok(serve)
}

/// The argv as one JSON line in the list layout the service tooling reads.
fn argv_json(command: &Command) -> String {
    let words: Vec<String> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word: &OsStr| {
            serde_json::Value::String(word.to_string_lossy().into_owned()).to_string()
        })
        .collect();
    format!("[{}]", words.join(", "))
}

/// Runs before the async runtime exists, so the exec replaces a single thread.
pub(super) fn run(args: &PrimaryArgs) -> i32 {
    let mut serve = match command(args, &|name| std::env::var_os(name)) {
        Ok(serve) => serve,
        Err(error) => {
            eprintln!("Files service is not ready: {error:#}");
            return NOT_READY_EXIT;
        }
    };
    if args.dry_run {
        println!("{}", argv_json(&serve));
        return 0;
    }
    let error = serve.exec();
    eprintln!("starting the Files daemon failed: {error}");
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_environment_names_the_checked_identity_and_private_runtime() {
        let dir = tempfile::tempdir().expect("isolated fixture");
        let home = dir.path().to_path_buf();
        std::fs::create_dir_all(home.join(".local/state/tilo/files")).expect("create cache");
        std::fs::write(home.join(".local/state/tilo/files/namespace.bin"), b"saved")
            .expect("write namespace");
        let env = |name: &str| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "XDG_RUNTIME_DIR" => Some(home.join("run").into_os_string()),
            "TILO_IDENTITY_ROOT" => Some(home.join("certified").into_os_string()),
            _ => None,
        };
        let args = PrimaryArgs {
            dry_run: true,
            admin_binary: PathBuf::from("/bin/true"),
        };
        let serve = command(&args, &env).expect("ready fixture");
        let envs: std::collections::HashMap<_, _> = serve.get_envs().collect();
        assert_eq!(
            envs[OsStr::new("TILO_IDENTITY_ROOT")],
            Some(home.join("certified").as_os_str())
        );
        assert_eq!(
            envs[OsStr::new("TILO_AUTHORITY_SOCKET_DIR")],
            Some(home.join("run/tilo/files-workspace").as_os_str())
        );
        assert!(!home.join("certified").exists());
    }
}
