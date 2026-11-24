//! `tilolang` installed as a person installs it, with a stand-in `tilo` beside it, on `PATH`, or
//! none: the commands that reach the workspace through `tilo` (the REPL, `push`) run here.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tilo_test_env::SpawnUnderEnvLock as _;

/// A `PATH` with the system's tools and no `tilo`.
pub const SYSTEM_PATH: &str = "/usr/bin:/bin";

/// Where `tilolang` is installed for one run: its own directory, so the `tilo` beside it is the
/// one this run puts there, or none.
pub struct Installed {
    root: tempfile::TempDir,
}

/// What a run of `tilolang` came to: whether it succeeded, its output and its errors.
pub struct Ran {
    pub ok: bool,
    pub out: String,
    pub err: String,
}

impl Installed {
    pub fn new() -> Self {
        let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        let tilolang = root.path().join("bin/tilolang");
        if std::fs::hard_link(env!("CARGO_BIN_EXE_tilolang"), &tilolang).is_err() {
            std::fs::copy(env!("CARGO_BIN_EXE_tilolang"), &tilolang).unwrap();
        }
        Self { root }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The stand-in `tilo` running `script`, in `directory` under the root.
    pub fn put_tilo(&self, directory: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = self.path(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let tilo = directory.join("tilo");
        std::fs::write(&tilo, script).unwrap();
        std::fs::set_permissions(&tilo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// What `tilo` was started with and read, as it wrote it to `TILO_FAKE_LOG`.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.path("tilo.log")).unwrap_or_default()
    }

    /// `tilolang` with `args`, `input` on its standard input, `tilo` answering as `fake`, `path`
    /// its `PATH` and `runtime` its runtime directory.
    pub fn run(&self, args: &[&str], input: &str, fake: &str, path: &str, runtime: &Path) -> Ran {
        let mut child = Command::new(self.path("bin/tilolang"))
            .args(args)
            .env("PATH", path)
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_RUNTIME_DIR", runtime)
            .env("TILO_FAKE", fake)
            .env("TILO_FAKE_LOG", self.path("tilo.log"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn_locked()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), input.as_bytes()).unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        Ran {
            ok: output.status.success(),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

/// What a person reads names no protocol or socket.
pub fn plain(said: &str) {
    for word in ["MCP", "socket", "api", "JSON"] {
        assert!(
            !said.contains(word),
            "no {word} in what a person reads: {said}"
        );
    }
}
