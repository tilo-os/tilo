//! Marking commands for Terminal: where each command starts and ends, with its
//! command line, working directory and exit status, written into the pty stream as marks
//! only Terminal can read.
//!
//! Terminal names a key per session (`"commands"` in `open-session`). A `bash` shell gets it
//! through [`KEY_ENV`] in its rc file ([`crate::shell_start::bash_rc`]), where a `DEBUG`
//! trap and a `PROMPT_COMMAND` write
//!
//! ```text
//! ESC ] 133 ; A ; k=<key> BEL                                       the prompt is shown
//! ESC ] 133 ; C ; k=<key> ; cmd=<b64> ; cwd=<b64> ; t=<n> BEL       a command line starts
//! ESC ] 133 ; D ; <status> ; k=<key> ; t=<n> BEL                    it ended with <status>
//! ```
//!
//! The rc file takes the key into a shell variable and out of the environment before
//! anything runs, so no program the session runs can write a mark Terminal believes. `t`
//! is the command's number in the shell's history, so an empty line writes none.

use std::path::Path;

pub const KEY_ENV: &str = "TILO_USH_MARKS";

/// Whether `key` may mark a session's runs: 16 to 64 ASCII letters and
/// digits, so it is safe inside a mark and a shell word.
#[must_use]
pub fn valid_key(key: &str) -> bool {
    (16..=64).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[must_use]
pub fn is_bash(shell: &Path) -> bool {
    shell.file_name().is_some_and(|name| name == "bash")
}

pub(crate) const MARKS: &str = r#"__ush_marks_key=$TILO_USH_MARKS
unset TILO_USH_MARKS
__ush_marks_armed=0
__ush_marks_ran=0
__ush_marks_seen=
__ush_marks_b64() { printf '%s' "$1" | base64 | tr -d '\n'; }
# The number and the line of the command being run, from the shell's own
# history: 0 and nothing when the history holds neither.
__ush_marks_read() {
  __ush_marks_number=0
  __ush_marks_line=
  local entry=${1-$(HISTTIMEFORMAT= builtin history 1)}
  entry=${entry#"${entry%%[![:space:]]*}"}
  __ush_marks_number=${entry%%[![:digit:]]*}
  __ush_marks_number=${__ush_marks_number:-0}
  __ush_marks_line=${entry#"$__ush_marks_number"}
  __ush_marks_line=${__ush_marks_line#"${__ush_marks_line%%[![:space:]]*}"}
}
# A command line starts: the history number says which, so a line that runs
# nothing (an empty one, `exit`) marks no run, and Terminal can tell a line
# its own `run` typed from one the person typed.
__ush_marks_start() {
  __ush_marks_seen=$__ush_marks_number
  __ush_marks_ran=1
  printf '\033]133;C;k=%s;cmd=%s;cwd=%s;t=%s\007' "$__ush_marks_key" \
    "$(__ush_marks_b64 "$__ush_marks_line")" "$(__ush_marks_b64 "$PWD")" \
    "$__ush_marks_number"
}
__ush_marks_pre() {
  [ "$__ush_marks_armed" = 1 ] || return 0
  case "$BASH_COMMAND" in __ush_marks_post*) return 0 ;; esac
  __ush_marks_armed=0
  __ush_marks_read
  if [ "$__ush_marks_number" = 0 ] || [ "$__ush_marks_number" = "$__ush_marks_seen" ]; then
    return 0
  fi
  __ush_marks_start
}
__ush_marks_post() {
  local status=$?
  if [ "$__ush_marks_ran" = 1 ]; then
    printf '\033]133;D;%s;k=%s;t=%s\007' "$status" "$__ush_marks_key" "$__ush_marks_seen"
    __ush_marks_ran=0
  fi
  __ush_marks_read
  if [ "$__ush_marks_number" = 0 ] || [ "$__ush_marks_number" = "$__ush_marks_seen" ]; then
    printf '\033]133;A;k=%s\007' "$__ush_marks_key"
    __ush_marks_armed=1
    return $status
  fi
  __ush_marks_start
  __ush_marks_pre
  return $status
}
PROMPT_COMMAND="__ush_marks_post${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
trap '__ush_marks_pre' DEBUG
"#;
