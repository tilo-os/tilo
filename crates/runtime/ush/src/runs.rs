//! Marking command runs for Terminal: where each command a session's shell
//! runs starts and ends, with its command line, working directory and exit
//! status, written into the pty stream as marks only Terminal can read
//! (docs/app-model/ush.md, "Command runs").
//!
//! Terminal names a key per session (`"runs"` in `open-session`). When the
//! session's shell is `bash`, it starts with [`bash_rc`] as its rc file: the
//! person's own rc first, then a `DEBUG` trap and a `PROMPT_COMMAND` that
//! write
//!
//! ```text
//! ESC ] 133 ; A ; k=<key> BEL                           the prompt is shown
//! ESC ] 133 ; C ; k=<key> ; cmd=<b64> ; cwd=<b64> BEL   a command line starts
//! ESC ] 133 ; D ; <status> ; k=<key> BEL                it ended with <status>
//! ```
//!
//! The key reaches the shell in [`KEY_ENV`], which the rc file takes into a
//! shell variable and out of the environment before anything runs, so no
//! program the session runs can write a mark Terminal believes.

use std::path::Path;

/// The environment variable that carries the session's key to the shell (and,
/// from the supervisor, to the `run` process that builds the guest).
pub const KEY_ENV: &str = "TILO_USH_RUNS";

/// Where the rc file is in a guest.
pub const GUEST_RC: &str = "/run/ush/runs.bash";

/// Whether `key` may mark a session's runs: 16 to 64 ASCII letters and
/// digits, so it is safe inside a mark and a shell word.
#[must_use]
pub fn valid_key(key: &str) -> bool {
    (16..=64).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Whether `shell` is `bash`, the shell the marks are written for.
#[must_use]
pub fn is_bash(shell: &Path) -> bool {
    shell.file_name().is_some_and(|name| name == "bash")
}

/// The rc file of a marked `bash`: `/etc/profile` first when `profile`
/// (the Host target's login shell, which this replaces), then the person's
/// `~/.bashrc`, then the marks.
#[must_use]
pub fn bash_rc(profile: bool) -> String {
    let mut rc = String::from("# ush: marks each command run for Terminal\n");
    if profile {
        rc.push_str("[ -f /etc/profile ] && . /etc/profile\n");
    }
    rc.push_str(MARKS);
    rc
}

const MARKS: &str = r#"[ -f ~/.bashrc ] && . ~/.bashrc
__ush_runs_key=$TILO_USH_RUNS
unset TILO_USH_RUNS
__ush_runs_armed=0
__ush_runs_ran=0
__ush_runs_seen=
__ush_runs_b64() { printf '%s' "$1" | base64 | tr -d '\n'; }
__ush_runs_pre() {
  [ "$__ush_runs_armed" = 1 ] || return 0
  case "$BASH_COMMAND" in __ush_runs_post*) return 0 ;; esac
  __ush_runs_armed=0
  local entry number line
  entry=$(HISTTIMEFORMAT= builtin history 1)
  entry=${entry#"${entry%%[![:space:]]*}"}
  number=${entry%%[![:digit:]]*}
  line=${entry#"$number"}
  line=${line#"${line%%[![:space:]]*}"}
  if [ -z "$line" ] || [ "$number" = "$__ush_runs_seen" ]; then
    line=$BASH_COMMAND
  fi
  __ush_runs_seen=$number
  __ush_runs_ran=1
  printf '\033]133;C;k=%s;cmd=%s;cwd=%s\007' "$__ush_runs_key" \
    "$(__ush_runs_b64 "$line")" "$(__ush_runs_b64 "$PWD")"
}
__ush_runs_post() {
  local status=$?
  if [ "$__ush_runs_ran" = 1 ]; then
    printf '\033]133;D;%s;k=%s\007' "$status" "$__ush_runs_key"
    __ush_runs_ran=0
  fi
  printf '\033]133;A;k=%s\007' "$__ush_runs_key"
  __ush_runs_armed=1
  return $status
}
PROMPT_COMMAND="__ush_runs_post${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
trap '__ush_runs_pre' DEBUG
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    #[test]
    fn keys_are_letters_and_digits_of_a_fair_length() {
        assert!(valid_key("abcdefghij012345"));
        assert!(!valid_key("short"));
        assert!(!valid_key("abcdefghij01234;"));
        assert!(!valid_key(&"a".repeat(65)));
        assert!(is_bash(Path::new("/usr/bin/bash")));
        assert!(!is_bash(Path::new("/usr/bin/zsh")));
    }

    /// A real `bash` with the rc file marks each command with its line,
    /// directory and status, and no program it runs sees the key.
    #[test]
    fn bash_marks_each_run_and_hides_the_key() {
        let Ok(bash) = which("bash") else {
            eprintln!("no bash on this machine; the marks are not exercised");
            return;
        };
        let home = tempfile::tempdir().unwrap();
        let rc = home.path().join("runs.bash");
        std::fs::write(&rc, bash_rc(false)).unwrap();
        let key = "k3y0fth1ss3ss10n";
        let output = std::process::Command::new(bash)
            .args(["--rcfile", rc.to_str().unwrap(), "-i"])
            .env("HOME", home.path())
            .env(KEY_ENV, key)
            .env("HISTFILE", home.path().join("history"))
            .current_dir(home.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write as _;
                child.stdin.take().unwrap().write_all(
                    b"echo hello | tr h j\nfalse\nenv | grep -c TILO_USH_RUNS\nexit\n",
                )?;
                child.wait_with_output()
            })
            .unwrap();
        let out = String::from_utf8_lossy(&output.stdout);
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let cwd = b64(&home.path().display().to_string());
        let started = |line: &str| format!("\x1b]133;C;k={key};cmd={};cwd={cwd}\x07", b64(line));
        assert!(out.contains(&started("echo hello | tr h j")), "{out:?}");
        assert!(out.contains("jello"), "{out:?}");
        assert!(
            out.contains(&format!("\x1b]133;D;0;k={key}\x07")),
            "{out:?}"
        );
        assert!(out.contains(&started("false")), "{out:?}");
        assert!(
            out.contains(&format!("\x1b]133;D;1;k={key}\x07")),
            "{out:?}"
        );
        assert!(
            out.contains(&format!("{}0\n", started("env | grep -c TILO_USH_RUNS"))),
            "no program sees the key: {out:?}"
        );
        assert!(out.contains(&format!("\x1b]133;A;k={key}\x07")), "{out:?}");
    }

    fn which(program: &str) -> Result<std::path::PathBuf, ()> {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
            .map(|dir| dir.join(program))
            .find(|path| path.is_file())
            .ok_or(())
    }
}
