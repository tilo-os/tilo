//! How the Host's interactive shell starts: `bash` reads one rc file, [`bash_rc`], with
//! what `bash --login` reads and then the marks of [`crate::command_marks`] when Terminal asked for
//! them; any other shell starts with `-l`.

use std::ffi::OsString;
use std::path::Path;

/// The rc file of a session's interactive `bash`: what `bash --login` reads, which
/// `--rcfile` would otherwise skip: `/etc/profile` (and with it the system's
/// completion setup in `/etc/profile.d`), then the first of the person's
/// `~/.bash_profile`, `~/.bash_login` and `~/.profile`; a home with none of
/// them still gets its `~/.bashrc`. The marks come last when `marks`, so
/// nothing the person's files set replaces them.
#[must_use]
pub fn bash_rc(marks: bool) -> String {
    let mut rc = String::from("# ush: the session's interactive login shell\n");
    rc.push_str(LOGIN);
    if marks {
        rc.push_str(crate::command_marks::MARKS);
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
        Some(rc) if crate::command_marks::is_bash(shell) => {
            vec!["--rcfile".into(), rc.as_os_str().to_owned(), "-i".into()]
        }
        _ => vec!["-l".into()],
    }
}
