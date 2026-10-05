//! Opening a container shell in the Tilo terminal.
//!
//! "Enter distrobox" and a container's "Attach shell" ask the shell's typed
//! tool service for Terminal attached to that container. The request names
//! only the container and its engine; the shell builds the command and runs it
//! inside Terminal's confined guest. The intents are built by pure helpers
//! ([`distrobox_enter_intent`], [`exec_attach_intent`]) so the wiring is
//! testable headlessly.

use runtime_portal::tools::{ContainerEngine, ToolIntent};

/// Terminal entering the distrobox `box_name`.
pub fn distrobox_enter_intent(box_name: &str) -> ToolIntent {
    ToolIntent::ContainerShell {
        container: box_name.to_owned(),
        engine: ContainerEngine::Distrobox,
    }
}

/// Terminal attached to an interactive shell in the running podman container
/// `container_id`.
pub fn exec_attach_intent(container_id: &str) -> ToolIntent {
    ToolIntent::ContainerShell {
        container: container_id.to_owned(),
        engine: ContainerEngine::Podman,
    }
}

/// Ask the shell to open `intent` in Terminal.
///
/// # Errors
///
/// Returns the reason when the intent is invalid (for example a container name
/// that is not a plain identifier), there is no Tilo session to ask, or the
/// shell could not open Terminal.
pub fn open(intent: ToolIntent) -> Result<(), String> {
    runtime_portal::tools::open_tool(intent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distrobox enter and exec attach name their container and engine, and a hostile name never reaches the shell.
    #[test]
    fn container_shell_intents_name_the_container_and_engine() {
        // --- phase: enter_runs_distrobox_enter ---
        {
            let intent = distrobox_enter_intent("my-distrobox");
            assert_eq!(
                intent.terminal_argv().unwrap(),
                ["distrobox", "enter", "my-distrobox"]
            );
        }

        // --- phase: exec_attach_runs_a_shell_in_the_container ---
        {
            let intent = exec_attach_intent("9f3c1a7be4d2");
            assert_eq!(
                intent.terminal_argv().unwrap(),
                ["podman", "exec", "-it", "9f3c1a7be4d2", "/bin/sh"]
            );
        }

        // --- phase: hostile_name_is_refused_before_any_request ---
        {
            let intent = distrobox_enter_intent("a'; rm -rf ~ #");
            assert!(intent.validate().is_err());
            assert!(open(intent).is_err());
        }
    }
}
