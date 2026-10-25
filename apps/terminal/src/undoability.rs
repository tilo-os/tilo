//! Whether what a run changed can be put back (docs/app-model/ush.md,
//! "A run"): a run that only looked (`ls`, `git status`, `cargo test`) is
//! marked *not undoable* whatever it touched, and a run that changed
//! something is marked *undoable* only by the commands whose changes
//! Tilo can take back — file writes and removals, the editor, the
//! package-manager-free moves of the filesystem.
//!
//! The past does not say: a run that deleted the only copy of a file is not
//! undoable, and the history must not offer to undo it. So a run that ran a
//! command this list does not know is marked not undoable, and the two
//! systems keep their own ledgers: the filesystem's history is Tilo's, and
//! what a shell did to a file is in it like any other change.

/// The commands that only read: whatever they print, nothing they run
/// changed.
const READING: &[&str] = &[
    "cat", "column", "cut", "date", "df", "diff", "du", "echo", "env", "file", "find", "free",
    "grep", "head", "help", "history", "hostname", "id", "jq", "less", "ls", "more", "printenv",
    "printf", "ps", "pwd", "rg", "sed", "sort", "stat", "tail", "tree", "uname", "uniq", "uptime",
    "wc", "which", "whoami", "xxd",
];

/// The commands whose changes Tilo can take back.
const CHANGING: &[&str] = &[
    "cp", "install", "mv", "patch", "rm", "rmdir", "tee", "touch", "truncate",
];

/// Whether `command`'s run is undoable: `Some(true)` when it changed what
/// Tilo can put back, `Some(false)` when it only read, and `None` when what
/// it changed is not something Tilo can put back (or not something known).
#[must_use]
pub fn of(command: &str) -> Option<bool> {
    let mut words = command.split_whitespace();
    // `sudo` and an assignment in front (`LANG=C grep …`) do not change what
    // the command is.
    while let Some(word) = words.next() {
        if word == "sudo" || (word.contains('=') && !word.starts_with('-')) {
            continue;
        }
        return classify(word, &words.collect::<Vec<_>>());
    }
    None
}

fn classify(program: &str, args: &[&str]) -> Option<bool> {
    let name = program.rsplit('/').next().unwrap_or(program);
    let changed_by_flags = args.iter().any(|arg| *arg == "-i" || *arg == "--in-place");
    match name {
        "git" => classify_git(args),
        // A build writes the target directory, which is the build's own and
        // not the person's content: the changes worth undoing are the files
        // they edit in an app.
        "cargo" => Some(false),
        "cp" | "ln" | "mkdir" | "mv" | "rm" | "tar" | "tee" | "truncate" | "unzip" => Some(true),
        "sed" if changed_by_flags => Some(true),
        name if READING.contains(&name) => Some(false),
        name if CHANGING.contains(&name) => Some(true),
        _ => None,
    }
}

fn classify_git(args: &[&str]) -> Option<bool> {
    let subcommand = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .copied()
        .unwrap_or("");
    match subcommand {
        "status" | "log" | "diff" | "show" | "blame" | "grep" | "rev-parse" | "branch"
        | "remote" | "describe" | "shortlog" | "ls-files" => Some(false),
        // Both change the repository, and both leave what they replaced in
        // it, which is a change Tilo's history cannot put back.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::of;

    /// The commands the person runs in a session: what only reads is never
    /// undoable, what Tilo can put back is, and the rest is not offered.
    #[test]
    fn a_runs_undoability_follows_what_its_command_can_change() {
        for command in [
            "ls -la",
            "git status --short",
            "cargo test -p terminal --lib",
            "cargo run --bin shots",
            "grep -rn TODO src",
            "env | grep -c X",
            "/usr/bin/cat notes.md",
            "LANG=C wc -l notes.md",
        ] {
            assert_eq!(of(command), Some(false), "{command}");
        }
        for command in [
            "rm -rf build",
            "mv a b",
            "cp -r src dst",
            "sed -i s/a/b/ notes.md",
            "touch notes.md",
        ] {
            assert_eq!(of(command), Some(true), "{command}");
        }
        for command in [
            "git commit -m 'save'",
            "git checkout main",
            "pandoc notes.md -o notes.pdf",
            "",
            "FOO=1",
        ] {
            assert_eq!(of(command), None, "{command}");
        }
    }
}
