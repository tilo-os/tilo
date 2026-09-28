//! Display tools the person lets rearrange their displays without asking.
//!
//! An external layout tool (`kanshi`, `way-displays`, `wlr-randr`) changes
//! the displays through the compositor's privileged output-management
//! socket. Its changes apply directly, except a risky one — a custom mode,
//! or a display in use turned off or given a mode it never showed — which
//! waits for the person to keep it. A tool they chose to trust is recorded
//! here, keyed by the executable the compositor read from the connection's
//! peer credentials, so even its risky changes are kept without a prompt.
//! Settings › Displays lists and revokes them.

use serde::{Deserialize, Serialize};

/// A program on the privileged display socket, identified by its executable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayTool {
    /// Absolute path of the executable (`/usr/bin/kanshi`).
    pub executable: String,
}

impl DisplayTool {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    /// What the person calls it: the executable's file name.
    pub fn name(&self) -> &str {
        self.executable
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.executable)
    }
}

/// Every tool whose risky display changes are kept without asking.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustedDisplayTools {
    pub tools: Vec<DisplayTool>,
}

impl TrustedDisplayTools {
    const SECTION: &'static str = "display_tools";

    pub fn load() -> Self {
        serde_json::from_value(serde_json::Value::Object(super::load_section(
            Self::SECTION,
        )))
        .unwrap_or_default()
    }

    /// # Errors
    ///
    /// Returns an error if the settings section can't be written.
    pub fn save(&self) -> std::io::Result<()> {
        let value = serde_json::to_value(self).map_err(std::io::Error::other)?;
        super::update_section(
            Self::SECTION,
            value.as_object().cloned().unwrap_or_default(),
        )
    }

    pub fn contains(&self, tool: &DisplayTool) -> bool {
        self.tools.iter().any(|known| known == tool)
    }

    /// Add `tool`; returns whether it was new.
    pub fn insert(&mut self, tool: DisplayTool) -> bool {
        if self.contains(&tool) {
            return false;
        }
        self.tools.push(tool);
        true
    }

    /// Remove the tool with this executable; returns whether it was trusted.
    pub fn remove(&mut self, executable: &str) -> bool {
        let before = self.tools.len();
        self.tools.retain(|tool| tool.executable != executable);
        self.tools.len() != before
    }

    /// Trust `tool` from now on.
    ///
    /// # Errors
    ///
    /// Returns an error if the settings section can't be written.
    pub fn trust(tool: DisplayTool) -> std::io::Result<()> {
        let mut trusted = Self::load();
        if trusted.insert(tool) {
            trusted.save()?;
        }
        Ok(())
    }

    /// Ask before keeping this tool's changes again.
    ///
    /// # Errors
    ///
    /// Returns an error if the settings section can't be written.
    pub fn forget(executable: &str) -> std::io::Result<()> {
        let mut trusted = Self::load();
        if trusted.remove(executable) {
            trusted.save()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_is_named_after_its_executable() {
        assert_eq!(DisplayTool::new("/usr/bin/kanshi").name(), "kanshi");
        assert_eq!(DisplayTool::new("kanshi").name(), "kanshi");
    }

    #[test]
    fn trust_is_per_executable_and_revocable() {
        let mut trusted = TrustedDisplayTools::default();
        assert!(trusted.insert(DisplayTool::new("/usr/bin/kanshi")));
        assert!(!trusted.insert(DisplayTool::new("/usr/bin/kanshi")));
        assert!(trusted.contains(&DisplayTool::new("/usr/bin/kanshi")));
        assert!(!trusted.contains(&DisplayTool::new("/home/me/bin/kanshi")));
        assert!(trusted.remove("/usr/bin/kanshi"));
        assert!(!trusted.remove("/usr/bin/kanshi"));
        assert!(trusted.tools.is_empty());
    }

    #[test]
    fn an_unreadable_section_trusts_nothing() {
        let parsed: TrustedDisplayTools =
            serde_json::from_value(serde_json::json!({ "tools": "kanshi" })).unwrap_or_default();
        assert!(parsed.tools.is_empty());
    }
}
