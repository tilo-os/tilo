//! Independent jobs: a child process that lives a life of its own.
//!
//! A language server, a build, a shell: a process an app starts that can
//! grow far beyond the app. Started through [`independent`], it runs in a
//! cgroup of its own below its app's on a Tilo session
//! (`runtime_portal::jobs`), so when memory runs out it ends alone and the
//! app keeps running, and the person is told which process ended. The app
//! declares nothing for it. Everywhere else (macOS, the web, Linux outside a
//! Tilo session, or without the `jobs` feature) the command is left as it
//! was and starts as any child does.

use std::process::Command;

/// Start `command` as an independent job named `label` (its program's name,
/// as the person knows it) where the system offers one.
pub fn independent<'a>(command: &'a mut Command, label: &str) -> &'a mut Command {
    backend::independent(command, label)
}

#[cfg(all(target_os = "linux", feature = "jobs"))]
mod backend {
    use std::process::Command;

    pub fn independent<'a>(command: &'a mut Command, label: &str) -> &'a mut Command {
        runtime_portal::jobs::independent(command, label)
    }
}

#[cfg(not(all(target_os = "linux", feature = "jobs")))]
mod backend {
    use std::process::Command;

    pub fn independent<'a>(command: &'a mut Command, _label: &str) -> &'a mut Command {
        command
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn outside_a_tilo_session_the_command_is_left_as_it_was() {
        // No launcher door in a test process: nothing is added.
        let mut command = std::process::Command::new("/bin/true");
        let program = super::independent(&mut command, "true")
            .get_program()
            .to_owned();
        assert_eq!(program, "/bin/true");
    }
}
