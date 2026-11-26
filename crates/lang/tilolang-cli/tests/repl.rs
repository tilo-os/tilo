//! `tilolang repl` as a person runs it: `tilolang` installed with a `tilo` beside it, or on
//! `PATH`, or none. The `tilo` here is a stand-in speaking the lines `tilo mcp --as-me` speaks
//! (`crates/core/tilo-cli/src/mcp/person.rs`): the REPL starts it, lists, runs programs and reads
//! refusals through it, and says what to do when it is missing or nobody is signed in.

use std::path::Path;

use crate::support::installed::{Installed, SYSTEM_PATH, plain};

/// A `tilo` answering MCP's JSON-RPC lines as `tilo mcp --as-me` does, its answers picked by
/// `TILO_FAKE`: `signed-in` (the login's workspace, Home), `account` (no default workspace;
/// the account lists one, Solo), `accounts` (the account lists two, Solo and Band), `nobody` (nobody signed in) or `old` (a `tilo` from before
/// `--as-me`, which refuses it). It writes its arguments, then
/// the runtime directory it was started in, then every line it reads, to `TILO_FAKE_LOG`.
const FAKE_TILO: &str = r#"#!/bin/sh
printf 'args: %s\n' "$*" >> "$TILO_FAKE_LOG"
printf 'runtime: %s\n' "$XDG_RUNTIME_DIR" >> "$TILO_FAKE_LOG"
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

/// `tilolang repl` with `args`, typed `entries`, `tilo` answering as `fake` and `path` its
/// `PATH`: whether it succeeded, its output and its errors.
fn repl(
    installed: &Installed,
    args: &[&str],
    entries: &str,
    fake: &str,
    path: &str,
    runtime: &Path,
) -> (bool, String, String) {
    let mut all = vec!["repl"];
    all.extend_from_slice(args);
    all.extend(["--color", "never"]);
    let ran = installed.run(&all, entries, fake, path, runtime);
    (ran.ok, ran.out, ran.err)
}

tilo_test_env::scenario! {
    fn the_repl_works_in_the_workspace_through_the_tilo_beside_it;

    phase it_lists_runs_and_reads_refusals_as_the_person;
    {
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
        let runtime = tempfile::tempdir().unwrap();
        let (ok, out, err) = repl(&installed, &["--workspace", "Home", "--key-file", "/keys/ada.key"],
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
        installed.put_tilo("path", FAKE_TILO);
        let runtime = tempfile::tempdir().unwrap();
        let path = format!("{}:{SYSTEM_PATH}", installed.path("path").display());
        let (ok, out, err) =
            repl(&installed, &[], "notes.pin(n3)\n", "account", &path, runtime.path());
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
        installed.put_tilo("bin", FAKE_TILO);
        let runtime = tempfile::tempdir().unwrap();
        let (ok, out, err) =
            repl(&installed, &[], "2\nnotes.pin(n3)\n", "accounts", SYSTEM_PATH, runtime.path());
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
        let (ok, out, err) = repl(&installed, &[], "", "signed-in", SYSTEM_PATH, runtime.path());
        assert!(!ok);
        assert!(out.is_empty(), "nothing was evaluated: {out}");
        assert_eq!(err.trim().lines().count(), 1, "one line: {err}");
        assert!(err.contains(tilolang_cli::repl::tilo::TILO_MISSING), "{err}");
        plain(&err);
    }

    phase a_tilo_from_before_the_repl_s_server_is_named_to_update;
    {
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
        let runtime = tempfile::tempdir().unwrap();
        let (ok, _, err) = repl(&installed, &[], "", "old", SYSTEM_PATH, runtime.path());
        assert!(!ok);
        assert_eq!(err.trim().lines().count(), 1, "one line: {err}");
        assert!(err.contains("is older than this tilolang"), "{err}");
        assert!(err.contains("curl -fsSL https://tilo.sh | sh"), "{err}");
    }

    phase nobody_signed_in_reads_how_to_sign_in;
    {
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
        let runtime = tempfile::tempdir().unwrap();
        let (ok, out, err) = repl(&installed, &[], "", "nobody", SYSTEM_PATH, runtime.path());
        assert!(!ok);
        assert!(out.is_empty(), "nothing was evaluated: {out}");
        assert!(err.contains(tilolang_cli::repl::NOT_SIGNED_IN), "{err}");
        assert!(!err.contains("--session"), "no session is open here: {err}");
        plain(&err);
        let (_, _, err) = repl(&installed, &["--session"], "", "nobody", SYSTEM_PATH, runtime.path());
        assert!(err.contains("No nested Tilo session is open"), "{err}");
    }

    phase a_running_session_is_named_beside_signing_in;
    {
        // A nested session as dist/dev/nested-session.sh leaves it: its session file in its
        // runtime directory, and its login's directory answering.
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
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
        let (_, _, err) = repl(&installed, &[], "", "nobody", SYSTEM_PATH, runtime.path());
        assert!(err.contains(tilolang_cli::repl::NOT_SIGNED_IN), "{err}");
        assert!(
            err.contains("`tilolang repl --session` works in it"),
            "the person reads how to work in the session: {err}"
        );
        // In that session, `tilo` runs in the session's own login, reaching its daemon.
        let (ok, out, err) =
            repl(&installed, &["--session"], "", "signed-in", SYSTEM_PATH, runtime.path());
        assert!(ok, "{err}");
        assert!(out.contains("in Home"), "{out}");
        let reached = format!("runtime: {}\n", nested.display());
        assert!(installed.log().contains(&reached), "{}", installed.log());
    }
}
