//! The code refresh daemon's bind-mount transaction and rollback with real
//! mounts, on a scratch root:
//!
//! ```text
//! unshare -m --propagation private target/debug/examples/code-refresh-mount-probe
//! ```
//!
//! Only the development VM gate is bypassed and one bind can be made to
//! fail; the lock, staging and transaction are the daemon's own, and
//! `mount` and `umount` run for real. Missing privileges, mount support or
//! isolation are failures, never skips. The hermetic transaction cases run
//! in `tests/code_refresh.rs`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::{Value, json};
use tilo_install_helpers::code_refresh::{Guest, apply};
use tilo_install_helpers::{Error, Result, require};

struct Scratch {
    root: PathBuf,
    fail_bind_of: Option<PathBuf>,
}

impl Guest for Scratch {
    fn base(&self) -> PathBuf {
        self.root.join("state")
    }
    fn root(&self) -> PathBuf {
        self.root.clone()
    }
    fn gate(&self) -> Result<()> {
        Ok(())
    }
    fn command(&self, argv: &[&str]) -> Result<String> {
        if argv.get(..2) == Some(&["mount", "--bind"][..])
            && self
                .fail_bind_of
                .as_ref()
                .is_some_and(|path| argv.last() == Some(&path.to_string_lossy().as_ref()))
        {
            return Err(Error::new("injected second-bind failure"));
        }
        let output =
            tilo_install_helpers::process::run(argv, None, std::time::Duration::from_secs(30))?;
        require(output.success(), &format!("{} failed", argv.join(" ")))?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

fn probe(root: &std::path::Path) -> Result<()> {
    require(
        rustix::process::geteuid().is_root(),
        "run this probe as root",
    )?;
    require(
        fs::read_link("/proc/self/ns/mnt")? != fs::read_link("/proc/1/ns/mnt")?,
        "run this probe under unshare -m --propagation private",
    )?;
    let names = [
        "usr/libexec/tilo-install",
        "usr/libexec/tilo-boot-mount-contract",
    ];
    let mut targets = Vec::new();
    for name in names {
        let target = root.join(name);
        fs::create_dir_all(target.parent().unwrap_or(root))?;
        fs::write(&target, b"old")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
        targets.push(target);
    }
    let files = vec![
        (names[0].to_owned(), b"new first".to_vec()),
        (names[1].to_owned(), b"new second".to_vec()),
    ];
    let receipt = |sha: char| -> Value { json!({"sha256": sha.to_string().repeat(64)}) };
    let mut guest = Scratch {
        root: root.to_path_buf(),
        fail_bind_of: None,
    };
    apply(&guest, &files, &receipt('a'))?;
    for (target, (_, content)) in targets.iter().zip(&files) {
        require(fs::read(target)? == *content, "bound helper differs")?;
        require(
            fs::metadata(target)?.permissions().mode() & 0o777 == 0o755,
            "bound helper lost its mode",
        )?;
        require(fs::write(target, b"x").is_err(), "bound helper is writable")?;
    }
    guest.fail_bind_of = targets.get(1).cloned();
    let attempt: Vec<(String, Vec<u8>)> = names
        .iter()
        .map(|name| ((*name).to_owned(), b"attempt".to_vec()))
        .collect();
    match apply(&guest, &attempt, &receipt('b')) {
        Err(error) if error.0 == "injected second-bind failure" => {}
        other => {
            return Err(Error::new(format!(
                "expected the injected failure, got {other:?}"
            )));
        }
    }
    for (target, (_, content)) in targets.iter().zip(&files) {
        require(
            fs::read(target)? == *content,
            "rollback left a partial view",
        )?;
    }
    let marker: Value = serde_json::from_slice(&fs::read(root.join("state/active.json"))?)
        .map_err(|error| Error::new(error.to_string()))?;
    require(
        marker.get("sha256") == Some(&json!("a".repeat(64))),
        "rollback lost the applied receipt",
    )
}

fn main() -> ExitCode {
    let Ok(scratch) = tempfile::tempdir() else {
        eprintln!("code refresh mount probe: no scratch directory");
        return ExitCode::FAILURE;
    };
    let outcome = probe(scratch.path());
    for name in [
        "usr/libexec/tilo-boot-mount-contract",
        "usr/libexec/tilo-install",
    ] {
        let target = scratch.path().join(name);
        while tilo_install_helpers::process::run(
            &["umount", &target.to_string_lossy()],
            None,
            std::time::Duration::from_secs(30),
        )
        .is_ok_and(|output| output.success())
        {}
    }
    match outcome {
        Ok(()) => {
            println!("code refresh mount transaction and rollback: ok");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("code refresh mount probe: {error}");
            ExitCode::FAILURE
        }
    }
}
