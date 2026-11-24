//! `tilolang push` as a person runs it: a build of their app, handed to a workspace through the
//! installed `tilo` (`tilo mcp --as-me`), or a sentence saying what stands in the way: no `tilo`,
//! one from before `--as-me`, nobody signed in, a workspace the key does not reach, or a
//! workspace that refuses the push.

use std::path::{Path, PathBuf};

use tilo_test_env::SpawnUnderEnvLock as _;

use crate::support::installed::{Installed, SYSTEM_PATH, plain};

/// A `tilo` answering the lines `tilo mcp --as-me` answers, as `TILO_FAKE` picks: `signed-in`
/// (the workspace `--workspace` names is Home, and it takes the push), `refused` (Home refuses the
/// install), `approval` (the install waits for an owner's approval, then is done), `nowhere` (the
/// key reaches no workspace by that name), `nobody` (nobody signed in) or `old` (a `tilo` from
/// before `--as-me`). It writes its arguments, then every line it reads, to `TILO_FAKE_LOG`.
const FAKE_TILO: &str = r#"#!/bin/sh
printf 'args: %s\n' "$*" >> "$TILO_FAKE_LOG"
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
        nowhere) answer '"error":{"code":-32002,"message":"no workspace called \"Nope\": this key reaches Home","data":{"reason":"unavailable"}}' ;;
        *) answer '"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"tilo"},"_meta":{"so.tilo.mcp":{"workspace":{"id":"3ee4a797","name":"Home"}}}}' ;;
      esac ;;
    *'workspace/apps#upload'*)
      typed '{"status":"ok","invocation_id":"u1","result":{}}' ;;
    *'workspace/apps#install'*)
      case $TILO_FAKE in
        refused) answer '"result":{"isError":true,"content":[{"type":"text","text":"only the workspace owners install apps"}],"structuredContent":{"error":{"code":"forbidden","message":"only the workspace owners install apps"}}}' ;;
        approval) typed '{"status":"pending","invocation_id":"i2","ask":{"url":"https://tilo.sh/approve/i2"}}' ;;
        *) typed '{"status":"ok","invocation_id":"i1","result":{"installed":true}}' ;;
      esac ;;
    *'"tilo_status"'*)
      typed '{"status":"ok","invocation_id":"i2","result":{"installed":true}}' ;;
    *) answer '"error":{"code":-32601,"message":"no such call"}' ;;
  esac
done
"#;

/// An app's directory holding a signed build of it: its source, the surface the workspace decides
/// its changes against, and the `.tapp` `tilolang build` signed with a key of this run's. It is
/// outside any Cargo workspace, as a person's app is.
fn built_app(installed: &Installed) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path();
    std::fs::write(
        app.join("app.tl"),
        "app t.push \"Push\"\ntype C {\n    n: Int\n}\nroot C\ndefault view v for C\nview v(c: C) {\n    text(\"{c.n}\")\n}\n",
    )
    .unwrap();
    std::fs::write(app.join("surface.toml"), "resources = []\n").unwrap();
    let dist = app.join("dist");
    let key = installed.path("state/publisher.json");
    let built = std::process::Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .arg("build")
        .arg(app)
        .arg("--out")
        .arg(&dist)
        .arg("--identity")
        .arg(&key)
        .output_locked()
        .unwrap();
    assert!(
        built.status.success(),
        "tilolang build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let package = std::fs::read_dir(&dist)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "tapp")
        })
        .expect("the build's package");
    (dir, package)
}

/// `tilolang push` of the app in `app`, with `args`, `tilo` answering as `fake`.
fn push(
    installed: &Installed,
    app: &Path,
    args: &[&str],
    fake: &str,
    path: &str,
) -> (bool, String) {
    let runtime = tempfile::tempdir().unwrap();
    let app = app.to_str().unwrap();
    let mut all = vec!["push", app];
    all.extend_from_slice(args);
    let ran = installed.run(&all, "", fake, path, runtime.path());
    (ran.ok, ran.err)
}

/// The calls `tilo` was asked to make on the workspace's apps, in order: each one's action and
/// arguments.
fn acted(log: &str) -> Vec<(String, serde_json::Value)> {
    log.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|request| request["params"]["name"] == "tilo_do")
        .map(|request| {
            let arguments = &request["params"]["arguments"];
            (
                arguments["path"].as_str().unwrap().to_owned(),
                arguments["args"].clone(),
            )
        })
        .collect()
}

tilo_test_env::scenario! {
    fn a_push_hands_the_workspace_the_build_as_the_developer;

    phase it_uploads_the_package_then_installs_it_as_the_key_in_the_workspace_named;
    {
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
        let (app, package) = built_app(&installed);
        let (ok, err) = push(
            &installed,
            app.path(),
            &["--workspace", "Home", "--key-file", "/keys/ada.key"],
            "signed-in",
            SYSTEM_PATH,
        );
        assert!(ok, "{err}");
        assert!(err.contains("t.push 0.1.0 is in 3ee4a797"), "{err}");
        let log = installed.log();
        assert!(
            log.starts_with("args: --key-file /keys/ada.key --workspace Home mcp --as-me\n"),
            "tilo acts as the key, in the workspace named, as the person: {log}"
        );
        let bytes = std::fs::read(&package).unwrap();
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let calls = acted(&log);
        let (install, uploads) = calls.split_last().expect("calls on the workspace's apps");
        assert_eq!(uploads.len(), bytes.len().div_ceil(256 * 1024), "{calls:?}");
        let mut offset = 0;
        for (path, args) in uploads {
            assert_eq!(path, "workspace/apps#upload");
            assert_eq!(args["file"], hash.as_str());
            assert_eq!(args["offset"], offset);
            assert_eq!(args["total"], bytes.len());
            offset += hex_len(&args["data"]);
        }
        assert_eq!(offset, bytes.len(), "every byte is handed over once");
        assert_eq!(install.0, "workspace/apps#install");
        assert_eq!(install.1["app"], "t.push");
        assert_eq!(install.1["file"], hash.as_str(), "the install names the uploaded file");
        assert_eq!(install.1["dev"], true);
        let titled = log
            .lines()
            .find(|line| line.contains("workspace/apps#install"))
            .unwrap();
        assert!(titled.contains(r#""title":"push t.push 0.1.0""#), "History names the push: {titled}");
    }

    phase an_install_waiting_for_approval_says_where_and_is_followed;
    {
        let installed = Installed::new();
        let path = format!("{}:{SYSTEM_PATH}", installed.path("path").display());
        installed.put_tilo("path", FAKE_TILO);
        let (app, _) = built_app(&installed);
        let (ok, err) = push(&installed, app.path(), &["--workspace", "Home"], "approval", &path);
        assert!(ok, "{err}");
        assert!(err.contains("approve it at https://tilo.sh/approve/i2"), "{err}");
        assert!(err.contains("is in 3ee4a797"), "{err}");
        let log = installed.log();
        assert!(log.starts_with("args: --workspace Home mcp --as-me\n"), "{log}");
        assert!(log.contains(r#""invocation_id":"i2""#), "the install is followed: {log}");
    }
}

tilo_test_env::scenario! {
    fn a_push_that_cannot_happen_says_why;

    phase without_tilo_it_names_what_to_install;
    {
        let installed = Installed::new();
        let (app, _) = built_app(&installed);
        let (ok, err) = push(&installed, app.path(), &["--workspace", "Home"], "signed-in", SYSTEM_PATH);
        assert!(!ok);
        assert!(err.contains(tilo_app_builder::tilo::TILO_MISSING), "{err}");
        plain(&err);
    }

    phase a_tilo_from_before_as_me_is_named_to_update;
    {
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
        let (app, _) = built_app(&installed);
        let (ok, err) = push(&installed, app.path(), &["--workspace", "Home"], "old", SYSTEM_PATH);
        assert!(!ok);
        assert!(err.contains("is older than this tilolang"), "{err}");
        assert!(err.contains("curl -fsSL https://tilo.sh | sh"), "{err}");
    }

    phase nobody_signed_in_reads_how_to_sign_in;
    {
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
        let (app, _) = built_app(&installed);
        let (ok, err) = push(&installed, app.path(), &["--workspace", "Home"], "nobody", SYSTEM_PATH);
        assert!(!ok);
        assert!(err.contains(tilo_app_builder::push::NOT_SIGNED_IN), "{err}");
        plain(&err);
        assert!(acted(&installed.log()).is_empty(), "nothing was sent");
    }

    phase a_workspace_the_key_does_not_reach_is_named;
    {
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
        let (app, _) = built_app(&installed);
        let (ok, err) = push(&installed, app.path(), &["--workspace", "Nope"], "nowhere", SYSTEM_PATH);
        assert!(!ok);
        assert!(err.contains(r#"no workspace called "Nope": this key reaches Home"#), "{err}");
        assert!(acted(&installed.log()).is_empty(), "nothing was sent");
    }

    phase a_refused_install_says_what_the_workspace_said;
    {
        let installed = Installed::new();
        installed.put_tilo("bin", FAKE_TILO);
        let (app, _) = built_app(&installed);
        let (ok, err) = push(&installed, app.path(), &["--workspace", "Home"], "refused", SYSTEM_PATH);
        assert!(!ok);
        assert!(
            err.contains("the workspace Home refused the push: only the workspace owners install apps"),
            "{err}"
        );
        plain(&err);
    }
}

/// The bytes a hex string carries.
fn hex_len(data: &serde_json::Value) -> usize {
    data.as_str().unwrap().len() / 2
}
