//! `tilolang repl` as a person runs it: `tilolang` installed with a `tilo` beside it, or on
//! `PATH`, or none. The `tilo` here is a stand-in speaking the lines `tilo mcp --as-me` speaks
//! (`crates/core/tilo-cli/src/mcp/person.rs`): the REPL starts it, lists, runs programs and reads
//! refusals through it, and says what to do when it is missing or nobody is signed in.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tilo_test_env::SpawnUnderEnvLock as _;

/// A `tilo` answering MCP's JSON-RPC lines as `tilo mcp --as-me` does, its answers picked by
/// `TILO_FAKE`: `signed-in` (the login's workspace, Home), `account` (no default workspace;
/// the account lists one, Solo), `accounts` (the account lists two, Solo and Band), `nobody` (nobody signed in) or `old` (a `tilo` from before
/// `--as-me`, which refuses it). It writes its arguments, then
/// the desktop socket it was pointed at, then every line it reads, to `TILO_FAKE_LOG`.
const FAKE_TILO: &str = r#"#!/bin/sh
printf 'args: %s\n' "$*" >> "$TILO_FAKE_LOG"
printf 'socket: %s\n' "$TILO_API_SOCKET" >> "$TILO_FAKE_LOG"
if [ "$TILO_FAKE" = old ]; then
  echo "error: unexpected argument '--as-me' found" >&2
  exit 2
fi
answer() { printf '{"jsonrpc":"2.0","id":%s,%s}\n' "$id" "$1"; }
typed() { answer "\"result\":{\"isError\":false,\"content\":[{\"type\":\"text\",\"text\":\"\"}],\"structuredContent\":$1}"; }
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$TILO_FAKE_LOG"
  case $line in *'"id":'*) ;; *) continue ;; esac
  rest=${line#*\"id\":}
  id=${rest%%[,\}]*}
  case $line in
    *'"initialize"'*)
      case $TILO_FAKE in
        nobody) answer '"error":{"code":-32002,"message":"not signed in","data":{"reason":"not_signed_in"}}' ;;
        account|accounts) answer '"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"tilo"}}' ;;
        *) answer '"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"tilo"},"_meta":{"so.tilo.mcp":{"workspace":{"id":"3ee4a797","name":"Home"}}}}' ;;
      esac ;;
    *'account/workspaces#list'*)
      case $TILO_FAKE in
        accounts) typed '{"status":"done","result":{"workspaces":[{"id":"5010","name":"Solo"},{"id":"7020","name":"Band"}]}}' ;;
        *) typed '{"status":"done","result":{"workspaces":[{"id":"5010","name":"Solo"}]}}' ;;
      esac ;;
    *'"tilo_find"'*)
      typed '{"result":{"hits":[{"path":"notes/Groceries","label":"Groceries"}]}}' ;;
    *'"program":"catalogue"'*)
      typed '{"status":"done","text":"{\"notes\":{\"signatures\":[{\"name\":\"pin\",\"signature\":\"action pin(note: Note)\"}]}}"}' ;;
    *'"program":"notes.pin(n3)"'*)
      typed '{"status":"done","output":["pinned n3 Groceries"],"text":"pinned n3 Groceries"}' ;;
    *'"program":"notes.nope()"'*)
      answer '"result":{"isError":true,"content":[{"type":"text","text":"error: notes has no action nope"}],"structuredContent":{"status":"failed","text":"error: notes has no action nope"}}' ;;
    *'"program":"undo"'*)
      typed '{"status":"done","text":"undone: step 1, pinned n3"}' ;;
    *'"path":"secret"'*)
      answer '"result":{"isError":true,"content":[{"type":"text","text":"you may not read secret"}],"structuredContent":{"error":{"code":"forbidden","message":"you may not read secret"}}}' ;;
    *'"tilo_ls"'*)
      typed '{"text":"notes  Notes\nfiles  Files"}' ;;
    *) answer '"error":{"code":-32601,"message":"no such call"}' ;;
  esac
done
"#;

/// Where `tilolang` is installed for one run: its own directory, so the `tilo` beside it is the
/// one this run puts there, or none.
struct Installed {
    root: tempfile::TempDir,
}

impl Installed {
    fn new() -> Self {
        let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        let tilolang = root.path().join("bin/tilolang");
        if std::fs::hard_link(env!("CARGO_BIN_EXE_tilolang"), &tilolang).is_err() {
            std::fs::copy(env!("CARGO_BIN_EXE_tilolang"), &tilolang).unwrap();
        }
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// The stand-in `tilo`, in `directory` under the root.
    fn put_tilo(&self, directory: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = self.path(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let tilo = directory.join("tilo");
        std::fs::write(&tilo, FAKE_TILO).unwrap();
        std::fs::set_permissions(&tilo, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// What `tilo` was started with and read, as it wrote it.
    fn log(&self) -> String {
        std::fs::read_to_string(self.path("tilo.log")).unwrap_or_default()
    }

    /// `tilolang repl` with `args`, typed `entries`, `tilo` answering as `fake` and `path` its
    /// `PATH`: whether it succeeded, its output and its errors.
    fn repl(
        &self,
        args: &[&str],
        entries: &str,
        fake: &str,
        path: &str,
        runtime: &Path,
    ) -> (bool, String, String) {
        let state = self.path("state");
        let mut child = Command::new(self.path("bin/tilolang"))
            .arg("repl")
            .args(args)
            .args(["--color", "never"])
            .env("PATH", path)
            .env("XDG_STATE_HOME", &state)
            .env("XDG_RUNTIME_DIR", runtime)
            .env("TILO_FAKE", fake)
            .env("TILO_FAKE_LOG", self.path("tilo.log"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn_locked()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), entries.as_bytes()).unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        (
            output.status.success(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

/// What a person reads names no protocol or socket.
fn plain(said: &str) {
    for word in ["MCP", "socket", "api", "JSON"] {
        assert!(
            !said.contains(word),
            "no {word} in what a person reads: {said}"
        );
    }
}

/// A `PATH` with the system's tools and no `tilo`.
const SYSTEM_PATH: &str = "/usr/bin:/bin";

tilo_test_env::scenario! {
    fn the_repl_works_in_the_workspace_through_the_tilo_beside_it;

    phase it_lists_runs_and_reads_refusals_as_the_person;
    {
        let installed = Installed::new();
        installed.put_tilo("bin");
        let runtime = tempfile::tempdir().unwrap();
        let (ok, out, err) = installed.repl(
            &["--workspace", "Home", "--key-file", "/keys/ada.key"],
            "ls\nnotes.pin(n3)\nnotes.nope()\nls secret\nfind groceries\n:apps\nundo\n",
            "signed-in",
            SYSTEM_PATH,
            runtime.path(),
        );
        assert!(ok, "{err}");
        assert!(out.contains("in Home; apps: notes"), "{out}");
        assert!(out.contains("notes  Notes"), "the listing: {out}");
        assert!(out.contains("pinned n3 Groceries"), "the action's answer: {out}");
        assert!(
            out.contains("error: notes has no action nope"),
            "a program the workspace refused: {out}"
        );
        assert!(
            out.contains("error: you may not read secret"),
            "a call the tool refused: {out}"
        );
        assert!(out.contains("notes/Groceries  Groceries"), "the search: {out}");
        assert!(out.contains(":apps\nnotes\n"), "the apps it learned: {out}");
        assert!(out.contains("undone: step 1, pinned n3"), "undo: {out}");
        let log = installed.log();
        assert!(
            log.starts_with("args: --key-file /keys/ada.key --workspace Home mcp --as-me\n"),
            "tilo acts as the key, in the workspace named, as the person: {log}"
        );
        assert!(
            !log.contains("\"workspace\":"),
            "the default workspace needs no naming: {log}"
        );
    }

    phase without_a_workspace_of_its_own_the_person_s_one_workspace_is_named;
    {
        let installed = Installed::new();
        installed.put_tilo("path");
        let runtime = tempfile::tempdir().unwrap();
        let path = format!("{}:{SYSTEM_PATH}", installed.path("path").display());
        let (ok, out, err) =
            installed.repl(&[], "notes.pin(n3)\n", "account", &path, runtime.path());
        assert!(ok, "{err}");
        assert!(out.contains("in Solo; apps: notes"), "{out}");
        let log = installed.log();
        assert!(log.starts_with("args: mcp --as-me\n"), "{log}");
        let pinned = log
            .lines()
            .find(|line| line.contains("notes.pin(n3)"))
            .unwrap();
        assert!(pinned.contains(r#""workspace":"5010""#), "{pinned}");
    }

    phase among_several_workspaces_the_person_picks_one;
    {
        let installed = Installed::new();
        installed.put_tilo("bin");
        let runtime = tempfile::tempdir().unwrap();
        let (ok, out, err) =
            installed.repl(&[], "2\nnotes.pin(n3)\n", "accounts", SYSTEM_PATH, runtime.path());
        assert!(ok, "{err}");
        assert!(out.contains("Which workspace?"), "{out}");
        assert!(out.contains("in Band; apps: notes"), "{out}");
        let log = installed.log();
        let pinned = log
            .lines()
            .find(|line| line.contains("notes.pin(n3)"))
            .unwrap();
        assert!(pinned.contains(r#""workspace":"7020""#), "{pinned}");
    }
}

tilo_test_env::scenario! {
    fn without_tilo_or_a_login_the_repl_says_what_to_do;

    phase without_tilo_it_names_what_to_install;
    {
        let installed = Installed::new();
        let runtime = tempfile::tempdir().unwrap();
        let (ok, out, err) = installed.repl(&[], "", "signed-in", SYSTEM_PATH, runtime.path());
        assert!(!ok);
        assert!(out.is_empty(), "nothing was evaluated: {out}");
        assert_eq!(err.trim().lines().count(), 1, "one line: {err}");
        assert!(err.contains(tilolang_cli::repl::tilo::TILO_MISSING), "{err}");
        plain(&err);
    }

    phase a_tilo_from_before_the_repl_s_server_is_named_to_update;
    {
        let installed = Installed::new();
        installed.put_tilo("bin");
        let runtime = tempfile::tempdir().unwrap();
        let (ok, _, err) = installed.repl(&[], "", "old", SYSTEM_PATH, runtime.path());
        assert!(!ok);
        assert_eq!(err.trim().lines().count(), 1, "one line: {err}");
        assert!(err.contains("is older than this tilolang"), "{err}");
        assert!(err.contains("curl -fsSL https://tilo.sh | sh"), "{err}");
    }

    phase nobody_signed_in_reads_how_to_sign_in;
    {
        let installed = Installed::new();
        installed.put_tilo("bin");
        let runtime = tempfile::tempdir().unwrap();
        let (ok, out, err) = installed.repl(&[], "", "nobody", SYSTEM_PATH, runtime.path());
        assert!(!ok);
        assert!(out.is_empty(), "nothing was evaluated: {out}");
        assert!(err.contains(tilolang_cli::repl::NOT_SIGNED_IN), "{err}");
        assert!(!err.contains("--session"), "no session is open here: {err}");
        plain(&err);
        let (_, _, err) = installed.repl(&["--session"], "", "nobody", SYSTEM_PATH, runtime.path());
        assert!(err.contains("No nested Tilo session is open"), "{err}");
    }

    phase a_running_session_is_named_beside_signing_in;
    {
        // A nested session as dist/dev/nested-session.sh leaves it: its session file in its
        // runtime directory, and its login's directory answering.
        let installed = Installed::new();
        installed.put_tilo("bin");
        let runtime = tempfile::tempdir().unwrap();
        let nested = runtime.path().join("tilo-nested-0123456789ab");
        std::fs::create_dir_all(nested.join("tilo-session")).unwrap();
        std::fs::write(nested.join("control.sock"), b"").unwrap();
        std::fs::write(
            nested.join("session.json"),
            serde_json::json!({
                "control": nested.join("control.sock"),
                "runtime": nested,
                "state": installed.path("state/session"),
            })
            .to_string(),
        )
        .unwrap();
        let _answering =
            std::os::unix::net::UnixListener::bind(nested.join("tilo-session/api.sock")).unwrap();
        let (_, _, err) = installed.repl(&[], "", "nobody", SYSTEM_PATH, runtime.path());
        assert!(err.contains(tilolang_cli::repl::NOT_SIGNED_IN), "{err}");
        assert!(
            err.contains("`tilolang repl --session` works in it"),
            "the person reads how to work in the session: {err}"
        );
        // In that session, `tilo` is pointed at the session's own API.
        let (ok, out, err) =
            installed.repl(&["--session"], "", "signed-in", SYSTEM_PATH, runtime.path());
        assert!(ok, "{err}");
        assert!(out.contains("in Home"), "{out}");
        let socket = format!("socket: {}\n", nested.join("tilo-session/api.sock").display());
        assert!(installed.log().contains(&socket), "{}", installed.log());
    }
}
