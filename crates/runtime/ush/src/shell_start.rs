//! How a session's interactive shell starts: one path for the Host and for
//! guests (docs/app-model/ush.md).
//!
//! The shell is the session's interactive login shell. `bash` starts with
//! one rc file ([`bash_rc`]): what must run before any of the person's files
//! (the native editor's builtin, in a guest), then what `bash --login`
//! reads, then the marks of [`crate::runs`] when Terminal asked for them.
//! Any other shell starts with `-l`.

use std::ffi::OsString;
use std::path::Path;

/// Where a guest's shell finds its rc file.
pub const GUEST_RC: &str = "/run/ush/shell.bash";

/// The rc file of a session's interactive `bash`. `prelude` runs first,
/// before any of the person's files. Then what `bash --login` reads, which
/// `--rcfile` would otherwise skip: `/etc/profile` (and with it the system's
/// completion setup in `/etc/profile.d`), then the first of the person's
/// `~/.bash_profile`, `~/.bash_login` and `~/.profile`; a home with none of
/// them still gets its `~/.bashrc`. The marks come last when `marks`, so
/// nothing the person's files set replaces them.
#[must_use]
pub fn bash_rc(prelude: &str, marks: bool) -> String {
    let mut rc = String::from("# ush: the session's interactive login shell\n");
    rc.push_str(prelude);
    rc.push_str(LOGIN);
    if marks {
        rc.push_str(crate::runs::MARKS);
    }
    rc
}

const LOGIN: &str = r"[ -f /etc/profile ] && . /etc/profile
if [ -f ~/.bash_profile ]; then . ~/.bash_profile
elif [ -f ~/.bash_login ]; then . ~/.bash_login
elif [ -f ~/.profile ]; then . ~/.profile
elif [ -f ~/.bashrc ]; then . ~/.bashrc
fi
";

/// The arguments that start `shell` as the session's interactive login
/// shell: `bash` reads `rc` (from [`bash_rc`]); any other shell, or a `bash`
/// whose rc could not be written, starts with `-l`.
#[must_use]
pub fn interactive_args(shell: &Path, rc: Option<&Path>) -> Vec<OsString> {
    match rc {
        Some(rc) if crate::runs::is_bash(shell) => {
            vec!["--rcfile".into(), rc.as_os_str().to_owned(), "-i".into()]
        }
        _ => vec!["-l".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_reads_its_rc_and_other_shells_log_in() {
        let rc = Path::new("/run/ush/shell.bash");
        assert_eq!(
            interactive_args(Path::new("/usr/bin/bash"), Some(rc)),
            ["--rcfile", "/run/ush/shell.bash", "-i"]
        );
        assert_eq!(
            interactive_args(Path::new("/usr/bin/fish"), Some(rc)),
            ["-l"]
        );
        assert_eq!(interactive_args(Path::new("/usr/bin/bash"), None), ["-l"]);
    }

    #[test]
    fn the_prelude_runs_before_the_persons_files_and_the_marks_after() {
        let rc = bash_rc("enable tilo_editable\n", true);
        let prelude = rc.find("enable tilo_editable").unwrap();
        let profile = rc.find(". /etc/profile").unwrap();
        let marks = rc.find("__ush_runs_key").unwrap();
        assert!(prelude < profile && profile < marks, "{rc}");
        assert!(!bash_rc("", false).contains("__ush_runs_key"));
    }
}
