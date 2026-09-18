//! Semantic platform conventions for app-authored controls and shortcuts.
//!
//! Apps describe intent (`command + K`, primary vs cancel); the host chooses
//! the modifier label, event modifier, and dialog ordering for its platform.

/// Platform family relevant to human-interface conventions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    MacOs,
    Windows,
    Linux,
    Other,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// Visual order of a primary confirmation and its secondary/cancel action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogActionOrder {
    /// Secondary/cancel first, primary at the trailing edge (macOS/GNOME).
    PrimaryLast,
    /// Primary first, secondary/cancel at the trailing edge (Windows).
    PrimaryFirst,
}

/// Host-selected conventions. Copyable so it can be propagated through a
/// retained tree and overridden by deterministic tests or a native shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformConventions {
    platform: Platform,
}

impl Default for PlatformConventions {
    fn default() -> Self {
        Self::new(Platform::current())
    }
}

impl PlatformConventions {
    pub const fn new(platform: Platform) -> Self {
        Self { platform }
    }

    pub const fn platform(self) -> Platform {
        self.platform
    }

    pub const fn dialog_action_order(self) -> DialogActionOrder {
        match self.platform {
            Platform::Windows => DialogActionOrder::PrimaryFirst,
            Platform::MacOs | Platform::Linux | Platform::Other => DialogActionOrder::PrimaryLast,
        }
    }

    /// Whether the platform's primary command modifier is currently held.
    pub fn command_modifier_down(self, modifiers: winit::keyboard::ModifiersState) -> bool {
        match self.platform {
            Platform::MacOs => modifiers.super_key(),
            Platform::Windows | Platform::Linux | Platform::Other => modifiers.control_key(),
        }
    }
}

/// A semantic shortcut whose display and matching use platform conventions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    key: String,
    command: bool,
    shift: bool,
    alt: bool,
}

impl Shortcut {
    pub fn command(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            command: true,
            shift: false,
            alt: false,
        }
    }

    #[must_use]
    pub fn shift(mut self) -> Self {
        self.shift = true;
        self
    }

    #[must_use]
    pub fn alt(mut self) -> Self {
        self.alt = true;
        self
    }

    pub fn label(&self, conventions: PlatformConventions) -> String {
        let parts = self.keycap_labels(conventions);
        if conventions.platform() == Platform::MacOs {
            parts.concat()
        } else {
            parts.join("+")
        }
    }

    /// Labels to paint as individual physical-looking caps.
    pub fn keycap_labels(&self, conventions: PlatformConventions) -> Vec<String> {
        let mut parts = Vec::with_capacity(4);
        if self.command {
            parts.push(match conventions.platform() {
                Platform::MacOs => "⌘".to_owned(),
                Platform::Windows | Platform::Linux | Platform::Other => "Ctrl".to_owned(),
            });
        }
        if self.alt {
            parts.push(match conventions.platform() {
                Platform::MacOs => "⌥".to_owned(),
                Platform::Windows | Platform::Linux | Platform::Other => "Alt".to_owned(),
            });
        }
        if self.shift {
            parts.push(match conventions.platform() {
                Platform::MacOs => "⇧".to_owned(),
                Platform::Windows | Platform::Linux | Platform::Other => "Shift".to_owned(),
            });
        }
        parts.push(self.key.clone());
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_shortcut_formats_for_each_platform() {
        let shortcut = Shortcut::command("K").shift();
        assert_eq!(
            shortcut.label(PlatformConventions::new(Platform::MacOs)),
            "⌘⇧K"
        );
        assert_eq!(
            shortcut.label(PlatformConventions::new(Platform::Windows)),
            "Ctrl+Shift+K"
        );
        assert_eq!(
            shortcut.label(PlatformConventions::new(Platform::Linux)),
            "Ctrl+Shift+K"
        );
    }

    #[test]
    fn command_modifier_and_dialog_order_follow_platform_hig() {
        let control = winit::keyboard::ModifiersState::CONTROL;
        let command = winit::keyboard::ModifiersState::SUPER;
        let mac = PlatformConventions::new(Platform::MacOs);
        let windows = PlatformConventions::new(Platform::Windows);
        assert!(mac.command_modifier_down(command));
        assert!(!mac.command_modifier_down(control));
        assert!(windows.command_modifier_down(control));
        assert!(!windows.command_modifier_down(command));
        assert_eq!(mac.dialog_action_order(), DialogActionOrder::PrimaryLast);
        assert_eq!(
            windows.dialog_action_order(),
            DialogActionOrder::PrimaryFirst
        );
    }
}
