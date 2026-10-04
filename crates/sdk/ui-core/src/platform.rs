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
    Android,
    Other,
}

impl Platform {
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "android") {
            Self::Android
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
    /// Secondary/cancel first, primary at the trailing edge (macOS/GNOME/Material).
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
            Platform::MacOs | Platform::Linux | Platform::Android | Platform::Other => {
                DialogActionOrder::PrimaryLast
            }
        }
    }

    /// Whether the platform's primary command modifier is currently held.
    pub fn command_modifier_down(self, modifiers: winit::keyboard::ModifiersState) -> bool {
        match self.platform {
            Platform::MacOs => modifiers.super_key(),
            Platform::Windows | Platform::Linux | Platform::Android | Platform::Other => {
                modifiers.control_key()
            }
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

    /// The key this shortcut is pressed with, as a keycap legend.
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn has_shift(&self) -> bool {
        self.shift
    }

    pub fn has_alt(&self) -> bool {
        self.alt
    }

    pub fn label(&self, conventions: PlatformConventions) -> String {
        let parts = self.keycap_labels(conventions);
        if conventions.platform() == Platform::MacOs {
            parts.concat()
        } else {
            parts.join("+")
        }
    }

    /// Labels to paint as individual physical-looking caps, in the legends
    /// of the active keyboard layout ([`crate::keycaps::active`]).
    pub fn keycap_labels(&self, conventions: PlatformConventions) -> Vec<String> {
        use crate::input::KeyboardModifier;
        let keycaps = crate::keycaps::active();
        let keycaps = if conventions.platform() == Platform::MacOs {
            keycaps.on_family(crate::interaction::KeyboardFamily::Apple)
        } else {
            keycaps
        };
        [
            (self.command, KeyboardModifier::Command),
            (self.alt, KeyboardModifier::Alt),
            (self.shift, KeyboardModifier::Shift),
        ]
        .into_iter()
        .filter(|(held, _)| *held)
        .map(|(_, modifier)| keycaps.modifier(modifier).to_owned())
        .chain(std::iter::once(self.key.clone()))
        .collect()
    }
}

/// A host service every app answers by shortcut, whichever chrome shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostShortcut {
    Search,
    History,
    Undo,
    Redo,
    /// Show or hide the window's sidebar (`NavShell`), in every app.
    Sidebar,
}

impl HostShortcut {
    /// The one shortcut table. Plain Ctrl+H stays with apps (Find and
    /// Replace), so History takes Shift where ⌘Y is not the convention.
    pub fn shortcut(self, conventions: PlatformConventions) -> Shortcut {
        let mac = conventions.platform() == Platform::MacOs;
        match self {
            Self::Search => Shortcut::command("K"),
            Self::History if mac => Shortcut::command("Y"),
            Self::History => Shortcut::command("H").shift(),
            Self::Undo => Shortcut::command("Z"),
            Self::Redo if conventions.platform() == Platform::Windows => Shortcut::command("Y"),
            Self::Redo => Shortcut::command("Z").shift(),
            Self::Sidebar => Shortcut::command("L").shift(),
        }
    }

    /// Which host service a command-modified character means. Besides each
    /// platform's own spelling, Ctrl+Shift+Z and Ctrl+Y both redo off macOS.
    pub fn resolve(conventions: PlatformConventions, character: &str, shift: bool) -> Option<Self> {
        let mac = conventions.platform() == Platform::MacOs;
        match (character.to_ascii_lowercase().as_str(), shift) {
            ("k", false) => Some(Self::Search),
            ("y", false) if mac => Some(Self::History),
            ("h", true) if !mac => Some(Self::History),
            ("z", false) => Some(Self::Undo),
            ("z", true) | ("y", false) => Some(Self::Redo),
            ("l", true) => Some(Self::Sidebar),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_shortcuts_resolve_to_the_label_each_platform_shows() {
        for platform in [Platform::MacOs, Platform::Windows, Platform::Linux] {
            let conventions = PlatformConventions::new(platform);
            for service in [
                HostShortcut::Search,
                HostShortcut::History,
                HostShortcut::Undo,
                HostShortcut::Redo,
                HostShortcut::Sidebar,
            ] {
                let shortcut = service.shortcut(conventions);
                assert_eq!(
                    HostShortcut::resolve(conventions, shortcut.key(), shortcut.has_shift()),
                    Some(service),
                    "{platform:?} {service:?}"
                );
            }
        }
        let mac = PlatformConventions::new(Platform::MacOs);
        let linux = PlatformConventions::new(Platform::Linux);
        assert_eq!(HostShortcut::History.shortcut(mac).label(mac), "⌘Y");
        assert_eq!(
            HostShortcut::Sidebar.shortcut(linux).label(linux),
            "Ctrl+Shift+L"
        );
        assert_eq!(
            HostShortcut::History.shortcut(linux).label(linux),
            "Ctrl+Shift+H"
        );
        assert_eq!(
            HostShortcut::resolve(linux, "y", false),
            Some(HostShortcut::Redo)
        );
        assert_eq!(
            HostShortcut::resolve(linux, "h", false),
            None,
            "Ctrl+H stays with apps"
        );
        assert_eq!(
            HostShortcut::resolve(mac, "y", false),
            Some(HostShortcut::History)
        );
    }

    #[test]
    fn shortcuts_modifiers_and_dialog_order_follow_each_platform_hig() {
        // Case `semantic_shortcut_formats_for_each_platform`.
        {
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
            assert_eq!(
                shortcut.label(PlatformConventions::new(Platform::Android)),
                "Ctrl+Shift+K"
            );
        }

        // Case `command_modifier_and_dialog_order_follow_platform_hig`.
        {
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
            let android = PlatformConventions::new(Platform::Android);
            assert!(android.command_modifier_down(control));
            assert!(!android.command_modifier_down(command));
            assert_eq!(
                android.dialog_action_order(),
                DialogActionOrder::PrimaryLast
            );
        }
    }
}
